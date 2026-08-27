//! JSON schema generation and the prompt contract shared by all providers.

use crate::{FieldType, Schema};
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
    for (name, field_type) in &schema.fields {
        properties.insert(name.clone(), field_type_schema(field_type));
        required.push(Value::String(name.clone()));
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

fn field_type_schema(field_type: &FieldType) -> Value {
    match field_type {
        FieldType::Str => json!({"type": "string"}),
        FieldType::Int => json!({"type": "integer"}),
        FieldType::Float => json!({"type": "number"}),
        FieldType::Bool => json!({"type": "boolean"}),
        FieldType::ListOfStr => json!({"type": "array", "items": {"type": "string"}}),
    }
}

/// System message explaining the output contract.
pub fn system_prompt(schema: &Schema) -> String {
    let field_list = schema
        .fields
        .iter()
        .map(|(name, ft)| format!("- {name}: {}", ft.display_name()))
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
                ("merchant".to_string(), FieldType::Str),
                ("amount_cents".to_string(), FieldType::Int),
                ("confidence_notes".to_string(), FieldType::ListOfStr),
                ("recurring".to_string(), FieldType::Bool),
                ("tax_rate".to_string(), FieldType::Float),
            ],
        }
    }

    #[test]
    fn json_schema_snapshot() {
        let value = build_json_schema(&sample_schema());
        let expected = json!({
            "type": "object",
            "properties": {
                "merchant": {"type": "string"},
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
        assert!(prompt.contains("merchant: string"));
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
