//! Provider clients: OpenAI (API) and Ollama (localhost).
//!
//! HTTP is isolated behind [`Transport`] so request construction and response
//! handling are testable without touching the network.

use serde_json::{json, Value};

use crate::base64;
use crate::schema::{build_json_schema, system_prompt, user_prompt};
use crate::validate::{parse_response, truncate};
use crate::{AnalyzeRequest, FieldType, ModelConfig, ModelError, Provider, Step, ToolSpec};

/// Long enough for a large local model to read an image, since a timeout
/// that fires on ordinary work teaches people to raise it blindly. Override
/// with `[models] timeout_secs`.
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;
/// Three attempts total. Enough to ride out a rate limit or a restarting
/// server; few enough that a provider which is genuinely down is reported
/// while somebody is still watching.
pub const DEFAULT_MAX_RETRIES: u32 = 2;
/// Wait before the first retry. Doubles each attempt.
const RETRY_BASE_MS: u64 = 500;
/// A `Retry-After` longer than this is not waited out: a provider asking for
/// a minute is telling the program to come back later, not to block.
const MAX_RETRY_AFTER_SECS: u64 = 20;
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
        timeout_secs: DEFAULT_TIMEOUT_SECS,
        max_retries: DEFAULT_MAX_RETRIES,
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
///
/// The two providers attach images differently — OpenAI splits the user
/// message into typed content parts, Ollama keeps plain text and hangs a
/// parallel `images` array off the message — so the provider decides the
/// shape rather than the caller.
fn messages(req: &AnalyzeRequest, provider: &Provider) -> Vec<Value> {
    let text = user_prompt(&req.prompt, &req.data_json);
    let user = if req.images.is_empty() {
        json!({"role": "user", "content": text})
    } else {
        match provider {
            Provider::OpenAI => {
                let mut parts = vec![json!({"type": "text", "text": text})];
                for image in &req.images {
                    parts.push(json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!(
                                "data:{};base64,{}",
                                image.mime,
                                base64::encode(&image.bytes)
                            )
                        }
                    }));
                }
                json!({"role": "user", "content": parts})
            }
            Provider::Ollama => {
                let encoded: Vec<Value> = req
                    .images
                    .iter()
                    .map(|i| Value::String(base64::encode(&i.bytes)))
                    .collect();
                json!({"role": "user", "content": text, "images": encoded})
            }
        }
    };

    let mut out = vec![
        json!({"role": "system", "content": system_prompt(&req.schema)}),
        user,
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
        "messages": messages(req, &Provider::OpenAI),
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
        "messages": messages(req, &Provider::Ollama),
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

/// The real network transport, carrying this config's timeout and retries.
///
/// Retrying here rather than around `step_with` keeps one HTTP request as the
/// unit that is retried: a tool loop that has already run three turns does not
/// start over because the fourth request was rate limited.
pub(crate) fn transport_for(config: &ModelConfig) -> Box<Transport> {
    // Zero is how "no timeout" sneaks back in, so it is clamped rather than
    // honoured -- the same rule the `http` module applies.
    let timeout = std::time::Duration::from_secs(config.timeout_secs.max(1));
    let attempts = config.max_retries.saturating_add(1);
    Box::new(move |url: &str, headers: &[(&str, String)], body: &Value| {
        retry_loop(attempts, || send(url, headers, body, timeout))
    })
}

/// The retry policy, with the request it retries passed in.
///
/// Split from the socket so the policy can be tested without one: how many
/// attempts a 429 is worth is the part that will be argued about, and it
/// should not need a listening port to check.
fn retry_loop<F>(attempts: u32, mut attempt_once: F) -> Result<String, ModelError>
where
    F: FnMut() -> Result<String, (ModelError, Option<u64>)>,
{
    let mut attempt = 0;
    loop {
        attempt += 1;
        let (error, retry_after) = match attempt_once() {
            Ok(text) => return Ok(text),
            Err(e) => e,
        };
        if !error.retryable || attempt >= attempts {
            return Err(error);
        }
        std::thread::sleep(retry_delay(attempt, retry_after));
    }
}

/// Exponential backoff, or what the provider asked for when it said.
///
/// The jitter matters more here than in a single-threaded client: a
/// `parallel for` fans out across every core, so a shared rate limit hits
/// every branch at once and an unjittered backoff marches them all back into
/// the provider together.
fn retry_delay(attempt: u32, retry_after: Option<u64>) -> std::time::Duration {
    if let Some(secs) = retry_after {
        return std::time::Duration::from_secs(secs.min(MAX_RETRY_AFTER_SECS));
    }
    let base = RETRY_BASE_MS.saturating_mul(1 << (attempt - 1).min(5));
    std::time::Duration::from_millis(base + jitter_ms(base))
}

/// Up to a quarter of the wait, from the clock rather than a random source.
///
/// A real generator would be one more thing to seed, and nothing here needs
/// to be unpredictable -- only for two threads to differ.
fn jitter_ms(base: u64) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos % (base / 4).max(1)
}

