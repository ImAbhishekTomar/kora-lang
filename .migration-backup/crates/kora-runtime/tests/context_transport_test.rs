//! `with context(...)` against a real tool loop.
//!
//! A fake transport that hands back a canned `AnalyzeOutcome` never builds a
//! request, so it cannot show whether a context fence actually shaped what
//! was sent. These tests run a real multi-turn tool loop against a loopback
//! HTTP server scripted like Ollama, and inspect the request bodies the
//! runtime actually sent -- the same technique `mcp_failure_test.rs` uses for
//! the wedged-tool-server tests.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// A scripted Ollama-shaped `/api/chat` responder.
///
/// Each connection is served the next response in `scripts`, in order, and
/// the request body that produced it is recorded, so a test can assert on
/// exactly what the runtime sent for turn N -- in particular, which tool
/// exchanges survived a context fence into a later request.
fn spawn_scripted_provider(scripts: Vec<serde_json::Value>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_writer = seen.clone();

    std::thread::spawn(move || {
        let mut scripts = scripts.into_iter();
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
            seen_writer
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&body).to_string());

            let Some(payload) = scripts.next() else {
                let _ = stream.write_all(b"HTTP/1.1 500 no more scripted turns\r\n\r\n");
                continue;
            };
            let payload = payload.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    (format!("http://127.0.0.1:{port}"), seen)
}

/// One turn's canned reply: call `lookup` with the given argument.
fn tool_call(id: &str) -> serde_json::Value {
    serde_json::json!({
        "message": {
            "content": "",
            "tool_calls": [{
                "function": { "name": "lookup", "arguments": { "id": id } }
            }]
        },
        "prompt_eval_count": 1,
        "eval_count": 1
    })
}

/// The final turn's reply: a plain-text `str` answer.
fn final_answer(text: &str) -> serde_json::Value {
    let content = serde_json::json!({ "__uncertain__": "", "answer": text }).to_string();
    serde_json::json!({
        "message": { "content": content },
        "prompt_eval_count": 1,
        "eval_count": 1
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

fn program(max_input_tokens: u64) -> String {
    format!(
        r#"tool lookup(id: str) -> str:
    "Look up a record by id."
    return id

def main():
    with context(max_input_tokens = {max_input_tokens}, reserve_output_tokens = 0):
        answer: str = analyze("go", "use the tool, then answer", tools=[lookup])
        match answer:
            case Ok(v):
                print(f"ok: {{v}}")
            case Failed(why):
                print(f"failed: {{why}}")
"#
    )
}

fn run(config_text: &str, src: &str) -> Vec<String> {
    let program = kora_syntax::parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = kora_runtime::Interpreter::new();
    let parsed = kora_runtime::Config::parse(config_text).unwrap();
    i.sinks = parsed.sinks.clone();
    i.config = parsed;
    i.program_name = "test.ko".into();
    i.run(&program)
        .unwrap_or_else(|e| panic!("the run should not fail: {}\n{src}", e.message));
    i.output
}

/// Three tool turns, each returning a marker string big enough that a tight
/// context fence cannot keep all three: the oldest whole exchange must be
/// dropped before the newest, and dropping never rewrites what it keeps.
const OLD_MARKER: &str = "OLDEST-EXCHANGE-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const MID_MARKER: &str = "MIDDLE-EXCHANGE-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
const NEW_MARKER: &str = "NEWEST-EXCHANGE-CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";

#[test]
fn context_fence_prunes_the_oldest_whole_exchange_first() {
    let (endpoint, seen) = spawn_scripted_provider(vec![
        tool_call(OLD_MARKER),
        tool_call(MID_MARKER),
        tool_call(NEW_MARKER),
        final_answer("done"),
    ]);

    // Tight enough that turn 4's request cannot carry all three exchanges,
    // but loose enough for the base prompt/data/tools plus at least one.
    let max_input_tokens = 400;
    let out = run(&config(&endpoint), &program(max_input_tokens));
    assert_eq!(out, vec!["ok: done"], "the call still completes");

    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 4, "one request per turn, got {requests:?}");
    let last = &requests[3];

    // Whole units, oldest first: the newest exchange survives, the oldest is
    // gone. A truncation bug would instead show a cut-off fragment of an
    // exchange rather than its clean absence.
    assert!(
        last.contains(NEW_MARKER),
        "the newest exchange must still be sent, got: {last}"
    );
    assert!(
        !last.contains(OLD_MARKER),
        "the oldest exchange must be dropped whole, got: {last}"
    );

    // What is kept travels unmodified, in its untrusted-provenance envelope:
    // pruning selects whole exchanges, it does not edit their content, so a
    // label a retained result carried (e.g. `unverified`) is never quietly
    // stripped or rewritten on the way out.
    assert!(
        last.contains("UNTRUSTED_TOOL_RESULT"),
        "a retained tool result must still be marked untrusted, got: {last}"
    );
}
