//! End-to-end tests for `analyze`, `match`, and cassettes.
//!
//! No test here touches the network: every model call is served from a
//! cassette written by the test itself.

use kora_runtime::cassette::{Entry, RecordedOutcome};
use kora_runtime::{Cassette, Config, Interpreter, Mode};
use kora_syntax::parse;
use std::path::PathBuf;

/// A scratch directory unique to each test, cleaned up on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("kora-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn program(&self, source: &str) -> PathBuf {
        let path = self.0.join("prog.ko");
        std::fs::write(&path, source).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

const CONFIG: &str = "[models]\ndefault = \"local:test-model\"\n";

/// Run `source`, serving analyze calls from `entries`.
fn run_with_cassette(name: &str, source: &str, entries: Vec<Entry>) -> Result<Vec<String>, String> {
    let scratch = Scratch::new(name);
    let path = scratch.program(source);

    // Seed the cassette by recording the prepared entries.
    let mut recording = Cassette::open(Mode::Record, &path);
    for entry in entries {
        recording.insert(entry);
    }
    recording.save().unwrap();

    let program = parse(source).map_err(|e| format!("parse error: {e}"))?;
    let mut interp = Interpreter::new();
    interp.program_name = path.to_string_lossy().to_string();
    interp.config = Config::parse(CONFIG).unwrap();
    interp.cassette = Some(std::sync::Arc::new(std::sync::Mutex::new(Cassette::open(
        Mode::Replay,
        &path,
    ))));

    interp
        .run(&program)
        .map_err(|e| e.message)
        .map(|()| interp.output)
}

/// Build a cassette entry matching a call site, using the same key the
/// interpreter computes.
fn entry_for(
    program_path: &str,
    line: u32,
    prompt: &str,
    data: &str,
    outcome: RecordedOutcome,
) -> Entry {
    let site = format!("{program_path}:{line}");
    let model = "ollama:test-model".to_string();
    Entry {
        key: kora_runtime::cassette::key_for(&site, &model, prompt, data),
        site,
        model,
        prompt: prompt.to_string(),
        data: data.to_string(),
        outcome,
    }
}

fn ok_fields(pairs: &[(&str, serde_json::Value)]) -> RecordedOutcome {
    let mut map = serde_json::Map::new();
    for (k, v) in pairs {
        map.insert((*k).to_string(), v.clone());
    }
    RecordedOutcome::Ok {
        fields: map,
        tokens_in: 100,
        tokens_out: 20,
    }
}

/// Full-path helper: the interpreter keys on the program path it was given.
fn scratch_program_path(name: &str) -> String {
    std::env::temp_dir()
        .join(format!("kora-test-{name}-{}", std::process::id()))
        .join("prog.ko")
        .to_string_lossy()
        .to_string()
}

const ANALYZE_PROGRAM: &str = r#"type Insight:
    summary: str
    severity: int

def main():
    result: Insight = analyze("some data", "assess this")
    match result:
        case Ok(value):
            print(f"{value.summary} / {value.severity}")
        case Uncertain(reason):
            print(f"uncertain: {reason}")
"#;

#[test]
fn analyze_ok_path_produces_typed_object() {
    let path = scratch_program_path("ok-path");
    let entry = entry_for(
        &path,
        6,
        "assess this",
        "\"some data\"",
        ok_fields(&[
            ("summary", serde_json::json!("disk nearly full")),
            ("severity", serde_json::json!(3)),
        ]),
    );
    let out = run_with_cassette("ok-path", ANALYZE_PROGRAM, vec![entry]).unwrap();
    assert_eq!(out, vec!["disk nearly full / 3"]);
}

#[test]
fn analyze_uncertain_path_is_matchable() {
    let path = scratch_program_path("uncertain-path");
    let entry = entry_for(
        &path,
        6,
        "assess this",
        "\"some data\"",
        RecordedOutcome::Uncertain {
            reason: "not enough context".into(),
            tokens_in: 10,
            tokens_out: 5,
        },
    );
    let out = run_with_cassette("uncertain-path", ANALYZE_PROGRAM, vec![entry]).unwrap();
    assert_eq!(out, vec!["uncertain: not enough context"]);
}

#[test]
fn replay_miss_is_an_error_not_a_live_call() {
    // No entries seeded: replay must refuse rather than reach a provider.
    let err = run_with_cassette("replay-miss", ANALYZE_PROGRAM, vec![]).unwrap_err();
    assert!(err.contains("no recorded model call"), "got: {err}");
}

#[test]
fn analyze_without_type_annotation_is_rejected() {
    let src = "def main():\n    x = analyze(\"data\", \"do something\")\n";
    let err = run_with_cassette("no-annotation", src, vec![]).unwrap_err();
    assert!(err.contains("needs a declared result type"), "got: {err}");
}

#[test]
fn analyze_with_undeclared_type_is_rejected() {
    let src = "def main():\n    x: Ghost = analyze(\"data\", \"do something\")\n";
    let err = run_with_cassette("undeclared", src, vec![]).unwrap_err();
    assert!(err.contains("not a declared type"), "got: {err}");
}

