//! `on tool_call(name, args):` -- the tool loop was closed, with no way for
//! a program to log, approve, rewrite, or veto an individual tool call
//! before it ran. These run against a real fake provider (not a mock or a
//! cassette, both of which stand in for the *whole* call and never enter the
//! tool loop at all) so the hook is proven against the actual multi-turn
//! path: a real `Step::CallTool`, a real dispatch, a real second turn.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

/// A stand-in Ollama server for exactly one tool call.
///
/// The first request gets back a `tool_calls` turn asking for `tool_name`
/// with `arguments`. The second request is the model seeing the result of
/// that call in its history (`messages(...)` renders it as `"Result of
/// {name}: {result}"`) -- this reads that result back out of the request and
/// echoes it into the final answer's `body` field, so what the test asserts
/// on is proof of what the tool (or the handler's veto) actually produced,
/// not a canned response that would pass either way.
fn spawn_echoing_provider(tool_name: &str, arguments: serde_json::Value) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let tool_name = tool_name.to_string();
    let marker = format!("Result of {tool_name}: ");

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = rest.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            let _ = reader.read_exact(&mut body);
            let request: serde_json::Value =
                serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);

            // The recorded result is itself JSON text (a tool's return value
            // is serialized before it goes in history), so the marker's
            // suffix is decoded once more to get the plain string back.
            let messages = request["messages"].as_array().cloned().unwrap_or_default();
            let result = messages.iter().rev().find_map(|m| {
                m["content"]
                    .as_str()
                    .and_then(|c| c.strip_prefix(&marker))
                    .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                    .and_then(|v| v.as_str().map(str::to_string))
            });

            let payload = match result {
                // Second turn: hand back what the tool loop actually
                // recorded, so the assertion checks the real effect.
                Some(result) => serde_json::json!({
                    "message": {
                        "content": serde_json::json!({"body": result, "__uncertain__": ""}).to_string()
                    },
                    "prompt_eval_count": 5,
                    "eval_count": 3
                }),
                // First turn: ask for the one tool.
                None => serde_json::json!({
                    "message": {
                        "content": "",
                        "tool_calls": [{"function": {"name": tool_name, "arguments": arguments}}]
                    },
                    "prompt_eval_count": 5,
                    "eval_count": 3
                }),
            }
            .to_string();

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    format!("http://127.0.0.1:{port}")
}

fn config(endpoint: &str) -> String {
    format!(
        r#"
[models]
default = "local:test-model"
max_retries = 0

[models.local]
endpoint = "{endpoint}"
"#
    )
}

fn run(config_text: &str, src: &str) -> Vec<String> {
    let program = kora_syntax::parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = kora_runtime::Interpreter::new();
    i.config = kora_runtime::Config::parse(config_text).unwrap();
    i.program_name = "test.ko".into();
    i.run(&program)
        .unwrap_or_else(|e| panic!("the run should not fail: {}\n{src}", e.message));
    i.output
}

const PROGRAM: &str = r#"type Answer:
    body: str

tool divide(a: int, b: int) -> str:
    return f"{a}-{b}"

def main():
    a: Answer = analyze("compute", "divide 10 by 0", tools=[divide]) on tool_call(name, args):
        print(f"calling {name} with {args}")
        if name == "divide" and args["b"] == 0:
            return "blocked: refuse to divide by zero"
    match a:
        case Ok(v):
            print(f"ok: {v.body}")
        case Failed(why):
            print(f"failed: {why}")
"#;

#[test]
fn on_tool_call_sees_the_call_before_it_runs() {
    let endpoint = spawn_echoing_provider("divide", serde_json::json!({"a": 10, "b": 0}));
    let out = run(&config(&endpoint), PROGRAM);
    // `args` is a `dict`, whose key order is not guaranteed, so check both
    // pairs are present rather than pinning one exact ordering.
    assert!(out[0].starts_with("calling divide with {"), "got: {}", out[0]);
    assert!(out[0].contains("\"a\": 10"), "got: {}", out[0]);
    assert!(out[0].contains("\"b\": 0"), "got: {}", out[0]);
}

#[test]
fn on_tool_call_can_veto_a_call_by_returning_a_string() {
    // `divide` has no zero-check of its own -- if it had run, the echoed
    // body would be "10-0". Seeing the handler's own message instead proves
    // the tool's body never executed.
    let endpoint = spawn_echoing_provider("divide", serde_json::json!({"a": 10, "b": 0}));
    let out = run(&config(&endpoint), PROGRAM);
    assert_eq!(out[1], "ok: blocked: refuse to divide by zero");
}

const REWRITE_PROGRAM: &str = r#"type Answer:
    body: str

tool divide(a: int, b: int) -> str:
    return f"{a}-{b}"

def main():
    a: Answer = analyze("compute", "divide 10 by 0", tools=[divide]) on tool_call(name, args):
        if args["b"] == 0:
            args["b"] = 2
    match a:
        case Ok(v):
            print(v.body)
        case Failed(why):
            print(f"failed: {why}")
"#;

#[test]
fn on_tool_call_rewrites_args_in_place_before_the_tool_runs() {
    let endpoint = spawn_echoing_provider("divide", serde_json::json!({"a": 10, "b": 0}));
    let out = run(&config(&endpoint), REWRITE_PROGRAM);
    // "10-2", not "10-0": the tool ran for real, and received the rewritten
    // b, not the value the model originally sent.
    assert_eq!(out, vec!["10-2"]);
}

const NO_HANDLER_PROGRAM: &str = r#"type Answer:
    body: str

tool divide(a: int, b: int) -> str:
    return f"{a}-{b}"

def main():
    a: Answer = analyze("compute", "divide 10 by 0", tools=[divide])
    match a:
        case Ok(v):
            print(v.body)
        case Failed(why):
            print(f"failed: {why}")
"#;

#[test]
fn without_a_handler_the_tool_runs_with_the_models_own_arguments() {
    // The control: with no `on tool_call` at all, nothing intercepts the
    // call, so `divide` sees exactly what the model asked for.
    let endpoint = spawn_echoing_provider("divide", serde_json::json!({"a": 10, "b": 0}));
    let out = run(&config(&endpoint), NO_HANDLER_PROGRAM);
    assert_eq!(out, vec!["10-0"]);
}
