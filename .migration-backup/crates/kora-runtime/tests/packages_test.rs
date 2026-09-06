//! Packages: `use pkg receipts as r`.
//!
//! What these pin down is the boundary. A package's dependencies, and its
//! type names, belong to it — not to whoever imported it. Without that, two
//! dependencies declaring `Config` would be a hard error the consumer could
//! not fix, since it owns neither of them.

use kora_runtime::{Config, Interpreter};
use kora_syntax::parse;
use std::path::PathBuf;

/// A scratch project unique to each test, cleaned up on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("kora-packages-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

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

fn run(path: &PathBuf) -> Result<Vec<String>, String> {
    let source = std::fs::read_to_string(path).unwrap();
    let program = parse(&source).map_err(|e| e.message)?;
    let mut interp = Interpreter::new();
    interp.config = Config::parse(CONFIG).unwrap();
    interp.packages = std::sync::Arc::new(kora_pkg::resolve(path));
    interp.program_name = path.to_string_lossy().to_string();
    interp.run(&program).map_err(|e| e.message)?;
    Ok(interp.output)
}

#[test]
fn a_package_function_is_callable_through_its_alias() {
    let scratch = Scratch::new("call");
    scratch.write(
        "kora.toml",
        "[dependencies]\ngreet = { path = \"./greet\" }\n",
    );
    scratch.write("greet/kora.toml", "[package]\nname = \"greet\"\n");
    scratch.write(
        "greet/src/lib.ko",
        "def hello(name: str) -> str:\n    return f\"hi, {name}\"\n",
    );
    let main = scratch.write(
        "main.ko",
        "use pkg greet as g\n\ndef main():\n    print(g.hello(\"Ada\"))\n",
    );
    assert_eq!(run(&main).unwrap(), vec!["hi, Ada"]);
}

#[test]
fn two_packages_may_declare_the_same_type_name() {
    // The reason types are namespaced at all. Before this, the second
    // declaration was a hard error, and the consumer owned neither package
    // so could not rename either one.
    let scratch = Scratch::new("collide");
    scratch.write(
        "kora.toml",
        "[dependencies]\nleft = { path = \"./left\" }\nright = { path = \"./right\" }\n",
    );
    scratch.write("left/kora.toml", "[package]\nname = \"left\"\n");
    scratch.write(
        "left/src/lib.ko",
        "type Config:\n    host: str\n    port: int\n\ndef make() -> Config:\n    return Config(\"left.example\", 80)\n",
    );
    scratch.write("right/kora.toml", "[package]\nname = \"right\"\n");
    scratch.write(
        "right/src/lib.ko",
        "type Config:\n    label: str\n    retries: int\n\ndef make() -> Config:\n    return Config(\"right\", 3)\n",
    );
    let main = scratch.write(
        "main.ko",
        "use pkg left as l\nuse pkg right as r\n\ndef main():\n    a = l.make()\n    b = r.make()\n    print(f\"{a.host}:{a.port}\")\n    print(f\"{b.label} x{b.retries}\")\n",
    );
    assert_eq!(run(&main).unwrap(), vec!["left.example:80", "right x3"]);
}

#[test]
fn a_packages_type_does_not_satisfy_a_same_named_type_here() {
    // Same short name, different types. The hint has to say so, or the
    // message reads `expected Config, got Config`.
    let scratch = Scratch::new("mismatch");
    scratch.write(
        "kora.toml",
        "[dependencies]\nleft = { path = \"./left\" }\n",
    );
    scratch.write("left/kora.toml", "[package]\nname = \"left\"\n");
    scratch.write(
        "left/src/lib.ko",
        "type Config:\n    host: str\n\ndef make() -> Config:\n    return Config(\"x\")\n",
    );
    let main = scratch.write(
        "main.ko",
        "use pkg left as l\n\ntype Config:\n    mine: str\n\ndef main():\n    wrong: Config = l.make()\n",
    );
    let err = run(&main).unwrap_err();
    assert!(err.contains("expected `Config`"), "{err}");
}

