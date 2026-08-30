//! `tools=[some_agent]` -- an `agent` used to be refused as a tool target,
//! so the supervisor pattern (one agent delegating to specialists) had no
//! way to hand a specialist its own budget. These run against a real fake
//! provider so the specialist's own nested `analyze()` call -- the thing
//! that makes it worth calling an *agent* rather than a plain `tool` -- is
//! proven to actually run, not just accepted by the type check.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// Scripted responses, one per request in order; the last one repeats for
/// any request past the end.
fn spawn_scripted_provider(turns: Vec<serde_json::Value>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let turns = Arc::new(turns);
    let call = Arc::new(Mutex::new(0usize));

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

            let mut n = call.lock().unwrap();
            let index = (*n).min(turns.len() - 1);
            *n += 1;
            let payload = turns[index].to_string();
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

fn tool_call_turn(name: &str, arguments: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "message": {
            "content": "",
            "tool_calls": [{"function": {"name": name, "arguments": arguments}}]
        },
        "prompt_eval_count": 5,
        "eval_count": 3
    })
}

fn done_turn(body: &str) -> serde_json::Value {
    serde_json::json!({
        "message": {
            "content": serde_json::json!({"body": body, "__uncertain__": ""}).to_string()
        },
        "prompt_eval_count": 5,
        "eval_count": 3
    })
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

const SUPERVISOR_PROGRAM: &str = r#"type Answer:
    body: str

agent specialist(question: str) -> str:
    budget: max_tokens = 5000
    a: Answer = analyze(question, "specialist: answer directly")
    match a:
        case Ok(v):
            return v.body
        case Failed(why):
            return f"specialist failed: {why}"

agent supervisor(question: str) -> str:
    budget: max_tokens = 20000
    a: Answer = analyze(question, "delegate to a specialist", tools=[specialist])
    match a:
        case Ok(v):
            return v.body
        case Failed(why):
            return f"failed: {why}"

def main():
    print(supervisor("what is the capital of France"))
"#;

#[test]
fn an_agent_can_be_passed_as_a_tool() {
    // Three real turns: the supervisor asks for `specialist`, the
    // specialist's *own* `analyze()` call runs for real (not skipped, not
    // stubbed), and the supervisor sees the result and answers.
    let endpoint = spawn_scripted_provider(vec![
        tool_call_turn(
            "specialist",
            serde_json::json!({"question": "what is the capital of France"}),
        ),
        done_turn("Paris"),
        done_turn("Paris"),
    ]);
    let out = run(&config(&endpoint), SUPERVISOR_PROGRAM);
    assert_eq!(out, vec!["Paris"]);
}

const DEF_STILL_REJECTED_PROGRAM: &str = r#"type Answer:
    body: str

def helper(question: str) -> str:
    return "no"

def main():
    a: Answer = analyze("q", "p", tools=[helper])
"#;

#[test]
fn a_plain_def_is_still_rejected_as_a_tool() {
    // Widening the gate to `agent` must not widen it to `def` too --
    // a plain function still has no model-facing schema story of its own
    // the way `tool`/`agent` signatures do.
    let program = kora_syntax::parse(DEF_STILL_REJECTED_PROGRAM).unwrap();
    let mut i = kora_runtime::Interpreter::new();
    let err = i.run(&program).unwrap_err();
    assert!(err.message.contains("is not a tool"), "got: {}", err.message);
}
