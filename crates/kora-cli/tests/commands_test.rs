//! Every documented command, run against the real binary.
//!
//! The CLI is the surface everybody actually touches, and it is the one part
//! of the system that unit tests reach past: argument parsing, exit codes,
//! and the sentence printed when something is wrong are only real once a
//! process has run. These check the shapes a user hits — including the
//! mistakes, which is where a bad message costs the most.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "kora-cmd-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn kora(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kora"))
        .args(args)
        .output()
        .expect("the binary should run")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Everything printed, since a message may go to either stream.
fn said(out: &Output) -> String {
    format!("{}{}", stdout(out), stderr(out))
}

const HELLO: &str = r#"def main():
    print("hello")
"#;

// --- the shapes that should work ---

#[test]
fn version_prints_the_version() {
    let out = kora(&["--version"]);
    assert!(out.status.success());
    assert!(
        stdout(&out).contains(env!("CARGO_PKG_VERSION")),
        "got: {}",
        stdout(&out)
    );
}

#[test]
fn no_arguments_prints_usage() {
    let out = kora(&[]);
    let text = said(&out);
    assert!(text.contains("usage:"), "got: {text}");
    // Every command in the banner is one a user can actually reach.
    for command in ["run", "check", "test", "audit", "runs", "trace"] {
        assert!(
            text.contains(&format!("kora {command}")),
            "usage should list `{command}`: {text}"
        );
    }
}

