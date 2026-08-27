//! Provider clients: OpenAI (API) and Ollama (localhost).
//!
//! HTTP is isolated behind [`Transport`] so request construction and response
//! handling are testable without touching the network.

use serde_json::{json, Value};

use crate::schema::{build_json_schema, system_prompt, user_prompt};
use crate::validate::{parse_response, truncate};
use crate::{AnalyzeRequest, FieldType, ModelConfig, ModelError, Provider, Step, ToolSpec};

const TIMEOUT_SECS: u64 = 120;
const OPENAI_BASE: &str = "https://api.openai.com/v1";
const OLLAMA_BASE: &str = "http://localhost:11434";

/// One HTTP POST: (url, headers, body) -> response body text.
pub(crate) type Transport = dyn Fn(&str, &[(&str, String)], &Value) -> Result<String, ModelError>;

/// `"openai:gpt-4o"` / `"local:llama3.1:8b"` -> config.
///
/// Everything after the first colon is the model name, so Ollama tags
/// (`llama3.1:8b`) survive intact.
pub fn parse_model_spec(spec: &str) -> Result<ModelConfig, ModelError> {
    let (scheme, model) = spec.split_once(':').ok_or_else(|| {
        ModelError::new(format!(
            "model spec `{spec}` needs a provider prefix, e.g. `openai:gpt-4o` or `local:llama3.1:8b`"
        ))
    })?;
    if model.trim().is_empty() {
        return Err(ModelError::new(format!(
            "model spec `{spec}` has no model name after `{scheme}:`"
        )));
    }
    let provider = match scheme {
        "openai" => Provider::OpenAI,
        "local" | "ollama" => Provider::Ollama,
        other => {
            return Err(ModelError::new(format!(
                "unknown model provider `{other}` (expected `openai` or `local`)"
            )))
        }
    };
    Ok(ModelConfig {
        provider,
        model: model.to_string(),
        endpoint: None,
        api_key: None,
        max_output_tokens: 4096,
    })
}

pub(crate) fn step_with(
    config: &ModelConfig,
    req: &AnalyzeRequest,
    transport: &Transport,
) -> Result<Step, ModelError> {
    match config.provider {
        Provider::OpenAI => openai(config, req, transport),
        Provider::Ollama => ollama(config, req, transport),
    }
}

/// Tool declarations in the shape both providers accept.
fn tools_json(tools: &[ToolSpec]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                let mut properties = serde_json::Map::new();
                let mut required = Vec::new();
                for (name, ty) in &tool.params {
                    properties.insert(name.clone(), param_schema(ty));
                    required.push(Value::String(name.clone()));
                }
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": {
                            "type": "object",
                            "properties": properties,
                            "required": required,
                        }
                    }
                })
            })
            .collect(),
    )
}

fn param_schema(ty: &FieldType) -> Value {
    match ty {
        FieldType::Str => json!({"type": "string"}),
        FieldType::Int => json!({"type": "integer"}),
        FieldType::Float => json!({"type": "number"}),
        FieldType::Bool => json!({"type": "boolean"}),
        FieldType::ListOfStr => json!({"type": "array", "items": {"type": "string"}}),
    }
}

/// Conversation messages: system, user, then any tool exchanges so far.
fn messages(req: &AnalyzeRequest) -> Vec<Value> {
    let mut out = vec![
        json!({"role": "system", "content": system_prompt(&req.schema)}),
        json!({"role": "user", "content": user_prompt(&req.prompt, &req.data_json)}),
    ];
    for exchange in &req.tool_history {
        out.push(json!({
            "role": "assistant",
            "content": format!("Calling {}({})", exchange.name, exchange.arguments_json),
        }));
        out.push(json!({
            "role": "user",
            "content": format!("Result of {}: {}", exchange.name, exchange.result_json),
        }));
    }
    out
}

