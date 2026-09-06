//! The diagnostics a program actually walks into.
//!
//! `interp.rs` is the largest file in the runtime and its widest untested
//! surface is not the happy path -- it is the error each wrong program gets.
//! A message is a feature: "cannot add str and int", with the fix in the
//! hint, is the difference between a language someone can learn from and one
//! they have to guess at. These pin the wording and the hint, so a refactor
//! that quietly degrades a message fails here rather than in someone's
//! terminal.

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

/// The message and the hint, joined, so a test can assert on either.
fn run_err(src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\nsource:\n{src}"));
    let mut interp = Interpreter::new();
    match interp.run(&program) {
        Err(e) => match e.hint {
            Some(hint) => format!("{}\n{hint}", e.message),
            None => e.message,
        },
        Ok(_) => panic!("expected a runtime error, program succeeded:\n{src}"),
    }
}

// --- arithmetic and concatenation ---

#[test]
fn adding_a_number_to_a_string_says_how_to_fix_it() {
    // The most common first mistake in any language. The hint carries the
    // fix, because "cannot add str and int" alone tells someone what they
    // already know.
    let err = run_err("print(\"total: \" + 3)\n");
    assert!(err.contains("cannot add str and int"), "got: {err}");
    assert!(
        err.contains("str(value)"),
        "the fix belongs in the hint: {err}"
    );
    assert!(err.contains("f-string"), "got: {err}");
}

#[test]
fn adding_two_things_that_are_not_addable_names_both() {
    let err = run_err("print([1] + 3)\n");
    assert!(err.contains("cannot add list and int"), "got: {err}");
}

#[test]
fn lists_concatenate() {
    assert_eq!(run("print([1, 2] + [3])\n"), vec!["[1, 2, 3]"]);
}

#[test]
fn int_and_float_mix_in_arithmetic() {
    assert_eq!(run("print(1 + 2.5)\n"), vec!["3.5"]);
    assert_eq!(run("print(2.5 + 1)\n"), vec!["3.5"]);
    assert_eq!(run("print(2.0 + 2.0)\n"), vec!["4.0"]);
}

#[test]
fn an_operator_that_does_not_apply_names_the_operator() {
    let err = run_err("print(\"a\" - \"b\")\n");
    assert!(
        err.contains("cannot apply `-` to str and str"),
        "the symbol, not the enum name: {err}"
    );
}

#[test]
fn a_string_multiplies_by_a_count() {
    assert_eq!(run("print(\"ab\" * 3)\n"), vec!["ababab"]);
    // A negative count is an empty string, not a panic on `as usize`.
    assert_eq!(run("print(\"ab\" * -2)\n"), vec![""]);
}

#[test]
fn every_zero_divisor_is_refused_not_just_the_first() {
    for source in ["print(1 / 0)", "print(1 // 0)", "print(1 % 0)"] {
        let err = run_err(&format!("{source}\n"));
        assert!(err.contains("division by zero"), "{source} gave: {err}");
    }
}

// --- comparison ---

#[test]
fn comparing_two_kinds_that_have_no_order_names_both_and_the_operator() {
    let err = run_err("print(1 < \"a\")\n");
    assert!(
        err.contains("cannot compare int and str with `<`"),
        "got: {err}"
    );
}

#[test]
fn numbers_of_either_kind_compare_with_each_other() {
    assert_eq!(run("print(1 < 2.5)\n"), vec!["True"]);
    assert_eq!(run("print(2.5 > 1)\n"), vec!["True"]);
    assert_eq!(run("print(1.5 <= 1.5)\n"), vec!["True"]);
    assert_eq!(run("print(2.0 >= 3.0)\n"), vec!["False"]);
}

#[test]
fn strings_compare_lexically() {
    assert_eq!(run("print(\"a\" < \"b\")\n"), vec!["True"]);
}

// --- `in` ---

#[test]
fn in_works_across_the_three_containers_that_have_a_membership_question() {
    assert_eq!(run("print(2 in [1, 2])\n"), vec!["True"]);
    assert_eq!(run("print(\"b\" in {\"b\": 1})\n"), vec!["True"]);
    assert_eq!(run("print(\"ell\" in \"hello\")\n"), vec!["True"]);
    assert_eq!(run("print(3 not in [1, 2])\n"), vec!["True"]);
}

#[test]
fn in_on_a_string_needs_a_string_on_the_left() {
    // Not `false`: asking whether a number is "in" a string is a mistake,
    // and answering it silently hides the mistake.
    let err = run_err("print(3 in \"abc\")\n");
    assert!(
        err.contains("`in` on a string needs a string on the left"),
        "got: {err}"
    );
}

#[test]
fn in_needs_a_container_at_all() {
    let err = run_err("print(3 in 5)\n");
    assert!(
        err.contains("`in` needs a list, dict, or str, got int"),
        "got: {err}"
    );
}

// --- iteration ---

#[test]
fn a_loop_over_something_that_is_not_a_sequence_says_what_can_be_looped() {
    let err = run_err("for x in 3:\n    print(x)\n");
    assert!(err.contains("cannot loop over int"), "got: {err}");
    assert!(
        err.contains("list, string, dict, or range"),
        "the hint must list what works: {err}"
    );
}

#[test]
fn a_loop_over_a_string_yields_its_characters() {
    assert_eq!(run("for c in \"ab\":\n    print(c)\n"), vec!["a", "b"]);
}

#[test]
fn a_loop_over_a_dict_yields_its_keys() {
    let out = run("for k in {\"a\": 1}:\n    print(k)\n");
    assert_eq!(out, vec!["a"]);
}

// --- calling ---

#[test]
fn calling_something_that_is_not_a_function_names_its_type() {
    let err = run_err("x = 3\nx()\n");
    assert!(err.contains("int is not callable"), "got: {err}");
}

// --- the `tools=` argument, checked before any model is reached ---

#[test]
fn tools_must_be_a_list() {
    // Checked before the request is built, so this needs no provider: the
    // error a user gets for `tools=lookup` should not depend on whether a
    // model happens to be reachable.
    let err = run_err(
        r#"tool lookup(id: str) -> str:
    return "x"

def main():
    answer: str = analyze("rows", "hi", tools=lookup)
    print(answer)
"#,
    );
    assert!(err.contains("tools must be a list"), "got: {err}");
    assert!(
        err.contains("tools=[lookup_customer]"),
        "the hint shows the shape: {err}"
    );
}

#[test]
fn a_tool_list_holding_something_that_is_not_a_tool_is_refused() {
    let err = run_err(
        r#"def main():
    answer: str = analyze("rows", "hi", tools=[3])
    print(answer)
"#,
    );
    assert!(err.contains("expected a tool, got int"), "got: {err}");
}

#[test]
fn a_plain_def_in_a_tool_list_says_which_keyword_to_use() {
    let err = run_err(
        r#"def lookup(id: str) -> str:
    return "x"

def main():
    answer: str = analyze("rows", "hi", tools=[lookup])
    print(answer)
"#,
    );
    assert!(
        err.contains("tool lookup(...)") && err.contains("agent lookup(...)"),
        "the hint must name both keywords: {err}"
    );
}

// --- constructing ---

#[test]
fn a_type_that_does_not_exist_is_named() {
    let err = run_err("def main():\n    x = Missing(1)\n    print(x)\n");
    assert!(
        err.contains("no type named `Missing`") || err.contains("Missing"),
        "got: {err}"
    );
}
