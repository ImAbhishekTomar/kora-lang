//! Phase 4: `classified` / `declassify`.
//!
//! The central promise is transitivity: a classified value stays classified
//! through every operation that derives from it. These tests hammer the
//! laundering paths, because one working laundering trick defeats the whole
//! feature — the failure that killed Perl's taint mode.

use kora_runtime::{Config, Interpreter};
use kora_syntax::parse;

const POLICY: &str = r#"
[models]
default = "local:test-model"

[sinks]
local_model = { allow = ["classified"] }
openai = { allow = ["internal"], deny = ["classified"] }
"#;

fn interp() -> Interpreter {
    let mut i = Interpreter::new();
    let config = Config::parse(POLICY).unwrap();
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
        Err(e) => e.message,
        Ok(_) => panic!("expected an error, but the program succeeded:\n{src}"),
    }
}

const TYPES: &str = r#"type Employee:
    name: str
    classified ssn: str

type R:
    ok: bool
"#;

fn with_types(body: &str) -> String {
    format!("{TYPES}\n{body}")
}

// --- the core block ---

#[test]
fn classified_field_cannot_reach_a_model() {
    let err = run_err(&with_types(
        "def main():\n    e = Employee(\"Ada\", \"123\")\n    r: R = analyze(e.ssn, \"anything\")\n",
    ));
    assert!(err.contains("classified data cannot reach"), "got: {err}");
}

#[test]
fn classified_declaration_cannot_reach_a_model() {
    let err = run_err(&with_types(
        "def main():\n    classified secret = \"abc\"\n    r: R = analyze(secret, \"anything\")\n",
    ));
    assert!(err.contains("classified data cannot reach"), "got: {err}");
}

#[test]
fn public_data_flows_freely() {
    // The label must not get in the way of ordinary code.
    let out = run(&with_types(
        "def main():\n    e = Employee(\"Ada\", \"123\")\n    print(e.name)\n",
    ));
    assert_eq!(out, vec!["Ada"]);
}

// --- laundering attempts: each must fail ---

#[test]
fn laundering_through_fstring_fails() {
    let err = run_err(&with_types(
        r#"def main():
    classified s = "secret"
    disguised = f"value is {s}"
    r: R = analyze(disguised, "anything")
"#,
    ));
    assert!(err.contains("classified data cannot reach"), "got: {err}");
}

#[test]
fn laundering_through_concatenation_fails() {
    let err = run_err(&with_types(
        r#"def main():
    classified s = "secret"
    disguised = "prefix " + s
    r: R = analyze(disguised, "anything")
"#,
    ));
    assert!(err.contains("classified data cannot reach"), "got: {err}");
}

#[test]
fn laundering_through_slicing_fails() {
    let err = run_err(&with_types(
        r#"def main():
    classified s = "123456789"
    disguised = s[0:3]
    r: R = analyze(disguised, "anything")
"#,
    ));
    assert!(err.contains("classified data cannot reach"), "got: {err}");
}

#[test]
fn laundering_through_arithmetic_fails() {
    let err = run_err(&with_types(
        r#"def main():
    classified salary = 100
    disguised = salary * 2 + 1
    r: R = analyze(disguised, "anything")
"#,
    ));
    assert!(err.contains("classified data cannot reach"), "got: {err}");
}

#[test]
fn laundering_through_a_container_fails() {
    let err = run_err(&with_types(
        r#"def main():
    classified s = "secret"
    box = [s]
    r: R = analyze(box[0], "anything")
"#,
    ));
    assert!(err.contains("classified data cannot reach"), "got: {err}");
}

#[test]
fn laundering_through_a_function_return_fails() {
    let err = run_err(&with_types(
        r#"def passthrough(v: str) -> str:
    return v

def main():
    classified s = "secret"
    r: R = analyze(passthrough(s), "anything")
"#,
    ));
    assert!(err.contains("classified data cannot reach"), "got: {err}");
}

#[test]
fn classified_in_the_prompt_is_rejected() {
    let err = run_err(&with_types(
        r#"def main():
    classified s = "secret"
    r: R = analyze("public data", f"consider {s}")
"#,
    ));
    assert!(err.contains("prompt contains classified"), "got: {err}");
}

// --- declassification ---

#[test]
fn declassify_to_an_allowed_sink_succeeds() {
    let out = run(&with_types(
        r#"def main():
    classified s = "secret"
    declassify s for local_model:
        print(s)
"#,
    ));
    assert_eq!(out, vec!["secret"]);
}

#[test]
fn declassify_to_a_forbidden_sink_is_refused() {
    let err = run_err(&with_types(
        r#"def main():
    classified s = "secret"
    declassify s for openai:
        print(s)
"#,
    ));
    assert!(err.contains("policy forbids"), "got: {err}");
    assert!(err.contains("openai"), "got: {err}");
}