#[test]
fn types_are_still_shared_across_the_files_of_one_package() {
    // Namespacing is per package, not per file: a program split across files
    // behaves exactly as it did before packages existed.
    let scratch = Scratch::new("shared");
    scratch.write(
        "lib.ko",
        "type Money:\n    amount: float\n\ndef make() -> Money:\n    return Money(1.5)\n",
    );
    let main = scratch.write(
        "main.ko",
        "use \"./lib.ko\" as lib\n\ndef main():\n    m: Money = lib.make()\n    print(m.amount)\n",
    );
    assert_eq!(run(&main).unwrap(), vec!["1.5"]);
}

#[test]
fn a_package_resolves_its_own_dependencies_not_the_importers() {
    // `helper` is declared only by `receipts`. The program can reach it
    // through receipts and cannot name it directly.
    let scratch = Scratch::new("own-deps");
    scratch.write(
        "kora.toml",
        "[dependencies]\nreceipts = { path = \"./receipts\" }\n",
    );
    scratch.write(
        "receipts/kora.toml",
        "[package]\nname = \"receipts\"\n\n[dependencies]\nhelper = { path = \"../helper\" }\n",
    );
    scratch.write(
        "receipts/src/lib.ko",
        "use pkg helper as h\n\ndef total(n: int) -> int:\n    return h.twice(n)\n",
    );
    scratch.write("helper/kora.toml", "[package]\nname = \"helper\"\n");
    scratch.write(
        "helper/src/lib.ko",
        "def twice(n: int) -> int:\n    return n * 2\n",
    );

    let main = scratch.write(
        "main.ko",
        "use pkg receipts as r\n\ndef main():\n    print(r.total(21))\n",
    );
    assert_eq!(run(&main).unwrap(), vec!["42"]);

    let direct = scratch.write("direct.ko", "use pkg helper as h\n");
    let err = run(&direct).unwrap_err();
    assert!(err.contains("no package named `helper`"), "{err}");
}

#[test]
fn a_nested_type_resolves_in_the_package_that_declared_it() {
    // The field type `Address` is written bare inside `Site`, so it belongs
    // to the package `Site` came from, not to whoever parses the JSON.
    let scratch = Scratch::new("nested");
    scratch.write(
        "kora.toml",
        "[dependencies]\nsites = { path = \"./sites\" }\n",
    );
    scratch.write("sites/kora.toml", "[package]\nname = \"sites\"\n");
    scratch.write(
        "sites/src/lib.ko",
        "type Address:\n    city: str\n\ntype Site:\n    name: str\n    where: Address\n",
    );
    let main = scratch.write(
        "main.ko",
        "use json\nuse pkg sites as s\n\ndef main():\n    text = \"{\\\"name\\\": \\\"hq\\\", \\\"where\\\": {\\\"city\\\": \\\"Pune\\\"}}\"\n    match json.parse(text, s.Site):\n        case Ok(site):\n            print(f\"{site.name} in {site.where.city}\")\n        case Err(why):\n            print(why)\n",
    );
    assert_eq!(run(&main).unwrap(), vec!["hq in Pune"]);
}

#[test]
fn a_packages_types_survive_the_parallel_boundary() {
    let scratch = Scratch::new("parallel");
    scratch.write(
        "kora.toml",
        "[dependencies]\nleft = { path = \"./left\" }\n",
    );
    scratch.write("left/kora.toml", "[package]\nname = \"left\"\n");
    scratch.write(
        "left/src/lib.ko",
        "type Config:\n    host: str\n\ndef make() -> Config:\n    return Config(\"left.example\")\n",
    );
    let main = scratch.write(
        "main.ko",
        "use pkg left as l\n\nagent build(n: int) -> str:\n    c = l.make()\n    return c.host\n\ndef main():\n    out = parallel for i in range(2):\n        return build(i)\n    for line in out:\n        print(line)\n",
    );
    assert_eq!(run(&main).unwrap(), vec!["left.example", "left.example"]);
}

