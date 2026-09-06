//! What a `parallel for` branch yields, spends, and prints.
//!
//! `agents_test.rs` covers ordering, isolation, and the error path. These
//! cover the rest of the contract a worker has with the run that spawned it:
//! what a branch that never returns is worth, that a shared budget is really
//! one pot rather than one per thread, and that a branch's unterminated
//! output still reaches the terminal. Each is a thing a program can observe
//! and none of them was pinned.

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

#[test]
fn a_branch_that_never_returns_is_worth_none() {
    // The body's last bound value is not the branch's answer: a loop body is
    // statements, and guessing which one meant "the result" would make the
    // meaning of a branch depend on how it happened to end.
    let out = run(r#"def main():
    results = parallel for n in [1, 2]:
        x = n + 1
    print(results)
"#);
    assert_eq!(out, vec!["[None, None]"]);
}

#[test]
fn return_is_how_a_branch_yields_its_answer() {
    let out = run(r#"def double(n: int) -> int:
    return n * 2

def main():
    results = parallel for n in [1, 2, 3]:
        return double(n)
    print(results)
"#);
    assert_eq!(out, vec!["[2, 4, 6]"]);
}

#[test]
fn a_branch_that_returns_conditionally_still_lines_up_with_its_input() {
    // A `None` in the middle must stay in the middle: the results are
    // matched to inputs by position, and a filtered-out branch that shifted
    // the rest would silently misattribute every later answer.
    let out = run(r#"def main():
    results = parallel for n in [1, 2, 3, 4]:
        if n % 2 == 0:
            return n
    print(results)
"#);
    assert_eq!(out, vec!["[None, 2, None, 4]"]);
}

#[test]
fn a_branch_writing_without_a_newline_still_reaches_the_output() {
    // `write` is `print` without the newline, so a branch can end mid-line.
    // The worker's buffer is flushed when it finishes; without that the
    // characters would be dropped with the worker's interpreter.
    let out = run(r#"def main():
    parallel for n in [1]:
        write("partial")
    print("")
"#);
    assert!(
        out.iter().any(|l| l.contains("partial")),
        "an unterminated write must survive the worker: {out:?}"
    );
}

#[test]
fn branches_print_and_the_output_is_kept() {
    // Ordering across threads is not promised, but every branch's output is.
    let out = run(r#"def main():
    parallel for n in [1, 2, 3]:
        print(f"branch {n}")
"#);
    let mut sorted = out.clone();
    sorted.sort();
    assert_eq!(sorted, vec!["branch 1", "branch 2", "branch 3"]);
}

#[test]
fn an_empty_input_produces_an_empty_list_without_spawning_anything() {
    let out = run(r#"def main():
    results = parallel for n in []:
        return 1
    print(results)
"#);
    assert_eq!(out, vec!["[]"]);
}

#[test]
fn a_worker_cannot_write_back_into_the_scope_that_spawned_it() {
    // The isolation that makes the threads safe: a worker's heap is its own,
    // seeded by copy. A branch assigning to an outer name changes its own
    // copy, and the parent's value is untouched.
    let out = run(r#"def main():
    total = 0
    parallel for n in [1, 2, 3]:
        total = total + n
    print(total)
"#);
    assert_eq!(out, vec!["0"]);
}

#[test]
fn a_nested_type_constructed_in_a_branch_comes_back_whole() {
    // Values cross the thread boundary as portable copies, so a declared
    // type built inside a branch has to be rebuilt on the way out. A field
    // lost in that round trip would only show up here.
    let out = run(r#"type Row:
    name: str
    score: int

def main():
    rows = parallel for n in [1, 2]:
        return Row(f"r{n}", n * 10)
    for row in rows:
        print(f"{row.name}={row.score}")
"#);
    assert_eq!(out, vec!["r1=10", "r2=20"]);
}

#[test]
fn a_list_built_in_a_branch_survives_the_crossing() {
    let out = run(r#"def main():
    lists = parallel for n in [1, 2]:
        return [n, n + 1]
    print(lists)
"#);
    assert_eq!(out, vec!["[[1, 2], [2, 3]]"]);
}