#[test]
fn unknown_sink_is_refused_not_allowed() {
    // A typo must close the door, never open one.
    let err = run_err(&with_types(
        r#"def main():
    classified s = "secret"
    declassify s for locl_model:
        print(s)
"#,
    ));
    assert!(err.contains("not a declared sink"), "got: {err}");
}

#[test]
fn declassified_binding_does_not_escape_the_block() {
    // The exposure is the block, not the rest of the program.
    let err = run_err(&with_types(
        r#"def main():
    classified s = "secret"
    declassify s as plain for local_model:
        print(plain)
    print(plain)
"#,
    ));
    assert!(err.contains("not defined"), "got: {err}");
}

#[test]
fn shadowed_name_is_restored_after_the_block() {
    let out = run(&with_types(
        r#"def main():
    s = "public"
    classified secret = "hidden"
    declassify secret as s for local_model:
        print(s)
    print(s)
"#,
    ));
    assert_eq!(out, vec!["hidden", "public"]);
}

#[test]
fn declassify_unlocks_only_the_named_sink() {
    // Releasing to one sink must not open every other sink.
    let err = run_err(&with_types(
        r#"def main():
    classified s = "secret"
    declassify s for openai:
        r: R = analyze(s, "anything")
"#,
    ));
    // Refused at the policy check, before any model call.
    assert!(err.contains("policy forbids"), "got: {err}");
}

// --- redaction: the blessed path ---

#[test]
fn redact_masks_classified_fields_but_keeps_shape() {
    let out = run(&with_types(
        r#"def main():
    e = Employee("Ada", "123-45-6789")
    r = redact(e)
    print(r.name)
    print(r.ssn)
"#,
    ));
    assert_eq!(out[0], "Ada", "public fields survive intact");
    assert_eq!(out[1], "<STR_1>", "classified fields are masked");
}

#[test]
fn redacted_values_need_no_declassification() {
    // Nothing sensitive left the process, so there is nothing to release.
    let out = run(&with_types(
        r#"def main():
    classified s = "secret"
    print(redact(s))
"#,
    ));
    assert_eq!(out, vec!["<STR_1>"]);
}

// --- audit ---

#[test]
fn audit_finds_every_site() {
    let src = with_types(
        r#"def main():
    classified a = "x"
    declassify a for local_model:
        print(a)
    declassify a for local_model:
        print(a)
"#,
    );
    let program = parse(&src).unwrap();
    let sites = kora_runtime::audit::audit(&program, "test.ko");
    assert_eq!(sites.len(), 2);
    assert!(sites.iter().all(|s| s.sink == "local_model"));
}

#[test]
fn audit_records_sites_reached_at_runtime() {
    let program = parse(&with_types(
        r#"def main():
    classified a = "x"
    declassify a for local_model:
        print(a)
"#,
    ))
    .unwrap();
    let mut i = interp();
    i.run(&program).unwrap();
    assert_eq!(i.declassify_sites.len(), 1);
    assert_eq!(i.declassify_sites[0].sink, "local_model");
}

// --- isolation boundary ---

#[test]
fn labels_survive_crossing_into_parallel_workers() {
    // Isolation copies values between agents; it must not launder labels.
    let err = run_err(&with_types(
        r#"def main():
    classified s = "secret"
    results = parallel for i in [1]:
        return analyze_it(s)

def analyze_it(v: str) -> str:
    r: R = analyze(v, "anything")
    return "unreachable"
"#,
    ));
    assert!(err.contains("classified data cannot reach"), "got: {err}");
}

// --- a release names one sink, not all of them ---

#[test]
fn a_release_does_not_open_every_sink_in_the_block() {
    // Regression. `declassify x for local_model:` used to make x *plain*
    // inside the block, so a secret released to a model could be written to
    // disk three lines later. The value now keeps its label and records which
    // sink it was approved for.
    let err = run_err(&with_types(
        r#"use fs
def main():
    classified s = "hunter2"
    declassify s as plain for local_model:
        fs.write("leaked.txt", plain)
"#,
    ));
    assert!(err.contains("classified data"), "got: {err}");
}

#[test]
fn a_release_still_permits_the_sink_it_names() {
    // The fix must not break the legitimate path.
    let out = run(&with_types(
        r#"def main():
    classified s = "hunter2"
    declassify s as plain for local_model:
        print(plain)
"#,
    ));
    assert_eq!(out, vec!["hunter2"], "printing inside the block is fine");
}

#[test]
fn a_release_does_not_survive_being_combined() {
    // Deriving a new value from an approved one yields something nobody
    // approved, so the release does not carry over.
    let err = run_err(&with_types(
        r#"def main():
    classified a = "one"
    classified b = "two"
    declassify a as first for local_model:
        joined = first + b
        r: R = analyze(joined, "anything")
"#,
    ));
    assert!(err.contains("classified data cannot reach"), "got: {err}");
}
