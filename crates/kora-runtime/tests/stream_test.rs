//! End-to-end tests for token-by-token `analyze()` streaming.
//!
//! These run the interpreter against `with mock analyze`, which covers
//! parsing, `on token` scoping, and the outcome match. A mock stands in for
//! the whole call and returns before the budget is consulted or a socket is
//! opened, so it proves nothing about accounting or the transport — those
//! live in `stream_transport_test.rs`, against a real loopback provider.

use kora_runtime::Interpreter;
use kora_syntax::parse;

fn run(src: &str) -> Vec<String> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\nsource:\n{src}"));
    let mut interp = Interpreter::new();
    interp
        .run(&program)
        .unwrap_or_else(|e| panic!("runtime error: {}\nsource:\n{src}", e.message));
    interp.output
}

fn run_err(src: &str) -> String {
    let program = parse(src).expect("should parse");
    let mut interp = Interpreter::new();
    match interp.run(&program) {
        Err(e) => e.message,
        Ok(_) => panic!("expected runtime error, program succeeded"),
    }
}

#[test]
fn on_token_runs_over_a_mocked_answer_and_the_outcome_still_matches() {
    let out = run(r#"
agent main():
    with mock analyze -> Ok("hello there"):
        answer: str = analyze("q", "answer this") on token(t):
            print(f"piece: {t}")
        match answer:
            case Ok(text):
                print(f"final: {text}")
            case Uncertain(reason):
                print(f"uncertain: {reason}")
"#);
    // A mock has no pieces to script, so the handler runs once over the
    // whole answer rather than not at all -- otherwise `with mock` would
    // make the handler body dead code under every test that uses it.
    assert_eq!(out, vec!["piece: hello there", "final: hello there"]);
}

#[test]
fn on_token_sees_an_uncertain_refusal_only_through_the_match_not_the_handler() {
    let out = run(r#"
agent main():
    with mock analyze -> Uncertain("too vague"):
        answer: str = analyze("q", "answer this") on token(t):
            print(f"piece: {t}")
        match answer:
            case Ok(text):
                print(f"final: {text}")
            case Uncertain(reason):
                print(f"uncertain: {reason}")
"#);
    // A refusal has no answer text, so the handler never fires -- only the
    // outcome match sees it, exactly as a non-streaming call would.
    assert_eq!(out, vec!["uncertain: too vague"]);
}

#[test]
fn on_token_needs_a_str_result() {
    let err = run_err(
        r#"
type Ticket:
    severity: str

agent main():
    t: Ticket = analyze("q", "classify") on token(x):
        print(x)
"#,
    );
    assert!(
        err.contains("needs a `str` result"),
        "should name why a declared type cannot stream: {err}"
    );
}

#[test]
fn on_token_only_watches_analyze() {
    let err = run_err(
        r#"
agent main():
    x: str = "not a call" on token(t):
        print(t)
"#,
    );
    assert!(
        err.contains("can only watch an `analyze()` call"),
        "should name the misuse: {err}"
    );
}

#[test]
fn break_inside_on_token_is_refused() {
    let err = run_err(
        r#"
agent main():
    with mock analyze -> Ok("hi"):
        answer: str = analyze("q", "answer this") on token(t):
            break
"#,
    );
    assert!(
        err.contains("nothing to leave"),
        "a stray `break` should be refused, not silently accepted: {err}"
    );
}

#[test]
fn the_handler_variable_does_not_leak_the_wrong_type() {
    // Regression for the type checker: `on token`'s variable is declared
    // like a loop variable, so a program that only ever runs the handler
    // must not report it as unknown.
    let out = run(r#"
agent main():
    with mock analyze -> Ok("x"):
        answer: str = analyze("q", "answer this") on token(piece):
            upper = piece
            print(upper)
"#);
    assert_eq!(out, vec!["x"]);
}
