//! Phase 3: agents, tools, `parallel for`, and budgets.
//!
//! Nothing here touches the network.

use kora_runtime::Interpreter;
use kora_syntax::parse;

fn run(src: &str) -> Vec<String> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut interp = Interpreter::new();
    interp
        .run(&program)
        .unwrap_or_else(|e| panic!("runtime error: {}\n{src}", e.message));
    interp.output
}

fn run_err(src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut interp = Interpreter::new();
    match interp.run(&program) {
        Err(e) => e.message,
        Ok(_) => panic!("expected an error, program succeeded"),
    }
}

// --- parallel for ---

#[test]
fn parallel_for_collects_in_input_order() {
    // Deterministic output order is a promise: parallel runs read like
    // sequential ones even though work is interleaved across threads.
    let src = r#"def double(n: int) -> int:
    return n * 2

def main():
    results = parallel for x in [1, 2, 3, 4, 5]:
        return double(x)
    print(results)
"#;
    assert_eq!(run(src), vec!["[2, 4, 6, 8, 10]"]);
}

#[test]
fn parallel_for_without_binding_runs_for_effect() {
    let src = r#"def main():
    parallel for x in [1, 2, 3]:
        y = x + 1
    print("done")
"#;
    assert_eq!(run(src), vec!["done"]);
}

#[test]
fn parallel_for_over_empty_list() {
    let src =
        "def main():\n    results = parallel for x in []:\n        return x\n    print(results)\n";
    assert_eq!(run(src), vec!["[]"]);
}

#[test]
fn parallel_workers_have_isolated_heaps() {
    // Each worker gets a copy, so mutations cannot leak between iterations.
    let src = r#"def main():
    shared = [0]
    results = parallel for x in [1, 2, 3]:
        append(shared, x)
        return len(shared)
    print(results)
    print(len(shared))
"#;
    let out = run(src);
    // Every worker sees its own copy of length 1, then appends to reach 2.
    assert_eq!(out[0], "[2, 2, 2]");
    // The parent's list is untouched: isolation, not shared mutable state.
    assert_eq!(out[1], "1");
}

#[test]
fn parallel_body_error_surfaces() {
    let src = "def main():\n    results = parallel for x in [1, 0]:\n        return 10 / x\n";
    assert!(run_err(src).contains("division by zero"));
}

#[test]
fn parallel_for_sees_enclosing_values() {
    let src = r#"def main():
    factor = 10
    results = parallel for x in [1, 2, 3]:
        return x * factor
    print(results)
"#;
    assert_eq!(run(src), vec!["[10, 20, 30]"]);
}

// --- budgets ---

#[test]
fn budget_introspection_reports_limits() {
    let src = r#"def main():
    with budget(max_tokens = 500):
        print(tokens_remaining())
        print(tokens_spent())
        print(calls_spent())
"#;
    assert_eq!(run(src), vec!["500", "0", "0"]);
}

#[test]
fn budget_is_unlimited_by_default() {
    // Budgets are opt-in: no declaration means no ceiling.
    let src = "def main():\n    print(tokens_remaining())\n";
    assert_eq!(run(src), vec!["None"]);
}

#[test]
fn nested_budget_tightens_never_loosens() {
    let src = r#"def main():
    with budget(max_tokens = 100):
        with budget(max_tokens = 999999):
            print(tokens_remaining())
"#;
    assert_eq!(run(src), vec!["100"], "the tightest enclosing limit wins");
}

#[test]
fn budget_scope_is_restored_on_exit() {
    let src = r#"def main():
    with budget(max_tokens = 100):
        print(tokens_remaining())
    print(tokens_remaining())
"#;
    assert_eq!(run(src), vec!["100", "None"]);
}

#[test]
fn agent_budget_applies_to_its_body() {
    let src = r#"agent worker() -> int:
    budget: max_tokens = 250
    print(tokens_remaining())
    return 1

def main():
    worker()
    print(tokens_remaining())
"#;
    assert_eq!(run(src), vec!["250", "None"]);
}

#[test]
fn budget_rejects_unknown_field() {
    let err = parse("def main():\n    with budget(max_dollars = 5):\n        pass\n").unwrap_err();
    assert!(err.message.contains("unknown budget field"));
    assert!(err.hint.as_deref().unwrap_or("").contains("max_tokens"));
}

#[test]
fn budget_rejects_non_integer_values() {
    let err =
        parse("def main():\n    with budget(max_tokens = \"lots\"):\n        pass\n").unwrap_err();
    assert!(err.message.contains("whole numbers"), "{}", err.message);
}

#[test]
fn budget_needs_at_least_one_limit() {
    let err = parse("def main():\n    with budget():\n        pass\n").unwrap_err();
    assert!(err.message.contains("at least one limit"));
}

// --- context fences ---

#[test]
fn context_fence_refuses_a_request_that_cannot_fit_without_calling_a_model() {
    let src = r#"type Answer:
    summary: str

def main():
    with context(max_input_tokens = 1):
        result: Answer = analyze("customer needs a refund", "summarize")
        match result:
            case Failed(why):
                print(why)
"#;
    let program = parse(src).unwrap();
    let mut interp = Interpreter::new();
    interp.config =
        kora_runtime::config::Config::parse("[models]\ndefault = \"local:not-running\"\n").unwrap();
    interp.run(&program).unwrap();
    assert!(
        interp.output[0].contains("before tool history"),
        "{:?}",
        interp.output
    );
}

#[test]
fn context_fence_is_lexical_and_can_nest() {
    let src = r#"def main():
    with context(max_input_tokens = 100):
        with context(reserve_output_tokens = 80):
            value = 1
    print(value)
"#;
    assert_eq!(run(src), vec!["1"]);
}

// --- agents and tools ---

#[test]
fn agents_are_callable_like_functions() {
    let src = r#"agent greet(name: str) -> str:
    return f"hello, {name}"

def main():
    print(greet("kora"))
"#;
    assert_eq!(run(src), vec!["hello, kora"]);
}

#[test]
fn tools_are_callable_directly_too() {
    let src = r#"tool add(a: int, b: int) -> int:
    "Add two numbers."
    return a + b

def main():
    print(add(2, 3))
"#;
    assert_eq!(run(src), vec!["5"]);
}

#[test]
fn docstring_is_not_executed_as_output() {
    // The docstring becomes the model-facing description, not a statement.
    let src = r#"tool noop() -> int:
    "This description goes to the model."
    return 0

def main():
    print(noop())
"#;
    assert_eq!(run(src), vec!["0"]);
}

#[test]
fn non_tool_passed_as_tool_is_rejected() {
    let src = r#"type R:
    x: int

def helper(a: int) -> int:
    return a

def main():
    r: R = analyze("data", "prompt", tools=[helper])
"#;
    let err = run_err(src);
    assert!(err.contains("is not a tool"), "got: {err}");
}

#[test]
fn tool_without_param_types_is_rejected() {
    let src = r#"type R:
    x: int

tool lookup(email) -> int:
    "Look something up."
    return 1

def main():
    r: R = analyze("data", "prompt", tools=[lookup])
"#;
    let err = run_err(src);
    assert!(err.contains("needs a type on parameter"), "got: {err}");
}
