//! `max_seconds` — a budget denominated in time.
//!
//! The other three meters are counted from the program's own spending, so a
//! replayed run re-derives them exactly. This one is read from a clock, which
//! makes it the meter that forced the journal to remember a refusal rather
//! than recompute it. These tests are mostly about that difference.

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
            "kora-deadline-{name}-{}-{:?}",
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

/// A provider that answers, and counts how often it was asked.
fn spawn_provider() -> (String, Arc<AtomicUsize>) {
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

            let payload = serde_json::json!({
                "message": {"content": r#"{"__uncertain__":"","answer":"hello"}"#},
                "done": true,
                "prompt_eval_count": 3,
                "eval_count": 2,
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len(),
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    (format!("http://127.0.0.1:{port}"), requests)
}

/// A provider that accepts the connection and then never answers.
///
/// This is the case a deadline could not previously touch: the request is
/// already sent, so no meter checked before dispatch will ever see it again,
/// and it runs to `[models] timeout_secs` -- ten minutes by default. The
/// listener is held so the port stays open for the whole test.
fn spawn_wedged_provider() -> (String, TcpListener) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let accepting = listener.try_clone().expect("a second handle");
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in accepting.incoming() {
            let Ok(stream) = stream else { return };
            // Held open, never written to.
            held.push(stream);
        }
    });
    (format!("http://127.0.0.1:{port}"), listener)
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
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = Interpreter::new();
    i.config = Config::parse(config_text).unwrap();
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

/// Zero seconds is the deterministic spelling of "already out of time" — a
/// scope that must not start new work — so these tests need no sleeping.
const OUT_OF_TIME: &str = r#"
def main():
    with budget(max_seconds = 0):
        answer: str = analyze("q", "greet")
        match answer:
            case Ok(text):
                print(f"ok: {text}")
            case Exhausted(meter):
                print(f"out of {meter}")
            case Failed(why):
                print("failed")
    print("after")
"#;

#[test]
fn a_scope_out_of_time_refuses_the_call_as_a_value() {
    // Exhaustion is a value in this language, not an exception: the work
    // done before the deadline survives, and the program decides what a
    // refusal is worth.
    let (endpoint, requests) = spawn_provider();
    let out = run(&config(&endpoint), OUT_OF_TIME);
    assert_eq!(out, vec!["out of seconds", "after"]);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "a refused call is refused before it is sent"
    );
}

#[test]
fn time_left_means_the_call_goes_through() {
    let (endpoint, requests) = spawn_provider();
    let out = run(
        &config(&endpoint),
        r#"
def main():
    with budget(max_seconds = 3600):
        answer: str = analyze("q", "greet")
        match answer:
            case Ok(text):
                print(f"ok: {text}")
            case Exhausted(meter):
                print(f"out of {meter}")
            case Failed(why):
                print("failed")
"#,
    );
    assert_eq!(out, vec!["ok: hello"]);
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[test]
fn the_meter_that_ran_out_is_named() {
    // A scope short of both time and tokens has run out of time. Naming
    // tokens would send someone to raise a limit that was not what stopped
    // them.
    let (endpoint, _) = spawn_provider();
    let out = run(
        &config(&endpoint),
        r#"
def main():
    with budget(max_seconds = 0, max_tokens = 1):
        answer: str = analyze("q", "greet")
        match answer:
            case Exhausted(meter):
                print(meter)
            case Ok(text):
                print("ok")
            case Failed(why):
                print("failed")
"#,
    );
    assert_eq!(out, vec!["seconds"]);
}

#[test]
fn a_deadline_reaches_the_workers_of_a_parallel_for() {
    // The budget is one shared pot across a fan-out, and time is part of it:
    // five hundred branches racing an expired deadline all stop, with no
    // coordination code in the program.
    let (endpoint, requests) = spawn_provider();
    let out = run(
        &config(&endpoint),
        r#"
def look(item: str) -> str:
    answer: str = analyze(item, "greet")
    match answer:
        case Ok(text):
            return "ok"
        case Exhausted(meter):
            return meter
        case Failed(why):
            return "failed"

def main():
    with budget(max_seconds = 0):
        results = parallel for item in ["a", "b", "c"]:
            return look(item)

        for r in results:
            print(r)
"#,
    );
    assert_eq!(out, vec!["seconds", "seconds", "seconds"]);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "no worker should reach the provider"
    );
}

#[test]
fn a_resumed_run_replays_the_refusal_instead_of_re_reading_the_clock() {
    // The reason this meter is journaled at all. Every other budget is
    // counted from the program's own spending, so a replay re-derives it;
    // this one is read from a clock, and a replay runs faster than the
    // original. Re-deciding would let the same run answer differently the
    // second time -- and, worse, reach a provider the first run refused.
    let scratch = Scratch::new("replay");
    let path = scratch.run_path("r1");
    let (endpoint, requests) = spawn_provider();

    // The first run is out of time and refuses.
    let src = r#"
def main():
    with budget(max_seconds = 0):
        answer: str = analyze("q", "greet")
        match answer:
            case Exhausted(meter):
                print(f"out of {meter}")
            case Ok(text):
                print(f"ok: {text}")
            case Failed(why):
                print("failed")
    print("after")
"#;
    let (first, run, err) = run_durable(
        &config(&endpoint),
        src,
        Run::new("r1".into(), "test.ko".into()),
        path.clone(),
    );
    assert!(err.is_none(), "{err:?}");
    assert_eq!(first, vec!["out of seconds", "after"]);
    assert_eq!(requests.load(Ordering::SeqCst), 0);

    // The resume must still refuse, even though the clock it would read now
    // says there is plenty of time.
    let (second, _, err) = run_durable(&config(&endpoint), src, run, path);
    assert_eq!(err, None, "a resume must not diverge");
    assert!(
        second.is_empty(),
        "a resume continues rather than retelling: {second:?}"
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "the replayed refusal must not become a live call"
    );
}

