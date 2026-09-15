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
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kora_runtime::journal::Journal;
use kora_runtime::{Config, Interpreter, Run, RunStatus};

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

fn agent_call(topic: &str) -> serde_json::Value {
    serde_json::json!({
        "message": {
            "content": "",
            "tool_calls": [{
                "function": { "name": "specialist", "arguments": { "topic": topic } }
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

fn run_durable(
    config_text: &str,
    src: &str,
    run: Run,
    path: PathBuf,
) -> (Vec<String>, Run, Option<String>) {
    let program = kora_syntax::parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = Interpreter::new();
    let parsed = Config::parse(config_text).unwrap();
    i.sinks = parsed.sinks.clone();
    i.config = parsed;
    i.program_name = "test.ko".into();
    i.journal = Arc::new(Mutex::new(Journal::open(run, path).unwrap()));

    let error = match i.run(&program) {
        Ok(()) => {
            i.journal
                .lock()
                .unwrap()
                .finish(RunStatus::Completed)
                .unwrap();
            None
        }
        Err(e) if e.is_suspension() => None,
        Err(e) => Some(e.message),
    };
    let saved = i.journal.lock().unwrap().run().clone();
    (i.output, saved, error)
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

#[test]
fn durable_resume_skips_context_work_inside_a_completed_model_call() {
    const PROGRAM: &str = r#"def main():
    with context(max_input_tokens = 1000, reserve_output_tokens = 100):
        result: str = analyze("request", "prepare a reply")
    match result:
        case Ok(reply):
            decision = ask_human("approve?", reply)
            print(f"got: {decision}")
"#;

    let (endpoint, seen) = spawn_scripted_provider(vec![final_answer("ready")]);
    let scratch = std::env::temp_dir().join(format!(
        "kora-context-durable-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&scratch).unwrap();
    let path = scratch.join("r1.jsonl");

    let (_, mut suspended, err) = run_durable(
        &config(&endpoint),
        PROGRAM,
        Run::new("r1".into(), "test.ko".into()),
        path.clone(),
    );
    assert!(err.is_none(), "{err:?}");
    assert_eq!(suspended.status, RunStatus::Suspended);
    assert_eq!(seen.lock().unwrap().len(), 1);

    {
        let mut journal = Journal::open(suspended, path.clone()).unwrap();
        journal.answer("yes").unwrap();
        suspended = journal.run().clone();
    }

    let (output, completed, err) = run_durable(&config(&endpoint), PROGRAM, suspended, path);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(completed.status, RunStatus::Completed);
    assert_eq!(output, vec!["got: yes"]);
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "resume must not send the completed model call again"
    );

    std::fs::remove_dir_all(scratch).ok();
}

#[test]
fn failed_context_admission_advances_its_empty_slot_on_resume() {
    const PROGRAM: &str = r#"def main():
    with context(max_input_tokens = 1, reserve_output_tokens = 0):
        result: str = analyze("request", "prepare a reply")
    match result:
        case Failed(reason):
            decision = ask_human("continue?", reason)
            print(f"got: {decision}")
"#;

    let (endpoint, seen) = spawn_scripted_provider(Vec::new());
    let scratch = std::env::temp_dir().join(format!(
        "kora-context-refusal-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&scratch).unwrap();
    let path = scratch.join("r1.jsonl");

    let (_, mut suspended, err) = run_durable(
        &config(&endpoint),
        PROGRAM,
        Run::new("r1".into(), "test.ko".into()),
        path.clone(),
    );
    assert!(err.is_none(), "the context refusal is a value: {err:?}");
    assert_eq!(suspended.status, RunStatus::Suspended);
    assert!(seen.lock().unwrap().is_empty(), "no request should be sent");

    {
        let mut journal = Journal::open(suspended, path.clone()).unwrap();
        journal.answer("yes").unwrap();
        suspended = journal.run().clone();
    }

    let (output, completed, err) = run_durable(&config(&endpoint), PROGRAM, suspended, path);
    assert!(
        err.is_none(),
        "the empty context slot must not diverge: {err:?}"
    );
    assert_eq!(completed.status, RunStatus::Completed);
    assert_eq!(output, vec!["got: yes"]);
    assert!(seen.lock().unwrap().is_empty(), "resume sends no request");

    std::fs::remove_dir_all(scratch).ok();
}

#[test]
fn completed_tool_call_advances_parallel_descendant_scopes() {
    const PROGRAM: &str = r#"use time

tool lookup(id: str) -> str:
    stamps = parallel for item in ["a", "b"]:
        return time.now()
    return id

def main():
    result: str = analyze("go", "use the tool, then answer", tools=[lookup])
    match result:
        case Ok(text):
            decision = ask_human("continue?", text)
            later = parallel for item in ["a", "b"]:
                return time.now()
            print(f"got: {decision} {later}")
"#;

    let (endpoint, seen) =
        spawn_scripted_provider(vec![tool_call("record"), final_answer("ready")]);
    let scratch = std::env::temp_dir().join(format!(
        "kora-context-descendants-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&scratch).unwrap();
    let path = scratch.join("r1.jsonl");

    let (_, mut suspended, err) = run_durable(
        &config(&endpoint),
        PROGRAM,
        Run::new("r1".into(), "test.ko".into()),
        path.clone(),
    );
    assert!(
        err.is_none(),
        "the first run should suspend cleanly: {err:?}"
    );
    assert_eq!(suspended.status, RunStatus::Suspended);
    assert_eq!(seen.lock().unwrap().len(), 2);

    {
        let mut journal = Journal::open(suspended, path.clone()).unwrap();
        journal.answer("yes").unwrap();
        suspended = journal.run().clone();
    }

    let (output, completed, err) = run_durable(&config(&endpoint), PROGRAM, suspended, path);
    assert!(err.is_none(), "descendant scopes must advance: {err:?}");
    assert_eq!(completed.status, RunStatus::Completed);
    assert_eq!(output.len(), 1);
    assert!(output[0].starts_with("got: yes ["), "got {output:?}");
    assert_eq!(seen.lock().unwrap().len(), 2, "model work is replayed");

    std::fs::remove_dir_all(scratch).ok();
}

#[test]
fn nested_agent_model_slots_preserve_the_outer_model_slot() {
    const PROGRAM: &str = r#"agent specialist(topic: str) -> str:
    nested: str = analyze(topic, "answer briefly")
    match nested:
        case Ok(text):
            return text
        case Failed(reason):
            return reason

def main():
    result: str = analyze("go", "ask the specialist, then answer", tools=[specialist])
    match result:
        case Ok(text):
            decision = ask_human("continue?", text)
            print(f"got: {decision}")
"#;

    let (endpoint, seen) = spawn_scripted_provider(vec![
        agent_call("topic"),
        final_answer("nested answer"),
        final_answer("outer answer"),
    ]);
    let scratch = std::env::temp_dir().join(format!(
        "kora-context-nested-agent-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&scratch).unwrap();
    let path = scratch.join("r1.jsonl");

    let (_, mut suspended, err) = run_durable(
        &config(&endpoint),
        PROGRAM,
        Run::new("r1".into(), "test.ko".into()),
        path.clone(),
    );
    assert!(
        err.is_none(),
        "the first run should suspend cleanly: {err:?}"
    );
    assert_eq!(suspended.status, RunStatus::Suspended);
    assert_eq!(seen.lock().unwrap().len(), 3);

    {
        let mut journal = Journal::open(suspended, path.clone()).unwrap();
        journal.answer("yes").unwrap();
        suspended = journal.run().clone();
    }

    let (output, completed, err) = run_durable(&config(&endpoint), PROGRAM, suspended, path);
    assert!(err.is_none(), "the outer model slot must replay: {err:?}");
    assert_eq!(completed.status, RunStatus::Completed);
    assert_eq!(output, vec!["got: yes"]);
    assert_eq!(seen.lock().unwrap().len(), 3, "neither model repeats");

    std::fs::remove_dir_all(scratch).ok();
}
