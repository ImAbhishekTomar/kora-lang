//! Streaming inside a durable run.
//!
//! Streaming and durability meet at the one place the language cares about
//! most: a stream hands characters to the program *before* the call has an
//! outcome, so the pieces are effects in their own right. A resumed run has
//! to arrive at the same place without writing those pieces twice and
//! without asking the provider again.
//!
//! These tests use a loopback provider rather than `with mock analyze`,
//! because the thing under test is what the journal holds after real frames
//! have crossed a socket.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kora_runtime::journal::Journal;
use kora_runtime::{Config, Interpreter, Run, RunStatus};
use kora_syntax::parse;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "kora-durstream-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn run_path(&self, id: &str) -> PathBuf {
        self.0.join(format!("{id}.jsonl"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Read one HTTP request off the socket and report that it arrived.
fn take_request(stream: &mut std::net::TcpStream, seen: &AtomicUsize) {
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
    seen.fetch_add(1, Ordering::SeqCst);
}

fn serve(stream: &mut std::net::TcpStream, content_type: &str, payload: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len(),
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// A provider that streams one visible piece and then reports an error,
/// which is how both providers signal a failure that began after the
/// headers went out.
fn spawn_error_provider(message: &str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&requests);
    let message = message.to_string();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            take_request(&mut stream, &seen);
            let payload = format!(
                "{}\n{}\n",
                serde_json::json!({"message": {"content": r#"{"answer":"par"#}, "done": false}),
                serde_json::json!({"error": {"message": message}}),
            );
            serve(&mut stream, "application/x-ndjson", &payload);
        }
    });

    (format!("http://127.0.0.1:{port}"), requests)
}

/// A provider that streams a complete answer in pieces.
fn spawn_streaming_provider(frames: Vec<String>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&requests);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            take_request(&mut stream, &seen);
            let mut payload = String::new();
            for frame in &frames {
                payload.push_str(
                    &serde_json::json!({"message": {"content": frame}, "done": false}).to_string(),
                );
                payload.push('\n');
            }
            payload.push_str(
                &serde_json::json!({
                    "message": {"content": ""},
                    "done": true,
                    "prompt_eval_count": 3,
                    "eval_count": 2,
                })
                .to_string(),
            );
            payload.push('\n');
            serve(&mut stream, "application/x-ndjson", &payload);
        }
    });

    (format!("http://127.0.0.1:{port}"), requests)
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

/// Run `src` against a durable journal, returning (output, run, error).
fn run_durable(
    config_text: &str,
    src: &str,
    run: Run,
    path: PathBuf,
) -> (Vec<String>, Run, Option<String>) {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut interp = Interpreter::new();
    interp.config = Config::parse(config_text).unwrap();
    interp.program_name = "test.ko".into();
    interp.journal = Arc::new(Mutex::new(Journal::open(run, path).unwrap()));

    let error = match interp.run(&program) {
        Ok(()) => {
            let mut j = interp.journal.lock().unwrap();
            j.finish(RunStatus::Completed).unwrap();
            None
        }
        Err(e) if e.is_suspension() => None,
        Err(e) => Some(e.message),
    };
    let saved = {
        let j = interp.journal.lock().unwrap();
        j.run().clone()
    };
    (interp.output, saved, error)
}

/// The operation id of a call in `src`, the way the runtime names it.
///
/// Computed rather than written down: effect identity is structural, so a
/// fixture that hard-coded it would be restating the numbering instead of
/// following it.
fn call_op(src: &str, callee: &str) -> String {
    let program = parse(src).expect("the fixture should parse");
    let ids = kora_syntax::ops::assign(&program);
    // A call's span begins at its opening parenthesis, not at the callee.
    let start = src.find(callee).expect("the call should be in the fixture") + callee.len() - 1;
    let span = kora_syntax::token::Span::new(start, start, 0, 0);
    ids.get(span)
        .unwrap_or_else(|| panic!("the call at byte {start} was not numbered"))
        .to_string()
}

/// The answer `{"answer":"hello there"}` split the way a provider would.
fn hello_frames() -> Vec<String> {
    vec![
        r#"{"ans"#.to_string(),
        r#"wer":"hel"#.to_string(),
        "lo ".to_string(),
        r#"there"}"#.to_string(),
    ]
}

const FAILING_PROGRAM: &str = r#"
def main():
    answer: str = analyze("q", "greet") on token(t):
        print(f"piece: {t}")
    match answer:
        case Failed(why):
            print("failed")
        case Ok(text):
            print(f"ok: {text}")
    print("after")
"#;

