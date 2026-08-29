//! Editor support for `use pkg`.
//!
//! A package is reached by name rather than by path, but once resolved it is
//! a file like any other. What these pin down is that the editor can see
//! through the name: completion after the alias, and go-to-definition into
//! the package's own source.

use kora_syntax::parse;
use std::path::PathBuf;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("kora-types-pkg-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn a_packages_exports_are_visible_to_the_editor() {
    let scratch = Scratch::new("exports");
    scratch.write(
        "kora.toml",
        "[dependencies]\ngreet = { path = \"./greet\" }\n",
    );
    scratch.write("greet/kora.toml", "[package]\nname = \"greet\"\n");
    scratch.write(
        "greet/src/lib.ko",
        "GREETING = \"hello\"\n\ntype Card:\n    name: str\n\ndef greet(name: str) -> str:\n    return name\n",
    );
    let main = scratch.write(
        "main.ko",
        "use pkg greet as g\n\ndef main():\n    print(g.greet(\"Ada\"))\n",
    );

    let source = std::fs::read_to_string(&main).unwrap();
    let analysis = kora_types::analyze_file(&parse(&source).unwrap(), &main);

    let module = analysis
        .file_modules
        .get("g")
        .expect("the alias resolves to a module");
    assert!(module.exports.contains_key("greet"), "{:?}", module.exports);
    assert!(module.exports.contains_key("Card"), "{:?}", module.exports);
    assert!(
        module.exports.contains_key("GREETING"),
        "top-level values are exported too: {:?}",
        module.exports
    );
    assert!(
        module.path.ends_with("lib.ko"),
        "go-to-definition needs the file: {}",
        module.path
    );
}

#[test]
fn an_unresolvable_package_is_not_reported_as_a_broken_name() {
    // Whether a dependency is declared, fetched, and verified is the
    // resolver's answer. Squiggling it in the editor would mark code that
    // `kora install` is about to make correct.
    let scratch = Scratch::new("unresolved");
    scratch.write("kora.toml", "[dependencies]\n");
    let main = scratch.write(
        "main.ko",
        "use pkg missing as m\n\ndef main():\n    print(m.anything())\n",
    );
    let source = std::fs::read_to_string(&main).unwrap();
    let analysis = kora_types::analyze_file(&parse(&source).unwrap(), &main);

    let errors: Vec<&str> = analysis
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}
