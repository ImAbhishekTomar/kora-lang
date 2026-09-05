//! Phase 5: durable execution.
//!
//! Durability here is replay-based: the Rust call stack cannot be serialized,
//! so a resumed run re-executes from the top with every effect served from
//! the journal. These tests drive that path directly — no network, no clock.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kora_runtime::journal::{self, Effect, Journal, Scope};
use kora_runtime::{Config, Interpreter, Run, RunStatus};
use kora_syntax::parse;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "kora-dur-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn run_path(&self, id: &str) -> PathBuf {
        self.0.join(format!("{id}.jsonl"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

const CONFIG: &str = "[models]\ndefault = \"local:test-model\"\n";

/// A path as a Kora string literal.
///
/// Windows separators are backslashes, and a backslash in Kora source starts
/// an escape — `D:\a\kora-lang\...` is `\a`, which is not one. Forward
/// slashes are accepted by the Windows APIs underneath, so this is a change
/// of spelling rather than of meaning.
fn ko_path(path: &std::path::Path) -> String {
    path.to_str().expect("a UTF-8 path").replace('\\', "/")
}

/// Run `src` against a durable journal, returning (output, run).
fn run_durable(src: &str, run: Run, path: PathBuf) -> (Vec<String>, Run, Option<String>) {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut interp = Interpreter::new();
    interp.config = Config::parse(CONFIG).unwrap();
    interp.program_name = "test.ko".into();
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

const ASK_PROGRAM: &str = r#"def main():
    print("before")
    answer = ask_human("proceed?", "some context")
    print(f"got: {answer}")
    print("after")
"#;

#[test]
fn ask_human_suspends_the_run() {
    let scratch = Scratch::new("suspend");
    let path = scratch.run_path("r1");
    let (output, run, err) =
        run_durable(ASK_PROGRAM, Run::new("r1".into(), "test.ko".into()), path);

    assert!(err.is_none(), "suspension is not an error: {err:?}");
    assert_eq!(run.status, RunStatus::Suspended);
    assert_eq!(output, vec!["before"], "execution stops at the question");

    let pending = run.pending.expect("a question should be waiting");
    assert_eq!(pending.question, "proceed?");
    assert_eq!(pending.context, "some context");
}

#[test]
fn answering_resumes_where_it_stopped() {
    let scratch = Scratch::new("resume");
    let path = scratch.run_path("r1");

    // First attempt parks on the question.
    let (_, mut run, _) = run_durable(
        ASK_PROGRAM,
        Run::new("r1".into(), "test.ko".into()),
        path.clone(),
    );
    let pending = run.pending.clone().unwrap();

    // A person answers; the answer becomes the effect that step was waiting on.
    run.entries.push(journal::Entry {
        scope: pending.scope,
        seq: pending.seq,
        site: pending.site,
        effect: Effect::Human {
            question: pending.question,
            answer: "yes".into(),
        },
    });
    run.pending = None;
    run.status = RunStatus::Running;

    // Second attempt replays and carries on.
    let (output, run, err) = run_durable(ASK_PROGRAM, run, path);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(
        output,
        vec!["got: yes", "after"],
        "resume continues the story instead of retelling it"
    );
}

#[test]
fn output_is_exactly_once_across_a_crash() {
    // The crash case that suppression alone cannot fix: the journal cannot
    // know whether the line before the crash was printed, so output is
    // journaled.
    let scratch = Scratch::new("exactly-once");
    let path = scratch.run_path("r1");

    let (first, run, _) = run_durable(
        ASK_PROGRAM,
        Run::new("r1".into(), "test.ko".into()),
        path.clone(),
    );
    assert_eq!(first, vec!["before"]);

    // Simulate a crash and restart with no answer yet: nothing new is shown.
    let (second, _, _) = run_durable(ASK_PROGRAM, run, path);
    assert!(
        second.is_empty(),
        "a restarted run must not repeat output it already showed, got {second:?}"
    );
}

#[test]
fn ask_human_outside_a_durable_run_is_refused() {
    // Without a journal there is nowhere to park, so say so plainly rather
    // than blocking forever or inventing an answer.
    let program = parse(ASK_PROGRAM).unwrap();
    let mut interp = Interpreter::new();
    interp.config = Config::parse(CONFIG).unwrap();
    let err = interp.run(&program).unwrap_err();
    assert!(
        err.message.contains("needs a durable run"),
        "{}",
        err.message
    );
    assert!(err.hint.as_deref().unwrap_or("").contains("--durable"));
}

#[test]
fn classified_data_cannot_be_shown_to_a_human_unreleased() {
    let src = r#"type E:
    classified ssn: str

def main():
    e = E("123-45-6789")
    answer = ask_human("is this right?", e.ssn)
"#;
    let scratch = Scratch::new("classified-human");
    let (_, _, err) = run_durable(
        src,
        Run::new("r1".into(), "t".into()),
        scratch.run_path("r1"),
    );
    let err = err.expect("should refuse");
    assert!(err.contains("classified"), "got: {err}");
}

#[test]
fn journal_replays_recorded_output_in_order() {
    let src = r#"def main():
    print("a")
    print("b")
    answer = ask_human("q", "")
    print("c")
"#;
    let scratch = Scratch::new("order");
    let path = scratch.run_path("r1");

    let (first, mut run, _) = run_durable(src, Run::new("r1".into(), "t".into()), path.clone());
    assert_eq!(first, vec!["a", "b"]);

    let pending = run.pending.clone().unwrap();
    run.entries.push(journal::Entry {
        scope: pending.scope,
        seq: pending.seq,
        site: pending.site,
        effect: Effect::Human {
            question: pending.question,
            answer: "ok".into(),
        },
    });
    run.pending = None;

    let (second, _, _) = run_durable(src, run, path);
    assert_eq!(second, vec!["c"], "only the new line appears");
}

#[test]
fn parallel_branches_journal_independently() {
    // Thread interleaving is not reproducible, so each branch counts its own
    // steps; replay must still be exact.
    let src = r#"def main():
    results = parallel for x in ["a", "b", "c"]:
        return work(x)
    for r in results:
        print(r)
    done = ask_human("ok?", "")

def work(x: str) -> str:
    print(f"working {x}")
    return x
"#;
    let scratch = Scratch::new("parallel");
    let path = scratch.run_path("r1");

    let (first, mut run, _) = run_durable(src, Run::new("r1".into(), "t".into()), path.clone());
    // Three branch lines plus three result lines, in some order.
    assert_eq!(first.len(), 6, "got {first:?}");

    let scopes: Vec<Scope> = run.entries.iter().map(|e| e.scope.clone()).collect();
    assert!(
        scopes.iter().any(|s| s.0 == vec![0]) && scopes.iter().any(|s| s.0 == vec![2]),
        "each branch should own a scope, got {scopes:?}"
    );

    // Resume: nothing repeats.
    let pending = run.pending.clone().unwrap();
    run.entries.push(journal::Entry {
        scope: pending.scope,
        seq: pending.seq,
        site: pending.site,
        effect: Effect::Human {
            question: pending.question,
            answer: "yes".into(),
        },
    });
    run.pending = None;

    let (second, _, err) = run_durable(src, run, path);
    assert!(err.is_none(), "{err:?}");
    assert!(
        second.is_empty(),
        "resumed run must not repeat completed work, got {second:?}"
    );
}

#[test]
fn a_changed_program_refuses_to_resume() {
    // Replay assumes the code between effects is unchanged. If it is not,
    // resuming would silently produce wrong answers, so refuse loudly.
    let scratch = Scratch::new("diverge");
    let path = scratch.run_path("r1");

    let (_, run, _) = run_durable(
        "def main():\n    print(\"a\")\n    print(\"b\")\n    x = ask_human(\"q\", \"\")\n",
        Run::new("r1".into(), "t".into()),
        path.clone(),
    );
    assert_eq!(run.entries.len(), 2, "two prints were journaled");

    // Line 3 now performs a different kind of effect. Replaying an output as
    // a human answer would be silently wrong, so it must be refused.
    let changed = "def main():\n    print(\"a\")\n    x = ask_human(\"q\", \"\")\n";
    let (_, _, err) = run_durable(changed, run, path);
    let err = err.expect("divergence must be reported");
    assert!(err.contains("does not match its journal"), "got: {err}");
}

#[test]
fn a_plain_run_journals_nothing() {
    // Durability is opt-in; ordinary runs pay nothing for it.
    let program = parse("def main():\n    print(\"hi\")\n").unwrap();
    let mut interp = Interpreter::new();
    interp.run(&program).unwrap();
    assert_eq!(interp.output, vec!["hi"]);
    assert!(!interp.journal.lock().unwrap().is_durable());
}

const WRITE_PROGRAM: &str = r#"use fs

def main():
    match fs.append("OUT", "row\n"):
        case Ok(_):
            print("wrote")
        case Err(why):
            print(why)
"#;

#[test]
fn a_write_replays_from_the_journal_instead_of_happening_twice() {
    let scratch = Scratch::new("write-once");
    let path = scratch.run_path("r1");
    let out = scratch.0.join("out.txt");
    let src = WRITE_PROGRAM.replace("OUT", &ko_path(&out));

    let (_, run, err) = run_durable(&src, Run::new("r1".into(), "test.ko".into()), path.clone());
    assert!(err.is_none(), "{err:?}");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "row\n");

    // The same run again — what a resume does — must not append a second row.
    let (_, _, err) = run_durable(&src, run, path);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "row\n",
        "a replayed write is served from the journal, not performed again"
    );
}

#[test]
fn a_write_interrupted_before_its_outcome_was_recorded_stops_the_resume() {
    // The one gap two-phase recording exists to close: the process died
    // after the write ran and before the journal learned what it returned.
    // Neither repeating it nor skipping it is honest, so the run stops.
    let scratch = Scratch::new("write-unknown");
    let path = scratch.run_path("r1");
    let out = scratch.0.join("out.txt");
    let src = WRITE_PROGRAM.replace("OUT", &ko_path(&out));

    let mut run = Run::new("r1".into(), "test.ko".into());
    run.entries.push(journal::Entry {
        scope: Scope::root(),
        seq: 0,
        site: "test.ko:4#fs.append".into(),
        effect: Effect::Attempted {
            name: "fs.append".into(),
        },
    });

    let (_, _, err) = run_durable(&src, run, path);
    let message = err.expect("an interrupted write must stop the resume");
    assert!(
        message.contains("whether it finished is unknown"),
        "the error should say what is unknown: {message}"
    );
    assert!(
        !out.exists(),
        "the interrupted write must not be attempted again"
    );
}
