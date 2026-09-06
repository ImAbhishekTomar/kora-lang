//! `http` — the one stdlib module that reaches the network.
//!
//! Driven against a loopback server rather than a mock, because the parts
//! worth arguing about are the ones a mock would stand in for: what a non-2xx
//! becomes, what a body that is not a string does, whether a private address
//! is refused, and whether a durable run replays an answer instead of asking
//! for it twice.

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
            "kora-http-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A server that answers every request with `status` and `body`, and reports
/// the method and body it was sent.
fn spawn_server(status: u16, body: &str) -> (String, Arc<Mutex<Vec<String>>>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(AtomicUsize::new(0));
    let log = Arc::clone(&seen);
    let count = Arc::clone(&requests);
    let body = body.to_string();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            let _ = reader.read_line(&mut first);
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
            let mut sent = vec![0u8; length];
            let _ = reader.read_exact(&mut sent);
            count.fetch_add(1, Ordering::SeqCst);
            log.lock().unwrap().push(format!(
                "{} {}",
                first.split_whitespace().next().unwrap_or(""),
                String::from_utf8_lossy(&sent)
            ));

            let reason = if status < 300 { "OK" } else { "Error" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    (format!("http://127.0.0.1:{port}"), seen, requests)
}

const CONFIG: &str = "[models]\ndefault = \"local:test-model\"\n\n[http]\nallow_private = true\n";

fn interpreter(config_text: &str) -> Interpreter {
    let mut i = Interpreter::new();
    i.config = Config::parse(config_text).unwrap();
    i.allow_private_hosts = i.config.http_allow_private;
    i.program_name = "test.ko".into();
    i
}

fn run(src: &str) -> Vec<String> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = interpreter(CONFIG);
    i.run(&program)
        .unwrap_or_else(|e| panic!("the run should not fail: {}\n{src}", e.message));
    i.output
}

fn run_err(src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = interpreter(CONFIG);
    match i.run(&program) {
        Ok(()) => panic!("the run should have failed\n{src}"),
        Err(e) => format!("{} {}", e.message, e.hint.unwrap_or_default()),
    }
}

