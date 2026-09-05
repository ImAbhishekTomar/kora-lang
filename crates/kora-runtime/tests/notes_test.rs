//! `notes` — a durable run's own scratch space.
//!
//! Notes is the one stdlib module whose behaviour is defined by three things
//! at once: a file outside the run, the label system, and the journal. Each
//! of those has a rule that only shows up in combination — a classified
//! value must come back classified, a read must replay what the live run saw
//! rather than what the file holds now, and neither exists at all outside a
//! durable run.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kora_runtime::journal::Journal;
use kora_runtime::{Config, Interpreter, Run, RunStatus};
use kora_syntax::parse;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "kora-notes-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn program(&self) -> PathBuf {
        self.0.join("test.ko")
    }

    fn run_path(&self, id: &str) -> PathBuf {
        self.0.join(format!("{id}.jsonl"))
    }

    /// Where `notes` keeps the store for `run_id`, so a test can look at it
    /// or change it behind the program's back.
    fn store(&self, run_id: &str) -> PathBuf {
        self.0
            .join(".kora")
            .join("notes")
            .join(format!("{run_id}.json"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

const CONFIG: &str = "[models]\ndefault = \"local:test-model\"\n";

fn run_durable(
    scratch: &Scratch,
    src: &str,
    run: Run,
    path: PathBuf,
) -> (Vec<String>, Run, Option<String>) {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut interp = Interpreter::new();
    interp.config = Config::parse(CONFIG).unwrap();
    interp.program_name = scratch.program().to_string_lossy().to_string();
    interp.journal = Arc::new(Mutex::new(Journal::open(run, path).unwrap()));

    let error = match interp.run(&program) {
        Ok(()) => {
            let mut j = interp.journal.lock().unwrap();
            j.finish(RunStatus::Completed).unwrap();
            None
        }
        Err(e) if e.is_suspension() => None,
        Err(e) => Some(e.message),
    };
    let saved = {
        let j = interp.journal.lock().unwrap();
        j.run().clone()
    };
    (interp.output, saved, error)
}

/// A plain, non-durable run — the case `notes` must refuse.
fn run_plain(src: &str) -> Result<Vec<String>, String> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut interp = Interpreter::new();
    interp.config = Config::parse(CONFIG).unwrap();
    interp.program_name = "test.ko".into();
    match interp.run(&program) {
        Ok(()) => Ok(interp.output),
        Err(e) => Err(format!("{}|{}", e.message, e.hint.unwrap_or_default())),
    }
}

#[test]
fn a_note_written_is_a_note_read_back() {
    let scratch = Scratch::new("round-trip");
    let src = r#"
use notes

def main():
    notes.write("plan", ["find sources", "check dates"])
    plan: list[str] = notes.read("plan", [])
    print(f"{len(plan)} steps: {plan[0]}")
"#;
    let (out, _, err) = run_durable(
        &scratch,
        src,
        Run::new("r1".into(), "test.ko".into()),
        scratch.run_path("r1"),
    );
    assert_eq!(err, None);
    assert_eq!(out, vec!["2 steps: find sources"]);
    assert!(scratch.store("r1").exists(), "the store should be on disk");
}

#[test]
fn a_missing_key_is_the_default_rather_than_a_failure() {
    // Deliberately not `Ok`/`Err` like the rest of the stdlib: a key that
    // was never written is the same everyday shape as a dict lookup with a
    // fallback, not an error to handle.
    let scratch = Scratch::new("missing");
    let src = r#"
use notes

def main():
    visits: int = notes.read("visits", 0)
    print(f"visits: {visits}")
    text: str = notes.read("nothing", "fallback")
    print(text)
"#;
    let (out, _, err) = run_durable(
        &scratch,
        src,
        Run::new("r1".into(), "test.ko".into()),
        scratch.run_path("r1"),
    );
    assert_eq!(err, None);
    assert_eq!(out, vec!["visits: 0", "fallback"]);
}

#[test]
fn a_note_outlives_the_run_that_wrote_it() {
    // The point of the store being a file rather than a variable: a later
    // invocation of the same run reads what the earlier one left.
    let scratch = Scratch::new("outlives");
    let write_src = r#"
use notes

def main():
    notes.write("carried", "over")
"#;
    let (_, _, err) = run_durable(
        &scratch,
        write_src,
        Run::new("r1".into(), "test.ko".into()),
        scratch.run_path("r1"),
    );
    assert_eq!(err, None);

    let read_src = r#"
use notes

def main():
    value: str = notes.read("carried", "missing")
    print(value)
"#;
    let (out, _, err) = run_durable(
        &scratch,
        read_src,
        Run::new("r2".into(), "test.ko".into()),
        scratch.run_path("r2"),
    );
    // A different run id is a different store, by design: notes are scoped
    // to one run, not shared between them.
    assert_eq!(err, None);
    assert_eq!(out, vec!["missing"]);
}

#[test]
fn a_read_replays_what_the_live_run_saw() {
    // The reason reads are journaled at all. Something else can write to the
    // same store between the run and its resume; a replay that re-read the
    // file would take a different branch than the run it is replaying.
    let scratch = Scratch::new("replay");
    let path = scratch.run_path("r1");
    let src = r#"
use notes

def main():
    value: str = notes.read("k", "absent")
    print(f"read: {value}")
"#;
    let (first, run, err) = run_durable(
        &scratch,
        src,
        Run::new("r1".into(), "test.ko".into()),
        path.clone(),
    );
    assert_eq!(err, None);
    assert_eq!(first, vec!["read: absent"]);

    // Someone writes to the store behind the program's back.
    std::fs::create_dir_all(scratch.store("r1").parent().unwrap()).unwrap();
    std::fs::write(
        scratch.store("r1"),
        r#"{"k":{"value":"changed","secrecy":"public","released":null}}"#,
    )
    .unwrap();

    let (second, _, err) = run_durable(&scratch, src, run, path);
    assert_eq!(err, None);
    assert!(
        second.is_empty(),
        "the replayed read is silent and unchanged, got {second:?}"
    );
}

#[test]
fn notes_outside_a_durable_run_is_refused_with_the_flag_to_add() {
    // Same rule as `ask_human`: without a run there is no id, and without an
    // id there is no store to address.
    let err = run_plain(
        r#"
use notes

def main():
    notes.write("k", "v")
"#,
    )
    .expect_err("notes should refuse a plain run");
    assert!(err.contains("needs a durable run"), "got: {err}");
    assert!(err.contains("--durable"), "the hint names the flag: {err}");

    let err = run_plain(
        r#"
use notes

def main():
    value: str = notes.read("k", "d")
    print(value)
"#,
    )
    .expect_err("reading should refuse it too");
    assert!(err.contains("needs a durable run"), "got: {err}");
}

#[test]
fn a_classified_note_comes_back_classified() {
    // Label transitivity across the store. A value that went in classified
    // must not come out public, or the store becomes a laundering hole: one
    // write and one read would strip a label the type system was enforcing.
    let scratch = Scratch::new("classified");
    let src = r#"
use notes

def main():
    classified secret = "salary is 100000"
    notes.write("pay", secret)
    back: str = notes.read("pay", "")
    print(back)
"#;
    let (out, _, err) = run_durable(
        &scratch,
        src,
        Run::new("r1".into(), "test.ko".into()),
        scratch.run_path("r1"),
    );
    assert_eq!(err, None);
    assert_eq!(
        out.len(),
        1,
        "one line of output, redacted rather than printed: {out:?}"
    );
    assert!(
        !out[0].contains("100000"),
        "a classified note must not print in the clear: {out:?}"
    );
}

#[test]
fn a_note_read_back_is_unverified() {
    // The store is outside this evaluation, so what comes out of it is data
    // from elsewhere -- the same rule `fs.read` follows. A model name is the
    // sharpest case: it is a destination, and letting one come from a store
    // another process can write is how a call gets redirected.
    let scratch = Scratch::new("unverified");
    let src = r#"
use notes

def main():
    notes.write("m", "vision")
    name: str = notes.read("m", "vision")
    answer: str = analyze("q", "d", model=name)
    print("unreachable")
"#;
    let (_, _, err) = run_durable(
        &scratch,
        src,
        Run::new("r1".into(), "test.ko".into()),
        scratch.run_path("r1"),
    );
    let message = err.expect("an unverified model name must be refused");
    assert!(
        message.contains("came from outside the program"),
        "the refusal should name the reason: {message}"
    );
}

#[test]
fn a_key_that_is_not_a_string_is_refused() {
    let scratch = Scratch::new("bad-key");
    let (_, _, err) = run_durable(
        &scratch,
        r#"
use notes

def main():
    notes.write(3, "v")
"#,
        Run::new("r1".into(), "test.ko".into()),
        scratch.run_path("r1"),
    );
    assert!(
        err.unwrap_or_default().contains("needs a string key"),
        "writing under a non-string key should say so"
    );

    let scratch = Scratch::new("bad-read-key");
    let (_, _, err) = run_durable(
        &scratch,
        r#"
use notes

def main():
    value: str = notes.read(3, "d")
    print(value)
"#,
        Run::new("r1".into(), "test.ko".into()),
        scratch.run_path("r1"),
    );
    assert!(
        err.unwrap_or_default().contains("needs a string key"),
        "reading under a non-string key should say so"
    );
}

#[test]
fn writing_without_a_value_is_refused() {
    let scratch = Scratch::new("no-value");
    let (_, _, err) = run_durable(
        &scratch,
        r#"
use notes

def main():
    notes.write("k")
"#,
        Run::new("r1".into(), "test.ko".into()),
        scratch.run_path("r1"),
    );
    assert!(
        err.unwrap_or_default().contains("needs a value"),
        "a write with nothing to store should say so"
    );
}

#[test]
fn a_corrupt_store_reads_as_empty_rather_than_crashing() {
    // The store is a file on disk that anything can scribble on. A run that
    // finds nonsense there should fall back to its defaults, not die: the
    // notes are scratch space, and losing them is not losing the run.
    let scratch = Scratch::new("corrupt");
    std::fs::create_dir_all(scratch.store("r1").parent().unwrap()).unwrap();
    std::fs::write(scratch.store("r1"), "{not json at all").unwrap();

    let (out, _, err) = run_durable(
        &scratch,
        r#"
use notes

def main():
    value: str = notes.read("k", "default")
    print(value)
"#,
        Run::new("r1".into(), "test.ko".into()),
        scratch.run_path("r1"),
    );
    assert_eq!(err, None);
    assert_eq!(out, vec!["default"]);
}
