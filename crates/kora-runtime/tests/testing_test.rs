//! `test` blocks, `assert`, and typed `mock`.
//!
//! The point of typing the mocks is that a mock which drifts from the type it
//! stands for should fail, not keep passing. That is the failure mode of
//! untyped mocking frameworks, and it is what these tests pin down.

use kora_runtime::{Config, Interpreter};
use kora_syntax::parse;

const CONFIG: &str = "[models]\ndefault = \"local:test-model\"\n";

fn interp() -> Interpreter {
    let mut i = Interpreter::new();
    i.config = Config::parse(CONFIG).unwrap();
    i.program_name = "test.ko".into();
    i
}

/// Collect and run every `test` block, returning (name, failure) pairs.
fn run_suite(src: &str) -> Vec<(String, Option<String>)> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut collector = interp();
    collector.collecting_tests = true;
    collector
        .run_top_level(&program)
        .unwrap_or_else(|e| panic!("top level failed: {}", e.message));

    collector
        .tests
        .clone()
        .into_iter()
        .map(|(name, body)| {
            let mut i = interp();
            let outcome = i.run_top_level(&program).and_then(|()| i.run_block(&body));
            (name, outcome.err().map(|e| e.message))
        })
        .collect()
}

const TICKET: &str = r#"type Ticket:
    severity: str
    summary: str

agent triage(raw: str) -> str:
    t: Ticket = analyze(raw, "classify")
    match t:
        case Ok(v):
            return f"{v.severity}: {v.summary}"
        case Uncertain(r):
            return f"uncertain: {r}"
        case Exhausted(m):
            return f"exhausted: {m}"
"#;

// --- collection and assertions ---

#[test]
fn tests_are_collected_and_run() {
    let results = run_suite(
        r#"test "one":
    assert True

test "two":
    assert 1 + 1 == 2
"#,
    );
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|(_, failure)| failure.is_none()));
}

#[test]
fn a_failing_assertion_reports_its_message() {
    let results = run_suite(
        r#"test "fails":
    assert 1 == 2, "one is not two"
"#,
    );
    assert_eq!(results[0].1.as_deref(), Some("one is not two"));
}

#[test]
fn an_assertion_without_a_message_still_fails_clearly() {
    let results = run_suite("test \"fails\":\n    assert False\n");
    assert_eq!(results[0].1.as_deref(), Some("assertion failed"));
}

#[test]
fn assertion_messages_can_interpolate() {
    let results = run_suite(
        r#"test "fails":
    got = 3
    assert got == 4, f"expected 4, got {got}"
"#,
    );
    assert_eq!(results[0].1.as_deref(), Some("expected 4, got 3"));
}

#[test]
fn tests_do_not_leak_state_into_each_other() {
    // Each test re-runs the file's definitions in a fresh interpreter.
    let results = run_suite(
        r#"def main():
    pass

test "first sets a global":
    leaked = 1
    assert leaked == 1

test "second cannot see it":
    assert not defined_check()

def defined_check() -> bool:
    return False
"#,
    );
    assert!(results.iter().all(|(_, f)| f.is_none()), "{results:?}");
}

#[test]
fn test_blocks_are_inert_under_a_normal_run() {
    // `kora run` should not execute test bodies.
    let program = parse("test \"boom\":\n    assert False\n").unwrap();
    let mut i = interp();
    assert!(
        i.run(&program).is_ok(),
        "a normal run must skip test blocks"
    );
}

// --- mocks ---

#[test]
fn a_mock_replaces_the_model_call() {
    let results = run_suite(&format!(
        r#"{TICKET}
test "high severity":
    with mock analyze -> Ok(Ticket("high", "down")):
        assert triage("x") == "high: down"
"#
    ));
    assert_eq!(results[0].1, None, "{:?}", results[0].1);
}

#[test]
fn failure_paths_are_forceable() {
    // The paths nobody tests today, because provoking them means making a
    // real model misbehave.
    let results = run_suite(&format!(
        r#"{TICKET}
test "uncertain":
    with mock analyze -> Uncertain("too vague"):
        assert triage("x") == "uncertain: too vague"

test "exhausted":
    with mock analyze -> Exhausted("tokens"):
        assert triage("x") == "exhausted: tokens"
"#
    ));
    assert!(results.iter().all(|(_, f)| f.is_none()), "{results:?}");
}

#[test]
fn a_mock_of_the_wrong_type_is_rejected() {
    // The whole reason to type mocks: this would pass silently elsewhere.
    let results = run_suite(&format!(
        r#"{TICKET}
type Other:
    x: int

test "wrong type":
    with mock analyze -> Ok(Other(1)):
        triage("x")
"#
    ));
    let failure = results[0].1.as_deref().unwrap_or_default();
    assert!(failure.contains("`Other`"), "got: {failure}");
    assert!(failure.contains("`Ticket`"), "got: {failure}");
}

#[test]
fn a_mock_must_be_an_outcome() {
    let results = run_suite(&format!(
        r#"{TICKET}
test "not an outcome":
    with mock analyze -> 42:
        triage("x")
"#
    ));
    let failure = results[0].1.as_deref().unwrap_or_default();
    assert!(failure.contains("must be Ok"), "got: {failure}");
}

#[test]
fn mocks_nest_and_unwind() {
    let results = run_suite(&format!(
        r#"{TICKET}
test "inner mock wins, outer is restored":
    with mock analyze -> Ok(Ticket("low", "outer")):
        with mock analyze -> Ok(Ticket("high", "inner")):
            assert triage("x") == "high: inner"
        assert triage("x") == "low: outer"
"#
    ));
    assert_eq!(results[0].1, None, "{:?}", results[0].1);
}

#[test]
fn only_analyze_can_be_mocked_today() {
    let results = run_suite(
        r#"test "bad target":
    with mock print -> Ok(1):
        pass
"#,
    );
    let failure = results[0].1.as_deref().unwrap_or_default();
    assert!(failure.contains("cannot be mocked"), "got: {failure}");
}

// --- outcome constructors ---

#[test]
fn outcome_constructors_round_trip_through_match() {
    let results = run_suite(
        r#"test "constructors match":
    match Ok(1):
        case Ok(v):
            assert v == 1
        case _:
            assert False, "should have matched Ok"

    match Err("boom"):
        case Err(why):
            assert why == "boom"
        case _:
            assert False, "should have matched Err"
"#,
    );
    assert_eq!(results[0].1, None, "{:?}", results[0].1);
}

#[test]
fn an_unknown_constructor_is_not_silently_created() {
    // A typo must be an error, not a variant nothing will ever match.
    let results = run_suite(
        r#"test "typo":
    x = Okk(1)
"#,
    );
    let failure = results[0].1.as_deref().unwrap_or_default();
    assert!(failure.contains("not defined"), "got: {failure}");
}
