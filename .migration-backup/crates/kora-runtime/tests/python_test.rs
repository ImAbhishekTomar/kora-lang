//! The Python sidecar, at the language level.
//!
//! These call a real interpreter, because the point of the feature is that it
//! reaches real Python. They use only the standard library, so nothing needs
//! installing. Tests that do not need Python skip it entirely.

use kora_runtime::{Config, Interpreter};
use kora_syntax::parse;

const CONFIG: &str = r#"
[models]
default = "local:test-model"

[sinks]
local_model = { allow = ["classified"] }
python = { allow = ["classified"] }
"#;

fn interp() -> Interpreter {
    let mut i = Interpreter::new();
    let config = Config::parse(CONFIG).unwrap();
    i.sinks = config.sinks.clone();
    i.config = config;
    i.program_name = "test.ko".into();
    i
}

fn run(src: &str) -> Vec<String> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = interp();
    i.run(&program)
        .unwrap_or_else(|e| panic!("runtime error: {}\n{src}", e.message));
    i.output
}

fn run_err(src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = interp();
    match i.run(&program) {
        Err(e) => format!("{} | {}", e.message, e.hint.unwrap_or_default()),
        Ok(_) => panic!("expected an error, program succeeded:\n{src}"),
    }
}

/// Whether a Python interpreter is available, so the tests that need one can
/// be skipped rather than failing on a machine without it.
fn python_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// --- syntax and configuration, no interpreter needed ---

#[test]
fn use_python_parses_with_and_without_an_alias() {
    parse("use python statistics as stats\n").expect("aliased");
    parse("use python math\n").expect("bare");
}

#[test]
fn the_interpreter_can_be_configured() {
    // A virtualenv's interpreter goes here.
    let config = Config::parse("[python]\ncommand = \"/venv/bin/python\"\n").unwrap();
    assert_eq!(config.python.command, "/venv/bin/python");
}

#[test]
fn the_default_interpreter_is_python3() {
    assert_eq!(Config::parse("").unwrap().python.command, "python3");
}

#[test]
fn a_missing_interpreter_says_what_to_do() {
    let program = parse("use python math\ndef main():\n    math.sqrt(4)\n").unwrap();
    let mut i = interp();
    i.config.python.command = "definitely-not-a-python-xyz".into();
    let e = i.run(&program).unwrap_err();
    assert!(e.message.contains("could not start"), "{}", e.message);
    assert!(
        e.message.contains("kora.toml"),
        "the message should point at the fix: {}",
        e.message
    );
}

#[test]
fn the_alias_is_known_to_the_checker() {
    let program = parse("use python math as m\ndef main():\n    x = m\n").unwrap();
    let analysis = kora_types::analyze(&program);
    assert!(
        !analysis
            .diagnostics
            .iter()
            .any(|d| d.message.contains("not defined")),
        "{:?}",
        analysis.diagnostics
    );
}

// --- the security boundary, no interpreter needed ---

#[test]
fn python_is_its_own_sink() {
    // Python runs in its own process, so releasing a secret to a model has
    // not released it to Python.
    let err = run_err(
        r#"use python base64 as b64
def main():
    classified secret = "hunter2"
    declassify secret as s for local_model:
        b64.b64encode(s)
"#,
    );
    assert!(err.contains("cannot reach Python"), "got: {err}");
    assert!(
        err.contains("its own process"),
        "the hint should say why: {err}"
    );
}

#[test]
fn classified_data_needs_a_release_to_reach_python() {
    let err = run_err(
        r#"use python math as m
def main():
    classified n = 16
    m.sqrt(n)
"#,
    );
    assert!(err.contains("cannot reach Python"), "got: {err}");
}

// --- calling real Python ---

#[test]
fn calls_reach_the_standard_library() {
    if !python_available() {
        return;
    }
    let out = run(r#"use python statistics as stats
def main():
    match stats.mean([1, 2, 3, 4]):
        case Ok(m):
            print(m)
        case Err(why):
            print(why)
"#);
    assert_eq!(out, vec!["2.5"]);
}

#[test]
fn a_python_exception_is_a_value() {
    // Python raising is something the program handles, not a crash.
    if !python_available() {
        return;
    }
    let out = run(r#"use python math
def main():
    match math.sqrt(-1):
        case Ok(r):
            print("unreachable")
        case Err(why):
            print(why)
"#);
    // The exception *type* is what crossed the boundary and what a program
    // would branch on. The message is CPython's wording and differs between
    // versions -- 3.13 says "math domain error", others say "expected a
    // nonnegative input" -- so asserting on it tests CPython, not Kora.
    assert!(out[0].contains("ValueError"), "got: {}", out[0]);
}

#[test]
fn a_missing_function_lists_what_exists() {
    if !python_available() {
        return;
    }
    let out = run(r#"use python statistics as stats
def main():
    match stats.nope([1]):
        case Ok(r):
            print("unreachable")
        case Err(why):
            print(why)
"#);
    assert!(out[0].contains("has no `nope`"), "got: {}", out[0]);
    assert!(
        out[0].contains("median") || out[0].contains("mean") || out[0].contains(","),
        "the error should list what the module provides: {}",
        out[0]
    );
}

#[test]
fn values_cross_in_both_directions() {
    if !python_available() {
        return;
    }
    let out = run(r#"use python json as pyjson
def main():
    match pyjson.dumps({"b": 2, "a": 1}):
        case Ok(text):
            print(text)
        case Err(why):
            print(why)
"#);
    assert!(out[0].contains("\"a\""), "got: {}", out[0]);
    assert!(out[0].contains("\"b\""), "got: {}", out[0]);
}

#[test]
fn results_are_unverified() {
    // Whatever Python returns came from outside the program, so it cannot
    // reach a dangerous sink until something narrows it.
    if !python_available() {
        return;
    }
    let err = run_err(
        r#"use python os.path as p
use fs
def main():
    match p.expanduser("~/notes.txt"):
        case Ok(path):
            fs.read(path)
        case Err(why):
            print(why)
"#,
    );
    assert!(err.contains("came from outside"), "got: {err}");
}

#[test]
fn a_released_value_reaches_python() {
    // The legitimate path: released for python, so it goes.
    if !python_available() {
        return;
    }
    let out = run(r#"use python math as m
def main():
    classified n = 16
    declassify n as plain for python:
        match m.sqrt(plain):
            case Ok(r):
                print(r)
            case Err(why):
                print(why)
"#);
    assert_eq!(out, vec!["4.0"]);
}

#[test]
fn one_worker_serves_many_calls() {
    // Starting an interpreter is the expensive part, so it happens once.
    if !python_available() {
        return;
    }
    let out = run(r#"use python math as m
def main():
    total = 0
    for i in [1, 4, 9, 16]:
        match m.sqrt(i):
            case Ok(r):
                total = total + r
            case Err(why):
                print(why)
    print(total)
"#);
    assert_eq!(out, vec!["10.0"]);
}
