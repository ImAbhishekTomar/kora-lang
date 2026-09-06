//! Package helpers: work a package carries out to its own process.
//!
//! The helper in these tests is a Python script, which is the point twice
//! over — it is a real process speaking the real protocol, and it is not
//! Rust, so nothing here depends on a helper being written in any particular
//! language.

use std::path::{Path, PathBuf};

use kora_runtime::{Config, Interpreter};
use kora_syntax::parse;

const KORA_TOML: &str = "[models]\ndefault = \"local:test-model\"\n";

/// A project on disk: a root program, a package beside it, and the package's
/// helper. Built per test so nothing is shared between them.
struct Project(PathBuf);

impl Project {
    fn new(name: &str) -> Project {
        let dir = std::env::temp_dir().join(format!(
            "kora-helper-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("lib/render/src")).unwrap();
        Project(dir)
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// Write the helper script and make it executable.
    fn helper(&self, body: &str) {
        let path = self.write("lib/render/helper.py", body);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// The package: a manifest naming the helper for every target, an entry
    /// file, and the root program's kora.toml granting it what it needs.
    fn package(&self, entry: &str, grants: &str, requires: &str) {
        let targets = [
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "x86_64-apple-darwin",
            "aarch64-apple-darwin",
            "x86_64-pc-windows-msvc",
            "aarch64-pc-windows-msvc",
        ];
        let mut manifest = String::from("[package]\nname = \"render\"\nversion = \"0.1.0\"\n");
        manifest.push_str(requires);
        manifest.push_str("\n[package.helper]\nprotocol = \"stdio/v1\"\ntimeout_secs = 20\n");
        for target in targets {
            manifest.push_str(&format!(
                "\n[package.helper.{target}]\npath = \"helper.py\"\n"
            ));
        }
        self.write("lib/render/kora.toml", &manifest);
        self.write("lib/render/src/lib.ko", entry);
        self.write(
            "kora.toml",
            &format!(
                "{KORA_TOML}\n[dependencies.render]\npath = \"lib/render\"\ngrants = {{ {grants} }}\n"
            ),
        );
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A helper that answers `stdio/v1`, with the body of `handle(call, args,
/// blobs)` supplied by the test.
fn helper_script(handle: &str) -> String {
    format!(
        r#"#!/usr/bin/env python3
import json, struct, sys

def read_frame():
    size = sys.stdin.buffer.read(4)
    if len(size) < 4:
        return None
    header = json.loads(sys.stdin.buffer.read(struct.unpack(">I", size)[0]))
    blobs = [sys.stdin.buffer.read(n) for n in header.get("blobs", [])]
    return header, blobs

def write_frame(payload, blobs):
    payload["blobs"] = [len(b) for b in blobs]
    encoded = json.dumps(payload).encode()
    sys.stdout.buffer.write(struct.pack(">I", len(encoded)))
    sys.stdout.buffer.write(encoded)
    for blob in blobs:
        sys.stdout.buffer.write(blob)
    sys.stdout.buffer.flush()

def handle(call, args, blobs):
{handle}

while True:
    frame = read_frame()
    if frame is None:
        break
    header, blobs = frame
    result, out = handle(header["call"], header.get("args", []), blobs)
    write_frame({{"id": header["id"], "ok": True, "result": result}}, out)
"#
    )
}

fn run(program: &Path) -> Vec<String> {
    execute(program).expect("the program failed")
}

fn run_err(program: &Path) -> String {
    match execute(program) {
        Err(message) => message,
        Ok(out) => panic!("expected an error, got {out:?}"),
    }
}

fn execute(program: &Path) -> Result<Vec<String>, String> {
    let source = std::fs::read_to_string(program).unwrap();
    let parsed = parse(&source).unwrap_or_else(|e| panic!("parse error: {e}"));

    let resolution = kora_pkg::resolve(program);

    let mut interp = Interpreter::new();
    let config = Config::parse(KORA_TOML).unwrap();
    interp.sinks = config.sinks.clone();
    interp.config = config;
    interp.packages = std::sync::Arc::new(resolution);
    interp.program_name = program.to_string_lossy().to_string();

    match interp.run(&parsed) {
        Ok(()) => Ok(interp.output),
        Err(e) => Err(e.message),
    }
}

/// The whole point: bytes out to another process, a value back.
#[test]
fn a_package_calls_its_helper() {
    let project = Project::new("call");
    project.helper(&helper_script(
        "    return {\"size\": len(blobs[0]), \"call\": call, \"dpi\": args[1]}, []",
    ));
    project.package(
        r#"
use helper
use fs

def describe(path: str) -> str:
    match fs.bytes(path):
        case Ok(document):
            match helper.render(document, 150):
                case Ok(answer):
                    return f"{answer["call"]} {answer["size"]} bytes at {answer["dpi"]}"
                case Err(why):
                    return f"failed: {why}"
        case Err(why):
            return f"unreadable: {why}"
"#,
        "helper = true, fs = true",
        "\n[package.requires]\nhelper = true\nfs = true\n",
    );
    let input = project.write("input.bin", "twelve bytes");
    let input = input.to_string_lossy().replace('\\', "\\\\");
    let program = project.write(
        "main.ko",
        &format!(
            r#"
use pkg render

def main():
    print(render.describe("{input}"))
"#
        ),
    );

    let out = run(&program);
    assert_eq!(out, vec!["render 12 bytes at 150"], "{out:?}");
}

/// A helper is another process, so an image can come back as bytes beside the
/// JSON rather than base64 inside it.
#[test]
fn a_helper_can_return_images() {
    let project = Project::new("images");
    // A one-pixel PNG, so the bytes are a real image rather than a claim.
    project.helper(&helper_script(
        r#"    import base64
    png = base64.b64decode(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
    )
    return [{"image": {"blob": 0, "source": "page 1"}}, {"image": {"blob": 1, "source": "page 2"}}], [png, png]"#,
    ));
    project.package(
        r#"
use helper

def pages() -> list:
    match helper.render():
        case Ok(images):
            return images
        case Err(why):
            return []
"#,
        "helper = true",
        "\n[package.requires]\nhelper = true\n",
    );
    let program = project.write(
        "main.ko",
        r#"
use pkg render

def main():
    pages = render.pages()
    print(len(pages))
    for page in pages:
        print(page)
"#,
    );

    let out = run(&program);
    assert_eq!(out[0], "2", "{out:?}");
    assert!(out[1].contains("image/png"), "{out:?}");
    assert!(out[1].contains("page 1"), "{out:?}");
}

/// Authority is the importer's to give. A package that was never granted
/// `helper` cannot start one, however its own manifest is written.
#[test]
fn a_package_without_the_grant_cannot_run_its_helper() {
    let project = Project::new("ungranted");
    project.helper(&helper_script("    return {\"ok\": 1}, []"));
    project.package(
        r#"
use helper

def go() -> str:
    match helper.anything():
        case Ok(_):
            return "ran"
        case Err(why):
            return why
"#,
        "fs = true",
        "",
    );
    let program = project.write(
        "main.ko",
        r#"
use pkg render

def main():
    print(render.go())
"#,
    );

    let message = run_err(&program);
    assert!(message.contains("helper"), "{message}");
}

/// A helper that stops answering must not stop the run with it.
#[test]
fn a_helper_that_hangs_is_killed() {
    let project = Project::new("hang");
    project.helper(&format!(
        "{}\n",
        helper_script("    import time\n    time.sleep(600)\n    return {}, []")
    ));
    // The manifest's own timeout, kept short so the test is not.
    let entry = r#"
use helper

def go() -> str:
    match helper.sleep():
        case Ok(_):
            return "answered"
        case Err(why):
            return why
"#;
    project.package(
        entry,
        "helper = true",
        "\n[package.requires]\nhelper = true\n",
    );
    let manifest = std::fs::read_to_string(project.0.join("lib/render/kora.toml")).unwrap();
    std::fs::write(
        project.0.join("lib/render/kora.toml"),
        manifest.replace("timeout_secs = 20", "timeout_secs = 1"),
    )
    .unwrap();

    let program = project.write(
        "main.ko",
        r#"
use pkg render

def main():
    print(render.go())
"#,
    );

    let message = run_err(&program);
    assert!(message.contains("did not answer within"), "{message}");
}

/// A helper that dies mid-call is a failure the program is told about, not a
/// crash it inherits.
#[test]
fn a_helper_that_dies_is_reported() {
    let project = Project::new("dies");
    project.helper(&helper_script("    sys.exit(1)"));
    project.package(
        r#"
use helper

def go() -> str:
    match helper.boom():
        case Ok(_):
            return "answered"
        case Err(why):
            return why
"#,
        "helper = true",
        "\n[package.requires]\nhelper = true\n",
    );
    let program = project.write(
        "main.ko",
        r#"
use pkg render

def main():
    print(render.go())
"#,
    );

    let message = run_err(&program);
    assert!(message.contains("helper"), "{message}");
}

/// `use helper` in something that declares none is a mistake about the
/// package, so it is reported at the import rather than at the first call.
#[test]
fn a_package_with_no_helper_cannot_use_one() {
    let project = Project::new("nohelper");
    project.write("kora.toml", KORA_TOML);
    let program = project.write(
        "main.ko",
        r#"
use helper

def main():
    print("unreachable")
"#,
    );

    let message = run_err(&program);
    assert!(message.contains("declares no helper"), "{message}");
}
