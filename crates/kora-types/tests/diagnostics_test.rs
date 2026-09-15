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

#[test]
fn declared_shapes_and_control_flow_are_checked_before_runtime() {
    let cases = [
        (
            r#"type User:
    name: str
    age: int

def main():
    u = User("Ada")
    print(u.name)
"#,
            "expects 2 field values",
        ),
        (
            r#"type User:
    name: str

def main():
    u = User("Ada")
    print(u.nam)
"#,
            "has no field `nam`",
        ),
        (
            r#"def add(a: int, b: int) -> int:
    return a + b

def main():
    print(f"{add(1)}")
"#,
            "expects 2 arguments",
        ),
        (
            r#"def thing() -> int:
    return 1

def thing() -> int:
    return 2

def main():
    print(f"{thing()}")
"#,
            "defined more than once",
        ),
        ("return 1\n", "only valid inside a function"),
        (
            r#"def main():
    break
"#,
            "only valid inside a loop",
        ),
    ];

    for (src, expected) in cases {
        let errors = errors(src);
        assert!(
            errors.iter().any(|error| error.contains(expected)),
            "expected `{expected}` for:\n{src}\ngot: {errors:?}",
        );
    }
}

#[test]
fn declared_types_and_classified_model_flow_are_checked() {
    let wrong_return = errors(
        r#"def wrong() -> int:
    return "not an int"
"#,
    );
    assert!(wrong_return.iter().any(|error| {
        error.contains("return value") && error.contains("str") && error.contains("int")
    }));

    let unsafe_flow = errors(
        r#"type Employee:
    name: str
    classified salary: int

def main():
    employee = Employee("Ada", 100)
    result: str = analyze(employee, "summarize")
"#,
    );
    assert!(unsafe_flow
        .iter()
        .any(|error| error.contains("classified data cannot be sent")));

    let safe_flow = errors(
        r#"type Employee:
    classified salary: int

def main():
    employee = Employee(100)
    declassify employee.salary as pay for local_model:
        result: str = analyze(pay, "summarize")
"#,
    );
    assert!(safe_flow.is_empty(), "declassified binding: {safe_flow:?}");

    let non_interprocedural_flow = errors(
        r#"def public_constant(value: str) -> str:
    return "public"

def main():
    classified secret = "private"
    result: str = analyze(public_constant(secret), "summarize")
"#,
    );
    assert!(
        non_interprocedural_flow.is_empty(),
        "a caller cannot assume a function preserves its argument label: {non_interprocedural_flow:?}"
    );
}

#[test]
fn declared_assignments_and_arguments_are_checked() {
    let assignment = errors("def main():\n    count: int = \"three\"\n");
    assert!(assignment
        .iter()
        .any(|error| error.contains("assignment") && error.contains("`int`")));

    let argument = errors(
        r#"def double(value: int) -> int:
    return value * 2

def main():
    print(double("two"))
"#,
    );
    assert!(argument
        .iter()
        .any(|error| error.contains("function argument") && error.contains("`int`")));
}

#[test]
fn a_parallel_branch_return_is_not_the_enclosing_functions_return() {
    let checked = errors(
        r#"def collect() -> list[int]:
    values = parallel for n in [1, 2]:
        return n
    return values
"#,
    );
    assert!(checked.is_empty(), "parallel branch return: {checked:?}");
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

/// `break <value>` is only meaningful where there is a results list to put the
/// value in. An ordinary loop has none, so the value is refused rather than
/// evaluated and dropped -- a silently ignored value is a bug that reads as
/// working code.
#[test]
fn break_carries_a_value_only_inside_a_parallel_for() {
    let inside = errors("def main():\n    out = parallel for n in [1, 2]:\n        break n\n");
    assert!(inside.is_empty(), "{inside:?}");

    for loop_head in ["for n in [1, 2]:", "while true:"] {
        let outside = joined(&format!("def main():\n    {loop_head}\n        break 1\n"));
        assert!(
            outside.contains("only carry a value inside a `parallel for`"),
            "{loop_head}: {outside}"
        );
    }

    // A plain loop *inside* a fan-out is still a plain loop: its `break`
    // leaves that loop and never reaches the fan-out.
    let nested = joined(
        "def main():\n    out = parallel for n in [1, 2]:\n        for m in [1]:\n            break m\n",
    );
    assert!(
        nested.contains("only carry a value inside a `parallel for`"),
        "{nested}"
    );

    // Neither does a function defined inside one: the call returns before the
    // loop sees anything.
    let in_function = joined(
        "def main():\n    out = parallel for n in [1, 2]:\n        def inner():\n            break n\n        return 1\n",
    );
    assert!(
        in_function.contains("only carry a value inside a `parallel for`"),
        "{in_function}"
    );
}

// --- tool signatures a model has to be able to read ---

#[test]
fn a_tool_given_to_a_model_cannot_take_a_declared_type() {
    // The runtime refuses this when it builds the request, so without the
    // check the program is accepted, starts, spends whatever the calls before
    // it cost, and only then stops for a reason visible in the source all
    // along.
    let found = errors(
        r#"type Item:
    name: str

type Answer:
    body: str

tool pick(items: list[Item]) -> str:
    "Pick one."
    return "first"

def main():
    a: Answer = analyze("data", "pick one", tools=[pick])
    print(a)
"#,
    );
    assert_eq!(found.len(), 1, "got: {found:?}");
    assert!(
        found[0].contains("`pick` takes `items: list[Item]`, which a model cannot be given"),
        "the tool and the parameter must both be named: {found:?}"
    );
    assert!(
        found[0].contains("`list[str]`"),
        "the hint must say what is allowed: {found:?}"
    );
}

#[test]
fn a_tool_with_a_shape_no_model_reads_is_fine_until_a_model_is_given_it() {
    // A `tool` is an ordinary callable too. Refusing the declaration would
    // reject working programs that never hand it to a provider.
    assert!(
        errors(
            r#"type Item:
    name: str

tool pick(items: list[Item]) -> str:
    "Pick one."
    return "first"

def main():
    print(pick([Item("a")]))
"#
        )
        .is_empty(),
        "a tool never given to a model must not be refused"
    );
}

#[test]
fn the_allowed_tool_parameter_shapes_are_accepted() {
    assert!(
        errors(
            r#"type Answer:
    body: str

tool score(name: str, count: int, ratio: float, ok: bool, tags: list[str]) -> str:
    "Every shape a model can be handed."
    return name

def main():
    a: Answer = analyze("data", "score it", tools=[score])
    print(a)
"#
        )
        .is_empty(),
        "str, int, float, bool and list[str] are all model-representable"
    );
}