#[test]
fn a_get_returns_status_and_body() {
    let (url, _, _) = spawn_server(200, r#"{"ok":true}"#);
    let out = run(&format!(
        r#"
use http

def main():
    match http.get("{url}/thing"):
        case Ok(r):
            code = r["status"]
            text = r["body"]
            print(f"{{code}} {{text}}")
        case Err(why):
            print(f"err: {{why}}")
"#
    ));
    assert_eq!(out, vec![r#"200 {"ok":true}"#]);
}

#[test]
fn a_post_sends_its_body() {
    let (url, seen, _) = spawn_server(200, "done");
    let out = run(&format!(
        r#"
use http

def main():
    match http.post("{url}/submit", "hello"):
        case Ok(r):
            code = r["status"]
            print(f"{{code}}")
        case Err(why):
            print(f"err: {{why}}")
"#
    ));
    assert_eq!(out, vec!["200"]);
    let log = seen.lock().unwrap();
    assert_eq!(log.len(), 1);
    assert!(log[0].starts_with("POST "), "got {:?}", log[0]);
    assert!(
        log[0].ends_with("hello"),
        "the body should arrive: {:?}",
        log[0]
    );
}

#[test]
fn a_non_2xx_is_an_err_not_a_crash() {
    // A failure the program decides about, like every other outcome in the
    // language. The status is in the reason so the program can tell a 404
    // from a 500 without re-requesting.
    let (url, _, _) = spawn_server(404, "missing");
    let out = run(&format!(
        r#"
use http

def main():
    match http.get("{url}/gone"):
        case Ok(r):
            print("unexpected ok")
        case Err(why):
            print(f"err: {{why}}")
"#
    ));
    assert_eq!(out.len(), 1);
    assert!(out[0].starts_with("err: "), "got {:?}", out[0]);
    assert!(
        out[0].contains("404"),
        "the status belongs in the reason: {:?}",
        out[0]
    );
}

#[test]
fn an_unreachable_host_is_an_err() {
    // Port 1 on loopback: nothing listens there, and the refusal is
    // immediate rather than a timeout the test has to wait out.
    let out = run(r#"
use http

def main():
    match http.get("http://127.0.0.1:1/nothing"):
        case Ok(r):
            print("unexpected ok")
        case Err(why):
            print("err")
"#);
    assert_eq!(out, vec!["err"]);
}

#[test]
fn a_private_address_is_refused_unless_allowed() {
    // The default. A program that fetches a URL it was handed should not be
    // able to reach the metadata service or a service on the same host.
    let program = parse(
        r#"
use http

def main():
    match http.get("http://127.0.0.1:9/x"):
        case Ok(r):
            print("unexpected ok")
        case Err(why):
            print(f"err: {why}")
"#,
    )
    .unwrap();
    let mut i = Interpreter::new();
    i.config = Config::parse("[models]\ndefault = \"local:m\"\n").unwrap();
    i.program_name = "test.ko".into();
    // Left at the default: private hosts are not allowed.
    i.run(&program).unwrap();
    assert_eq!(i.output.len(), 1);
    assert!(
        i.output[0].contains("private") || i.output[0].contains("loopback"),
        "the refusal should say why: {:?}",
        i.output[0]
    );
}

#[test]
fn a_body_that_is_not_a_string_is_refused() {
    let err = run_err(
        r#"
use http

def main():
    match http.post("http://127.0.0.1:9/x", 12):
        case Ok(r):
            print("ok")
        case Err(why):
            print("err")
"#,
    );
    assert!(err.contains("body must be a string"), "got: {err}");
}

#[test]
fn a_url_from_outside_the_program_is_refused() {
    // A URL assembled from a fetched document or a model answer is how a
    // request ends up pointed at an internal service.
    let (url, _, _) = spawn_server(200, "{}");
    let err = run_err(&format!(
        r#"
use http
use json

def main():
    match http.get("{url}/first"):
        case Ok(r):
            match http.get(r["body"]):
                case Ok(second):
                    print("followed")
                case Err(why):
                    print("err")
        case Err(why):
            print("err")
"#
    ));
    assert!(
        err.contains("came from outside the program"),
        "following a fetched URL should be refused: {err}"
    );
}

#[test]
fn classified_data_cannot_be_sent_without_declassifying() {
    let err = run_err(
        r#"
use http

def main():
    classified secret = "token"
    match http.post("http://127.0.0.1:9/x", secret):
        case Ok(r):
            print("ok")
        case Err(why):
            print("err")
"#,
    );
    assert!(err.contains("classified data"), "got: {err}");
    assert!(err.contains("declassify"), "the hint names the fix: {err}");
}

#[test]
fn a_durable_run_replays_a_response_instead_of_asking_twice() {
    // A network call is nondeterministic, so a resume must see what the
    // live run saw -- and must not spend a second request to get it.
    let scratch = Scratch::new("replay");
    let path = scratch.0.join("r1.jsonl");
    let (url, _, requests) = spawn_server(200, "first");
    let src = format!(
        r#"
use http

def main():
    match http.get("{url}/once"):
        case Ok(r):
            text = r["body"]
            print(f"body: {{text}}")
        case Err(why):
            print("err")
    print("after")
"#
    );

    let program = parse(&src).unwrap();
    // Scoped so the first run's journal -- and the OS lock it holds on the
    // run -- is dropped before the resume opens the same one.
    let (first, run) = {
        let mut i = interpreter(CONFIG);
        i.journal = Arc::new(Mutex::new(
            Journal::open(Run::new("r1".into(), "test.ko".into()), path.clone()).unwrap(),
        ));
        i.run(&program).unwrap();
        let saved = {
            let mut j = i.journal.lock().unwrap();
            j.finish(RunStatus::Completed).unwrap();
            j.run().clone()
        };
        (i.output.clone(), saved)
    };
    assert_eq!(first, vec!["body: first", "after"]);
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    let mut i = interpreter(CONFIG);
    i.journal = Arc::new(Mutex::new(Journal::open(run, path).unwrap()));
    i.run(&program).unwrap();
    assert!(
        i.output.is_empty(),
        "a resume continues rather than retelling: {:?}",
        i.output
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "a replayed request must not reach the network again"
    );
}

#[test]
fn calling_without_a_url_is_refused() {
    let err = run_err(
        r#"
use http

def main():
    match http.get():
        case Ok(r):
            print("ok")
        case Err(why):
            print("err")
"#,
    );
    assert!(err.contains("needs a url"), "got: {err}");
}
