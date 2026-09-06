//! `parallel for x in xs first:` — stopping a fan-out as soon as one branch
//! answers.
//!
//! The half of cancellation a deadline does not cover. `budget: max_seconds`
//! stops a scope when time runs out; this stops one when the work is *done*,
//! which is the shape every "race these providers" and "search until you find
//! it" program wants and had no way to say.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kora_runtime::{Config, Interpreter, Journal, Run, RunStatus};
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
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\nsource:\n{src}"));
    let mut interp = Interpreter::new();
    match interp.run(&program) {
        Err(e) => e.message,
        Ok(_) => panic!("expected a runtime error, program succeeded:\n{src}"),
    }
}

// --- what it yields ---

#[test]
fn a_race_yields_one_value_not_a_list() {
    // The whole point of the modifier: `parallel for` collects, `first`
    // answers. A program racing three providers wants the answer, not a list
    // with two holes in it.
    let out = run(r#"def main():
    answer = parallel for n in [1, 2, 3] first:
        return n * 10
    print(answer)
"#);
    assert_eq!(out, vec!["10"]);
}

#[test]
fn the_winner_is_the_earliest_in_input_order_not_the_earliest_to_finish() {
    // Determinism over speed, deliberately. Two runs on the same inputs must
    // not disagree about who won purely on which core happened to be free --
    // this language replays its own runs, and an answer that depends on
    // scheduling cannot be replayed.
    let out = run(r#"def answer(n: int) -> int:
    if n < 3:
        return None
    return n

def main():
    winner = parallel for n in [1, 2, 3, 4, 5] first:
        return answer(n)
    print(winner)
"#);
    assert_eq!(out, vec!["3"], "the lowest index that produced a value");
}

#[test]
fn a_branch_that_answers_nothing_does_not_win() {
    // A body that falls off its end has not answered the question the race
    // was asking, so it must not be what stops it.
    let out = run(r#"def main():
    winner = parallel for n in [1, 2, 3] first:
        if n == 3:
            return "found"
    print(winner)
"#);
    assert_eq!(out, vec!["found"]);
}

#[test]
fn a_race_nobody_wins_is_none() {
    let out = run(r#"def main():
    winner = parallel for n in [1, 2, 3] first:
        x = n + 1
    print(winner)
"#);
    assert_eq!(out, vec!["None"]);
}

#[test]
fn racing_an_empty_list_is_none_not_an_empty_list() {
    // `parallel for` over nothing collects nothing; `first` over nothing has
    // no first. The two forms answer in their own shapes.
    let out = run(r#"def main():
    winner = parallel for n in [] first:
        return 1
    print(winner)
"#);
    assert_eq!(out, vec!["None"]);
}

// --- it really stops ---

#[test]
fn work_after_the_winner_is_never_started() {
    // The observable difference between a race and a fan-out you throw most
    // of away: the branches that never ran printed nothing, because they
    // never ran.
    // Far more items than any machine has cores, so "some never started" is
    // a property of the stop flag rather than of how fast this box is.
    let program = r#"def main():
    winner = parallel for n in range(500) first:
        print(f"ran {n}")
        return n
    print(f"winner {winner}")
"#;
    let parsed = parse(program).expect("parses");
    let mut interp = Interpreter::new();
    interp.run(&parsed).expect("runs");
    let ran = interp
        .output
        .iter()
        .filter(|l| l.starts_with("ran "))
        .count();
    assert!(
        ran < 500,
        "a race must stop starting work; all 500 branches ran: {ran}"
    );
    assert!(ran >= 1, "at least one branch has to run: {ran}");
}

#[test]
fn a_branch_already_in_flight_is_not_interrupted() {
    // The honest limit, stated in the same terms `max_seconds` states it: no
    // further work is *started*, but a branch already running finishes. A
    // test that pretended otherwise would be asserting a guarantee the
    // runtime does not make.
    let program = r#"def main():
    winner = parallel for n in [1, 2, 3, 4] first:
        print(f"start {n}")
        print(f"end {n}")
        return n
    print(f"winner {winner}")
"#;
    let parsed = parse(program).expect("parses");
    let mut interp = Interpreter::new();
    interp.run(&parsed).expect("runs");
    let starts = interp
        .output
        .iter()
        .filter(|l| l.starts_with("start "))
        .count();
    let ends = interp
        .output
        .iter()
        .filter(|l| l.starts_with("end "))
        .count();
    assert_eq!(
        starts, ends,
        "every branch that started must have finished: {:?}",
        interp.output
    );
}

// --- it is still a `parallel for` ---

#[test]
fn a_racing_branch_still_has_its_own_heap() {
    let out = run(r#"def main():
    total = 0
    winner = parallel for n in [1, 2, 3] first:
        total = total + n
        return n
    print(total)
"#);
    assert_eq!(out, vec!["0"]);
}

#[test]
fn a_branch_that_raises_still_fails_the_race() {
    // Unchanged from a plain `parallel for`: an expected failure is an
    // outcome and comes back as a value, and a raise is a bug. Stopping
    // early is not a reason to start swallowing bugs.
    let err = run_err(
        r#"def main():
    winner = parallel for n in [1] first:
        return n / 0
    print(winner)
"#,
    );
    assert!(err.contains("division by zero"), "got: {err}");
}

#[test]
fn a_declared_type_survives_the_crossing() {
    let out = run(r#"type Row:
    name: str
    score: int

def main():
    winner = parallel for n in [1, 2] first:
        return Row(f"r{n}", n * 10)
    print(f"{winner.name}={winner.score}")
"#);
    assert_eq!(out, vec!["r1=10"]);
}

// --- the plain form is untouched ---

#[test]
fn without_first_a_parallel_for_still_collects_every_branch() {
    let out = run(r#"def main():
    results = parallel for n in [1, 2, 3]:
        return n * 2
    print(results)
"#);
    assert_eq!(out, vec!["[2, 4, 6]"]);
}

#[test]
fn first_is_contextual_so_a_program_may_still_use_it_as_a_name() {
    // `stream` and `on` are contextual for the same reason: a new keyword
    // that breaks working programs is a cost the language does not have to
    // pay.
    let out = run(r#"def main():
    first = 7
    print(first)
"#);
    assert_eq!(out, vec!["7"]);
}

#[test]
fn a_function_named_first_still_works() {
    let out = run(r#"def first(xs: list) -> int:
    return xs[0]

def main():
    print(first([4, 5]))
"#);
    assert_eq!(out, vec!["4"]);
}

#[test]
fn a_race_runs_for_effect_without_a_binding() {
    // "Try these until one of them works" is a real program, and it has no
    // use for the value.
    let out = run(r#"def main():
    parallel for n in range(500) first:
        print(f"tried {n}")
        return n
"#);
    assert!(
        !out.is_empty() && out.len() < 500,
        "it should stop early and print at least once: {} lines",
        out.len()
    );
}

// --- the race is journaled, because it cannot be re-derived ---

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "kora-race-{name}-{}-{:?}",
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

fn run_durable(src: &str, run: Run, path: PathBuf) -> (Vec<String>, Run) {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut interp = Interpreter::new();
    interp.config = Config::parse(CONFIG).unwrap();
    interp.program_name = "test.ko".into();
    interp.journal = Arc::new(Mutex::new(Journal::open(run, path).unwrap()));
    interp
        .run(&program)
        .unwrap_or_else(|e| panic!("the run should not fail: {}", e.message));
    let mut j = interp.journal.lock().unwrap();
    j.finish(RunStatus::Completed).unwrap();
    let saved = j.run().clone();
    drop(j);
    (interp.output, saved)
}

/// A race whose winner depends on how the threads were scheduled: every
/// branch answers, so which ones got started before the stop is the only
/// thing that decides the result. Exactly the program a replay must not be
/// allowed to re-decide.
const RACY: &str = r#"def main():
    winner = parallel for n in [1, 2, 3, 4, 5, 6, 7, 8] first:
        return n
    print(f"winner {winner}")
"#;

/// The `race` records in a journal file, as raw lines.
///
/// Read from the file rather than from the interpreter: what a resume sees
/// is what was written down, and a test that asked the live interpreter
/// would be checking the wrong side of the boundary.
fn race_records(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .expect("a journal file")
        .lines()
        .filter(|line| line.contains("\"race\""))
        .map(str::to_string)
        .collect()
}

#[test]
fn a_resumed_race_replays_its_winner_rather_than_racing_again() {
    // The reason this effect is journaled at all, and the reason no other
    // meter is. A replay runs against a warm cache on a differently loaded
    // machine; re-running the race could pick a different branch, and a
    // durable run that answers differently the second time is not durable.
    //
    // The probe is the journal, not the output: a resumed run replays what
    // it already printed rather than reprinting it, so comparing terminal
    // output would compare a full run against a silent one.
    let scratch = Scratch::new("replay");
    let path = scratch.run_path("r1");
    let (live_output, run) =
        run_durable(RACY, Run::new("r1".into(), "test.ko".into()), path.clone());
    assert_eq!(live_output.len(), 1, "the live run prints a winner");
    let before = race_records(&path);
    assert_eq!(before.len(), 1, "one race, one record: {before:?}");

    // The resume must not fail, and must not write a second, different
    // answer over the first.
    run_durable(RACY, run, path.clone());
    assert_eq!(
        race_records(&path),
        before,
        "a resumed race must keep the winner the live run chose"
    );
}

#[test]
fn the_journal_records_which_branch_won() {
    // The index is what makes a trace readable, and it is what distinguishes
    // two branches that happened to return equal answers.
    let scratch = Scratch::new("record");
    let path = scratch.run_path("r1");
    let (output, _) = run_durable(RACY, Run::new("r1".into(), "test.ko".into()), path.clone());
    let records = race_records(&path);
    assert_eq!(records.len(), 1, "got: {records:?}");
    assert!(
        records[0].contains("\"index\""),
        "the winning branch must be named: {}",
        records[0]
    );
    assert_eq!(output.len(), 1, "one winner is printed: {output:?}");
}

#[test]
fn a_race_nobody_wins_is_journaled_too() {
    // Otherwise a resume would find the slot empty, run the race again, and
    // could answer this time -- the same divergence, arriving by the other
    // door.
    let scratch = Scratch::new("nowinner");
    let path = scratch.run_path("r1");
    let src = r#"def main():
    winner = parallel for n in [1, 2, 3] first:
        x = n
    print(f"winner {winner}")
"#;
    let (output, run) = run_durable(src, Run::new("r1".into(), "test.ko".into()), path.clone());
    assert_eq!(output, vec!["winner None"]);
    assert_eq!(
        race_records(&path).len(),
        1,
        "the empty race still took a slot"
    );
    run_durable(src, run, path.clone());
    assert_eq!(
        race_records(&path).len(),
        1,
        "and the resume did not add another"
    );
}

#[test]
fn effects_after_a_race_keep_their_positions() {
    // A race takes exactly one journal slot. If it took none, or two, every
    // effect after it would be looked up at the wrong position on resume --
    // which the journal reports as a step mismatch, so a resume that
    // completes at all is the assertion.
    let scratch = Scratch::new("positions");
    let path = scratch.run_path("r1");
    let src = r#"def main():
    winner = parallel for n in [1, 2] first:
        return n
    print(f"winner {winner}")
    print("after one")
    print("after two")
"#;
    let (live, run) = run_durable(src, Run::new("r1".into(), "test.ko".into()), path.clone());
    assert_eq!(live.len(), 3);
    // Panics inside `run_durable` if the resume hits a step mismatch.
    run_durable(src, run, path);
}