fn openai(
    config: &ModelConfig,
    req: &AnalyzeRequest,
    transport: &Transport,
) -> Result<Step, ModelError> {
    let key = config
        .api_key
        .clone()
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| {
            ModelError::new("OPENAI_API_KEY not set (export it, or set api_key in kora.toml)")
        })?;

    let mut body = json!({
        "model": config.model,
        "max_completion_tokens": config.max_output_tokens,
        "messages": messages(req),
    });
    if req.tools.is_empty() {
        // Structured output and tool calling are mutually exclusive shapes:
        // constrain the answer only once no tool can still be requested.
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {
                "name": sanitize_schema_name(&req.schema.type_name),
                "strict": true,
                "schema": build_json_schema(&req.schema),
            }
        });
    } else {
        body["tools"] = tools_json(&req.tools);
    }

    let headers = [
        ("Authorization", format!("Bearer {key}")),
        ("Content-Type", "application/json".to_string()),
    ];
    let url = format!("{OPENAI_BASE}/chat/completions");
    let text = transport(&url, &headers, &body)?;
    let response: Value = serde_json::from_str(&text).map_err(|e| {
        ModelError::new(format!(
            "OpenAI returned a non-JSON body ({e}): {}",
            truncate(&text, 300)
        ))
    })?;

    let tokens_in = response["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
    let tokens_out = response["usage"]["completion_tokens"].as_u64().unwrap_or(0);

    let message = &response["choices"][0]["message"];
    if let Some(call) = message["tool_calls"].get(0) {
        let name = call["function"]["name"].as_str().unwrap_or_default();
        let arguments_json = call["function"]["arguments"]
            .as_str()
            .unwrap_or("{}")
            .to_string();
        return Ok(Step::CallTool {
            name: name.to_string(),
            arguments_json,
            tokens_in,
            tokens_out,
        });
    }

    let content = message["content"].as_str().ok_or_else(|| {
        ModelError::new(format!(
            "OpenAI response had no message content: {}",
            truncate(&text, 300)
        ))
    })?;
    parse_response(content, &req.schema, tokens_in, tokens_out).map(Step::Done)
}

fn ollama(
    config: &ModelConfig,
    req: &AnalyzeRequest,
    transport: &Transport,
) -> Result<Step, ModelError> {
    let base = config.endpoint.as_deref().unwrap_or(OLLAMA_BASE);
    let mut body = json!({
        "model": config.model,
        "stream": false,
        "options": {"num_predict": config.max_output_tokens},
        "messages": messages(req),
    });
    if req.tools.is_empty() {
        // Ollama takes the JSON schema directly in `format`.
        body["format"] = build_json_schema(&req.schema);
    } else {
        body["tools"] = tools_json(&req.tools);
    }

    let headers = [("Content-Type", "application/json".to_string())];
    let url = format!("{}/api/chat", base.trim_end_matches('/'));
    let text = transport(&url, &headers, &body)?;
    let response: Value = serde_json::from_str(&text).map_err(|e| {
        ModelError::new(format!(
            "Ollama returned a non-JSON body ({e}): {}",
            truncate(&text, 300)
        ))
    })?;

    let tokens_in = response["prompt_eval_count"].as_u64().unwrap_or(0);
    let tokens_out = response["eval_count"].as_u64().unwrap_or(0);

    let message = &response["message"];
    if let Some(call) = message["tool_calls"].get(0) {
        let name = call["function"]["name"].as_str().unwrap_or_default();
        // Ollama returns arguments as a JSON object, not a string.
        let arguments_json = match &call["function"]["arguments"] {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        return Ok(Step::CallTool {
            name: name.to_string(),
            arguments_json,
            tokens_in,
            tokens_out,
        });
    }

    let content = message["content"].as_str().ok_or_else(|| {
        ModelError::new(format!(
            "Ollama response had no message content: {}",
            truncate(&text, 300)
        ))
    })?;
    parse_response(content, &req.schema, tokens_in, tokens_out).map(Step::Done)
}

/// OpenAI requires schema names to match `^[a-zA-Z0-9_-]+$`.
fn sanitize_schema_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "Result".to_string()
    } else {
        cleaned
    }
}

