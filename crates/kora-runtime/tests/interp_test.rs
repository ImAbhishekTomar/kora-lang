//! End-to-end interpreter tests: parse real Kora source, run it, check output.

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

fn run_err(src: &str) -> String {
    let program = parse(src).expect("should parse");
    let mut interp = Interpreter::new();
    match interp.run(&program) {
        Err(e) => e.message,
        Ok(_) => panic!("expected runtime error, program succeeded"),
    }
}

#[test]
fn hello_world() {
    assert_eq!(run("print(\"hello, world\")\n"), vec!["hello, world"]);
}

#[test]
fn arithmetic_precedence() {
    assert_eq!(run("print(1 + 2 * 3)\n"), vec!["7"]);
    assert_eq!(run("print((1 + 2) * 3)\n"), vec!["9"]);
    assert_eq!(run("print(7 // 2)\n"), vec!["3"]);
    assert_eq!(run("print(7 % 2)\n"), vec!["1"]);
    assert_eq!(run("print(2 ** 10)\n"), vec!["1024"]);
    assert_eq!(run("print(1 / 2)\n"), vec!["0.5"]);
}

#[test]
fn variables_and_fstrings() {
    let out = run("name = \"kora\"\nversion = 1\nprint(f\"{name} v{version}\")\n");
    assert_eq!(out, vec!["kora v1"]);
}

