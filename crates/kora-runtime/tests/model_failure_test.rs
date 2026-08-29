//! A provider that does not answer is an outcome, not a crash.
//!
//! These run against a port with nothing behind it, which is the cheapest
//! honest stand-in for the failures that actually happen: a refused
//! connection, a provider restarting, a rate limit that outlasts the backoff.
//! The behaviour they pin down is the one the README promises -- failure is a
//! value, and partial work survives.

use kora_runtime::{Config, Interpreter};
use kora_syntax::parse;

/// Nothing listens here, and `max_retries = 0` keeps the suite fast: the
/// retry policy has its own tests in `kora-models`, where no socket is
/// involved at all.
const CONFIG: &str = r#"
[models]
default = "local:test-model"
max_retries = 0

[models.local]
endpoint = "http://127.0.0.1:1"
"#;

fn run(src: &str) -> Vec<String> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = Interpreter::new();
    i.config = Config::parse(CONFIG).unwrap();
    i.program_name = "test.ko".into();
    i.run(&program)
        .unwrap_or_else(|e| panic!("the run should not fail: {}\n{src}", e.message));
    i.output
}

const PIECE: &str = r#"type Piece:
    body: str
"#;

#[test]
fn an_unreachable_provider_is_a_failed_outcome() {
    let out = run(&format!(
        r#"{PIECE}
def main():
    p: Piece = analyze("cats", "write about this")
    match p:
        case Ok(v):
            print(v.body)
        case Uncertain(why):
            print(f"uncertain: {{why}}")
        case Exhausted(meter):
            print(f"exhausted: {{meter}}")
        case Failed(why):
            print("failed")
"#
    ));
    assert_eq!(out, vec!["failed"]);
}

#[test]
fn the_else_binding_catches_a_provider_failure() {
    // The flat form is the one most programs will use, so it is the one that
    // most needs to cover this: a program written with `else` should not need
    // rewriting the first time a provider has a bad afternoon.
    let out = run(&format!(
        r#"{PIECE}
def main():
    p: Piece = analyze("cats", "write about this") else (why):
        print("degraded")
        return
    print(p.body)
"#
    ));
    assert_eq!(out, vec!["degraded"]);
}

#[test]
fn a_failure_in_one_parallel_branch_does_not_discard_the_others() {
    // The regression that motivated all of this: one branch losing the
    // provider used to take every finished branch down with it.
    let out = run(&format!(
        r#"{PIECE}
agent one(topic: str) -> str:
    p: Piece = analyze(topic, "write about this") else:
        return f"degraded {{topic}}"
    return p.body

def main():
    results = parallel for t in ["a", "b", "c"]:
        return one(t)
    for r in results:
        print(r)
    print("finished")
"#
    ));
    assert_eq!(
        out,
        vec!["degraded a", "degraded b", "degraded c", "finished"]
    );
}

#[test]
fn a_failure_names_the_provider_and_the_reason() {
    // Half the value of an outcome is what it says when it is printed into a
    // log at three in the morning.
    let out = run(&format!(
        r#"{PIECE}
def main():
    p: Piece = analyze("cats", "write about this") else (why):
        print(why)
        return
    print(p.body)
"#
    ));
    let reason = out.first().map(String::as_str).unwrap_or_default();
    assert!(
        reason.contains("127.0.0.1:1"),
        "the reason should name where the call went: {reason}"
    );
}
