use super::{ConfigError, Deserialize, invalid};

const MAX_PATTERNS: usize = 64;
const MAX_PATTERN_BYTES: usize = 256;

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelFilter {
    pub include: Option<Vec<String>>,
    pub exclude: Vec<String>,
}

impl ModelFilter {
    pub(super) fn validate(&self, connection: &str) -> Result<(), ConfigError> {
        for (name, patterns) in [
            ("include", self.include.as_deref().unwrap_or_default()),
            ("exclude", self.exclude.as_slice()),
        ] {
            let field = || format!("connections.{connection}.models.{name}");
            if patterns.len() > MAX_PATTERNS {
                return Err(invalid(
                    field(),
                    format!("at most {MAX_PATTERNS} patterns are allowed"),
                ));
            }
            for (index, pattern) in patterns.iter().enumerate() {
                let key = || format!("{}[{index}]", field());
                if pattern.is_empty() || pattern.len() > MAX_PATTERN_BYTES {
                    return Err(invalid(
                        key(),
                        format!("pattern must contain 1 to {MAX_PATTERN_BYTES} UTF-8 bytes"),
                    ));
                }
                if pattern.chars().any(|c| c.is_whitespace() || c.is_control()) {
                    return Err(invalid(
                        key(),
                        "pattern must not contain whitespace or control characters",
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn matches(&self, model: &str) -> bool {
        self.include
            .as_ref()
            .is_none_or(|include| include.iter().any(|p| wildcard(p, model)))
            && !self.exclude.iter().any(|p| wildcard(p, model))
    }
}

fn wildcard(mut pattern: &str, mut model: &str) -> bool {
    // Only the most recent `*` can need more input; backtrack by one Unicode scalar.
    let mut star = None;
    while let Some(actual) = model.chars().next() {
        match pattern.chars().next() {
            Some('*') => {
                pattern = &pattern[1..];
                star = Some((pattern, model));
            }
            Some(expected) if expected == '?' || expected == actual => {
                pattern = &pattern[expected.len_utf8()..];
                model = &model[actual.len_utf8()..];
            }
            _ => {
                let Some((suffix, remaining)) = star else {
                    return false;
                };
                let Some(next) = remaining.chars().next() else {
                    return false;
                };
                model = &remaining[next.len_utf8()..];
                pattern = suffix;
                star = Some((pattern, model));
            }
        }
    }
    pattern.bytes().all(|b| b == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn model_filter_grammar_and_validation() {
        assert!(wildcard("gpt-*?", "gpt-\u{00e9}"));
        assert!(!wildcard("gpt-?", "gpt-xx"));
        assert!(!wildcard("gpt-*", "GPT-4"));
        assert!(wildcard("*a**ab", "zzazab"));
        assert!(wildcard("x[1].+", "x[1].+"));
        assert!(!wildcard("x[1].+", "x1x"));
        assert!(ModelFilter::default().matches("default"));
        let parse = |key: &str, value: &str| {
            Config::parse(&format!(
                "[connections.test]\nbase_url='http://localhost/v1'\nmodels.{key}={value}\n"
            ))
        };
        assert!(
            !parse("include", "[]").unwrap().connections["test"]
                .models
                .matches("any")
        );
        assert!(parse("exclude", "['*']").is_ok());
        for (key, value, reason) in [
            ("include", "['']".to_owned(), "1 to 256"),
            ("exclude", "['a b']".into(), "whitespace"),
            ("include", format!("['{}']", "a".repeat(257)), "1 to 256"),
            (
                "exclude",
                format!("[{}]", ["'*'"; 65].join(",")),
                "at most 64",
            ),
        ] {
            let error = parse(key, &value).unwrap_err().to_string();
            assert!(
                error.contains(&format!("connections.test.models.{key}")),
                "{error}"
            );
            assert!(error.contains(reason), "{error}");
        }
        assert!(parse("mode", "'include'").is_err());
    }
}
