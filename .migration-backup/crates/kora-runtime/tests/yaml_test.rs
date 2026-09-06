//! `yaml` — the three defects it exists to fix, plus the shape checking it
//! shares with `json`.
//!
//! Each of the first three tests would fail against PyYAML, js-yaml, or
//! `gopkg.in/yaml.v3`, which is the reason this module is in the language
//! rather than left to a package.

use kora_runtime::{Config, Interpreter};
use kora_syntax::parse;

const CONFIG: &str = r#"
[models]
default = "local:test-model"

[sinks]
local_model = { allow = ["classified"] }
"#;

fn run(src: &str) -> Vec<String> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut interp = Interpreter::new();
    let config = Config::parse(CONFIG).unwrap();
    interp.sinks = config.sinks.clone();
    interp.config = config;
    interp
        .run(&program)
        .unwrap_or_else(|e| panic!("runtime error: {}\n{src}", e.message));
    interp.output
}

fn run_err(src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut interp = Interpreter::new();
    let config = Config::parse(CONFIG).unwrap();
    interp.sinks = config.sinks.clone();
    interp.config = config;
    match interp.run(&program) {
        Err(e) => e.message,
        Ok(_) => panic!("expected an error, program succeeded:\n{src}"),
    }
}

/// The program every "parse this text" test runs: print the value at `path`,
/// or the reason parsing stopped.
fn parse_and_get(yaml: &str, path: &str) -> Vec<String> {
    run(&format!(
        r#"use yaml
def main():
    match yaml.parse("{}"):
        case Ok(d):
            match yaml.get(d, "{path}"):
                case Ok(v):
                    print(v)
                case Err(w):
                    print(w)
        case Err(why):
            print(why)
"#,
        escape(yaml)
    ))
}

/// YAML into a Kora string literal.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

// --- the three defects ---

#[test]
fn a_duplicate_key_is_an_error_not_the_last_one() {
    // The fix: PyYAML, js-yaml and yaml.v3 all keep the last duplicate, so
    // appending `admin: true` to a file is an edit no diff of the parsed
    // result would show.
    let out = parse_and_get("user: ada\nadmin: false\nadmin: true\n", "admin");
    assert!(
        out[0].contains("`admin` is set twice"),
        "a duplicate key must be refused, got: {}",
        out[0]
    );
    assert!(
        out[0].contains("line 3"),
        "and named by line, got: {}",
        out[0]
    );
}

#[test]
fn norway_survives() {
    // The fix: YAML 1.1 reads `NO` as false, so a country code becomes a
    // boolean. Kora parses the 1.2 core schema, where it stays a string.
    assert_eq!(parse_and_get("region: NO\n", "region"), vec!["NO"]);
    assert_eq!(parse_and_get("ship: yes\n", "ship"), vec!["yes"]);
    assert_eq!(parse_and_get("debug: off\n", "debug"), vec!["off"]);
    // The two spellings that really are boolean still are.
    assert_eq!(parse_and_get("debug: true\n", "debug"), vec!["True"]);
}

#[test]
fn an_alias_bomb_is_an_error_not_an_out_of_memory_kill() {
    // The fix: the "billion laughs" file is nine lines that expand to
    // gigabytes. Every library that resolves aliases eagerly will try.
    let mut bomb = String::from("a: &a [\"x\",\"x\",\"x\",\"x\",\"x\",\"x\",\"x\",\"x\",\"x\"]\n");
    for (n, prev) in [
        ('b', 'a'),
        ('c', 'b'),
        ('d', 'c'),
        ('e', 'd'),
        ('f', 'e'),
        ('g', 'f'),
    ] {
        bomb.push_str(&format!(
            "{n}: &{n} [*{prev},*{prev},*{prev},*{prev},*{prev},*{prev},*{prev},*{prev},*{prev}]\n"
        ));
    }
    let out = parse_and_get(&bomb, "a.0");
    assert!(
        out[0].contains("expands past"),
        "the bomb must be refused, got: {}",
        out[0]
    );
}