#[test]
fn a_stream_that_failed_replays_its_pieces_instead_of_diverging() {
    // The pieces a broken stream managed to write are output the program
    // already produced, so they are journaled like any other line. A resume
    // that forgot them would find those lines where its own next effect
    // should be, and refuse the whole run as a changed program.
    let scratch = Scratch::new("failed-replay");
    let path = scratch.run_path("r1");
    let (endpoint, requests) = spawn_error_provider("upstream is down");
    let cfg = config(&endpoint);

    let (first, run, err) = run_durable(
        &cfg,
        FAILING_PROGRAM,
        Run::new("r1".into(), "test.ko".into()),
        path.clone(),
    );
    assert!(err.is_none(), "the first run should not fail: {err:?}");
    assert_eq!(first, vec!["piece: par", "failed", "after"]);
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    let (second, _, err) = run_durable(&cfg, FAILING_PROGRAM, run, path);
    assert_eq!(err, None, "a resume must not diverge");
    assert!(
        second.is_empty(),
        "a resume continues the story instead of retelling it, got {second:?}"
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "a resumed streamed call must not reach the provider again"
    );
}

const OK_PROGRAM: &str = r#"
def main():
    answer: str = analyze("q", "greet") on token(t):
        print(f"piece: {t}")
    match answer:
        case Ok(text):
            print(f"ok: {text}")
        case Failed(why):
            print("failed")
    print("after")
"#;

#[test]
fn a_completed_stream_replays_its_pieces_exactly_once() {
    let scratch = Scratch::new("ok-replay");
    let path = scratch.run_path("r1");
    let (endpoint, requests) = spawn_streaming_provider(hello_frames());
    let cfg = config(&endpoint);

    let (first, run, err) = run_durable(
        &cfg,
        OK_PROGRAM,
        Run::new("r1".into(), "test.ko".into()),
        path.clone(),
    );
    assert!(err.is_none(), "the first run should not fail: {err:?}");
    assert_eq!(
        first,
        vec![
            "piece: hel",
            "piece: lo ",
            "piece: there",
            "ok: hello there",
            "after"
        ]
    );

    let (second, _, err) = run_durable(&cfg, OK_PROGRAM, run, path);
    assert_eq!(err, None, "a resume must not diverge");
    assert!(
        second.is_empty(),
        "a resume continues the story instead of retelling it, got {second:?}"
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "a replayed call must not reach the provider again"
    );
}

/// The journal a crash mid-stream leaves behind: the pieces already handed
/// to the program are on disk, the model call has no outcome, and the run
/// never reached a terminal status.
///
/// Built by truncating a completed run rather than by killing a process, so
/// the state under test is exact instead of a race. The kill-and-resume
/// version of the same thing runs against the real binary in
/// `kora-cli/tests/durable_crash_test.rs`.
fn interrupted_mid_stream(mut run: Run, keep_pieces: usize) -> Run {
    use kora_runtime::journal::Effect;

    let mut kept = Vec::new();
    let mut pieces = 0;
    for mut entry in run.entries {
        match &entry.effect {
            // The mark a streamed call leaves before it is sent. The
            // completed run replaced it with the outcome; a crashed one
            // never got that far.
            Effect::Model { .. } => {
                entry.effect = Effect::Attempted {
                    name: "a streamed analyze()".to_string(),
                };
                kept.push(entry);
            }
            Effect::Output { text } if text.starts_with("piece: ") && pieces < keep_pieces => {
                pieces += 1;
                kept.push(entry);
            }
            // Everything else happened after the call returned, so a crash
            // during the stream never wrote it.
            _ => {}
        }
    }
    run.entries = kept;
    run.status = RunStatus::Running;
    run
}

#[test]
fn a_crash_mid_stream_does_not_resume_into_a_second_request() {
    // A stream is the one model call whose pieces are already output by the
    // time it has an outcome. Resuming it as though nothing had happened
    // would ask the provider again and write the answer on top of the half
    // the user can already see.
    let scratch = Scratch::new("crash");
    let path = scratch.run_path("r1");
    let (endpoint, requests) = spawn_streaming_provider(hello_frames());
    let cfg = config(&endpoint);

    let (first, run, err) = run_durable(
        &cfg,
        FAILING_PROGRAM,
        Run::new("r1".into(), "test.ko".into()),
        path.clone(),
    );
    assert!(err.is_none(), "the first run should not fail: {err:?}");
    assert_eq!(first[0], "piece: hel");
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    let crashed = interrupted_mid_stream(run, 2);
    std::fs::remove_file(&path).ok();
    let (second, _, err) = run_durable(&cfg, FAILING_PROGRAM, crashed, path);

    assert_eq!(err, None, "an interrupted stream must not stop the resume");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "an interrupted stream must not be sent to the provider again"
    );
    assert!(
        !second.iter().any(|line| line.starts_with("piece: ")),
        "pieces already written must not be written again, got {second:?}"
    );
    assert_eq!(
        second,
        vec!["failed", "after"],
        "the program sees the interrupted stream as a failure it can match on"
    );
}

