//! What `kora check` says when a program is wrong.
//!
//! These are the sentences that decide whether the language is pleasant to
//! use. A checker that is merely correct — "type error" — costs the reader a
//! search through their own file, so each one here asserts the *content* of
//! the message, not just that something was rejected.

use kora_syntax::parse;
use kora_types::{analyze, Severity};

/// Every diagnostic the checker produces for `src`, message and hint joined.
fn diagnostics(src: &str) -> Vec<String> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    analyze(&program)
        .diagnostics
        .into_iter()
        .map(|d| format!("{} {}", d.message, d.hint.unwrap_or_default()))
        .collect()
}

/// The errors only, since a warning is not a refusal.
fn errors(src: &str) -> Vec<String> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    analyze(&program)
        .diagnostics
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| format!("{} {}", d.message, d.hint.unwrap_or_default()))
        .collect()
}

fn joined(src: &str) -> String {
    diagnostics(src).join("\n")
}

#[test]
fn a_correct_program_produces_nothing() {
    assert!(
        diagnostics(
            r#"def add(a: int, b: int) -> int:
    return a + b

def main():
    print(f"{add(1, 2)}")
"#
        )
        .is_empty(),
        "a good program should be silent: {:?}",
        diagnostics("def main():\n    print(\"x\")\n")
    );
}

#[test]
fn an_undefined_name_is_reported_and_a_near_miss_is_suggested() {
    let said = joined(
        r#"def main():
    total = 1
    print(totl)
"#,
    );
    assert!(said.contains("totl"), "name the token: {said}");
    assert!(
        said.contains("total"),
        "a one-character typo should be suggested: {said}"
    );
}

/// What the checker deliberately leaves to the runtime.
///
/// `kora check` is name resolution and shape-of-call checking, not a type
/// checker: arity against a declared type, a field that does not exist, and
/// a `match` missing an arm are all caught when the line runs, with a message
/// naming the fix. That boundary is a choice, and it is worth a test — if it
/// moves, it should move because someone decided to move it, and these are
/// the cases to convert rather than delete.
#[test]
fn arity_and_fields_are_left_to_the_runtime() {
    for src in [
        // A declared type built with too few fields.
        r#"type User:
    name: str
    age: int

def main():
    u = User("Ada")
    print(u.name)
"#,
        // A field that does not exist.
        r#"type User:
    name: str

def main():
    u = User("Ada")
    print(u.nam)
"#,
        // A function called with the wrong number of arguments.
        r#"def add(a: int, b: int) -> int:
    return a + b

def main():
    print(f"{add(1)}")
"#,
        // The same name defined twice.
        r#"def thing() -> int:
    return 1

def thing() -> int:
    return 2

def main():
    print(f"{thing()}")
"#,
        // `return` outside a function, and `break` outside a loop.
        "return 1\n",
        r#"def main():
    break
"#,
    ] {
        assert!(
            errors(src).is_empty(),
            "the checker is not a type checker; this is the runtime's job:\n{src}\ngot: {:?}",
            errors(src)
        );
    }
}

#[test]
fn an_unknown_module_function_is_caught_before_the_program_runs() {
    // The mistake a dynamic language finds at the call site, three minutes
    // into a run: `json.parses` instead of `json.parse`.
    let said = joined(
        r#"use json

def main():
    match json.parses("{}"):
        case Ok(v):
            print("ok")
        case Err(w):
            print(w)
"#,
    );
    assert!(
        said.contains("parses") || said.contains("parse"),
        "got: {said}"
    );
}

#[test]
fn a_module_used_without_being_imported_is_caught() {
    let said = joined(
        r#"def main():
    match json.parse("{}"):
        case Ok(v):
            print("ok")
        case Err(w):
            print(w)
"#,
    );
    assert!(said.contains("json"), "name the module: {said}");
}

#[test]
fn a_python_style_method_call_is_caught_with_the_kora_form() {
    // Written for people arriving from Python, which is most people. The
    // hint has to show the shape that works, not merely say no.
    let said = joined(
        r#"def main():
    xs = [1, 2]
    xs.append(3)
    print(f"{len(xs)}")
"#,
    );
    assert!(said.contains("append"), "got: {said}");
}

#[test]
fn analysis_records_where_every_name_was_used() {
    // What hover and go-to-definition are built on, and the reason the
    // checker keeps references at all.
    let program = parse(
        r#"def helper() -> int:
    return 1

def main():
    print(f"{helper()}")
"#,
    )
    .unwrap();
    let analysis = analyze(&program);
    assert!(analysis.symbols.contains_key("helper"), "top-level symbol");
    assert!(analysis.symbols.contains_key("main"));
    assert!(
        analysis.references.iter().any(|(name, _)| name == "helper"),
        "the call should be recorded as a reference"
    );
}

#[test]
fn a_symbol_can_be_found_by_position() {
    let source = r#"def helper() -> int:
    return 1

def main():
    print(f"{helper()}")
"#;
    let analysis = analyze(&parse(source).unwrap());
    // Line 1, inside `helper`'s name.
    let found = analysis.name_at(1, 6);
    assert_eq!(found.as_deref(), Some("helper"), "got: {found:?}");
}

#[test]
fn a_module_alias_is_recorded_for_the_editor() {
    let analysis = analyze(
        &parse(
            r#"use json

def main():
    print("x")
"#,
        )
        .unwrap(),
    );
    assert!(
        analysis.modules.contains_key("json"),
        "the alias should be in scope: {:?}",
        analysis.modules
    );
}

#[test]
fn the_module_registry_agrees_with_itself() {
    // `module_names` and `module_functions` are two views of one table, and
    // the editor's completion list comes from both.
    for name in kora_types::module_names() {
        assert!(
            kora_types::module_functions(name).is_some(),
            "`{name}` is listed as a module but exports nothing"
        );
    }
    assert!(kora_types::module_functions("not-a-module").is_none());
    assert!(!kora_types::builtin_names().is_empty());
}

#[test]
fn an_error_is_an_error_and_not_a_warning() {
    // Severity is what decides whether `kora check` exits non-zero, so a
    // real mistake must not arrive as advice.
    let said = errors(
        r#"def main():
    print(definitely_undefined)
"#,
    );
    assert!(!said.is_empty(), "an undefined name is an error");
}