#[test]
fn an_ordinary_anchor_still_works() {
    // The budget is a ceiling on hostile expansion, not a ban on the feature
    // every docker-compose file uses.
    let out = parse_and_get(
        "defaults: &defaults\n  retries: 3\nweb:\n  <<: *defaults\nworker: *defaults\n",
        "worker.retries",
    );
    assert_eq!(out, vec!["3"]);
}

// --- merge keys ---

#[test]
fn a_merge_key_merges_and_an_explicit_key_wins() {
    // `<<` is not in the YAML 1.2 core schema, but every docker-compose file
    // uses it, so reading it as a literal key named `<<` would be a wrong
    // answer rather than a missing feature.
    let doc = "defaults: &d\n  retries: 3\n  image: nginx\nweb:\n  <<: *d\n  retries: 5\n";
    assert_eq!(parse_and_get(doc, "web.retries"), vec!["5"]);
    assert_eq!(parse_and_get(doc, "web.image"), vec!["nginx"]);
}

#[test]
fn an_explicit_key_may_override_a_merged_one_in_either_order() {
    // The override must not be read as the duplicate-key attack: one of the
    // two spellings came from a merge, and that is the whole point of `<<`.
    let doc = "defaults: &d\n  retries: 3\nweb:\n  retries: 5\n  <<: *d\n";
    assert_eq!(parse_and_get(doc, "web.retries"), vec!["5"]);
}

#[test]
fn two_explicit_keys_are_still_a_duplicate_even_beside_a_merge() {
    let doc = "defaults: &d\n  image: nginx\nweb:\n  <<: *d\n  retries: 3\n  retries: 5\n";
    let out = parse_and_get(doc, "web.retries");
    assert!(out[0].contains("`retries` is set twice"), "got: {}", out[0]);
}

#[test]
fn a_merge_of_several_mappings_takes_the_first_that_has_the_key() {
    let doc = "a: &a\n  v: 1\nb: &b\n  v: 2\n  w: 9\nc:\n  <<: [*a, *b]\n";
    assert_eq!(parse_and_get(doc, "c.v"), vec!["1"]);
    assert_eq!(parse_and_get(doc, "c.w"), vec!["9"]);
}

#[test]
fn merging_something_that_is_not_a_mapping_is_an_error() {
    let out = parse_and_get("web:\n  <<: 3\n", "web");
    assert!(out[0].contains("`<<` merges a mapping"), "got: {}", out[0]);
}

// --- shape checking, shared with json ---

