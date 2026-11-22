use serde_json::{Value, json};

pub(crate) fn assert_result(operation: &str, result: &Value) {
    let mut schema: Value =
        serde_json::from_str(include_str!("../../docs/reference/protocol.schema.json"))
            .expect("published protocol schema parses");
    schema["$ref"] = json!(format!("#/operation_results/{operation}"));
    let validator = jsonschema::validator_for(&schema).expect("operation result schema compiles");
    let errors = validator
        .iter_errors(result)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "{operation} result rejected by published schema: {errors:?}; result={result}"
    );
}
