//! The native standard library.
//!
//! These tests are mostly about the *fixes*: the defects listed in
//! DECISIONS.md are the reason for writing these modules at all, so each one
//! gets a test that would fail against the library it replaces.

use kora_runtime::{Config, Interpreter};
use kora_syntax::parse;

const CONFIG: &str = r#"
[models]
default = "local:test-model"

[sinks]
local_model = { allow = ["classified"] }
"#;

fn run(src: &str) -> Vec<String> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut interp = Interpreter::new();
    let config = Config::parse(CONFIG).unwrap();
    interp.sinks = config.sinks.clone();
    interp.config = config;
    interp
        .run(&program)
        .unwrap_or_else(|e| panic!("runtime error: {}\n{src}", e.message));
    interp.output
}

fn run_err(src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut interp = Interpreter::new();
    let config = Config::parse(CONFIG).unwrap();
    interp.sinks = config.sinks.clone();
    interp.config = config;
    match interp.run(&program) {
        Err(e) => e.message,
        Ok(_) => panic!("expected an error, program succeeded:\n{src}"),
    }
}

/// A scratch directory the test can safely write into.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "kora-std-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// A path safe to paste into Kora source.
    ///
    /// Windows paths contain backslashes, and a backslash starts an escape
    /// sequence in a Kora string literal -- `C:\Users` reads as an unknown
    /// escape `\U`. Escaping here keeps these tests honest on every platform
    /// rather than only where paths happen to use forward slashes.
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().replace('\\', "\\\\")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

// --- modules ---

#[test]
fn unknown_module_is_refused_with_a_suggestion() {
    let err = run_err("use jsonn\n");
    assert!(err.contains("no module named"), "got: {err}");
}

#[test]
fn unknown_function_lists_what_the_module_has() {
    let program = parse("use json\ndef main():\n    json.parze(\"{}\")\n").unwrap();
    let mut interp = Interpreter::new();
    let e = interp.run(&program).unwrap_err();
    assert!(e.message.contains("no function `parze`"), "{}", e.message);
    assert!(e.hint.as_deref().unwrap_or("").contains("parse"));
}

#[test]
fn modules_can_be_aliased() {
    let out = run("use json as j\ndef main():\n    match j.parse(\"[1,2]\"):\n        case Ok(v):\n            print(len(v))\n        case Err(w):\n            print(w)\n");
    assert_eq!(out, vec!["2"]);
}

// --- json ---