#[test]
fn analyze_rejects_unsupported_field_types() {
    let src = "type Bad:\n    items: dict\n\ndef main():\n    x: Bad = analyze(\"d\", \"p\")\n";
    let err = run_with_cassette("bad-field", src, vec![]).unwrap_err();
    assert!(err.contains("cannot request"), "got: {err}");
}

#[test]
fn analyze_arity_is_checked() {
    let src = "type T:\n    a: str\n\ndef main():\n    x: T = analyze(\"only one arg\")\n";
    let err = run_with_cassette("arity", src, vec![]).unwrap_err();
    assert!(err.contains("takes 2 arguments"), "got: {err}");
}

#[test]
fn token_usage_accumulates_from_cassette() {
    let scratch = Scratch::new("tokens");
    let path = scratch.program(ANALYZE_PROGRAM);
    let entry = entry_for(
        &path.to_string_lossy(),
        6,
        "assess this",
        "\"some data\"",
        ok_fields(&[
            ("summary", serde_json::json!("x")),
            ("severity", serde_json::json!(1)),
        ]),
    );
    let mut recording = Cassette::open(Mode::Record, &path);
    recording.insert(entry);
    recording.save().unwrap();

    let program = parse(ANALYZE_PROGRAM).unwrap();
    let mut interp = Interpreter::new();
    interp.program_name = path.to_string_lossy().to_string();
    interp.config = Config::parse(CONFIG).unwrap();
    interp.cassette = Some(std::sync::Arc::new(std::sync::Mutex::new(Cassette::open(
        Mode::Replay,
        &path,
    ))));
    interp.run(&program).unwrap();

    assert_eq!(interp.tokens_in, 100);
    assert_eq!(interp.tokens_out, 20);
    assert_eq!(interp.model_calls, 0, "replay must not call a provider");
}

// --- match/pattern tests, independent of analyze ---

fn run_plain(src: &str) -> Vec<String> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}"));
    let mut interp = Interpreter::new();
    interp
        .run(&program)
        .unwrap_or_else(|e| panic!("runtime error: {}", e.message));
    interp.output
}

#[test]
fn match_on_literals() {
    let src = r#"def classify(n: int) -> str:
    match n:
        case 0:
            return "zero"
        case 1:
            return "one"
        case _:
            return "many"

print(classify(0))
print(classify(1))
print(classify(7))
"#;
    assert_eq!(run_plain(src), vec!["zero", "one", "many"]);
}

#[test]
fn match_on_strings_and_bools() {
    let src = r#"match "high":
    case "low":
        print("l")
    case "high":
        print("h")
match True:
    case True:
        print("yes")
    case False:
        print("no")
"#;
    assert_eq!(run_plain(src), vec!["h", "yes"]);
}

#[test]
fn match_binding_arm_captures_value() {
    let src = "match 42:\n    case n:\n        print(n)\n";
    assert_eq!(run_plain(src), vec!["42"]);
}

#[test]
fn unmatched_value_errors_with_hint() {
    let program = parse("match 5:\n    case 1:\n        print(\"one\")\n").unwrap();
    let mut interp = Interpreter::new();
    let err = interp.run(&program).unwrap_err();
    assert!(err.message.contains("no `case` arm matched"));
    assert!(err.hint.as_deref().unwrap_or("").contains("case _:"));
}

#[test]
fn parallel_workers_share_one_cassette() {
    // Regression: workers used to take the cassette out of a shared slot, so
    // concurrent workers saw None and recordings were lost. All parallel
    // branches must replay from the same cassette.
    let scratch = Scratch::new("parallel-cassette");
    let src = r#"type Insight:
    summary: str
    severity: int

agent look(item: str) -> str:
    result: Insight = analyze(item, "assess this")
    match result:
        case Ok(v):
            return v.summary
        case Uncertain(reason):
            return reason

def main():
    results = parallel for item in ["a", "b", "c"]:
        return look(item)
    for r in results:
        print(r)
"#;
    let path = scratch.program(src);
    let path_str = path.to_string_lossy().to_string();

    // One entry per distinct input, all at the same call site and line.
    let mut recording = Cassette::open(Mode::Record, &path);
    for item in ["a", "b", "c"] {
        recording.insert(entry_for(
            &path_str,
            6,
            "assess this",
            &format!("\"{item}\""),
            ok_fields(&[
                ("summary", serde_json::json!(format!("saw {item}"))),
                ("severity", serde_json::json!(1)),
            ]),
        ));
    }
    recording.save().unwrap();

    let program = parse(src).unwrap();
    let mut interp = Interpreter::new();
    interp.program_name = path_str;
    interp.config = Config::parse(CONFIG).unwrap();
    interp.cassette = Some(std::sync::Arc::new(std::sync::Mutex::new(Cassette::open(
        Mode::Replay,
        &path,
    ))));
    interp.run(&program).expect("all branches should replay");

    assert_eq!(interp.output, vec!["saw a", "saw b", "saw c"]);
    assert_eq!(interp.model_calls, 0, "replay must not call a provider");
}
