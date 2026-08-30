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

const CONFIG: &str = "[models]\ndefault = \"local:test-model\"\nvision = \"local:test-vision\"\n";

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
    entry_for_media(program_path, line, prompt, data, "", outcome)
}

/// `entry_for` for a call that also sent images: `media` is the fingerprint
/// of those images, which is part of the key.
fn entry_for_media(
    program_path: &str,
    line: u32,
    prompt: &str,
    data: &str,
    media: &str,
    outcome: RecordedOutcome,
) -> Entry {
    let site = format!("{program_path}:{line}");
    let model = "ollama:test-model".to_string();
    Entry {
        key: kora_runtime::cassette::key_for(&site, &model, prompt, data, media),
        site,
        model,
        prompt: prompt.to_string(),
        data: data.to_string(),
        media: media.to_string(),
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
        chunks: Vec::new(),
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

const NESTED_PROGRAM: &str = r#"type Section:
    name: str
    description: str

type Plan:
    sections: list[Section]

def main():
    result: Plan = analyze("some data", "plan this")
    match result:
        case Ok(value):
            for section in value.sections:
                print(f"{section.name}: {section.description}")
        case Uncertain(reason):
            print(f"uncertain: {reason}")
"#;

#[test]
fn analyze_result_field_may_be_a_list_of_declared_type() {
    let path = scratch_program_path("nested-list");
    let entry = entry_for(
        &path,
        9,
        "plan this",
        "\"some data\"",
        ok_fields(&[(
            "sections",
            serde_json::json!([
                {"name": "research", "description": "gather sources"},
                {"name": "draft", "description": "write the sections"},
            ]),
        )]),
    );
    let out = run_with_cassette("nested-list", NESTED_PROGRAM, vec![entry]).unwrap();
    assert_eq!(
        out,
        vec![
            "research: gather sources".to_string(),
            "draft: write the sections".to_string(),
        ]
    );
}

const NESTED_OBJECT_PROGRAM: &str = r#"type Address:
    city: str
    zip: str

type Customer:
    name: str
    address: Address

def main():
    result: Customer = analyze("some data", "extract this")
    match result:
        case Ok(value):
            print(f"{value.name} lives in {value.address.city} {value.address.zip}")
        case Uncertain(reason):
            print(f"uncertain: {reason}")
"#;

#[test]
fn analyze_result_field_may_be_a_declared_type() {
    let path = scratch_program_path("nested-object");
    let entry = entry_for(
        &path,
        10,
        "extract this",
        "\"some data\"",
        ok_fields(&[
            ("name", serde_json::json!("Ada")),
            (
                "address",
                serde_json::json!({"city": "London", "zip": "SW1"}),
            ),
        ]),
    );
    let out = run_with_cassette("nested-object", NESTED_OBJECT_PROGRAM, vec![entry]).unwrap();
    assert_eq!(out, vec!["Ada lives in London SW1".to_string()]);
}

#[test]
fn analyze_rejects_a_type_that_refers_to_itself() {
    let src = "type A:\n    b: B\n\ntype B:\n    a: A\n\ndef main():\n    x: A = analyze(\"d\", \"p\")\n";
    let err = run_with_cassette("self-referential", src, vec![]).unwrap_err();
    assert!(err.contains("refers to itself"), "got: {err}");
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

// --- images ---

/// A minimal but real PNG header, so `fs.image` accepts it.
fn png_bytes(tail: u8) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&[tail; 16]);
    bytes
}

/// A program that loads one image and classifies it, plus the cassette entry
/// that serves the call. Written to `dir` so the image and the program share
/// a directory.
fn image_program(path: &str) -> String {
    format!(
        r#"use fs

type Receipt:
    merchant: str
    amount: float

def main():
    match fs.image("{path}"):
        case Ok(picture):
            receipt: Receipt = analyze(picture, "read this receipt")
            match receipt:
                case Ok(r):
                    print(f"{{r.merchant}} {{r.amount}}")
                case Uncertain(why):
                    print(f"uncertain: {{why}}")
        case Err(why):
            print(f"could not read: {{why}}")
"#
    )
}

#[test]
fn an_image_reaches_the_model_and_replays_from_a_cassette() {
    let scratch = Scratch::new("analyze-image");
    let image_path = scratch.0.join("receipt.png");
    std::fs::write(&image_path, png_bytes(1)).unwrap();
    let source = image_program(&image_path.to_string_lossy().replace('\\', "\\\\"));
    let program_path = scratch.0.join("prog.ko").to_string_lossy().to_string();

    // The data argument is the image alone, so the JSON the model sees is
    // just the marker; the pixels ride beside it.
    let media = kora_runtime::cassette::media_key(&[("image/png", &png_bytes(1))]);
    let entry = entry_for_media(
        &program_path,
        10,
        "read this receipt",
        "\"<image>\"",
        &media,
        ok_fields(&[
            ("merchant", serde_json::json!("Blue Bottle")),
            ("amount", serde_json::json!(12.5)),
        ]),
    );

    let out = run_with_cassette("analyze-image", &source, vec![entry]).unwrap();
    assert_eq!(out, vec!["Blue Bottle 12.5"]);
}

/// The cassette must follow the picture. Editing the image behind an
/// unchanged path is a different question, and replaying the old answer for
/// it would be silently wrong.
#[test]
fn editing_the_image_misses_the_cassette() {
    let scratch = Scratch::new("analyze-image-edited");
    let image_path = scratch.0.join("receipt.png");
    std::fs::write(&image_path, png_bytes(2)).unwrap();
    let source = image_program(&image_path.to_string_lossy().replace('\\', "\\\\"));
    let program_path = scratch.0.join("prog.ko").to_string_lossy().to_string();

    // Recorded against a *different* picture at the same path.
    let media = kora_runtime::cassette::media_key(&[("image/png", &png_bytes(9))]);
    let entry = entry_for_media(
        &program_path,
        10,
        "read this receipt",
        "\"<image>\"",
        &media,
        ok_fields(&[
            ("merchant", serde_json::json!("Stale")),
            ("amount", serde_json::json!(1.0)),
        ]),
    );

    let err = run_with_cassette("analyze-image-edited", &source, vec![entry]).unwrap_err();
    assert!(err.contains("no recorded model call"), "got: {err}");
}

// --- choosing a model per call ---

/// A program names a *role*; kora.toml says which model fills it. Without
/// this, a vision call and a text call cannot share one program, which is
/// what forces model routing out into environment variables.
#[test]
fn a_call_can_name_a_model_from_the_config() {
    let scratch = Scratch::new("model-kwarg");
    let source = r#"type Insight:
    summary: str

def main():
    result: Insight = analyze("data", "summarize", model="vision")
    match result:
        case Ok(i):
            print(i.summary)
"#;
    let program_path = scratch.0.join("prog.ko").to_string_lossy().to_string();
    let site = format!("{program_path}:5");
    let model = "ollama:test-vision".to_string();
    let entry = Entry {
        key: kora_runtime::cassette::key_for(&site, &model, "summarize", "\"data\"", ""),
        site,
        model,
        prompt: "summarize".into(),
        data: "\"data\"".into(),
        media: String::new(),
        outcome: ok_fields(&[("summary", serde_json::json!("looked at it"))]),
    };

    let out = run_with_cassette("model-kwarg", source, vec![entry]).unwrap();
    assert_eq!(out, vec!["looked at it"]);
}

#[test]
fn an_unknown_model_name_lists_the_configured_ones() {
    let source = r#"type Insight:
    summary: str

def main():
    result: Insight = analyze("data", "summarize", model="gpt-9")
"#;
    let err = run_with_cassette("model-unknown", source, Vec::new()).unwrap_err();
    assert!(err.contains("no model named `gpt-9`"), "got: {err}");
}

/// A model name that came from outside could redirect the call to a
/// destination the program never chose.
#[test]
fn a_model_name_from_outside_the_program_is_refused() {
    let data = std::env::temp_dir().join(format!("kora-model-name-{}.txt", std::process::id()));
    std::fs::write(&data, "gpt-9").unwrap();
    let path = data.to_string_lossy().replace('\\', "\\\\");
    let source = format!(
        r#"use fs

type Insight:
    summary: str

def main():
    match fs.read("{path}"):
        case Ok(name):
            result: Insight = analyze("data", "summarize", model=name)
        case Err(why):
            print(why)
"#
    );
    let err = run_with_cassette("model-unverified", &source, Vec::new()).unwrap_err();
    std::fs::remove_file(&data).ok();
    assert!(err.contains("came from outside"), "got: {err}");
}