#[test]
fn a_program_runs_and_prints() {
    let scratch = Scratch::new("run");
    let program = scratch.write("hello.ko", HELLO);
    let out = kora(&["run", program.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "hello");
}

#[test]
fn a_file_alone_means_run() {
    // `kora <file.ko>` is documented as the same thing as `kora run`, so it
    // has to stay the same thing.
    let scratch = Scratch::new("bare");
    let program = scratch.write("hello.ko", HELLO);
    let bare = kora(&[program.to_str().unwrap()]);
    let explicit = kora(&["run", program.to_str().unwrap()]);
    assert!(bare.status.success(), "{}", stderr(&bare));
    assert_eq!(stdout(&bare), stdout(&explicit));
}

#[test]
fn check_accepts_a_good_program_without_running_it() {
    let scratch = Scratch::new("check");
    let program = scratch.write("hello.ko", HELLO);
    let out = kora(&["check", program.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !stdout(&out).contains("hello"),
        "check must not run the program: {}",
        stdout(&out)
    );
}

#[test]
fn check_takes_more_than_one_file() {
    let scratch = Scratch::new("check-many");
    let a = scratch.write("a.ko", HELLO);
    let b = scratch.write("b.ko", "def main():\n    print(\"b\")\n");
    let out = kora(&["check", a.to_str().unwrap(), b.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn check_syntax_stops_before_name_resolution() {
    // `--syntax` parses only. A program that parses but names something
    // undefined passes it and fails a full check, which is the whole
    // distinction the flag exists to draw.
    let scratch = Scratch::new("syntax-only");
    let program = scratch.write("undefined.ko", "def main():\n    print(nope)\n");
    let syntax = kora(&["check", "--syntax", program.to_str().unwrap()]);
    let full = kora(&["check", program.to_str().unwrap()]);
    assert!(syntax.status.success(), "{}", stderr(&syntax));
    assert!(!full.status.success(), "a full check should catch it");
}

#[test]
fn test_runs_the_test_blocks() {
    let scratch = Scratch::new("test");
    let program = scratch.write(
        "suite.ko",
        r#"def double(n: int) -> int:
    return n * 2

test "doubling":
    assert double(2) == 4

def main():
    print("not the tests")
"#,
    );
    let out = kora(&["test", program.to_str().unwrap()]);
    assert!(out.status.success(), "{}", said(&out));
    assert!(said(&out).contains("doubling"), "got: {}", said(&out));
}

#[test]
fn a_failing_test_fails_the_command() {
    let scratch = Scratch::new("test-fail");
    let program = scratch.write(
        "suite.ko",
        r#"def double(n: int) -> int:
    return n * 3

test "doubling":
    assert double(2) == 4

def main():
    print("x")
"#,
    );
    let out = kora(&["test", program.to_str().unwrap()]);
    assert!(
        !out.status.success(),
        "a failing test must fail the command: {}",
        said(&out)
    );
}

#[test]
fn audit_lists_declassification_sites() {
    let scratch = Scratch::new("audit");
    scratch.write(
        "kora.toml",
        "[models]\ndefault = \"local:m\"\n\n[sinks]\nlog = { allow = [\"classified\"] }\n",
    );
    let program = scratch.write(
        "secret.ko",
        r#"def main():
    classified pay = "100000"
    declassify pay for log:
        print("released")
"#,
    );
    let out = kora(&["audit", program.to_str().unwrap()]);
    assert!(out.status.success(), "{}", said(&out));
    assert!(
        said(&out).contains("log"),
        "the audit should name the sink: {}",
        said(&out)
    );
}

#[test]
fn runs_lists_nothing_for_a_program_that_never_ran_durably() {
    let scratch = Scratch::new("runs-empty");
    let program = scratch.write("hello.ko", HELLO);
    let out = kora(&["runs", program.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn runs_lists_a_durable_run_after_one() {
    let scratch = Scratch::new("runs");
    let program = scratch.write("hello.ko", HELLO);
    let ran = kora(&["run", "--durable", program.to_str().unwrap()]);
    assert!(ran.status.success(), "{}", stderr(&ran));
    let out = kora(&["runs", program.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("completed") || stdout(&out).contains("Completed"),
        "a finished run should be listed as such: {}",
        stdout(&out)
    );
}

#[test]
fn report_prints_usage_after_the_run() {
    let scratch = Scratch::new("report");
    let program = scratch.write("hello.ko", HELLO);
    let out = kora(&["run", "--report", program.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = said(&out);
    assert!(text.contains("hello"), "the program still runs: {text}");
    assert!(
        text.contains("token") || text.contains("call"),
        "the report should mention what was spent: {text}"
    );
}

#[test]
fn trace_shows_the_spans_of_the_last_traced_run() {
    let scratch = Scratch::new("trace");
    let program = scratch.write("hello.ko", HELLO);
    let ran = kora(&["run", "--trace", program.to_str().unwrap()]);
    assert!(ran.status.success(), "{}", stderr(&ran));
    let out = kora(&["trace", program.to_str().unwrap()]);
    assert!(out.status.success(), "{}", said(&out));
}

#[test]
fn tree_shows_the_packages_a_program_uses() {
    let scratch = Scratch::new("tree");
    let program = scratch.write("hello.ko", HELLO);
    let out = kora(&["tree", program.to_str().unwrap()]);
    assert!(out.status.success(), "{}", said(&out));
}

// --- the mistakes ---

#[test]
fn a_missing_file_says_so_and_fails() {
    let out = kora(&["run", "no-such-file.ko"]);
    assert!(!out.status.success());
    let text = said(&out);
    assert!(
        text.contains("no-such-file.ko"),
        "the message should name the file: {text}"
    );
}

#[test]
fn a_parse_error_points_at_the_line() {
    let scratch = Scratch::new("parse-error");
    let program = scratch.write("broken.ko", "def main(:\n    print(\"x\")\n");
    let out = kora(&["run", program.to_str().unwrap()]);
    assert!(!out.status.success());
    let text = said(&out);
    assert!(text.contains("broken.ko"), "got: {text}");
    assert!(text.contains('1'), "the line number should appear: {text}");
}

#[test]
fn a_program_with_no_main_is_refused() {
    let scratch = Scratch::new("no-main");
    let program = scratch.write("nomain.ko", "def helper():\n    print(\"x\")\n");
    let out = kora(&["run", program.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(said(&out).contains("main"), "got: {}", said(&out));
}

#[test]
fn an_unknown_command_is_not_silently_a_filename() {
    let out = kora(&["frobnicate"]);
    assert!(!out.status.success(), "got: {}", said(&out));
}

#[test]
fn replay_without_a_cassette_refuses_rather_than_calling_a_provider() {
    let scratch = Scratch::new("replay-miss");
    scratch.write("kora.toml", "[models]\ndefault = \"local:test-model\"\n");
    let program = scratch.write(
        "ask.ko",
        "def main():\n    answer: str = analyze(\"q\", \"d\")\n    print(\"done\")\n",
    );
    let out = kora(&["run", "--replay", program.to_str().unwrap()]);
    assert!(!out.status.success(), "got: {}", said(&out));
    assert!(
        said(&out).contains("no recorded model call"),
        "the message should say what is missing: {}",
        said(&out)
    );
}

#[test]
fn resuming_a_run_that_does_not_exist_says_so() {
    let scratch = Scratch::new("resume-missing");
    let program = scratch.write("hello.ko", HELLO);
    let out = kora(&[
        "run",
        "--durable",
        "--resume",
        "nosuchrun",
        program.to_str().unwrap(),
    ]);
    assert!(!out.status.success(), "got: {}", said(&out));
    assert!(
        said(&out).contains("nosuchrun"),
        "the message should name the run: {}",
        said(&out)
    );
}

#[test]
fn answering_a_run_that_is_not_waiting_says_so() {
    let scratch = Scratch::new("answer-missing");
    let program = scratch.write("hello.ko", HELLO);
    let out = kora(&["answer", program.to_str().unwrap(), "nosuchrun", "yes"]);
    assert!(!out.status.success(), "got: {}", said(&out));
}

#[test]
fn a_command_that_needs_a_file_and_is_given_none_says_so() {
    for command in ["check", "test", "audit", "runs", "trace", "tree"] {
        let out = kora(&[command]);
        assert!(
            !out.status.success(),
            "`kora {command}` with no file should fail: {}",
            said(&out)
        );
    }
}

#[test]
fn a_directory_where_a_program_belongs_is_refused() {
    let scratch = Scratch::new("dir");
    let out = kora(&["run", scratch.0.to_str().unwrap()]);
    assert!(!out.status.success(), "got: {}", said(&out));
}

/// A run that suspends on `ask_human`, then is answered from another
/// invocation — the whole point of `kora answer` existing as a command.
#[test]
fn a_suspended_run_can_be_answered_from_the_command_line() {
    let scratch = Scratch::new("answer");
    let program = scratch.write(
        "ask.ko",
        r#"def main():
    print("before")
    reply = ask_human("proceed?", "some context")
    print(f"got: {reply}")
"#,
    );
    let ran = kora(&["run", "--durable", program.to_str().unwrap()]);
    // A suspended run is not a finished one, and its exit code says so: a
    // shell script that treated parking on a question as success would carry
    // on as though the answer had been given.
    assert_eq!(
        ran.status.code(),
        Some(3),
        "a run waiting for an answer has its own exit code: {}",
        said(&ran)
    );
    assert!(stdout(&ran).contains("before"), "got: {}", stdout(&ran));
    assert!(
        said(&ran).contains("kora answer"),
        "it should say how to answer: {}",
        said(&ran)
    );

    let id = only_run_id(&program);
    let answered = kora(&["answer", program.to_str().unwrap(), &id, "yes"]);
    assert!(answered.status.success(), "{}", said(&answered));
    assert!(
        said(&answered).contains("got: yes"),
        "the answer should reach the program: {}",
        said(&answered)
    );
}

/// The run id `kora runs` reports for a program's single run.
fn only_run_id(program: &Path) -> String {
    let listing = kora(&["runs", program.to_str().unwrap()]);
    let text = stdout(&listing);
    let ids: Vec<String> = text
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|word| word.len() > 8 && word.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|w| w.to_string())
        .collect();
    assert_eq!(ids.len(), 1, "expected one run, got: {text}");
    ids[0].clone()
}
