//! Streamed `analyze()` against a real socket.
//!
//! The other streaming tests run against `with mock analyze`, which stands in
//! for the whole call and returns before the budget is ever consulted. That
//! leaves the parts worth arguing about untested: whether a streamed call is
//! charged like a blocking one, whether `max_calls` stops the second one,
//! and whether the pieces the handler saw match the frames the provider
//! actually sent. Those are properties of the transport and the accounting
//! around it, so they need a provider that speaks over a socket.
//!
//! No network: the fixture binds a loopback port and serves scripted frames.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A stand-in Ollama server that streams `frames` as newline-delimited JSON,
/// then reports usage on a final frame.
///
/// Returns the endpoint and a counter of how many requests arrived, so a test
/// can assert that a budget stopped a call rather than merely that the answer
/// looked short.
fn spawn_streaming_provider(
    frames: Vec<String>,
    tokens_in: u64,
    tokens_out: u64,
) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&requests);

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
            seen.fetch_add(1, Ordering::SeqCst);

            // The answer travels as the JSON object the schema asks for, so
            // the frames spell out `{"answer":"..."}` a piece at a time --
            // exactly the fragmentation `TextExtractor` exists to handle.
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
                    "prompt_eval_count": tokens_in,
                    "eval_count": tokens_out,
                })
                .to_string(),
            );
            payload.push('\n');

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
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

fn interpreter(config_text: &str) -> kora_runtime::Interpreter {
    let mut i = kora_runtime::Interpreter::new();
    i.config = kora_runtime::Config::parse(config_text).unwrap();
    i.program_name = "test.ko".into();
    i
}

fn run(config_text: &str, src: &str) -> kora_runtime::Interpreter {
    let program = kora_syntax::parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = interpreter(config_text);
    i.run(&program)
        .unwrap_or_else(|e| panic!("the run should not fail: {}\n{src}", e.message));
    i
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

#[test]
fn a_streamed_answer_arrives_in_pieces_and_still_matches_as_one_outcome() {
    let (endpoint, _) = spawn_streaming_provider(hello_frames(), 11, 7);
    let i = run(
        &config(&endpoint),
        r#"
def main():
    answer: str = analyze("q", "greet") on token(t):
        print(f"piece: {t}")
    match answer:
        case Ok(text):
            print(f"final: {text}")
        case Uncertain(reason):
            print(f"uncertain: {reason}")
        case Failed(why):
            print(f"failed: {why}")
"#,
    );
    // The JSON around the answer never reaches the handler: the pieces are
    // the answer's characters, and the syntax carrying them is not the
    // program's business.
    assert_eq!(
        i.output,
        vec![
            "piece: hel",
            "piece: lo ",
            "piece: there",
            "final: hello there"
        ]
    );
}

#[test]
fn a_streamed_call_is_charged_to_the_budget_like_a_blocking_one() {
    // The guarantee under test: streaming changes when characters arrive,
    // never what a call costs. A streamed call that skipped the budget would
    // make `max_tokens` a limit on blocking calls only.
    let (endpoint, _) = spawn_streaming_provider(hello_frames(), 11, 7);
    let i = run(
        &config(&endpoint),
        r#"
def main():
    answer: str = analyze("q", "greet") on token(t):
        pass
    match answer:
        case Ok(text):
            print(text)
"#,
    );
    assert_eq!(i.tokens_in, 11, "prompt tokens should be charged");
    assert_eq!(i.tokens_out, 7, "completion tokens should be charged");
    assert_eq!(i.model_calls, 1);
}

#[test]
fn max_calls_stops_a_second_streamed_call_before_it_reaches_the_provider() {
    // The counter proves the budget refused rather than the provider
    // answering twice and the second answer being discarded.
    let (endpoint, requests) = spawn_streaming_provider(hello_frames(), 11, 7);
    let i = run(
        &config(&endpoint),
        r#"
def main():
    with budget(max_calls = 1):
        first: str = analyze("q", "greet") on token(t):
            pass
        match first:
            case Ok(text):
                print(f"first: {text}")
            case Exhausted(limit):
                print(f"first exhausted: {limit}")
        second: str = analyze("q", "greet again") on token(t):
            pass
        match second:
            case Ok(text):
                print(f"second: {text}")
            case Exhausted(limit):
                print(f"second exhausted: {limit}")
"#,
    );
    assert_eq!(
        i.output,
        vec!["first: hello there", "second exhausted: calls"]
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "the refused call must never reach the socket"
    );
}

#[test]
fn max_tokens_counts_what_a_streamed_call_spent() {
    // The first call spends 18. The ceiling is checked before a call rather
    // than predicted for it -- nothing knows what the next answer will cost
    // -- so the limit has to sit below what was already spent for the second
    // call to be refused. That refusal is only possible if the streamed
    // usage was recorded at all, which is the point of the test.
    let (endpoint, requests) = spawn_streaming_provider(hello_frames(), 11, 7);
    let i = run(
        &config(&endpoint),
        r#"
def main():
    with budget(max_tokens = 15):
        first: str = analyze("q", "greet") on token(t):
            pass
        match first:
            case Ok(text):
                print("first ok")
        second: str = analyze("q", "again") on token(t):
            pass
        match second:
            case Exhausted(limit):
                print(f"second exhausted: {limit}")
            case Ok(text):
                print("second ok")
"#,
    );
    assert_eq!(i.output, vec!["first ok", "second exhausted: tokens"]);
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[test]
fn a_provider_error_delivered_after_a_200_becomes_a_failed_outcome() {
    // Both providers report a mid-stream failure inside the body rather than
    // with a status code, because the status went out before anything went
    // wrong. It has to read as `Failed`, not as a crash.
    let (endpoint, _) = spawn_error_provider("upstream is down");
    let i = run(
        &config(&endpoint),
        r#"
def main():
    answer: str = analyze("q", "greet") on token(t):
        print(f"piece: {t}")
    match answer:
        case Failed(why):
            print(f"failed: {why}")
        case Ok(text):
            print(f"ok: {text}")
"#,
    );
    // The characters that did arrive are not taken back. The program has
    // already acted on them, and a failure that pretended nothing happened
    // would be the one thing the handler cannot recover from.
    assert_eq!(i.output[0], "piece: par");
    assert!(
        i.output[1].starts_with("failed: "),
        "got: {:?}",
        i.output[1]
    );
    assert!(
        i.output[1].contains("upstream is down"),
        "the provider's reason should survive: {:?}",
        i.output[1]
    );
    // How much was already seen is part of the reason, because "it failed"
    // and "it failed after writing to your terminal" call for different
    // recovery.
    assert!(
        i.output[1].contains("already arrived"),
        "the reason should say output escaped: {:?}",
        i.output[1]
    );
}

/// A provider that streams one text frame and then reports an error, which is
/// how both providers signal a failure that began after the headers went out.
fn spawn_error_provider(message: &str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&requests);
    let message = message.to_string();

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
            seen.fetch_add(1, Ordering::SeqCst);

            let payload = format!(
                "{}\n{}\n",
                serde_json::json!({"message": {"content": r#"{"answer":"par"#}, "done": false}),
                serde_json::json!({"error": {"message": message}}),
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    (format!("http://127.0.0.1:{port}"), requests)
}
