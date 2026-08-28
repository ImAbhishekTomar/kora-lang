//! Validation of a model's JSON response against the declared schema.

use serde_json::{Map, Value};

use crate::schema::UNCERTAIN_KEY;
use crate::{AnalyzeOutcome, FieldType, ModelError, Schema, SchemaField};

/// Turn a raw model response body into an outcome.
///
/// `text` is the assistant's message content: it must be a single JSON object.
/// Anything else (prose, code fences, arrays) is a `ModelError` — the runtime
/// treats that as a hard failure rather than an `Uncertain`, because it means
/// the provider ignored the schema contract.
pub fn parse_response(
    text: &str,
    schema: &Schema,
    tokens_in: u64,
    tokens_out: u64,
) -> Result<AnalyzeOutcome, ModelError> {
    let trimmed = strip_code_fence(text.trim());
    let parsed: Value = serde_json::from_str(trimmed).map_err(|e| {
        ModelError::new(format!(
            "model did not return valid JSON ({e}); got: {}",
            truncate(trimmed, 300)
        ))
    })?;

    let Value::Object(mut obj) = parsed else {
        return Err(ModelError::new(format!(
            "model returned {}, expected a JSON object; got: {}",
            json_kind(&parsed),
            truncate(trimmed, 300)
        )));
    };

    // Refusal channel wins over field validation: a model that is bailing out
    // is not expected to have filled the other fields meaningfully.
    if let Some(reason) = obj.get(UNCERTAIN_KEY).and_then(Value::as_str) {
        if !reason.trim().is_empty() {
            return Ok(AnalyzeOutcome::Uncertain {
                reason: reason.trim().to_string(),
                tokens_in,
                tokens_out,
            });
        }
    }
    obj.remove(UNCERTAIN_KEY);

    let fields_json = validate_fields(obj, schema)?;
    Ok(AnalyzeOutcome::Ok {
        fields_json,
        tokens_in,
        tokens_out,
    })
}

/// Check every declared field is present with the right type, and normalize
/// whole floats into integers where the schema asks for an integer.
fn validate_fields(
    mut obj: Map<String, Value>,
    schema: &Schema,
) -> Result<Map<String, Value>, ModelError> {
    let mut out = Map::new();
    for field in &schema.fields {
        let value = obj.remove(&field.name).ok_or_else(|| {
            ModelError::new(format!(
                "model response is missing field `{}` (expected {})",
                field.name,
                field.field_type.display_name()
            ))
        })?;
        out.insert(field.name.clone(), coerce(field, value)?);
    }
    Ok(out)
}

fn coerce(field: &SchemaField, value: Value) -> Result<Value, ModelError> {
    let name = &field.name;
    let field_type = &field.field_type;
    let bad = |got: &str| {
        Err(ModelError::new(format!(
            "field `{name}` should be {}, but the model returned {got}",
            field_type.display_name()
        )))
    };
    match field_type {
        FieldType::Str => match value {
            Value::String(text) => {
                if let Some(pattern) = &field.pattern {
                    let regex = regex::Regex::new(pattern).map_err(|e| {
                        ModelError::new(format!(
                            "field `{name}` has an invalid pattern `{pattern}`: {e}"
                        ))
                    })?;
                    if !regex.is_match(&text) {
                        return Err(ModelError::new(format!(
                            "field `{name}` should match pattern `{pattern}`, but the model returned `{text}`"
                        )));
                    }
                }
                Ok(Value::String(text))
            }
            other => bad(&json_kind(&other)),
        },
        FieldType::Int => match &value {
            Value::Number(n) if n.is_i64() || n.is_u64() => Ok(value),
            // Accept whole floats: models often emit 3.0 for an integer.
            Value::Number(n) => match n.as_f64() {
                Some(f) if f.fract() == 0.0 && f.is_finite() => {
                    Ok(Value::Number((f as i64).into()))
                }
                _ => bad("a fractional number"),
            },
            other => bad(&json_kind(other)),
        },
        FieldType::Float => match &value {
            Value::Number(_) => Ok(value),
            other => bad(&json_kind(other)),
        },
        FieldType::Bool => match value {
            Value::Bool(_) => Ok(value),
            other => bad(&json_kind(&other)),
        },
        FieldType::ListOfStr => match &value {
            Value::Array(items) => {
                if items.iter().all(Value::is_string) {
                    Ok(value)
                } else {
                    bad("a list containing non-strings")
                }
            }
            other => bad(&json_kind(other)),
        },
    }
}

/// Some providers still wrap JSON in ```json fences despite instructions.
fn strip_code_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    rest.trim_start_matches('\n')
        .trim_end()
        .strip_suffix("```")
        .unwrap_or(rest)
        .trim()
}

fn json_kind(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(_) => "a boolean".into(),
        Value::Number(_) => "a number".into(),
        Value::String(_) => "a string".into(),
        Value::Array(_) => "a list".into(),
        Value::Object(_) => "an object".into(),
    }
}

pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Schema {
        Schema {
            type_name: "Insight".into(),
            fields: vec![
                SchemaField {
                    name: "summary".into(),
                    field_type: FieldType::Str,
                    description: None,
                    pattern: None,
                },
                SchemaField {
                    name: "count".into(),
                    field_type: FieldType::Int,
                    description: None,
                    pattern: None,
                },
                SchemaField {
                    name: "score".into(),
                    field_type: FieldType::Float,
                    description: None,
                    pattern: None,
                },
                SchemaField {
                    name: "urgent".into(),
                    field_type: FieldType::Bool,
                    description: None,
                    pattern: None,
                },
                SchemaField {
                    name: "tags".into(),
                    field_type: FieldType::ListOfStr,
                    description: None,
                    pattern: None,
                },
            ],
        }
    }

    const VALID: &str = r#"{
        "summary": "revenue dipped in EMEA",
        "count": 3,
        "score": 0.82,
        "urgent": true,
        "tags": ["emea", "revenue"],
        "__uncertain__": ""
    }"#;

    #[test]
    fn valid_response_parses() {
        let outcome = parse_response(VALID, &schema(), 10, 20).unwrap();
        match outcome {
            AnalyzeOutcome::Ok {
                fields_json,
                tokens_in,
                tokens_out,
            } => {
                assert_eq!(tokens_in, 10);
                assert_eq!(tokens_out, 20);
                assert_eq!(fields_json.len(), 5, "__uncertain__ must be stripped");
                assert_eq!(fields_json["summary"], "revenue dipped in EMEA");
                assert_eq!(fields_json["count"], 3);
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn uncertain_path() {
        let body = r#"{"summary":"","count":0,"score":0.0,"urgent":false,"tags":[],
                       "__uncertain__":"the data has no revenue column"}"#;
        match parse_response(body, &schema(), 1, 2).unwrap() {
            AnalyzeOutcome::Uncertain { reason, .. } => {
                assert_eq!(reason, "the data has no revenue column")
            }
            other => panic!("expected Uncertain, got {other:?}"),
        }
    }

    #[test]
    fn missing_field_names_it() {
        let body = r#"{"summary":"x","score":0.1,"urgent":false,"tags":[],"__uncertain__":""}"#;
        let err = parse_response(body, &schema(), 0, 0).unwrap_err();
        assert!(
            err.message.contains("missing field `count`"),
            "{}",
            err.message
        );
        assert!(err.message.contains("integer"), "{}", err.message);
    }

    #[test]
    fn wrong_type_names_field_and_expectation() {
        let body = r#"{"summary":"x","count":"three","score":0.1,"urgent":false,
                       "tags":[],"__uncertain__":""}"#;
        let err = parse_response(body, &schema(), 0, 0).unwrap_err();
        assert!(err.message.contains("`count`"), "{}", err.message);
        assert!(err.message.contains("integer"), "{}", err.message);
        assert!(err.message.contains("a string"), "{}", err.message);
    }

    #[test]
    fn whole_float_accepted_as_int() {
        let body = r#"{"summary":"x","count":3.0,"score":0.1,"urgent":false,
                       "tags":[],"__uncertain__":""}"#;
        match parse_response(body, &schema(), 0, 0).unwrap() {
            AnalyzeOutcome::Ok { fields_json, .. } => assert_eq!(fields_json["count"], 3),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn fractional_float_rejected_for_int() {
        let body = r#"{"summary":"x","count":3.5,"score":0.1,"urgent":false,
                       "tags":[],"__uncertain__":""}"#;
        let err = parse_response(body, &schema(), 0, 0).unwrap_err();
        assert!(err.message.contains("`count`"), "{}", err.message);
    }

    #[test]
    fn list_with_non_strings_rejected() {
        let body = r#"{"summary":"x","count":1,"score":0.1,"urgent":false,
                       "tags":["a", 2],"__uncertain__":""}"#;
        let err = parse_response(body, &schema(), 0, 0).unwrap_err();
        assert!(err.message.contains("`tags`"), "{}", err.message);
    }

    #[test]
    fn pattern_rejects_a_model_value_that_does_not_match() {
        let mut constrained = schema();
        constrained.fields[0].pattern = Some("^[A-Z]{3}$".into());
        let body = r#"{"summary":"lowercase","count":1,"score":0.1,"urgent":false,
                       "tags":[],"__uncertain__":""}"#;
        let err = parse_response(body, &constrained, 0, 0).unwrap_err();
        assert!(
            err.message.contains("should match pattern"),
            "{}",
            err.message
        );
    }

    #[test]
    fn code_fence_stripped() {
        let body = format!("```json\n{VALID}\n```");
        assert!(matches!(
            parse_response(&body, &schema(), 0, 0).unwrap(),
            AnalyzeOutcome::Ok { .. }
        ));
    }

    #[test]
    fn prose_is_a_hard_error() {
        let err = parse_response("Sure! Here is the answer.", &schema(), 0, 0).unwrap_err();
        assert!(err.message.contains("valid JSON"), "{}", err.message);
    }

    #[test]
    fn non_object_json_rejected() {
        let err = parse_response("[1, 2, 3]", &schema(), 0, 0).unwrap_err();
        assert!(
            err.message.contains("expected a JSON object"),
            "{}",
            err.message
        );
    }
}
