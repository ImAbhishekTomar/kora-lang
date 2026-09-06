//! A tool server that does not answer is an outcome, not a dead run.
//!
//! End to end, because that is where the bug lived: a real child process on a
//! real pipe, a real model transport asking for the tool, and a program that
//! has to be able to say what happened. A fake transport never hung, which is
//! why nothing caught this.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

/// The MCP fixture lives with the protocol tests that own it, rather than
/// being copied here where the two could drift apart.
fn wedged_server_path() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../kora-mcp/tests/fixtures/awkward_server.py")
        .to_string_lossy()
        // Forward slashes so the path survives a TOML literal string on
        // Windows too, where `\` would otherwise have to be escaped.
        .replace('\\', "/")
}

/// A stand-in provider that asks for one tool and nothing else.
///
/// Every turn it replies with the same tool call, so if the runtime handed a
/// server failure back to the model the loop would keep calling the wedged
/// tool until the budget ran out -- which is the behaviour these tests exist
/// to rule out.
fn spawn_tool_calling_provider() -> (String, std::sync::mpsc::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let (calls, seen) = std::sync::mpsc::channel();

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
            let _ = calls.send(());

            let payload = serde_json::json!({
                "message": {
                    "content": "",
                    "tool_calls": [{
                        "function": { "name": "awkward__act", "arguments": {} }
                    }]
                },
                "prompt_eval_count": 1,
                "eval_count": 1
            })
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

    (format!("http://127.0.0.1:{port}"), seen)
}

/// Whether a Python interpreter is available. The fixture server is a Python
/// script, so a machine without one skips these rather than failing -- the
/// same gate `python_test.rs` already uses.
fn python_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn config(endpoint: &str, mode: &str) -> String {
    format!(
        r#"
[models]
default = "local:test-model"
max_retries = 0

[models.local]
endpoint = "{endpoint}"

[mcp]
timeout_secs = 2

[mcp.awkward]
command = "python3"
args = ['{server}', '{mode}']
"#,
        server = wedged_server_path(),
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

const PROGRAM: &str = r#"use mcp awkward as srv

type Answer:
    body: str

def main():
    a: Answer = analyze("go", "use the tool", tools=srv.tools)
    match a:
        case Ok(v):
            print(f"ok: {v.body}")
        case Uncertain(why):
            print("uncertain")
        case Exhausted(meter):
            print("exhausted")
        case Failed(why):
            print(f"failed: {why}")
"#;

#[test]
fn a_wedged_tool_server_ends_the_call_as_failed() {
    if !python_available() {
        return;
    }
    let (endpoint, seen) = spawn_tool_calling_provider();
    let out = run(&config(&endpoint, "wedge"), PROGRAM);

    assert_eq!(out.len(), 1, "one line of output, got {out:?}");
    let line = &out[0];
    assert!(line.starts_with("failed: "), "got: {line}");
    // The reason has to name the server and the tool, or the program is told
    // something failed without being told what to go and restart.
    assert!(line.contains("awkward.act"), "got: {line}");
    assert!(line.contains("did not answer"), "got: {line}");

    // The provider was asked once. A runtime that fed the failure back to the
    // model would have gone round again, paying the timeout every turn and
    // then blaming the budget.
    let turns = std::iter::from_fn(|| seen.try_recv().ok()).count();
    assert_eq!(turns, 1, "the tool loop took {turns} turns, not one");
}

#[test]
fn a_tool_server_that_exits_ends_the_call_as_failed() {
    if !python_available() {
        return;
    }
    let (endpoint, _seen) = spawn_tool_calling_provider();
    let out = run(&config(&endpoint, "die"), PROGRAM);
    assert!(out[0].starts_with("failed: "), "got: {}", out[0]);
    assert!(out[0].contains("closed the connection"), "got: {}", out[0]);
}

#[test]
fn the_timeout_is_configurable_per_server() {
    let text = r#"
[mcp]
timeout_secs = 5

[mcp.slow]
command = "true"
timeout_secs = 120

[mcp.quick]
command = "true"
"#;
    let parsed = kora_runtime::Config::parse(text).unwrap();
    assert_eq!(parsed.mcp_servers["slow"].timeout_secs, 120);
    // The `[mcp]` default covers a server that does not say.
    assert_eq!(parsed.mcp_servers["quick"].timeout_secs, 5);
}
