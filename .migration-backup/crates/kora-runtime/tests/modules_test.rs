//! File modules: `use "./lib.ko" as lib`.
//!
//! What these pin down is separation. Two files may bind the same name to
//! different things, and a function must keep reading the file it was written
//! in no matter who imports it — otherwise importing a module could silently
//! change what its code means.

use kora_runtime::{Config, Interpreter};
use kora_syntax::parse;
use std::path::PathBuf;

/// A scratch directory unique to each test, cleaned up on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("kora-modules-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// Write a file, creating any directories its path names.
    fn write(&self, name: &str, source: &str) -> PathBuf {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, source).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

const CONFIG: &str = "[models]\ndefault = \"local:test-model\"\n";

/// Run the file at `path`, returning its printed output or the error message.
fn run(path: &PathBuf) -> Result<Vec<String>, String> {
    let source = std::fs::read_to_string(path).unwrap();
    let program = parse(&source).map_err(|e| e.message)?;
    let mut interp = Interpreter::new();
    interp.config = Config::parse(CONFIG).unwrap();
    interp.program_name = path.to_string_lossy().to_string();
    interp.run(&program).map_err(|e| e.message)?;
    Ok(interp.output)
}

#[test]
fn an_imported_function_is_callable_through_its_alias() {
    let scratch = Scratch::new("call");
    scratch.write(
        "lib/tax.ko",
        "def with_tax(amount: float) -> float:\n    return amount * 1.2\n",
    );
    let main = scratch.write(
        "main.ko",
        "use \"./lib/tax.ko\" as tax\n\ndef main():\n    print(tax.with_tax(100.0))\n",
    );
    assert_eq!(run(&main).unwrap(), vec!["120.0"]);
}

#[test]
fn each_file_keeps_its_own_top_level_names() {
    let scratch = Scratch::new("isolation");
    scratch.write(
        "lib.ko",
        "RATE = 0.2\n\ndef rate() -> float:\n    return RATE\n",
    );
    let main = scratch.write(
        "main.ko",
        "use \"./lib.ko\" as lib\n\nRATE = 99.0\n\ndef main():\n    print(lib.rate())\n    print(RATE)\n",
    );
    // The imported function reads its own file's RATE, not the caller's.
    assert_eq!(run(&main).unwrap(), vec!["0.2", "99.0"]);
}

#[test]
fn a_type_declared_in_an_imported_file_can_be_constructed() {
    let scratch = Scratch::new("types");
    scratch.write(
        "lib.ko",
        "type Money:\n    amount: float\n    currency: str\n\ndef label(m: Money) -> str:\n    return f\"{m.amount} {m.currency}\"\n",
    );
    let main = scratch.write(
        "main.ko",
        "use \"./lib.ko\" as lib\n\ndef main():\n    m = lib.Money(12.5, \"USD\")\n    print(lib.label(m))\n",
    );
    assert_eq!(run(&main).unwrap(), vec!["12.5 USD"]);
}

#[test]
fn a_name_the_module_does_not_define_is_an_error_that_lists_what_it_does() {
    let scratch = Scratch::new("missing-name");
    scratch.write("lib.ko", "def one() -> int:\n    return 1\n");
    let main = scratch.write(
        "main.ko",
        "use \"./lib.ko\" as lib\n\ndef main():\n    print(lib.two())\n",
    );
    let error = run(&main).unwrap_err();
    assert!(error.contains("`lib` has no name `two`"), "{error}");
}

#[test]
fn an_unreadable_import_names_the_path_it_looked_for() {
    let scratch = Scratch::new("missing-file");
    let main = scratch.write(
        "main.ko",
        "use \"./nope.ko\" as nope\n\ndef main():\n    pass\n",
    );
    let error = run(&main).unwrap_err();
    assert!(error.contains("cannot read `./nope.ko`"), "{error}");
}

#[test]
fn a_path_that_is_not_a_kora_file_is_refused() {
    let scratch = Scratch::new("not-kora");
    scratch.write("lib.txt", "not code\n");
    let main = scratch.write(
        "main.ko",
        "use \"./lib.txt\" as lib\n\ndef main():\n    pass\n",
    );
    let error = run(&main).unwrap_err();
    assert!(error.contains("is not a Kora file"), "{error}");
}