#[test]
fn a_crash_before_any_piece_escaped_is_sent_again() {
    // The other direction of the same rule. If the stream died before a
    // single piece was written down, nothing observable happened, and
    // refusing the call would cost the run its answer for no reason.
    let scratch = Scratch::new("crash-early");
    let path = scratch.run_path("r1");
    let (endpoint, requests) = spawn_streaming_provider(hello_frames());
    let cfg = config(&endpoint);

    let (_, run, err) = run_durable(
        &cfg,
        OK_PROGRAM,
        Run::new("r1".into(), "test.ko".into()),
        path.clone(),
    );
    assert!(err.is_none(), "the first run should not fail: {err:?}");

    let crashed = interrupted_mid_stream(run, 0);
    std::fs::remove_file(&path).ok();
    let (second, _, err) = run_durable(&cfg, OK_PROGRAM, crashed, path);

    assert_eq!(err, None);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        2,
        "a stream that showed nothing is as safe to send again as one that never started"
    );
    assert_eq!(
        second,
        vec![
            "piece: hel",
            "piece: lo ",
            "piece: there",
            "ok: hello there",
            "after"
        ]
    );
}

#[test]
fn resuming_an_interrupted_stream_twice_says_the_same_thing() {
    // A resume can itself be killed. The second one has to reach the same
    // answer as the first, or the run's history stops being a history.
    let scratch = Scratch::new("crash-twice");
    let path = scratch.run_path("r1");
    let (endpoint, requests) = spawn_streaming_provider(hello_frames());
    let cfg = config(&endpoint);

    let (_, run, _) = run_durable(
        &cfg,
        FAILING_PROGRAM,
        Run::new("r1".into(), "test.ko".into()),
        path.clone(),
    );
    let crashed = interrupted_mid_stream(run, 2);

    std::fs::remove_file(&path).ok();
    let (second, _, err) = run_durable(&cfg, FAILING_PROGRAM, crashed.clone(), path.clone());
    assert_eq!(err, None);
    assert_eq!(second, vec!["failed", "after"]);

    std::fs::remove_file(&path).ok();
    let (third, _, err) = run_durable(&cfg, FAILING_PROGRAM, crashed, path);
    assert_eq!(err, None);
    assert_eq!(third, second, "a second resume must agree with the first");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

// --- tool-using calls, interrupted ---

const TOOLS_PROGRAM: &str = r#"type Answer:
    body: str

tool divide(a: int, b: int) -> str:
    return f"{a}-{b}"

def main():
    a: Answer = analyze("compute", "divide 10 by 2", tools=[divide])
    match a:
        case Ok(v):
            print(f"ok: {v.body}")
        case Failed(why):
            print("failed")
    print("after")
"#;

#[test]
fn an_interrupted_tool_using_call_stops_the_resume_instead_of_running_tools_twice() {
    // A tool loop is many turns inside one journaled effect, and the tools
    // it ran leave no trace of their own: they are the model's decisions,
    // made inside the call. So a crash part way through is exactly the case
    // where "did it happen" stops being knowable, and re-running the loop
    // could open the same issue or charge the same card a second time. The
    // honest answer is the one writes already give: stop, and name the call
    // in doubt.
    use kora_runtime::journal::{Effect, Entry, Scope};

    let scratch = Scratch::new("tools-interrupted");
    let path = scratch.run_path("r1");

    let mut run = Run::new("r1".into(), "test.ko".into());
    run.entries = vec![Entry {
        scope: Scope::root(),
        seq: 0,
        site: format!(
            "test.ko:{}#analyze#model",
            call_op(TOOLS_PROGRAM, "analyze(")
        ),
        effect: Effect::Attempted {
            name: "a tool-using analyze()".to_string(),
        },
    }];

    // No provider is configured, which is itself part of the assertion: if
    // the resume tried to run the loop again it would fail reaching for one
    // instead of stopping on the mark.
    let (output, _, err) = run_durable(
        "[models]\ndefault = \"local:test-model\"\n",
        TOOLS_PROGRAM,
        run,
        path,
    );
    let message = err.expect("an interrupted tool-using call must stop the resume");
    assert!(
        message.contains("whether its tools ran is unknown"),
        "the resume should name what is in doubt: {message}"
    );
    assert!(output.is_empty(), "nothing runs past the mark: {output:?}");
}

#[test]
fn a_plain_call_leaves_no_mark_because_repeating_it_is_only_tokens() {
    // The rule has to stay narrow. A call with no stream and no tools
    // changes nothing outside the process, so marking it would turn a
    // recoverable crash into a dead run for no gain.
    let scratch = Scratch::new("plain-unmarked");
    let path = scratch.run_path("r1");
    let (endpoint, requests) = spawn_streaming_provider(hello_frames());
    let cfg = config(&endpoint);

    let src = r#"
def main():
    answer: str = analyze("q", "greet")
    match answer:
        case Ok(text):
            print(f"ok: {text}")
        case Failed(why):
            print("failed")
"#;
    let (_, run, err) = run_durable(&cfg, src, Run::new("r1".into(), "test.ko".into()), path);
    assert!(err.is_none(), "the first run should not fail: {err:?}");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(
        run.entries
            .iter()
            .all(|e| !matches!(e.effect, kora_runtime::journal::Effect::Attempted { .. })),
        "a plain call should leave an outcome, not a mark: {:?}",
        run.entries
    );
}