#[test]
fn if_elif_else() {
    let src = "\
x = 15
if x > 20:
    print(\"big\")
elif x > 10:
    print(\"medium\")
else:
    print(\"small\")
";
    assert_eq!(run(src), vec!["medium"]);
}

#[test]
fn while_loop_with_break_continue() {
    let src = "\
i = 0
while True:
    i += 1
    if i == 3:
        continue
    if i > 5:
        break
    print(i)
";
    assert_eq!(run(src), vec!["1", "2", "4", "5"]);
}

#[test]
fn for_over_range_and_list() {
    assert_eq!(
        run("for i in range(3):\n    print(i)\n"),
        vec!["0", "1", "2"]
    );
    assert_eq!(
        run("for x in [\"a\", \"b\"]:\n    print(x)\n"),
        vec!["a", "b"]
    );
}

#[test]
fn functions_and_recursion() {
    let src = "\
def fib(n: int) -> int:
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)

print(fib(10))
";
    assert_eq!(run(src), vec!["55"]);
}

#[test]
fn main_is_called_automatically() {
    let src = "\
def main():
    print(\"from main\")
";
    assert_eq!(run(src), vec!["from main"]);
}

#[test]
fn type_def_and_construction() {
    let src = "\
type Point:
    x: int
    y: int

p = Point(3, 4)
print(p.x + p.y)
print(p)
";
    let out = run(src);
    assert_eq!(out[0], "7");
    assert!(out[1].starts_with("Point("));
}

#[test]
fn field_assignment() {
    let src = "\
type Counter:
    n: int

c = Counter(0)
c.n = c.n + 5
print(c.n)
";
    assert_eq!(run(src), vec!["5"]);
}

#[test]
fn field_metadata_validates_construction_and_assignment() {
    let src = "\
type Expense:
    merchant: str
        description: \"Exactly three uppercase characters\"
        pattern: \"^[A-Z]{3}$\"

e = Expense(\"ABC\")
e.merchant = \"XYZ\"
print(e.merchant)
";
    assert_eq!(run(src), vec!["XYZ"]);
    assert!(
        run_err("type E:\n    code: str @pattern(\"^[A-Z]{3}$\")\ne = E(\"bad\")\n")
            .contains("must match pattern")
    );
    assert!(run_err(
        "type E:\n    code: str @pattern(\"^[A-Z]{3}$\")\ne = E(\"ABC\")\ne.code = \"bad\"\n"
    )
    .contains("must match pattern"));
}

#[test]
fn lists_dicts_slices() {
    let src = "\
xs = [10, 20, 30, 40]
print(xs[0])
print(xs[-1])
print(xs[1:3])
d = {\"a\": 1, \"b\": 2}
print(d[\"a\"])
print(len(xs))
print(\"a\" in d)
print(20 in xs)
";
    assert_eq!(
        run(src),
        vec!["10", "40", "[20, 30]", "1", "4", "True", "True"]
    );
}

#[test]
fn builtins() {
    assert_eq!(run("print(sum([1, 2, 3]))\n"), vec!["6"]);
    assert_eq!(run("print(min([3, 1, 2]))\n"), vec!["1"]);
    assert_eq!(run("print(max(3, 7))\n"), vec!["7"]);
    assert_eq!(run("print(sorted([3, 1, 2]))\n"), vec!["[1, 2, 3]"]);
    assert_eq!(run("print(str(42) + \"!\")\n"), vec!["42!"]);
    assert_eq!(run("print(int(\"7\") + 1)\n"), vec!["8"]);
    assert_eq!(run("xs = [1]\nappend(xs, 2)\nprint(xs)\n"), vec!["[1, 2]"]);
}

#[test]
fn string_ops() {
    assert_eq!(run("print(\"ab\" + \"cd\")\n"), vec!["abcd"]);
    assert_eq!(run("print(\"ab\" * 3)\n"), vec!["ababab"]);
    assert_eq!(run("print(\"hello\"[1])\n"), vec!["e"]);
    assert_eq!(run("print(\"hello\"[1:3])\n"), vec!["el"]);
    assert_eq!(run("print(\"ell\" in \"hello\")\n"), vec!["True"]);
}

#[test]
fn typed_annotation_enforced() {
    let msg = run_err("x: int = \"not an int\"\n");
    assert!(msg.contains("expected `int`"), "got: {msg}");
}

#[test]
fn param_annotation_enforced() {
    let msg = run_err("def f(n: int):\n    return n\nf(\"oops\")\n");
    assert!(msg.contains("argument `n`"), "got: {msg}");
}

#[test]
fn undefined_name_suggests() {
    let program = parse("count = 1\nprint(cont)\n").unwrap();
    let mut interp = Interpreter::new();
    let err = interp.run(&program).unwrap_err();
    assert!(err.message.contains("not defined"));
    assert!(
        err.hint.as_deref().unwrap_or("").contains("count"),
        "hint should suggest `count`, got {:?}",
        err.hint
    );
}

#[test]
fn division_by_zero() {
    assert!(run_err("print(1 / 0)\n").contains("division by zero"));
}

#[test]
fn index_out_of_range() {
    assert!(run_err("xs = [1]\nprint(xs[5])\n").contains("out of range"));
}

#[test]
fn no_field_hint() {
    let program = parse("type P:\n    x: int\np = P(1)\nprint(p.y)\n").unwrap();
    let mut interp = Interpreter::new();
    let err = interp.run(&program).unwrap_err();
    assert!(err.message.contains("no field `y`"));
    assert!(err.hint.as_deref().unwrap_or("").contains("x"));
}

#[test]
fn short_circuit() {
    // Right side would explode; short-circuit must prevent evaluation.
    assert_eq!(run("print(False and (1 / 0))\n"), vec!["False"]);
    assert_eq!(run("print(True or (1 / 0))\n"), vec!["True"]);
}

#[test]
fn wrong_constructor_arity_hint() {
    let program = parse("type P:\n    x: int\n    y: int\np = P(1)\n").unwrap();
    let mut interp = Interpreter::new();
    let err = interp.run(&program).unwrap_err();
    assert!(err.message.contains("takes 2 field(s)"));
    assert!(err.hint.as_deref().unwrap_or("").contains("x, y"));
}

// --- `case` guards and `else` bindings ---
//
// Both exist for the same reason: chaining model calls otherwise nests one
// level per call, and the happy path ends up buried at the bottom of a
// pyramid. These tests pin the behaviour that makes the flat form safe.

#[test]
fn guard_selects_a_later_arm_with_the_same_pattern() {
    let src = r#"
match Ok(3):
    case Ok(v) if v > 10:
        print("big")
    case Ok(v):
        print(f"small {v}")
"#;
    assert_eq!(run(src), vec!["small 3"]);
}

#[test]
fn guard_reads_the_patterns_binders() {
    let src = r#"
match Ok(42):
    case Ok(v) if v == 42:
        print("exactly")
    case _:
        print("no")
"#;
    assert_eq!(run(src), vec!["exactly"]);
}

#[test]
fn guards_are_tried_in_order() {
    let src = r#"
for n in [1, 7, 20]:
    match Ok(n):
        case Ok(v) if v > 10:
            print("big")
        case Ok(v) if v > 5:
            print("medium")
        case Ok(v):
            print("small")
"#;
    assert_eq!(run(src), vec!["small", "medium", "big"]);
}

#[test]
fn every_matching_arm_rejected_by_its_guard_says_so() {
    let src = r#"
match Ok(1):
    case Ok(v) if v > 10:
        print("big")
"#;
    let err = run_err(src);
    assert!(
        err.contains("rejected by its guard"),
        "an arm refused by a guard should not be reported as an unmatched value: {err}"
    );
}

#[test]
fn a_guard_cannot_call_a_model_even_through_a_helper() {
    let src = r#"
type J:
    ok: bool

def sneaky(x: str) -> bool:
    r: J = analyze(x, "well?")
    match r:
        case Ok(v):
            return v.ok
        case _:
            return False

agent main():
    with mock analyze -> Ok(J(True)):
        match Ok(1):
            case Ok(v) if sneaky("hi"):
                print("spent budget in a guard")
            case _:
                print("no")
"#;
    let err = run_err(src);
    assert!(
        err.contains("guard cannot call a model"),
        "a guard reaching analyze indirectly must still be refused: {err}"
    );
}

#[test]
fn else_binding_unwraps_ok_and_takes_the_failure_path() {
    let src = r#"
def degrade(outcome, fallback: str) -> str:
    v = outcome else:
        return fallback
    return f"got {v}"

print(degrade(Ok("hi"), "fb"))
print(degrade(Uncertain("meh"), "fb"))
print(degrade(Exhausted("tokens"), "fb"))
print(degrade(Err("nope"), "fb"))
"#;
    assert_eq!(run(src), vec!["got hi", "fb", "fb", "fb"]);
}

#[test]
fn else_binding_can_name_the_reason() {
    let src = r#"
def why(outcome) -> str:
    v = outcome else (reason):
        return f"failed: {reason}"
    return f"got {v}"

print(why(Uncertain("too vague")))
print(why(Exhausted("tokens")))
"#;
    assert_eq!(run(src), vec!["failed: too vague", "failed: tokens"]);
}

#[test]
fn else_binding_can_continue_a_loop() {
    let src = r#"
total = 0
for item in [Ok(1), Uncertain("x"), Ok(2), Exhausted("t"), Ok(3)]:
    v = item else:
        continue
    total = total + v
print(total)
"#;
    assert_eq!(run(src), vec!["6"]);
}

#[test]
fn else_binding_refuses_a_value_that_is_not_an_outcome() {
    let err = run_err("v = 5 else:\n    print(\"x\")\n");
    assert!(
        err.contains("expects an outcome"),
        "a plain value must not be treated as success: {err}"
    );
}

#[test]
fn matching_a_classified_outcome_finds_the_arm_and_keeps_the_label() {
    // Reading structure through a label must not change which arm runs, and
    // must not strip the label off what the arm binds.
    let src = r#"
use fs

classified secret = Ok("password123")
match secret:
    case Ok(v):
        fs.write("/tmp/kora-should-not-exist.txt", v)
    case _:
        print("wrong arm")
"#;
    let err = run_err(src);
    assert!(
        err.contains("classified data"),
        "destructuring a classified outcome must not launder the label: {err}"
    );
}

#[test]
fn else_binding_keeps_the_label_of_a_classified_outcome() {
    let src = r#"
use fs

classified outcome = Ok("password123")
v = outcome else:
    print("no")
fs.write("/tmp/kora-should-not-exist.txt", v)
"#;
    let err = run_err(src);
    assert!(
        err.contains("classified data"),
        "unwrapping a classified outcome must not launder the label: {err}"
    );
}