#[test]
fn a_declared_type_is_checked_and_a_mismatch_names_its_path() {
    let out = run(r#"use yaml
type Service:
    image: str
    port: int

def main():
    match yaml.parse("image: nginx\nport: eighty\n", Service):
        case Ok(s):
            print(s.image)
        case Err(why):
            print(why)
"#);
    assert_eq!(out, vec!["$.port: expected int, got str"]);
}

#[test]
fn a_declared_type_that_matches_comes_back_typed() {
    let out = run(r#"use yaml
type Service:
    image: str
    port: int

def main():
    match yaml.parse("image: nginx\nport: 80\n", Service):
        case Ok(s):
            print(s.port)
        case Err(why):
            print(why)
"#);
    assert_eq!(out, vec!["80"]);
}

#[test]
fn a_parsed_document_is_unverified() {
    // The rule every stdlib module follows: data from outside the program
    // cannot reach a sink until something narrows it.
    let err = run_err(
        r#"use fs
use yaml
def main():
    match yaml.parse("target: /etc/passwd\n"):
        case Ok(d):
            match yaml.get(d, "target"):
                case Ok(target):
                    fs.read(target)
                case Err(w):
                    print(w)
        case Err(w):
            print(w)
"#,
    );
    assert!(
        err.contains("came from outside"),
        "a parsed value must stay unverified, got: {err}"
    );
}

// --- documents ---

#[test]
fn parse_refuses_a_multi_document_file_and_names_the_count() {
    // Taking the first document silently is how half a manifest bundle gets
    // applied.
    let out = parse_and_get("a: 1\n---\nb: 2\n", "a");
    assert!(
        out[0].contains("2 documents") && out[0].contains("yaml.documents"),
        "got: {}",
        out[0]
    );
}

#[test]
fn documents_reads_them_all_and_checks_each_one() {
    let out = run(r#"use yaml
type Service:
    image: str

def main():
    match yaml.documents("image: nginx\n---\nimage: redis\n", Service):
        case Ok(all):
            for s in all:
                print(s.image)
        case Err(why):
            print(why)
"#);
    assert_eq!(out, vec!["nginx", "redis"]);
}

#[test]
fn a_mismatch_in_documents_names_which_document_failed() {
    let out = run(r#"use yaml
type Service:
    image: str

def main():
    match yaml.documents("image: nginx\n---\nimage: 7\n", Service):
        case Ok(all):
            print("unreachable")
        case Err(why):
            print(why)
"#);
    assert_eq!(out, vec!["$.1.image: expected str, got int"]);
}

// --- scalars, keys, and the edges ---

#[test]
fn quoting_decides_the_type() {
    assert_eq!(parse_and_get("v: \"true\"\n", "v"), vec!["true"]);
    assert_eq!(parse_and_get("v: 3\n", "v"), vec!["3"]);
    assert_eq!(parse_and_get("v: \"3\"\n", "v"), vec!["3"]);
    // Distinguishable through a type, which is the point of the distinction.
    let out = run(r#"use yaml
type Row:
    v: int
def main():
    match yaml.parse("v: \"3\"\n", Row):
        case Ok(r):
            print("unreachable")
        case Err(why):
            print(why)
"#);
    assert_eq!(out, vec!["$.v: expected int, got str"]);
}

#[test]
fn an_infinity_is_an_error_rather_than_the_string_inf() {
    let out = parse_and_get("ratio: .inf\n", "ratio");
    assert!(out[0].contains("has no value in Kora"), "got: {}", out[0]);
}

#[test]
fn a_collection_cannot_be_a_key() {
    let out = parse_and_get("? [a, b]\n: value\n", "x");
    assert!(out[0].contains("must be a scalar"), "got: {}", out[0]);
}

#[test]
fn an_empty_document_is_an_error_not_an_empty_object() {
    let out = parse_and_get("", "x");
    assert_eq!(out, vec!["the document is empty"]);
}

#[test]
fn malformed_yaml_names_a_line_and_column() {
    let out = parse_and_get("a:\n  - 1\n b: 2\n", "a");
    assert!(out[0].contains("invalid YAML at line"), "got: {}", out[0]);
}

// --- stringify ---

#[test]
fn stringify_round_trips() {
    let out = run(r#"use yaml
def main():
    match yaml.parse("name: ada\nports:\n  - 80\n  - 443\n"):
        case Ok(d):
            match yaml.stringify(d):
                case Ok(text):
                    print(text)
                case Err(w):
                    print(w)
        case Err(w):
            print(w)
"#);
    let text = out.join("\n");
    assert!(text.contains("name: ada"), "got: {text}");
    assert!(text.contains("- 80"), "got: {text}");
    assert!(
        !text.starts_with("---"),
        "the leading marker comes off: {text}"
    );
}

#[test]
fn stringify_refuses_a_classified_value() {
    // Writing a config file is usually a prelude to sending it somewhere,
    // the same reason `json.stringify` refuses.
    let err = run_err(
        r#"use yaml
type E:
    name: str
    classified token: str

def main():
    e = E("ada", "sk-live-1")
    yaml.stringify(e)
"#,
    );
    assert!(
        err.contains("classified data"),
        "a classified field must not be serialized, got: {err}"
    );
}