/// The real network transport.
pub(crate) fn ureq_transport(
    url: &str,
    headers: &[(&str, String)],
    body: &Value,
) -> Result<String, ModelError> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .build();
    let mut request = agent.post(url);
    for (name, value) in headers {
        request = request.set(name, value);
    }
    match request.send_json(body.clone()) {
        Ok(response) => response
            .into_string()
            .map_err(|e| ModelError::new(format!("could not read response body from {url}: {e}"))),
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            Err(ModelError::new(format!(
                "{url} returned HTTP {code}: {}",
                truncate(&body, 300)
            )))
        }
        Err(e) => Err(ModelError::new(format!("request to {url} failed: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnalyzeOutcome, FieldType, Schema};
    use std::cell::RefCell;

    fn schema() -> Schema {
        Schema {
            type_name: "Insight".into(),
            fields: vec![
                ("summary".into(), FieldType::Str),
                ("count".into(), FieldType::Int),
            ],
        }
    }

    fn request() -> AnalyzeRequest {
        AnalyzeRequest {
            prompt: "find anomalies".into(),
            data_json: "{\"rows\":2}".into(),
            schema: schema(),
            tools: Vec::new(),
            tool_history: Vec::new(),
        }
    }

    /// Records the outgoing request and replays a canned response body.
    /// What the recording transport captured: (url, request body).
    type Captured = std::rc::Rc<RefCell<Option<(String, Value)>>>;

    /// A boxed transport plus the handle that observes what it was sent.
    type Recorder = (Box<Transport>, Captured);

    /// Build a transport that replays `reply` and remembers the request.
    fn recording(reply: &'static str) -> Recorder {
        let seen: Captured = std::rc::Rc::new(RefCell::new(None));
        let sink = seen.clone();
        let transport = Box::new(move |url: &str, _h: &[(&str, String)], body: &Value| {
            *sink.borrow_mut() = Some((url.to_string(), body.clone()));
            Ok(reply.to_string())
        });
        (transport, seen)
    }

    #[test]
    fn spec_openai() {
        let c = parse_model_spec("openai:gpt-4o").unwrap();
        assert_eq!(c.provider, Provider::OpenAI);
        assert_eq!(c.model, "gpt-4o");
        assert_eq!(c.max_output_tokens, 4096);
    }

    #[test]
    fn spec_local_keeps_tag() {
        let c = parse_model_spec("local:llama3.1:8b").unwrap();
        assert_eq!(c.provider, Provider::Ollama);
        assert_eq!(c.model, "llama3.1:8b");
    }

    #[test]
    fn spec_errors() {
        assert!(parse_model_spec("gpt-4o")
            .unwrap_err()
            .message
            .contains("prefix"));
        assert!(parse_model_spec("openai:")
            .unwrap_err()
            .message
            .contains("no model name"));
        assert!(parse_model_spec("groq:x")
            .unwrap_err()
            .message
            .contains("unknown model provider"));
    }

    #[test]
    fn openai_request_shape_and_parse() {
        let reply = r#"{
            "choices":[{"message":{"content":"{\"summary\":\"ok\",\"count\":2,\"__uncertain__\":\"\"}"}}],
            "usage":{"prompt_tokens":11,"completion_tokens":7}
        }"#;
        let (transport, seen) = recording(reply);
        let mut config = parse_model_spec("openai:gpt-4o").unwrap();
        config.api_key = Some("test-key".into());

        let outcome = step_with(&config, &request(), &*transport).unwrap();
        match outcome {
            Step::Done(AnalyzeOutcome::Ok {
                fields_json,
                tokens_in,
                tokens_out,
            }) => {
                assert_eq!(fields_json["summary"], "ok");
                assert_eq!(tokens_in, 11);
                assert_eq!(tokens_out, 7);
            }
            other => panic!("expected Ok, got {other:?}"),
        }

        let (url, body) = seen.borrow().clone().unwrap();
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert!(body["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("DATA:"));
    }

    #[test]
    fn openai_missing_key_is_clear() {
        // Ensure the env var cannot rescue the call.
        std::env::remove_var("OPENAI_API_KEY");
        let (transport, _seen) = recording("{}");
        let config = parse_model_spec("openai:gpt-4o").unwrap();
        let err = step_with(&config, &request(), &*transport).unwrap_err();
        assert!(
            err.message.contains("OPENAI_API_KEY not set"),
            "{}",
            err.message
        );
    }

    #[test]
    fn ollama_request_shape_and_uncertain() {
        let reply = r#"{
            "message":{"content":"{\"summary\":\"\",\"count\":0,\"__uncertain__\":\"no revenue column\"}"},
            "prompt_eval_count":30,"eval_count":9
        }"#;
        let (transport, seen) = recording(reply);
        let config = parse_model_spec("local:llama3.1:8b").unwrap();

        match step_with(&config, &request(), &*transport).unwrap() {
            Step::Done(AnalyzeOutcome::Uncertain {
                reason,
                tokens_in,
                tokens_out,
            }) => {
                assert_eq!(reason, "no revenue column");
                assert_eq!(tokens_in, 30);
                assert_eq!(tokens_out, 9);
            }
            other => panic!("expected Uncertain, got {other:?}"),
        }

        let (url, body) = seen.borrow().clone().unwrap();
        assert_eq!(url, "http://localhost:11434/api/chat");
        assert_eq!(body["stream"], false);
        assert_eq!(body["format"]["type"], "object");
    }

    #[test]
    fn ollama_endpoint_override() {
        let reply =
            r#"{"message":{"content":"{\"summary\":\"a\",\"count\":1,\"__uncertain__\":\"\"}"}}"#;
        let (transport, seen) = recording(reply);
        let mut config = parse_model_spec("local:llama3.1:8b").unwrap();
        config.endpoint = Some("http://box:11434/".into());

        step_with(&config, &request(), &*transport).unwrap();
        assert_eq!(
            seen.borrow().clone().unwrap().0,
            "http://box:11434/api/chat"
        );
    }

    #[test]
    fn schema_name_sanitized() {
        assert_eq!(sanitize_schema_name("Insight"), "Insight");
        assert_eq!(sanitize_schema_name("my type!"), "my_type_");
        assert_eq!(sanitize_schema_name(""), "Result");
    }
}
