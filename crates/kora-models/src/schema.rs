//! JSON schema generation and the prompt contract shared by all providers.

use crate::{FieldType, Schema, SchemaField};
use serde_json::{json, Value};

/// Reserved field used for explicit refusal. The model puts a non-empty
/// reason here when it cannot comply; empty string when confident.
pub const UNCERTAIN_KEY: &str = "__uncertain__";

/// Build the JSON schema object sent to the provider.
///
/// Every declared field is a required property, plus the required
/// `__uncertain__` string field (OpenAI strict mode forbids alternative
/// response shapes, so refusal travels inside the object).
pub fn build_json_schema(schema: &Schema) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required: Vec<Value> = Vec::new();
    for field in &schema.fields {
        properties.insert(field.name.clone(), field_schema(field));
        required.push(Value::String(field.name.clone()));
    }
    properties.insert(
        UNCERTAIN_KEY.to_string(),
        json!({
            "type": "string",
            "description": "Empty string when confident; otherwise the reason you cannot comply."
        }),
    );
    required.push(Value::String(UNCERTAIN_KEY.to_string()));
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn field_schema(field: &SchemaField) -> Value {
    let mut schema = match field.field_type {
        FieldType::Str => json!({"type": "string"}),
        FieldType::Int => json!({"type": "integer"}),
        FieldType::Float => json!({"type": "number"}),
        FieldType::Bool => json!({"type": "boolean"}),
        FieldType::ListOfStr => json!({"type": "array", "items": {"type": "string"}}),
    };
    let object = schema.as_object_mut().expect("field schemas are objects");
    if let Some(description) = &field.description {
        object.insert(
            "description".to_string(),
            Value::String(description.clone()),
        );
    }
    if let Some(pattern) = &field.pattern {
        object.insert("pattern".to_string(), Value::String(pattern.clone()));
    }
    schema
}

/// System message explaining the output contract.
pub fn system_prompt(schema: &Schema) -> String {
    let field_list = schema
        .fields
        .iter()
        .map(|field| {
            let mut line = format!("- {}: {}", field.name, field.field_type.display_name());
            if let Some(description) = &field.description {
                line.push_str(&format!(" - {description}"));
            }
            if let Some(pattern) = &field.pattern {
                line.push_str(&format!(" (must match /{pattern}/)"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "You are a data analysis engine. Respond with ONLY a single JSON object \
         matching the type `{type_name}` with exactly these fields:\n{field_list}\n\
         - {UNCERTAIN_KEY}: string (refusal channel, see below)\n\n\
         Contract for `{UNCERTAIN_KEY}`:\n\
         - If you can fulfill the instruction, fill every field with your answer and \
           set \"{UNCERTAIN_KEY}\" to the empty string \"\".\n\
         - If you cannot comply (the instruction is impossible, the data is \
           insufficient, or you must refuse), return {{\"{UNCERTAIN_KEY}\": \"<reason>\"}} \
           with a short non-empty reason; if the schema forces you to emit the other \
           fields anyway, fill them with placeholder values and put the reason in \
           \"{UNCERTAIN_KEY}\".\n\
         Never output prose, markdown, or code fences. Output only the JSON object.",
        type_name = schema.type_name,
    )
}

/// User message: instruction plus the data payload.
pub fn user_prompt(prompt: &str, data_json: &str) -> String {
    format!("{prompt}\n\nDATA:\n{data_json}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_schema() -> Schema {
        Schema {
            type_name: "Expense".to_string(),
            fields: vec![
                SchemaField {
                    name: "merchant".to_string(),
                    field_type: FieldType::Str,
                    description: Some("Merchant name".to_string()),
                    pattern: Some("^[A-Za-z]+$".to_string()),
                },
                SchemaField {
                    name: "amount_cents".to_string(),
                    field_type: FieldType::Int,
                    description: None,
                    pattern: None,
                },
                SchemaField {
                    name: "confidence_notes".to_string(),
                    field_type: FieldType::ListOfStr,
                    description: None,
                    pattern: None,
                },
                SchemaField {
                    name: "recurring".to_string(),
                    field_type: FieldType::Bool,
                    description: None,
                    pattern: None,
                },
                SchemaField {
                    name: "tax_rate".to_string(),
                    field_type: FieldType::Float,
                    description: None,
                    pattern: None,
                },
            ],
        }
    }

    #[test]
    fn json_schema_snapshot() {
        let value = build_json_schema(&sample_schema());
        let expected = json!({
            "type": "object",
            "properties": {
                "merchant": {"type": "string", "description": "Merchant name", "pattern": "^[A-Za-z]+$"},
                "amount_cents": {"type": "integer"},
                "confidence_notes": {"type": "array", "items": {"type": "string"}},
                "recurring": {"type": "boolean"},
                "tax_rate": {"type": "number"},
                "__uncertain__": {
                    "type": "string",
                    "description": "Empty string when confident; otherwise the reason you cannot comply."
                }
            },
            "required": [
                "merchant",
                "amount_cents",
                "confidence_notes",
                "recurring",
                "tax_rate",
                "__uncertain__"
            ],
            "additionalProperties": false
        });
        assert_eq!(value, expected);
        // Snapshot of the serialized text, to lock property ordering too.
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            serde_json::to_string(&expected).unwrap()
        );
    }

    #[test]
    fn system_prompt_mentions_contract() {
        let prompt = system_prompt(&sample_schema());
        assert!(prompt.contains("Expense"));
        assert!(prompt.contains(UNCERTAIN_KEY));
        assert!(prompt.contains("merchant: string - Merchant name (must match /^[A-Za-z]+$/)"));
        assert!(prompt.contains("amount_cents: integer"));
    }

    #[test]
    fn user_prompt_layout() {
        assert_eq!(
            user_prompt("Summarize this", "{\"a\":1}"),
            "Summarize this\n\nDATA:\n{\"a\":1}"
        );
    }
}
