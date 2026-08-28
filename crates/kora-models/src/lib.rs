//! kora-models: model-provider clients with schema-constrained JSON output
//! for the `analyze()` language primitive.
//!
//! Synchronous/blocking HTTP only (the interpreter is a sync tree-walker).

mod provider;
mod schema;
mod validate;

use std::fmt;

pub use provider::parse_model_spec;

/// JSON-schema-ish description of the expected result shape.
/// Built by the runtime from Kora `type` declarations.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Schema {
    pub type_name: String,
    /// Ordered field list.
    pub fields: Vec<SchemaField>,
}

/// A model-visible field and its native Kora metadata.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SchemaField {
    pub name: String,
    pub field_type: FieldType,
    pub description: Option<String>,
    pub pattern: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub enum FieldType {
    Str,
    Int,
    Float,
    Bool,
    ListOfStr,
}

impl FieldType {
    /// Human-readable name used in validation error messages.
    pub(crate) fn display_name(&self) -> &'static str {
        match self {
            FieldType::Str => "string",
            FieldType::Int => "integer",
            FieldType::Float => "float",
            FieldType::Bool => "boolean",
            FieldType::ListOfStr => "list of strings",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub provider: Provider,
    /// e.g. "gpt-4o" or "llama3.1:8b"
    pub model: String,
    /// Ollama base URL override; default http://localhost:11434
    pub endpoint: Option<String>,
    /// OpenAI; read from OPENAI_API_KEY if None.
    pub api_key: Option<String>,
    /// Default 4096.
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Provider {
    OpenAI,
    Ollama,
}

#[derive(Debug, Clone)]
pub struct AnalyzeRequest {
    /// The user's natural-language instruction.
    pub prompt: String,
    /// The input data serialized as JSON text.
    pub data_json: String,
    pub schema: Schema,
    /// Tools the model may call before producing its final answer.
    pub tools: Vec<ToolSpec>,
    /// Results of tool calls already performed, appended to the conversation
    /// as the loop progresses.
    pub tool_history: Vec<ToolExchange>,
}

/// A function the model may call. Built from a Kora `tool` declaration:
/// the signature becomes the schema, the docstring becomes the description.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub params: Vec<(String, FieldType)>,
}

/// One completed tool call and its result.
#[derive(Debug, Clone)]
pub struct ToolExchange {
    pub name: String,
    pub arguments_json: String,
    pub result_json: String,
}

/// What the model wants next.
#[derive(Debug, Clone)]
pub enum Step {
    /// Final answer produced.
    Done(AnalyzeOutcome),
    /// Model asked to run a tool; the runtime should execute it and loop.
    CallTool {
        name: String,
        arguments_json: String,
        tokens_in: u64,
        tokens_out: u64,
    },
}

#[derive(Debug, Clone)]
pub enum AnalyzeOutcome {
    /// Model produced a JSON object conforming to the schema.
    Ok {
        fields_json: serde_json::Map<String, serde_json::Value>,
        tokens_in: u64,
        tokens_out: u64,
    },
    /// Model explicitly refused / could not comply.
    Uncertain {
        reason: String,
        tokens_in: u64,
        tokens_out: u64,
    },
}

#[derive(Debug)]
pub struct ModelError {
    pub message: String,
}

impl ModelError {
    pub fn new(message: impl Into<String>) -> Self {
        ModelError {
            message: message.into(),
        }
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ModelError {}

/// Run one schema-constrained analyze call against the configured provider.
pub fn analyze(config: &ModelConfig, req: &AnalyzeRequest) -> Result<AnalyzeOutcome, ModelError> {
    match provider::step_with(config, req, &provider::ureq_transport)? {
        Step::Done(outcome) => Ok(outcome),
        // Without tools declared the model has nothing to call, so a tool
        // request here means it ignored the contract.
        Step::CallTool { name, .. } => Err(ModelError::new(format!(
            "model tried to call tool `{name}`, but no tools were provided"
        ))),
    }
}

/// One turn of the tool loop: either the final answer, or a tool to run.
pub fn step(config: &ModelConfig, req: &AnalyzeRequest) -> Result<Step, ModelError> {
    provider::step_with(config, req, &provider::ureq_transport)
}