#[test]
fn a_refusal_is_journaled_where_the_call_would_have_been() {
    // It occupies the slot rather than skipping it, so the effects after it
    // keep the positions they had. A refusal that left a hole would put the
    // next effect where the journal expects this one.
    let scratch = Scratch::new("slot");
    let path = scratch.run_path("r1");
    let (endpoint, _) = spawn_provider();
    let (_, run, err) = run_durable(
        &config(&endpoint),
        OUT_OF_TIME,
        Run::new("r1".into(), "test.ko".into()),
        path,
    );
    assert!(err.is_none(), "{err:?}");
    let model_entries = run
        .entries
        .iter()
        .filter(|e| e.site.ends_with("#model"))
        .count();
    assert_eq!(
        model_entries, 1,
        "the refused call should hold its own slot: {:?}",
        run.entries
    );
}

#[test]
fn a_budget_with_no_limits_at_all_is_still_refused_by_the_parser() {
    // Guarding the neighbouring case: adding a field must not make an empty
    // budget suddenly legal.
    let err = parse("def main():\n    with budget():\n        pass\n")
        .expect_err("an empty budget says nothing");
    assert!(
        err.message.contains("at least one limit"),
        "got: {}",
        err.message
    );
}

/// The deadline reaches a request that has already been sent.
///
/// Before this, `max_seconds` bounded the decision to make a call and nothing
/// else: a provider that accepted the connection and went quiet held the run
/// for the full `[models] timeout_secs`. The transport now carries the
/// smaller of its own timeout and what the budget has left, so the scope ends
/// roughly when it said it would.
#[test]
fn a_deadline_cuts_short_a_request_already_in_flight() {
    let (endpoint, _listener) = spawn_wedged_provider();
    // Ten minutes if the deadline is not honoured; two seconds if it is.
    let config_text = format!(
        r#"
[models]
default = "local:test-model"
max_retries = 0
timeout_secs = 600

[models.local]
endpoint = "{endpoint}"
"#
    );

    let src = r#"
def main():
    with budget(max_seconds = 2):
        answer: str = analyze("q", "greet")
        match answer:
            case Ok(text):
                print("ok")
            case Exhausted(meter):
                print(f"out of {meter}")
            case Uncertain(reason):
                print("uncertain")
            case Failed(why):
                print("failed")
"#;

    let started = std::time::Instant::now();
    let out = run(&config_text, src);
    let elapsed = started.elapsed();

    assert_eq!(out, vec!["failed"], "the call should end as a failure");
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "the call ran for {elapsed:?}; the deadline did not reach it"
    );
}

/// A `parallel for` where one branch finds the answer stops the rest.
///
/// The bound is deliberately loose: how many branches are in flight when the
/// first one breaks depends on the machine. What is being asserted is that
/// the loop stops being proportional to the input, which is the whole point.
#[test]
fn a_break_stops_a_fan_out_from_starting_more_work() {
    let src = r#"
def main():
    found = parallel for n in range(400):
        if n > 0 and n % 89 == 0:
            break n
        return 0
    print(f"{len(found)}")
    print(f"{sum(found)}")
"#;
    let program = parse(src).expect("parses");
    let mut i = Interpreter::new();
    i.program_name = "test.ko".into();
    i.run(&program).expect("the run succeeds");

    let ran: usize = i.output[0].trim().parse().expect("a count");
    assert!(
        ran < 300,
        "{ran} of 400 branches ran; the break did nothing"
    );
    // The branch that broke contributed its find; every other branch that
    // finished contributed zero.
    let total: i64 = i.output[1].trim().parse().expect("a total");
    assert!(total >= 89, "the breaking branch's value was lost");
}

/// A branch let go of part way through is not a failure, and it does not
/// leave a hole in the results either: it produced nothing, exactly like a
/// branch that was never started.
#[test]
fn a_stopped_branch_is_neither_an_error_nor_an_empty_slot() {
    let src = r#"
def slow(n: int) -> int:
    total = 0
    for i in range(200000):
        total = total + i
    return total

def main():
    out = parallel for n in range(8):
        if n == 0:
            break 7
        x = slow(n)
        return x
    print(f"{len(out)}")
    print(f"{sum(out)}")
"#;
    let program = parse(src).expect("parses");
    let mut i = Interpreter::new();
    i.program_name = "test.ko".into();
    // A branch cut short mid-`slow()` must not surface as a runtime error.
    i.run(&program).expect("a stopped branch is not a failure");

    let slots: usize = i.output[0].trim().parse().expect("a count");
    assert!(slots >= 1, "the breaking branch kept no slot");
    // Every slot holds an int: a stopped branch contributed nothing rather
    // than a `none` for `sum` to trip over.
    let _: i64 = i.output[1].trim().parse().expect("summable results");
}