#[test]
fn json_round_trip() {
    let out = run(r#"use json
def main():
    match json.parse("{\"n\": 3, \"xs\": [1, 2]}"):
        case Ok(d):
            match json.get(d, "xs.1"):
                case Ok(v):
                    print(v)
                case Err(w):
                    print(w)
        case Err(w):
            print(w)
"#);
    assert_eq!(out, vec!["2"]);
}

#[test]
fn json_errors_show_the_offending_text() {
    // The fix: "line 1 column 4318" is useless on one-line JSON, so the
    // message quotes the text around the problem.
    let out = run(r#"use json
def main():
    match json.parse("{\"a\": 1,,}"):
        case Ok(d):
            print("unreachable")
        case Err(why):
            print(why)
"#);
    assert!(out[0].contains("near"), "got: {}", out[0]);
    assert!(out[0].contains("invalid JSON"), "got: {}", out[0]);
}

#[test]
fn json_path_errors_name_where_they_stopped() {
    // The fix: a KeyError naming only the last segment does not tell you
    // which part of a nested path was missing.
    let out = run(r#"use json
def main():
    match json.parse("{\"users\": [{\"name\": \"ada\"}]}"):
        case Ok(d):
            match json.get(d, "users.0.email"):
                case Ok(v):
                    print("unreachable")
                case Err(why):
                    print(why)
        case Err(w):
            print(w)
"#);
    assert_eq!(out[0], "$.users.0.email: not found");
}

#[test]
fn json_refuses_to_serialize_a_classified_field() {
    // The leak this closes: `classified` marks a field on the *type*, so a
    // shallow check passes when the whole object is handed over instead.
    let err = run_err(
        r#"use json
type E:
    name: str
    classified ssn: str

def main():
    e = E("ada", "123-45-6789")
    json.stringify(e)
"#,
    );
    assert!(err.contains("classified data"), "got: {err}");
}

#[test]
fn json_serializes_ordinary_values() {
    let out = run(r#"use json
def main():
    match json.stringify({"a": 1}):
        case Ok(text):
            print(text)
        case Err(w):
            print(w)
"#);
    assert_eq!(out, vec!["{\"a\":1}"]);
}

// --- fs ---

#[test]
fn fs_write_then_read() {
    let scratch = Scratch::new("rw");
    let path = scratch.path("note.txt");
    let out = run(&format!(
        r#"use fs
def main():
    match fs.write("{path}", "hello"):
        case Ok(_):
            match fs.read("{path}"):
                case Ok(text):
                    print(text)
                case Err(w):
                    print(w)
        case Err(w):
            print(w)
"#
    ));
    assert_eq!(out, vec!["hello"]);
}

#[test]
fn fs_write_is_atomic() {
    // A crash mid-write must not leave a truncated file, so the write lands
    // via a temporary file and a rename. Check no temp file survives.
    let scratch = Scratch::new("atomic");
    let path = scratch.path("data.txt");
    run(&format!(
        "use fs\ndef main():\n    fs.write(\"{path}\", \"contents\")\n"
    ));
    let leftovers: Vec<_> = std::fs::read_dir(&scratch.0)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("kora-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temporary files must be cleaned up");
}

#[test]
fn fs_refuses_path_traversal() {
    let err = run_err("use fs\ndef main():\n    fs.read(\"../../etc/passwd\")\n");
    assert!(err.contains("`..`"), "got: {err}");
}

#[test]
fn fs_refuses_a_path_that_came_from_outside() {
    // The fix: paths built from model output or file contents are how
    // traversal happens; the usual defence is a review comment.
    let scratch = Scratch::new("unverified");
    let path = scratch.path("evil.txt");
    std::fs::write(&path, "/etc/passwd").unwrap();
    let err = run_err(&format!(
        r#"use fs
def main():
    match fs.read("{path}"):
        case Ok(contents):
            fs.read(contents)
        case Err(w):
            print(w)
"#
    ));
    assert!(err.contains("came from outside"), "got: {err}");
}

#[test]
fn fs_missing_file_names_the_path() {
    // io errors that omit the path are the first thing you want and never get.
    let out = run(r#"use fs
def main():
    match fs.read("definitely-not-here.txt"):
        case Ok(t):
            print("unreachable")
        case Err(why):
            print(why)
"#);
    assert_eq!(out, vec!["no such file: definitely-not-here.txt"]);
}

#[test]
fn fs_refuses_to_write_classified_data() {
    let scratch = Scratch::new("classified-write");
    let path = scratch.path("leak.txt");
    let err = run_err(&format!(
        r#"use fs
def main():
    classified secret = "hunter2"
    fs.write("{path}", secret)
"#
    ));
    assert!(err.contains("classified data"), "got: {err}");
}

// --- time ---

#[test]
fn time_formats_absolute_instants() {
    // There is no naive type to misuse: an instant is always absolute, and
    // formatting is the only place a zone appears.
    let out = run(r#"use time
def main():
    match time.format(0, "iso"):
        case Ok(s):
            print(s)
        case Err(w):
            print(w)
    match time.format(1709164800, "date"):
        case Ok(s):
            print(s)
        case Err(w):
            print(w)
"#);
    assert_eq!(out, vec!["1970-01-01T00:00:00Z", "2024-02-29"]);
}

#[test]
fn time_rejects_an_unknown_format() {
    let out = run(r#"use time
def main():
    match time.format(0, "rfc2822"):
        case Ok(s):
            print(s)
        case Err(why):
            print(why)
"#);
    assert!(out[0].contains("unknown time format"), "got: {}", out[0]);
}

#[test]
fn time_now_advances() {
    let out = run("use time\ndef main():\n    t = time.now()\n    print(t > 1700000000)\n");
    assert_eq!(out, vec!["True"]);
}

// --- re ---

#[test]
fn re_finds_and_replaces() {
    let out = run(r#"use re
def main():
    match re.find_all("[0-9]+", "a1 b22 c333"):
        case Ok(nums):
            print(nums)
        case Err(w):
            print(w)
    match re.replace("[aeiou]", "banana", "-"):
        case Ok(s):
            print(s)
        case Err(w):
            print(w)
"#);
    assert_eq!(out[0], "[\"1\", \"22\", \"333\"]");
    assert_eq!(out[1], "b-n-n-");
}

#[test]
fn re_reports_a_bad_pattern_as_a_value() {
    // Patterns often come from config or a model, so a bad one is data, not
    // a crash.
    let out = run(r#"use re
def main():
    match re.matches("(unclosed", "text"):
        case Ok(b):
            print("unreachable")
        case Err(why):
            print(why)
"#);
    assert!(out[0].contains("invalid pattern"), "got: {}", out[0]);
}

#[test]
fn re_survives_a_redos_pattern() {
    // The fix: `(a+)+$` against a long run of `a`s hangs a backtracking
    // engine. A finite-automaton engine answers in linear time. If this test
    // ever hangs, the guarantee has been lost.
    let out = run(r#"use re
def main():
    text = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab"
    match re.matches("(a+)+$", text):
        case Ok(hit):
            print(hit)
        case Err(w):
            print(w)
"#);
    assert_eq!(out, vec!["False"]);
}

#[test]
fn re_miss_is_a_value_not_a_none() {
    let out = run(r#"use re
def main():
    match re.find("z+", "abc"):
        case Ok(m):
            print("unreachable")
        case Err(why):
            print(why)
"#);
    assert_eq!(out, vec!["no match"]);
}

// --- labels flow through the stdlib ---

#[test]
fn parsed_data_stays_unverified_through_a_path_walk() {
    let scratch = Scratch::new("flow");
    let path = scratch.path("payload.json");
    std::fs::write(&path, "{\"target\": \"/etc/passwd\"}").unwrap();
    let err = run_err(&format!(
        r#"use fs
use json
def main():
    match fs.read("{path}"):
        case Ok(text):
            match json.parse(text):
                case Ok(doc):
                    match json.get(doc, "target"):
                        case Ok(target):
                            fs.read(target)
                        case Err(w):
                            print(w)
                case Err(w):
                    print(w)
        case Err(w):
            print(w)
"#
    ));
    assert!(
        err.contains("came from outside"),
        "the label must survive read -> parse -> path walk, got: {err}"
    );
}