// --- capability grants ---
//
// A dependency has no ambient authority. What these pin down is that the
// confinement follows *execution*, not the call site: a package cannot shed
// it by spawning, by being called through a tool, or by handing work to a
// dependency of its own.

#[test]
fn a_dependency_cannot_reach_the_filesystem_ungranted() {
    let scratch = Scratch::new("nofs");
    scratch.write("kora.toml", "[dependencies.reader]\npath = \"./reader\"\n");
    scratch.write("reader/kora.toml", "[package]\nname = \"reader\"\n");
    scratch.write(
        "reader/src/lib.ko",
        "use fs\n\ndef peek(p: str) -> str:\n    match fs.read(p):\n        case Ok(t):\n            return t\n        case Err(w):\n            return \"no\"\n",
    );
    let main = scratch.write(
        "main.ko",
        "use pkg reader as r\n\ndef main():\n    print(r.peek(\"kora.toml\"))\n",
    );
    let err = run(&main).unwrap_err();
    assert!(err.contains("not allowed to use `fs`"), "{err}");
}

#[test]
fn a_granted_capability_lets_the_call_through() {
    let scratch = Scratch::new("withfs");
    scratch.write(
        "kora.toml",
        "[dependencies.reader]\npath = \"./reader\"\ngrants = { fs = true }\n",
    );
    scratch.write("reader/kora.toml", "[package]\nname = \"reader\"\n");
    let data = scratch.write("reader/data.txt", "contents");
    scratch.write(
        "reader/src/lib.ko",
        "use fs\n\ndef peek(p: str) -> str:\n    match fs.read(p):\n        case Ok(t):\n            return t\n        case Err(w):\n            return \"no\"\n",
    );
    // An absolute path: `fs` resolves against the process's working
    // directory, which is the crate root under `cargo test`.
    let main = scratch.write(
        "main.ko",
        &format!(
            "use pkg reader as r\n\ndef main():\n    print(r.peek({:?}))\n",
            data.display().to_string()
        ),
    );
    assert_eq!(run(&main).unwrap(), vec!["contents"]);
}

#[test]
fn confinement_survives_a_parallel_for() {
    // A worker is a fresh interpreter. If grants did not cross that
    // boundary, spawning would be the way out of the sandbox.
    let scratch = Scratch::new("parallel-escape");
    scratch.write("kora.toml", "[dependencies.reader]\npath = \"./reader\"\n");
    scratch.write("reader/kora.toml", "[package]\nname = \"reader\"\n");
    scratch.write(
        "reader/src/lib.ko",
        "use fs\n\nagent sneak(n: int) -> str:\n    match fs.read(\"kora.toml\"):\n        case Ok(t):\n            return \"LEAKED\"\n        case Err(w):\n            return \"no\"\n\ndef escape() -> str:\n    out = parallel for i in range(2):\n        return sneak(i)\n    return out[0]\n",
    );
    let main = scratch.write(
        "main.ko",
        "use pkg reader as r\n\ndef main():\n    print(r.escape())\n",
    );
    let err = run(&main).unwrap_err();
    assert!(err.contains("not allowed to use `fs`"), "{err}");
}

#[test]
fn confinement_follows_a_tool_a_model_would_call() {
    let scratch = Scratch::new("tool-escape");
    scratch.write("kora.toml", "[dependencies.reader]\npath = \"./reader\"\n");
    scratch.write("reader/kora.toml", "[package]\nname = \"reader\"\n");
    scratch.write(
        "reader/src/lib.ko",
        "use fs\n\ntool sniff(p: str) -> str:\n    \"Read a file.\"\n    match fs.read(p):\n        case Ok(t):\n            return \"LEAKED\"\n        case Err(w):\n            return \"no\"\n",
    );
    let main = scratch.write(
        "main.ko",
        "use pkg reader as r\n\ndef main():\n    print(r.sniff(\"kora.toml\"))\n",
    );
    let err = run(&main).unwrap_err();
    assert!(err.contains("not allowed to use `fs`"), "{err}");
}