#[test]
fn an_import_cycle_is_reported_rather_than_recursing() {
    let scratch = Scratch::new("cycle");
    scratch.write(
        "a.ko",
        "use \"./b.ko\" as b\n\ndef go() -> int:\n    return 1\n",
    );
    scratch.write(
        "b.ko",
        "use \"./a.ko\" as a\n\ndef go() -> int:\n    return 2\n",
    );
    let main = scratch.write(
        "main.ko",
        "use \"./a.ko\" as a\n\ndef main():\n    print(a.go())\n",
    );
    let error = run(&main).unwrap_err();
    assert!(error.contains("import cycle"), "{error}");
}

#[test]
fn a_module_imported_twice_runs_its_top_level_once() {
    let scratch = Scratch::new("diamond");
    scratch.write(
        "shared.ko",
        "print(\"loading shared\")\n\ndef go() -> int:\n    return 7\n",
    );
    scratch.write(
        "left.ko",
        "use \"./shared.ko\" as s\n\ndef go() -> int:\n    return s.go()\n",
    );
    scratch.write(
        "right.ko",
        "use \"./shared.ko\" as s\n\ndef go() -> int:\n    return s.go()\n",
    );
    let main = scratch.write(
        "main.ko",
        "use \"./left.ko\" as left\nuse \"./right.ko\" as right\n\ndef main():\n    print(left.go() + right.go())\n",
    );
    assert_eq!(run(&main).unwrap(), vec!["loading shared", "14"]);
}

#[test]
fn imports_resolve_relative_to_the_file_that_writes_them() {
    let scratch = Scratch::new("relative");
    scratch.write("lib/inner.ko", "def go() -> int:\n    return 3\n");
    // `deep.ko` sits in lib/, so its import is a bare sibling path.
    scratch.write(
        "lib/deep.ko",
        "use \"./inner.ko\" as inner\n\ndef go() -> int:\n    return inner.go()\n",
    );
    let main = scratch.write(
        "main.ko",
        "use \"./lib/deep.ko\" as deep\n\ndef main():\n    print(deep.go())\n",
    );
    assert_eq!(run(&main).unwrap(), vec!["3"]);
}

#[test]
fn an_error_inside_an_imported_file_is_reported_against_that_file() {
    let scratch = Scratch::new("blame");
    let lib = scratch.write(
        "lib.ko",
        "def go() -> int:\n    xs = [1]\n    return xs[9]\n",
    );
    let main = scratch.write(
        "main.ko",
        "use \"./lib.ko\" as lib\n\ndef main():\n    print(lib.go())\n",
    );
    let source = std::fs::read_to_string(&main).unwrap();
    let program = parse(&source).unwrap();
    let mut interp = Interpreter::new();
    interp.config = Config::parse(CONFIG).unwrap();
    interp.program_name = main.to_string_lossy().to_string();
    let error = interp.run(&program).unwrap_err();

    assert_eq!(error.file.as_deref(), Some(lib.to_string_lossy().as_ref()));
    assert_eq!(error.span.line, 3);
    // Rendering reads the imported file, so the quoted line is the real one.
    let rendered = error.render(&source, &main.to_string_lossy());
    assert!(rendered.contains("return xs[9]"), "{rendered}");
}

#[test]
fn imported_functions_work_inside_a_parallel_body() {
    let scratch = Scratch::new("parallel");
    scratch.write(
        "lib.ko",
        "FACTOR = 2\n\ndef scale(n: int) -> int:\n    return n * FACTOR\n",
    );
    let main = scratch.write(
        "main.ko",
        "use \"./lib.ko\" as lib\n\ndef main():\n    out = parallel for n in [1, 2, 3]:\n        return lib.scale(n)\n    print(out)\n",
    );
    assert_eq!(run(&main).unwrap(), vec!["[2, 4, 6]"]);
}

#[test]
fn a_type_declared_twice_with_different_fields_is_an_error() {
    let scratch = Scratch::new("type-clash");
    scratch.write("lib.ko", "type Point:\n    x: int\n");
    let main = scratch.write(
        "main.ko",
        "use \"./lib.ko\" as lib\n\ntype Point:\n    x: int\n    y: int\n\ndef main():\n    pass\n",
    );
    let error = run(&main).unwrap_err();
    assert!(error.contains("declared twice"), "{error}");
}
