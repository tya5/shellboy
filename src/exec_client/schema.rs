//! Deliberately bounded JSON Schema subset; unknown semantics fail closed.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Schema(Value);
const TYPES: &[&str] = &[
    "object", "array", "string", "integer", "number", "boolean", "null",
];
impl Schema {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value: Value = serde_json::from_slice(bytes).map_err(|_| "Schema is not valid JSON")?;
        let mut nodes = 0;
        preflight(&value, 0, &mut nodes)?;
        Ok(Self(value))
    }
    pub(super) fn value(&self) -> &Value {
        &self.0
    }
    pub(super) fn check_raw(&self, text: &str) -> Vec<String> {
        let mut errors = Vec::new();
        match serde_json::from_str::<Value>(text) {
            Ok(value) => match Spans::parse(text) {
                Ok(spans) => validate(&self.0, &value, &spans, "", &mut 100_000, &mut errors),
                Err(error) => errors.push(error.into()),
            },
            Err(error) => errors.push(format!(
                "Final answer is not exactly one supported JSON value: {error}"
            )),
        }
        errors
    }
    #[cfg(test)]
    fn check(&self, value: &Value) -> Vec<String> {
        self.check_raw(&value.to_string())
    }
}
fn preflight(schema: &Value, depth: usize, nodes: &mut usize) -> Result<(), String> {
    *nodes += 1;
    if depth > 32 || *nodes > 2048 {
        return Err("Schema exceeds the depth/node limit".into());
    }
    if schema.is_boolean() {
        return Ok(());
    }
    let object = schema
        .as_object()
        .ok_or("Schema must be an object or boolean")?;
    for (key, value) in object {
        match key.as_str() {
            "$schema" => {
                if !matches!(
                    value.as_str(),
                    Some(
                        "https://json-schema.org/draft/2020-12/schema"
                            | "http://json-schema.org/draft-07/schema#"
                    )
                ) {
                    return Err("Unsupported Schema dialect URI".into());
                }
            }
            "title" | "description" => {
                if !value.is_string() {
                    return Err("Schema annotations must be strings".into());
                }
            }
            "type" => {
                if !value.as_str().is_some_and(|kind| TYPES.contains(&kind)) {
                    return Err("Only single, recognized Schema type strings are supported".into());
                }
            }
            "properties" => {
                for child in value
                    .as_object()
                    .ok_or("properties must be an object")?
                    .values()
                {
                    preflight(child, depth + 1, nodes)?;
                }
            }
            "items" => preflight(value, depth + 1, nodes)?,
            "required" => {
                let mut names = BTreeSet::new();
                for name in value.as_array().ok_or("required must be an array")? {
                    if !names.insert(name.as_str().ok_or("required entries must be strings")?) {
                        return Err("required entries must be unique".into());
                    }
                }
            }
            "additionalProperties" => {
                if !value.is_boolean() {
                    return Err("Only boolean additionalProperties is supported".into());
                }
            }
            "minItems" | "maxItems" | "minLength" | "maxLength" => {
                if !value.as_u64().is_some_and(|n| n <= 1_000_000) {
                    return Err("Schema counts must be integer literals in 0..1000000".into());
                }
            }
            _ => {
                return Err(format!(
                    "Unsupported Schema keyword: {}",
                    key.chars().take(128).collect::<String>()
                ));
            }
        }
    }
    Ok(())
}
fn error(errors: &mut Vec<String>, path: &str, message: &str) {
    if errors.len() < 8 {
        errors.push(format!(
            "{}: {message}",
            if path.is_empty() {
                "/".into()
            } else {
                path.chars().take(512).collect::<String>()
            }
        ));
    }
}
fn step(remaining: &mut usize, errors: &mut Vec<String>, path: &str) -> bool {
    if *remaining == 0 {
        error(errors, path, "validation work limit exceeded");
        false
    } else {
        *remaining -= 1;
        true
    }
}
// Index raw scalar spans only after serde_json has validated the complete JSON.
// This preserves decimal integer semantics without globally enabling serde features.
enum Spans<'a> {
    Scalar(&'a str),
    Object(BTreeMap<String, Spans<'a>>),
    Array(Vec<Spans<'a>>),
}
impl<'a> Spans<'a> {
    fn parse(text: &'a str) -> Result<Self, &'static str> {
        struct Reader<'a> {
            text: &'a str,
            pos: usize,
            nodes: usize,
        }
        impl<'a> Reader<'a> {
            fn ws(&mut self) {
                while self
                    .text
                    .as_bytes()
                    .get(self.pos)
                    .is_some_and(u8::is_ascii_whitespace)
                {
                    self.pos += 1;
                }
            }
            fn quoted(&mut self) -> &'a str {
                let start = self.pos;
                self.pos += 1;
                while self.pos < self.text.len() {
                    match self.text.as_bytes()[self.pos] {
                        b'\\' => self.pos += 2,
                        b'"' => {
                            self.pos += 1;
                            break;
                        }
                        _ => self.pos += 1,
                    }
                }
                &self.text[start..self.pos]
            }
            fn value(&mut self, depth: usize) -> Result<Spans<'a>, &'static str> {
                self.nodes += 1;
                if self.nodes > 100_000 || depth > 128 {
                    return Err("JSON instance exceeds the bounded node/depth index");
                }
                self.ws();
                let start = self.pos;
                match self.text.as_bytes()[self.pos] {
                    b'{' => {
                        self.pos += 1;
                        self.ws();
                        let mut map = BTreeMap::new();
                        if self.text.as_bytes()[self.pos] == b'}' {
                            self.pos += 1;
                            return Ok(Spans::Object(map));
                        }
                        loop {
                            self.ws();
                            let name: String =
                                serde_json::from_str(self.quoted()).expect("validated object key");
                            self.ws();
                            self.pos += 1;
                            let child = self.value(depth + 1)?;
                            map.insert(name, child);
                            self.ws();
                            let end = self.text.as_bytes()[self.pos];
                            self.pos += 1;
                            if end == b'}' {
                                break;
                            }
                        }
                        Ok(Spans::Object(map))
                    }
                    b'[' => {
                        self.pos += 1;
                        self.ws();
                        let mut array = Vec::new();
                        if self.text.as_bytes()[self.pos] == b']' {
                            self.pos += 1;
                            return Ok(Spans::Array(array));
                        }
                        loop {
                            array.push(self.value(depth + 1)?);
                            self.ws();
                            let end = self.text.as_bytes()[self.pos];
                            self.pos += 1;
                            if end == b']' {
                                break;
                            }
                        }
                        Ok(Spans::Array(array))
                    }
                    b'"' => Ok(Spans::Scalar(self.quoted())),
                    _ => {
                        while self.text.as_bytes().get(self.pos).is_some_and(|byte| {
                            !matches!(byte, b',' | b']' | b'}' | b' ' | b'\n' | b'\r' | b'\t')
                        }) {
                            self.pos += 1;
                        }
                        Ok(Spans::Scalar(&self.text[start..self.pos]))
                    }
                }
            }
        }
        Reader {
            text,
            pos: 0,
            nodes: 0,
        }
        .value(0)
    }
}

fn integer(text: &str) -> bool {
    let text = text.strip_prefix('-').unwrap_or(text);
    let (mantissa, exponent) = text.split_once(['e', 'E']).unwrap_or((text, "0"));
    if mantissa.bytes().all(|byte| byte == b'0' || byte == b'.') {
        return true;
    }
    let fractional = mantissa
        .split_once('.')
        .map_or(0, |(_, digits)| digits.len()) as i64;
    let zeros = mantissa
        .bytes()
        .rev()
        .filter(|byte| *byte != b'.')
        .take_while(|byte| *byte == b'0')
        .count() as i64;
    let exponent = exponent.parse::<i64>().unwrap_or_else(|_| {
        if exponent.starts_with('-') {
            i64::MIN
        } else {
            i64::MAX
        }
    });
    fractional.saturating_sub(exponent) <= zeros
}
fn validate(
    schema: &Value,
    value: &Value,
    spans: &Spans<'_>,
    path: &str,
    left: &mut usize,
    errors: &mut Vec<String>,
) {
    if errors.len() >= 8 || !step(left, errors, path) {
        return;
    }
    if let Some(allowed) = schema.as_bool() {
        if !allowed {
            error(errors, path, "false Schema rejects this value");
        }
        return;
    }
    if let Some(kind) = schema["type"].as_str() {
        let matches = match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.is_number() && matches!(spans,Spans::Scalar(text)if integer(text)),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        };
        if !matches {
            error(errors, path, &format!("expected {kind}"));
            return;
        }
    }
    if let (Some(object), Spans::Object(raw)) = (value.as_object(), spans) {
        if let Some(required) = schema["required"].as_array() {
            for name in required {
                if !step(left, errors, path) {
                    return;
                }
                let name = name.as_str().expect("preflight");
                if !object.contains_key(name) {
                    error(
                        errors,
                        path,
                        &format!(
                            "required property missing: {}",
                            name.chars().take(128).collect::<String>()
                        ),
                    );
                }
            }
        }
        if let Some(properties) = schema["properties"].as_object() {
            for (name, child) in properties {
                if !step(left, errors, path) {
                    return;
                }
                if let Some(value) = object.get(name) {
                    validate(
                        child,
                        value,
                        &raw[name],
                        &format!("{path}/{}", name.replace('~', "~0").replace('/', "~1")),
                        left,
                        errors,
                    );
                }
            }
        }
        if schema["additionalProperties"] == false {
            for name in object.keys() {
                if !step(left, errors, path) {
                    return;
                }
                if !schema["properties"]
                    .as_object()
                    .is_some_and(|p| p.contains_key(name))
                {
                    error(
                        errors,
                        path,
                        &format!(
                            "additional property: {}",
                            name.chars().take(128).collect::<String>()
                        ),
                    );
                }
            }
        }
    }
    if let (Some(array), Spans::Array(raw)) = (value.as_array(), spans) {
        lengths(schema, array.len(), "minItems", "maxItems", path, errors);
        if let Some(items) = schema.get("items") {
            for (index, value) in array.iter().enumerate() {
                validate(
                    items,
                    value,
                    &raw[index],
                    &format!("{path}/{index}"),
                    left,
                    errors,
                );
                if errors.len() >= 8 {
                    return;
                }
            }
        }
    }
    if let Some(text) = value.as_str() {
        lengths(
            schema,
            text.chars().count(),
            "minLength",
            "maxLength",
            path,
            errors,
        );
    }
}
fn lengths(
    schema: &Value,
    length: usize,
    min: &str,
    max: &str,
    path: &str,
    errors: &mut Vec<String>,
) {
    if schema[min].as_u64().is_some_and(|n| (length as u64) < n) {
        error(errors, path, &format!("below {min}"));
    }
    if schema[max].as_u64().is_some_and(|n| (length as u64) > n) {
        error(errors, path, &format!("above {max}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn supported_shapes_validate_without_ignoring_required_or_extra_fields() {
        let schema=Schema::parse(br#"{"type":"object","properties":{"items":{"type":"array","minItems":1,"items":{"type":"integer"}},"text":{"type":"string","minLength":1,"maxLength":1}},"required":["items","text"],"additionalProperties":false}"#).unwrap();
        assert!(
            schema
                .check(&json!({"items":[1.0],"text":"\u{65e5}"}))
                .is_empty()
        );
        for value in [
            json!({"items":[],"text":"ab"}),
            json!({"items":["bad"],"text":"x"}),
            json!({"items":[1]}),
            json!({"items":[1],"text":"x","extra":true}),
        ] {
            assert!(!schema.check(&value).is_empty());
        }
        assert!(
            Schema::parse(br#"{"properties":{"x":{"type":"string"}}}"#)
                .unwrap()
                .check(&json!(42))
                .is_empty()
        );
        assert!(
            !Schema::parse(b"false")
                .unwrap()
                .check(&json!(null))
                .is_empty()
        );
    }
    #[test]
    fn unsupported_semantics_and_excessive_schemas_are_rejected() {
        for schema in [
            json!({"$ref":"https://example.invalid/schema"}),
            json!({"type":["string","null"]}),
            json!({"properties":{"nested":{"format":"email"}}}),
            json!({"minimum":0}),
            json!({"items":[]}),
            json!({"additionalProperties":{}}),
            json!({"required":["x","x"]}),
            json!({"minLength":1.0}),
        ] {
            assert!(Schema::parse(&serde_json::to_vec(&schema).unwrap()).is_err());
        }
        let mut nested = json!(true);
        for _ in 0..34 {
            nested = json!({"items":nested});
        }
        assert!(Schema::parse(&serde_json::to_vec(&nested).unwrap()).is_err());
    }
    #[test]
    fn exhausted_validation_budget_never_accepts_an_unchecked_array_tail() {
        let schema = json!({"type":"array","items":{"type":"integer"}});
        let mut errors = Vec::new();
        validate(
            &schema,
            &json!([1, 2, "unchecked"]),
            &Spans::parse(r#"[1,2,"unchecked"]"#).unwrap(),
            "",
            &mut 2,
            &mut errors,
        );
        assert!(!errors.is_empty());
        let integers = Schema::parse(br#"{"type":"integer"}"#).unwrap();
        assert!(integers.check_raw("1e30").is_empty());
        assert!(!integers.check_raw("1.00000000000000001").is_empty());
        assert!(!integers.check_raw("9007199254740991.5").is_empty());
        assert!(integers.check_raw("100e-2").is_empty());
        assert!(integer("0e-999999999999999999999999"));
        assert!(integers.check(&json!(u64::MAX)).is_empty());
    }
    #[test]
    fn scalar_spans_preserve_escaped_keys_duplicates_and_private_looking_object_names() {
        let schema=Schema::parse(br#"{"type":"object","properties":{"n":{"type":"integer"},"$serde_json::private::RawValue":{"type":"string"}},"required":["n"]}"#).unwrap();
        assert!(
            schema
                .check_raw(r#"{"\u006e":1.0,"$serde_json::private::RawValue":"{\"n\":2}"}"#)
                .is_empty()
        );
        assert!(
            !schema
                .check_raw(r#"{"n":1,"n":1.00000000000000001}"#)
                .is_empty()
        );
        assert!(schema.check_raw(r#"{"n":1.5,"n":2}"#).is_empty());
    }
}