#[test]
fn a_dependency_cannot_declassify_by_default() {
    // Otherwise adding a dependency becomes the way to launder a secret out
    // of a program.
    let scratch = Scratch::new("launder");
    scratch.write(
        "kora.toml",
        "[sinks]\nlocal_model = { allow = [\"classified\"] }\n\n[dependencies.reader]\npath = \"./reader\"\n",
    );
    scratch.write("reader/kora.toml", "[package]\nname = \"reader\"\n");
    scratch.write(
        "reader/src/lib.ko",
        "def launder(secret: str) -> str:\n    declassify secret as s for local_model:\n        return s\n    return \"\"\n",
    );
    let main = scratch.write(
        "main.ko",
        "use pkg reader as r\n\ndef main():\n    classified token = \"sk-secret\"\n    print(r.launder(token))\n",
    );
    let err = run(&main).unwrap_err();
    assert!(err.contains("not allowed to declassify"), "{err}");
}

#[test]
fn declassify_needs_the_sink_as_well_as_the_permission() {
    let scratch = Scratch::new("sink-grant");
    scratch.write(
        "kora.toml",
        "[sinks]\nlocal_model = { allow = [\"classified\"] }\n\n[dependencies.reader]\npath = \"./reader\"\ngrants = { declassify = true }\n",
    );
    scratch.write("reader/kora.toml", "[package]\nname = \"reader\"\n");
    scratch.write(
        "reader/src/lib.ko",
        "def launder(secret: str) -> str:\n    declassify secret as s for local_model:\n        return s\n    return \"\"\n",
    );
    let main = scratch.write(
        "main.ko",
        "use pkg reader as r\n\ndef main():\n    classified token = \"sk-secret\"\n    print(r.launder(token))\n",
    );
    let err = run(&main).unwrap_err();
    assert!(err.contains("not allowed to release data to"), "{err}");
}

#[test]
fn a_dependency_cannot_pass_on_a_capability_it_lacks() {
    // `receipts` grants `helper` the filesystem it was never given itself.
    let scratch = Scratch::new("capped-runtime");
    scratch.write(
        "kora.toml",
        "[dependencies.receipts]\npath = \"./receipts\"\n",
    );
    scratch.write(
        "receipts/kora.toml",
        "[package]\nname = \"receipts\"\n\n[dependencies.helper]\npath = \"../helper\"\ngrants = { fs = true }\n",
    );
    scratch.write(
        "receipts/src/lib.ko",
        "use pkg helper as h\n\ndef go() -> str:\n    return h.peek(\"kora.toml\")\n",
    );
    scratch.write("helper/kora.toml", "[package]\nname = \"helper\"\n");
    scratch.write(
        "helper/src/lib.ko",
        "use fs\n\ndef peek(p: str) -> str:\n    match fs.read(p):\n        case Ok(t):\n            return \"LEAKED\"\n        case Err(w):\n            return \"no\"\n",
    );
    let main = scratch.write(
        "main.ko",
        "use pkg receipts as r\n\ndef main():\n    print(r.go())\n",
    );
    let err = run(&main).unwrap_err();
    assert!(err.contains("not allowed to use `fs`"), "{err}");
}

#[test]
fn the_root_program_keeps_its_own_authority() {
    // Grants confine dependencies. A program is bounded by its own
    // kora.toml and nothing else, or every program predating them breaks.
    let scratch = Scratch::new("root-free");
    scratch.write("kora.toml", "[dependencies]\n");
    scratch.write("data.txt", "mine");
    let main = scratch.write(
        "main.ko",
        "use fs\n\ndef main():\n    match fs.read(\"data.txt\"):\n        case Ok(t):\n            print(t)\n        case Err(w):\n            print(\"no\")\n",
    );
    // Paths resolve against the process's working directory, so this only
    // needs to reach the module at all — the point is that it is not refused.
    let out = run(&main);
    assert!(
        !format!("{out:?}").contains("not allowed"),
        "the root program must not be confined: {out:?}"
    );
}
