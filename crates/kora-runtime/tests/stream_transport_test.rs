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

/// A provider whose stream is interleaved with the protocol frames a real
/// one sends: SSE keep-alive comments, and payloads behind `data:` with and
/// without the optional space.
fn spawn_noisy_provider(frames: Vec<String>) -> (String, Arc<AtomicUsize>) {
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

            let mut payload = String::new();
            // A keep-alive before anything else, which is when a provider is
            // most likely to send one.
            payload.push_str(": ping\n");
            for (n, frame) in frames.iter().enumerate() {
                let event =
                    serde_json::json!({"message": {"content": frame}, "done": false}).to_string();
                // Alternate the optional space after `data:`; both are legal
                // SSE and a provider may use either.
                if n % 2 == 0 {
                    payload.push_str(&format!("data: {event}\n"));
                } else {
                    payload.push_str(&format!("data:{event}\n"));
                }
                payload.push_str(": keep-alive\n");
            }
            payload.push_str(&format!(
                "data: {}\n",
                serde_json::json!({
                    "message": {"content": ""},
                    "done": true,
                    "prompt_eval_count": 3,
                    "eval_count": 2,
                })
            ));
            payload.push_str("data: [DONE]\n");

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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

#[test]
fn keep_alive_comments_and_done_markers_do_not_break_the_stream() {
    // A `: ping` line is an SSE comment: a frame that exists to say nothing.
    // Passing it on as a payload failed the whole call as "not JSON" over a
    // keep-alive, so a slow answer was more likely to fail than a fast one.
    let (endpoint, _) = spawn_noisy_provider(hello_frames());
    let i = run(
        &config(&endpoint),
        r#"
def main():
    answer: str = analyze("q", "greet") on token(t):
        print(f"piece: {t}")
    match answer:
        case Ok(text):
            print(f"final: {text}")
        case Failed(why):
            print(f"failed: {why}")
"#,
    );
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
fn protocol_only_frames_are_not_counted_as_answer_text() {
    // The usage frame and `[DONE]` arrive after the answer but carry none of
    // it, and the handler is only ever handed characters of the answer.
    let (endpoint, _) = spawn_noisy_provider(hello_frames());
    let i = run(
        &config(&endpoint),
        r#"
def main():
    answer: str = analyze("q", "greet") on token(t):
        print(f"[{t}]")
    match answer:
        case Ok(text):
            print("done")
"#,
    );
    assert_eq!(i.output, vec!["[hel]", "[lo ]", "[there]", "done"]);
    // Usage still reaches the budget, even though those frames showed the
    // program nothing.
    assert_eq!(i.tokens_in, 3);
    assert_eq!(i.tokens_out, 2);
}

/// A provider whose first attempt dies after sending nothing but protocol —
/// a keep-alive and a usage frame — and whose second attempt answers.
///
/// The connection is dropped without terminating the chunked body, which is
/// what a real socket failure looks like from the client's side: a read
/// error part way through the response, not a clean end.
fn spawn_provider_that_breaks_before_answering(frames: Vec<String>) -> (String, Arc<AtomicUsize>) {
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
            let attempt = seen.fetch_add(1, Ordering::SeqCst);

            if attempt == 0 {
                // Protocol only: a keep-alive comment and a usage frame.
                // Neither puts a character of the answer in front of the
                // program, so the attempt is still safe to repeat.
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
                );
                for line in [
                    ": ping\n".to_string(),
                    format!(
                        "data: {}\n",
                        serde_json::json!({
                            "message": {"content": ""},
                            "done": false,
                            "prompt_eval_count": 3,
                        })
                    ),
                ] {
                    let _ = stream.write_all(format!("{:x}\r\n{line}\r\n", line.len()).as_bytes());
                }
                let _ = stream.flush();
                // No terminating chunk: the body just stops.
                drop(stream);
                continue;
            }

            let mut payload = String::new();
            for frame in &frames {
                payload.push_str(&format!(
                    "data: {}\n",
                    serde_json::json!({"message": {"content": frame}, "done": false})
                ));
            }
            payload.push_str(&format!(
                "data: {}\n",
                serde_json::json!({
                    "message": {"content": ""},
                    "done": true,
                    "prompt_eval_count": 3,
                    "eval_count": 2,
                })
            ));
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    (format!("http://127.0.0.1:{port}"), requests)
}

fn config_with_retries(endpoint: &str, retries: u32) -> String {
    format!(
        r#"
[models]
default = "local:test-model"
max_retries = {retries}

[models.local]
endpoint = "{endpoint}"
"#
    )
}

#[test]
fn a_stream_that_showed_nothing_is_retried_even_after_protocol_frames() {
    // The frames a stream opens with — a keep-alive, a role-only delta,
    // usage totals, `[DONE]` — carry none of the answer. A connection that
    // dies just after one of those has shown the program nothing, so
    // forfeiting the retry over it loses an answer for no reason.
    let (endpoint, requests) = spawn_provider_that_breaks_before_answering(hello_frames());
    let i = run(
        &config_with_retries(&endpoint, 2),
        r#"
def main():
    answer: str = analyze("q", "greet") on token(t):
        print(f"piece: {t}")
    match answer:
        case Ok(text):
            print(f"final: {text}")
        case Failed(why):
            print(f"failed: {why}")
"#,
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        2,
        "the first attempt showed nothing, so it should have been retried"
    );
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
fn a_stream_that_already_wrote_is_never_retried() {
    // The other half of the same rule, and the one that must not regress:
    // once characters have reached the program, a second attempt would
    // write the answer twice on top of output already acted on.
    let (endpoint, requests) = spawn_error_provider("upstream is down");
    let i = run(
        &config_with_retries(&endpoint, 3),
        r#"
def main():
    answer: str = analyze("q", "greet") on token(t):
        print(f"piece: {t}")
    match answer:
        case Failed(why):
            print("failed")
        case Ok(text):
            print(f"final: {text}")
"#,
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "a stream that emitted must not be sent again"
    );
    assert_eq!(i.output, vec!["piece: par", "failed"]);
}

/// Run and expect the program to fail, keeping the interpreter so the test
/// can look at what the run spent before it stopped.
fn run_failing(config_text: &str, src: &str) -> (kora_runtime::Interpreter, String) {
    let program = kora_syntax::parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = interpreter(config_text);
    let message = match i.run(&program) {
        Ok(()) => panic!("the run should have failed\n{src}"),
        Err(e) => e.message,
    };
    (i, message)
}

#[test]
fn a_handler_that_fails_still_charges_the_call_it_was_watching() {
    // The handler raising is the program failing, not the provider. The
    // call was made and the tokens are spent either way, so a budget that
    // forgot it would make raising in a handler the cheapest way to run a
    // model: the meter would read zero for work that really happened.
    let (endpoint, requests) = spawn_streaming_provider(hello_frames(), 11, 7);
    let (i, message) = run_failing(
        &config(&endpoint),
        r#"
def main():
    answer: str = analyze("q", "greet") on token(t):
        zero = len("")
        print(f"{1 / zero}")
    print("unreachable")
"#,
    );
    assert!(
        message.contains("division by zero"),
        "the handler's own error should surface: {message}"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(
        i.model_calls, 1,
        "the call happened, whatever the handler did next"
    );
    assert_eq!(
        i.budget.spent_calls(),
        1,
        "a call the budget never saw is a call that can be repeated for free"
    );
}

/// A provider that accepts the request and then says nothing at all.
fn spawn_silent_provider() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&requests);

    std::thread::spawn(move || {
        // Held so the connections stay open rather than being closed by the
        // socket dropping, which would read as a refusal instead of silence.
        let mut open = Vec::new();
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
            }
            seen.fetch_add(1, Ordering::SeqCst);
            open.push(stream);
        }
    });

    (format!("http://127.0.0.1:{port}"), requests)
}

#[test]
fn a_stream_that_never_answers_times_out_as_a_failed_outcome() {
    // A provider that holds the connection open forever is the failure a
    // deadline exists for. It has to end the call as a value the program
    // matches on, inside the configured timeout, rather than hanging the run.
    let (endpoint, requests) = spawn_silent_provider();
    let config_text = format!(
        r#"
[models]
default = "local:test-model"
max_retries = 0
timeout_secs = 1

[models.local]
endpoint = "{endpoint}"
"#
    );
    let started = std::time::Instant::now();
    let i = run(
        &config_text,
        r#"
def main():
    answer: str = analyze("q", "greet") on token(t):
        print(f"piece: {t}")
    match answer:
        case Failed(why):
            print("failed")
        case Ok(text):
            print(f"final: {text}")
"#,
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "the deadline should end the call, not the test's patience"
    );
    assert_eq!(i.output, vec!["failed"]);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "no retry was configured"
    );
}
