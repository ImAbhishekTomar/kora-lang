//! The tool loop's failure paths.
//!
//! `tool_call_hook_test.rs` proves the loop works. These prove what it does
//! when it cannot: a model that asks forever, a budget that runs out
//! mid-loop, an argument the model left out, a tool trying to launder a
//! classified value back to the model, and a handler that returns the wrong
//! thing. Every one of these is a real message a user will read, and none of
//! them was covered.
//!
//! They run against a real fake provider rather than a mock or a cassette,
//! because both of those stand in for the *whole* call and never enter the
//! tool loop at all.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

/// A stand-in Ollama server that asks for `tool_name` on every turn and never
/// stops -- the shape that drives the loop into its own limits.
fn spawn_insatiable_provider(tool_name: &str, arguments: serde_json::Value) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let tool_name = tool_name.to_string();

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

            let payload = serde_json::json!({
                "message": {
                    "content": "",
                    "tool_calls": [{"function": {"name": tool_name, "arguments": arguments}}]
                },
                "prompt_eval_count": 5,
                "eval_count": 3
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

/// The error message and its hint, joined.
fn run_err(config_text: &str, src: &str) -> String {
    let program = kora_syntax::parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = kora_runtime::Interpreter::new();
    let config = kora_runtime::Config::parse(config_text).unwrap();
    i.sinks = config.sinks.clone();
    i.config = config;
    i.program_name = "test.ko".into();
    match i.run(&program) {
        Err(e) => match e.hint {
            Some(hint) => format!("{}\n{hint}", e.message),
            None => e.message,
        },
        Ok(_) => panic!("expected a runtime error, program succeeded:\n{src}"),
    }
}

const TOOL: &str = r#"type Answer:
    body: str

tool ping(id: str) -> str:
    return "pong"
"#;

// --- the loop's own limits ---

#[test]
fn a_model_that_never_stops_asking_for_tools_ends_with_a_turn_limit() {
    // Without this the loop is unbounded, and a model stuck in a tool cycle
    // spends real money until someone notices.
    let endpoint = spawn_insatiable_provider("ping", serde_json::json!({"id": "a"}));
    let err = run_err(
        &config(&endpoint),
        &format!(
            r#"{TOOL}
def main():
    a: Answer = analyze("data", "ping it", tools=[ping])
    print(a)
"#
        ),
    );
    assert!(
        err.contains("kept asking for tools after") && err.contains("turns"),
        "got: {err}"
    );
    assert!(
        err.contains("max_steps"),
        "the hint must name the knob that bounds it: {err}"
    );
}

#[test]
fn a_budget_that_runs_out_mid_loop_says_which_meter_stopped_it() {
    // The loop charges every turn against the enclosing budget. A run that
    // hits the ceiling inside the loop must say so by name, not report the
    // turn limit above, which would name the wrong cause.
    let endpoint = spawn_insatiable_provider("ping", serde_json::json!({"id": "a"}));
    let err = run_err(
        &config(&endpoint),
        &format!(
            r#"{TOOL}
def main():
    with budget(max_calls = 2):
        a: Answer = analyze("data", "ping it", tools=[ping])
        print(a)
"#
        ),
    );
    assert!(
        err.contains("budget exhausted") && err.contains("during tool loop"),
        "got: {err}"
    );
    assert!(err.contains("calls"), "the meter must be named: {err}");
}

// --- what the model sends ---

#[test]
fn a_tool_call_missing_an_argument_names_the_tool_and_the_argument() {
    // The model is outside the trust boundary: it can send a call with a
    // field left out. Reading that as `None` and running the tool anyway is
    // how a wrong answer gets computed from a missing input.
    let endpoint = spawn_insatiable_provider("ping", serde_json::json!({"wrong": "a"}));
    let err = run_err(
        &config(&endpoint),
        &format!(
            r#"{TOOL}
def main():
    a: Answer = analyze("data", "ping it", tools=[ping])
    print(a)
"#
        ),
    );
    assert!(
        err.contains("model called `ping` without argument `id`"),
        "got: {err}"
    );
}

// --- what a tool sends back ---

#[test]
fn a_classified_tool_result_cannot_be_laundered_back_to_the_model() {
    // The leak this closes: `analyze`'s own data is checked against the model
    // sink, so a program that cannot pass a secret directly could otherwise
    // return it from a tool inside the same closed loop instead.
    let endpoint = spawn_insatiable_provider("secret", serde_json::json!({"id": "a"}));
    let err = run_err(
        &config(&endpoint),
        r#"type Answer:
    body: str

type Record:
    classified ssn: str

tool secret(id: str) -> str:
    r = Record("123-45-6789")
    return r.ssn

def main():
    a: Answer = analyze("data", "look it up", tools=[secret])
    print(a)
"#,
    );
    assert!(
        err.contains("classified result of tool `secret`")
            && err.contains("cannot reach model sink"),
        "got: {err}"
    );
    assert!(
        err.contains("declassify"),
        "the hint must name the way through: {err}"
    );
}

#[test]
fn a_declassified_tool_result_is_allowed_through_when_policy_permits_it() {
    // The other half of the rule: the check is a gate, not a wall, and a
    // test that only proves the refusal would pass against a wall.
    let endpoint = spawn_insatiable_provider("secret", serde_json::json!({"id": "a"}));
    let mut config_text = config(&endpoint);
    config_text.push_str("\n[sinks]\nlocal_model = { allow = [\"classified\"] }\n");
    let err = run_err(
        &config_text,
        r#"type Answer:
    body: str

type Record:
    classified ssn: str

tool secret(id: str) -> str:
    r = Record("123-45-6789")
    s = r.ssn
    declassify s for local_model:
        return s

def main():
    a: Answer = analyze("data", "look it up", tools=[secret])
    print(a)
"#,
    );
    // The provider never stops asking, so the run still ends at the turn
    // limit -- but it got there, which means the classified value passed the
    // sink check rather than stopping the run.
    assert!(
        err.contains("kept asking for tools"),
        "the declassified value should have been allowed through, got: {err}"
    );
}

// --- what the `on tool_call` handler sends back ---

#[test]
fn a_handler_that_returns_something_other_than_a_string_is_refused() {
    // The handler's return value is handed to the model as a tool result,
    // which is text. Returning an int and having it silently stringified
    // would make a typo look like an answer.
    let endpoint = spawn_insatiable_provider("ping", serde_json::json!({"id": "a"}));
    let err = run_err(
        &config(&endpoint),
        &format!(
            r#"{TOOL}
def main():
    a: Answer = analyze("data", "ping it", tools=[ping]) on tool_call(name, args):
        return 3
    print(a)
"#
        ),
    );
    assert!(
        err.contains("`on tool_call` must `return` a string, got int"),
        "got: {err}"
    );
    assert!(
        err.contains("fall off the end of the block"),
        "the hint must say how to let the tool run: {err}"
    );
}

#[test]
fn break_inside_a_handler_says_there_is_no_loop_to_leave() {
    // The handler runs once per call. `break` there is a misunderstanding of
    // what the block is, and the hint says so rather than quietly ending the
    // tool loop.
    let endpoint = spawn_insatiable_provider("ping", serde_json::json!({"id": "a"}));
    let err = run_err(
        &config(&endpoint),
        &format!(
            r#"{TOOL}
def main():
    a: Answer = analyze("data", "ping it", tools=[ping]) on tool_call(name, args):
        break
    print(a)
"#
        ),
    );
    assert!(
        err.contains("`break` and `continue` have nothing to leave"),
        "got: {err}"
    );
    assert!(
        err.contains("once per tool call"),
        "the hint must explain why: {err}"
    );
}

// --- a tool the loop cannot reach ---

#[test]
fn a_streamed_call_cannot_also_use_tools() {
    // Refused rather than silently degraded: a streamed answer and a tool
    // loop want the same response body.
    let endpoint = spawn_insatiable_provider("ping", serde_json::json!({"id": "a"}));
    let err = run_err(
        &config(&endpoint),
        &format!(
            r#"{TOOL}
def main():
    answer: str = analyze("data", "ping it", tools=[ping]) stream
    print(answer)
"#
        ),
    );
    assert!(
        err.contains("streaming cannot be combined with tools"),
        "got: {err}"
    );
}

// --- the loop still works ---

#[test]
fn a_tool_the_model_asks_for_actually_runs() {
    // The control: every test above asserts on a failure, and a loop that
    // was broken outright would pass all of them.
    let endpoint = spawn_insatiable_provider("ping", serde_json::json!({"id": "a"}));
    let err = run_err(
        &config(&endpoint),
        &format!(
            r#"{TOOL}
def main():
    a: Answer = analyze("data", "ping it", tools=[ping]) on tool_call(name, args):
        print(f"ran {{name}}")
    print(a)
"#
        ),
    );
    assert!(err.contains("kept asking for tools"), "got: {err}");
}

#[test]
fn the_handler_runs_once_per_turn() {
    let endpoint = spawn_insatiable_provider("ping", serde_json::json!({"id": "a"}));
    let program = format!(
        r#"{TOOL}
def main():
    with budget(max_calls = 3):
        a: Answer = analyze("data", "ping it", tools=[ping]) on tool_call(name, args):
            print("turn")
        print(a)
"#
    );
    let parsed = kora_syntax::parse(&program).expect("parses");
    let mut i = kora_runtime::Interpreter::new();
    i.config = kora_runtime::Config::parse(&config(&endpoint)).unwrap();
    i.program_name = "test.ko".into();
    let _ = i.run(&parsed);
    // Three calls allowed, so the handler saw three tool requests before the
    // budget stopped the loop.
    assert_eq!(
        i.output.iter().filter(|l| *l == "turn").count(),
        3,
        "got: {:?}",
        i.output
    );
}

// --- a fan-out that loses finished work ---

#[test]
fn a_failing_branch_says_how_much_finished_work_it_took_with_it() {
    // A branch that raises fails the whole loop. The count is the difference
    // between "this is broken" and "this is broken on one input out of two
    // hundred", and nothing tested that it was reported.
    let program = r#"def check(n: int) -> int:
    if n == 9:
        return n / 0
    return n

def main():
    results = parallel for n in [1, 2, 3, 9]:
        check(n)
    print(results)
"#;
    let parsed = kora_syntax::parse(program).expect("parses");
    let mut i = kora_runtime::Interpreter::new();
    let err = i
        .run(&parsed)
        .expect_err("the failing branch fails the loop");
    assert!(
        err.message.contains("division by zero"),
        "got: {}",
        err.message
    );
    let hint = err.hint.unwrap_or_default();
    assert!(
        hint.contains("of 4 branches had already finished"),
        "the hint must say how much work was lost: {hint}"
    );
    assert!(hint.contains("lost with this error"), "got: {hint}");
}
