//! kora-models: model-provider clients with schema-constrained JSON output
//! for the `analyze()` language primitive.
//!
//! Synchronous/blocking HTTP only (the interpreter is a sync tree-walker).

mod base64;
mod provider;
mod schema;
mod stream;
mod validate;

use std::fmt;
use std::rc::Rc;

pub use provider::{parse_model_spec, DEFAULT_TIMEOUT_SECS};
pub use schema::TEXT_KEY;
pub use stream::Flow;

/// JSON-schema-ish description of the expected result shape.
/// Built by the runtime from Kora `type` declarations.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Schema {
    pub type_name: String,
    /// Ordered field list.
    pub fields: Vec<SchemaField>,
    /// True when the program asked for `str` rather than a declared type.
    ///
    /// The wire shape is still a JSON object with one field, because the
    /// refusal channel has to survive: a plain-text completion has nowhere
    /// to put `Uncertain`, and dropping one of the four outcomes for the
    /// one result type people reach for first would be the wrong trade.
    /// What changes is the answer's shape on the way back — a string, not
    /// an object with a single field the program never asked for.
    pub text: bool,
}

impl Schema {
    /// The schema for `answer: str = analyze(...)`.
    pub fn for_text() -> Schema {
        Schema {
            type_name: "str".to_string(),
            fields: vec![SchemaField {
                name: schema::TEXT_KEY.to_string(),
                field_type: FieldType::Str,
                description: Some("Your answer, as plain prose.".to_string()),
                pattern: None,
            }],
            text: true,
        }
    }
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
    /// Another declared type, nested inline. Shared rather than copied since
    /// the same nested schema is rebuilt on every recursive field lookup.
    Object(Rc<Schema>),
    /// `list[T]` where `T` is a declared type, not `str`.
    ListOfObject(Rc<Schema>),
}

impl FieldType {
    /// Human-readable name used in validation error messages.
    pub(crate) fn display_name(&self) -> String {
        match self {
            FieldType::Str => "string".to_string(),
            FieldType::Int => "integer".to_string(),
            FieldType::Float => "float".to_string(),
            FieldType::Bool => "boolean".to_string(),
            FieldType::ListOfStr => "list of strings".to_string(),
            FieldType::Object(schema) => format!("object `{}`", schema.type_name),
            FieldType::ListOfObject(schema) => format!("list of `{}` objects", schema.type_name),
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
    /// How long to wait for one response. There is no "off": a request that
    /// waits forever is the most common way a program hangs.
    pub timeout_secs: u64,
    /// How many times to retry a request that failed for a reason that may
    /// not repeat: a refused connection, a timeout, a 429, a 5xx.
    ///
    /// There is no "off" for the same reason `http` retries a GET: a provider
    /// under load is the ordinary case, not the exceptional one, and a
    /// program that gives up on the first 429 is a program that gives up
    /// several times an hour.
    pub max_retries: u32,
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
    /// Images accompanying the data, in the order the program listed them.
    ///
    /// Text and pixels travel together in one request: a receipt is not a
    /// JSON blob with a picture attached, it *is* the picture, and splitting
    /// them into two calls loses the association the model needs.
    pub images: Vec<ImagePart>,
    pub schema: Schema,
    /// Tools the model may call before producing its final answer.
    pub tools: Vec<ToolSpec>,
    /// Results of tool calls already performed, appended to the conversation
    /// as the loop progresses.
    pub tool_history: Vec<ToolExchange>,
}

/// One image travelling to a multimodal model.
///
/// Bytes, not base64: the encoding is a wire detail each provider spells
/// differently, so it happens at request construction rather than being
/// carried around pre-encoded.
#[derive(Debug, Clone)]
pub struct ImagePart {
    /// An image MIME type, e.g. `image/png`.
    pub mime: String,
    pub bytes: Vec<u8>,
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
    /// The provider did not answer: refused connection, timeout, rate limit,
    /// server error, or a response that was not a model response at all.
    ///
    /// Distinct from `Uncertain`, which is the model answering "no". These
    /// have different fixes -- one is a prompt, the other is a provider --
    /// and collapsing them would hide an outage inside a refusal.
    ///
    /// Token counts are what the call had already spent when it failed: a
    /// tool loop may have completed several turns before the provider
    /// stopped answering, and that spend is real.
    Failed {
        reason: String,
        tokens_in: u64,
        tokens_out: u64,
    },
}

#[derive(Debug)]
pub struct ModelError {
    pub message: String,
    /// Whether trying the same request again could plausibly succeed.
    ///
    /// Set where the failure is observed rather than guessed at from the
    /// message afterwards: only the transport knows that a 429 is worth
    /// waiting on and a 401 never will be.
    pub retryable: bool,
}

impl ModelError {
    pub fn new(message: impl Into<String>) -> Self {
        ModelError {
            message: message.into(),
            retryable: false,
        }
    }

    /// A failure that may not repeat: connection refused, timeout, 429, 5xx.
    pub fn retryable(message: impl Into<String>) -> Self {
        ModelError {
            message: message.into(),
            retryable: true,
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
    match provider::step_with(config, req, &*provider::transport_for(config))? {
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
    provider::step_with(config, req, &*provider::transport_for(config))
}

/// Run an analyze call, handing over the answer as it is written.
///
/// The outcome is parsed from the complete response exactly as a blocking
/// call would parse it, so streaming never changes *what* a call returns.
/// `on_text` sees only the answer itself — never the JSON around it — and
/// returning [`Flow::Stop`] ends the request without draining the rest.
///
/// A stream that breaks after characters were already handed over is not
/// retried: see [`stream`] for why.
pub fn analyze_streaming(
    config: &ModelConfig,
    req: &AnalyzeRequest,
    on_text: &mut dyn FnMut(&str) -> Result<Flow, ModelError>,
) -> Result<AnalyzeOutcome, ModelError> {
    stream::analyze_streaming_with(config, req, &*stream::stream_transport_for(config), on_text)
}