/// Whether waiting could plausibly change the answer, and for how long.
///
/// 408, 409, 429 and every 5xx are the provider saying "not now". Everything
/// else in the 4xx range is the request itself being wrong, and a retry only
/// wastes the caller's time twice.
fn retryable_status(code: u16) -> bool {
    matches!(code, 408 | 409 | 429) || (500..600).contains(&code)
}

fn retry_after_secs(response: &ureq::Response) -> Option<u64> {
    response.header("retry-after")?.trim().parse::<u64>().ok()
}

/// One attempt. The `Option<u64>` alongside an error is the provider's own
/// `Retry-After`, which is worth more than any backoff guessed at locally.
#[allow(clippy::type_complexity)]
fn send(
    url: &str,
    headers: &[(&str, String)],
    body: &Value,
    timeout: std::time::Duration,
) -> Result<String, (ModelError, Option<u64>)> {
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let mut request = agent.post(url);
    for (name, value) in headers {
        request = request.set(name, value);
    }
    match request.send_json(body.clone()) {
        Ok(response) => response.into_string().map_err(|e| {
            (
                ModelError::retryable(format!("could not read response body from {url}: {e}")),
                None,
            )
        }),
        Err(ureq::Error::Status(code, response)) => {
            let retry_after = retry_after_secs(&response);
            let body = response.into_string().unwrap_or_default();
            let message = format!("{url} returned HTTP {code}: {}", truncate(&body, 300));
            let error = if retryable_status(code) {
                ModelError::retryable(message)
            } else {
                ModelError::new(message)
            };
            Err((error, retry_after))
        }
        // Everything left is transport: a refused connection, a DNS failure,
        // a timeout. None of those say anything about the request itself.
        Err(e) => Err((
            ModelError::retryable(format!("request to {url} failed: {e}")),
            None,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnalyzeOutcome, FieldType, Schema, SchemaField};
    use std::cell::RefCell;

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
            ],
        }
    }

    fn request() -> AnalyzeRequest {
        AnalyzeRequest {
            prompt: "find anomalies".into(),
            data_json: "{\"rows\":2}".into(),
            images: Vec::new(),
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
        assert_eq!(c.timeout_secs, DEFAULT_TIMEOUT_SECS);
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

    /// The same image must arrive in each provider's own shape: OpenAI wants
    /// typed content parts with a data URL, Ollama wants bare base64 in a
    /// sibling array. Getting either wrong is a silently text-only request.
    #[test]
    fn openai_attaches_images_as_content_parts() {
        let reply = r#"{
            "choices":[{"message":{"content":"{\"summary\":\"ok\",\"count\":1,\"__uncertain__\":\"\"}"}}],
            "usage":{"prompt_tokens":1,"completion_tokens":1}
        }"#;
        let (transport, seen) = recording(reply);
        let mut config = parse_model_spec("openai:gpt-4o").unwrap();
        config.api_key = Some("test-key".into());
        let mut req = request();
        req.images = vec![crate::ImagePart {
            mime: "image/png".into(),
            bytes: b"foobar".to_vec(),
        }];

        step_with(&config, &req, &*transport).unwrap();
        let (_, body) = seen.borrow().clone().unwrap();
        let parts = &body["messages"][1]["content"];
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(
            parts[1]["image_url"]["url"],
            "data:image/png;base64,Zm9vYmFy"
        );
    }

    #[test]
    fn ollama_attaches_images_beside_the_text() {
        let reply =
            r#"{"message":{"content":"{\"summary\":\"a\",\"count\":1,\"__uncertain__\":\"\"}"}}"#;
        let (transport, seen) = recording(reply);
        let config = parse_model_spec("local:llava:7b").unwrap();
        let mut req = request();
        req.images = vec![crate::ImagePart {
            mime: "image/png".into(),
            bytes: b"foobar".to_vec(),
        }];

        step_with(&config, &req, &*transport).unwrap();
        let (_, body) = seen.borrow().clone().unwrap();
        let message = &body["messages"][1];
        assert!(message["content"].as_str().unwrap().contains("DATA:"));
        assert_eq!(message["images"][0], "Zm9vYmFy");
    }

    /// A text-only call must keep the plain-string content shape: some
    /// providers and local models reject the content-parts form outright.
    #[test]
    fn no_images_keeps_plain_string_content() {
        let reply = r#"{
            "choices":[{"message":{"content":"{\"summary\":\"ok\",\"count\":1,\"__uncertain__\":\"\"}"}}]
        }"#;
        let (transport, seen) = recording(reply);
        let mut config = parse_model_spec("openai:gpt-4o").unwrap();
        config.api_key = Some("test-key".into());

        step_with(&config, &request(), &*transport).unwrap();
        let (_, body) = seen.borrow().clone().unwrap();
        assert!(body["messages"][1]["content"].is_string());
    }

    #[test]
    fn schema_name_sanitized() {
        assert_eq!(sanitize_schema_name("Insight"), "Insight");
        assert_eq!(sanitize_schema_name("my type!"), "my_type_");
        assert_eq!(sanitize_schema_name(""), "Result");
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn a_bad_request_is_never_retried() {
        // 400 and 401 are the request being wrong. Retrying one wastes the
        // caller's time twice and reaches the same answer.
        for code in [400, 401, 403, 404, 422] {
            assert!(!retryable_status(code), "{code} should not be retried");
        }
    }

    #[test]
    fn a_rate_limit_or_a_server_error_is_retried() {
        for code in [408, 409, 429, 500, 502, 503, 504] {
            assert!(retryable_status(code), "{code} should be retried");
        }
    }

    #[test]
    fn the_backoff_grows_and_stays_bounded() {
        let first = retry_delay(1, None);
        let second = retry_delay(2, None);
        assert!(
            first.as_millis() >= RETRY_BASE_MS as u128,
            "the first wait should be at least the base"
        );
        assert!(
            second >= first,
            "waits should grow: {second:?} came after {first:?}"
        );
        // Jitter is a fraction of the wait, not a multiple of it.
        assert!(first.as_millis() < (RETRY_BASE_MS as u128) * 2);
    }

    #[test]
    fn the_provider_is_believed_over_the_local_backoff() {
        assert_eq!(retry_delay(1, Some(3)).as_secs(), 3);
    }

    #[test]
    fn an_absurd_retry_after_is_capped_rather_than_waited_out() {
        // A provider asking for an hour is telling the program to come back
        // later, not to hold a thread open until then.
        assert_eq!(
            retry_delay(1, Some(3600)).as_secs(),
            MAX_RETRY_AFTER_SECS,
            "a long Retry-After should be capped"
        );
    }

    #[test]
    fn retries_stop_at_the_configured_count() {
        // Counts attempts rather than sleeping: the policy is what is under
        // test, not the clock.
        let attempts = std::cell::Cell::new(0);
        let result = retry_loop(3, || {
            attempts.set(attempts.get() + 1);
            Err((ModelError::retryable("nope"), Some(0)))
        });
        assert!(result.is_err());
        assert_eq!(attempts.get(), 3, "three attempts, then give up");
    }

    #[test]
    fn an_unretryable_failure_is_reported_on_the_first_attempt() {
        let attempts = std::cell::Cell::new(0);
        let result = retry_loop(3, || {
            attempts.set(attempts.get() + 1);
            Err((ModelError::new("bad api key"), None))
        });
        assert!(result.is_err());
        assert_eq!(attempts.get(), 1, "a 401 does not improve with waiting");
    }

    #[test]
    fn a_retry_that_succeeds_returns_the_answer() {
        let attempts = std::cell::Cell::new(0);
        let result = retry_loop(3, || {
            attempts.set(attempts.get() + 1);
            if attempts.get() < 2 {
                Err((ModelError::retryable("try again"), Some(0)))
            } else {
                Ok("body".to_string())
            }
        });
        assert_eq!(result.unwrap(), "body");
        assert_eq!(attempts.get(), 2);
    }
}
