//! The stdlib's error paths.
//!
//! Every module here is documented by what it does when the input is wrong —
//! a typed parse that names the field, a regex that will not compile, a
//! column that is not a number. Those messages are the product; a test suite
//! that only covers the happy path is testing the half nobody needs help
//! with.

use kora_runtime::{Config, Interpreter};
use kora_syntax::parse;

const CONFIG: &str = "[models]\ndefault = \"local:test-model\"\n";

fn run(src: &str) -> Vec<String> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = Interpreter::new();
    i.config = Config::parse(CONFIG).unwrap();
    i.program_name = "test.ko".into();
    i.run(&program)
        .unwrap_or_else(|e| panic!("the run should not fail: {}\n{src}", e.message));
    i.output
}

fn run_err(src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = Interpreter::new();
    i.config = Config::parse(CONFIG).unwrap();
    i.program_name = "test.ko".into();
    match i.run(&program) {
        Ok(()) => panic!("the run should have failed\n{src}"),
        Err(e) => format!("{} {}", e.message, e.hint.unwrap_or_default()),
    }
}

// --- json ---

#[test]
fn a_typed_parse_names_the_field_that_was_wrong() {
    // The whole reason `json.parse(text, Type)` exists: a mistake surfaces
    // here, naming the path, instead of three functions later as a missing
    // attribute.
    let out = run(r#"
use json

type User:
    name: str
    age: int

def main():
    match json.parse("{\"name\": \"Ada\", \"age\": \"old\"}", User):
        case Ok(u):
            print("unexpected ok")
        case Err(why):
            print(why)
"#);
    assert_eq!(out.len(), 1);
    assert!(
        out[0].contains("age"),
        "the path should name the field: {out:?}"
    );
    assert!(out[0].contains("int"), "and the type expected: {out:?}");
}

#[test]
fn a_missing_field_is_named_too() {
    let out = run(r#"
use json

type User:
    name: str
    age: int

def main():
    match json.parse("{\"name\": \"Ada\"}", User):
        case Ok(u):
            print("unexpected ok")
        case Err(why):
            print(why)
"#);
    assert!(out[0].contains("age"), "got: {out:?}");
    assert!(out[0].contains("missing"), "got: {out:?}");
}

#[test]
fn a_wrong_element_inside_a_list_is_named_by_index() {
    let out = run(r#"
use json

type Post:
    tags: list[str]

def main():
    match json.parse("{\"tags\": [\"a\", 2]}", Post):
        case Ok(p):
            print("unexpected ok")
        case Err(why):
            print(why)
"#);
    assert!(out[0].contains("tags"), "got: {out:?}");
    assert!(
        out[0].contains('1'),
        "the index belongs in the path: {out:?}"
    );
}

#[test]
fn malformed_json_says_where_rather_than_a_byte_offset() {
    let out = run(r#"
use json

def main():
    match json.parse("{\"a\": }"):
        case Ok(v):
            print("unexpected ok")
        case Err(why):
            print(why)
"#);
    assert_eq!(out.len(), 1);
    assert!(!out[0].is_empty(), "a reason should be given");
}

#[test]
fn an_untyped_parse_round_trips_through_stringify() {
    let out = run(r#"
use json

def main():
    match json.parse("{\"a\": [1, 2, 3]}"):
        case Ok(v):
            match json.stringify(v):
                case Ok(text):
                    print(text)
                case Err(why):
                    print(why)
        case Err(why):
            print(why)
"#);
    assert_eq!(out, vec![r#"{"a":[1,2,3]}"#]);
}

#[test]
fn stringify_refuses_classified_data() {
    // Serializing is how a label gets lost: the string that comes out has
    // no memory of the value that went in.
    let err = run_err(
        r#"
use json

def main():
    classified pay = "100000"
    match json.stringify(pay):
        case Ok(text):
            print(text)
        case Err(why):
            print(why)
"#,
    );
    assert!(err.contains("classified"), "got: {err}");
}

#[test]
fn json_get_walks_a_path_and_names_where_it_stopped() {
    let out = run(r#"
use json

def main():
    match json.parse("{\"users\": [{\"email\": \"a@b.c\"}]}"):
        case Ok(doc):
            match json.get(doc, "users.0.email"):
                case Ok(found):
                    print(found)
                case Err(why):
                    print(f"err: {why}")
            match json.get(doc, "users.0.phone"):
                case Ok(found):
                    print("unexpected")
                case Err(why):
                    print(f"missing: {why}")
        case Err(why):
            print(why)
"#);
    assert_eq!(out[0], "a@b.c");
    assert!(out[1].contains("not found"), "got: {:?}", out[1]);
}

#[test]
fn json_get_refuses_a_name_where_a_list_index_belongs() {
    let out = run(r#"
use json

def main():
    match json.parse("{\"users\": [1, 2]}"):
        case Ok(doc):
            match json.get(doc, "users.first"):
                case Ok(found):
                    print("unexpected")
                case Err(why):
                    print(why)
        case Err(why):
            print(why)
"#);
    assert!(
        out[0].contains("number") || out[0].contains("index"),
        "got: {out:?}"
    );
}

// --- re ---

#[test]
fn a_pattern_that_will_not_compile_is_an_err_not_a_crash() {
    let out = run(r#"
use re

def main():
    match re.matches("(unclosed", "text"):
        case Ok(hit):
            print("unexpected ok")
        case Err(why):
            print("err")
"#);
    assert_eq!(out, vec!["err"]);
}

#[test]
fn find_and_find_all_and_replace_and_split() {
    let out = run(r#"
use re

def main():
    match re.find("[0-9]+", "order 42 of 7"):
        case Ok(hit):
            print(hit)
        case Err(why):
            print(why)
    match re.find_all("[0-9]+", "order 42 of 7"):
        case Ok(hits):
            print(f"{len(hits)}")
        case Err(why):
            print(why)
    match re.replace("[0-9]+", "order 42", "N"):
        case Ok(text):
            print(text)
        case Err(why):
            print(why)
    match re.split(",", "a,b,c"):
        case Ok(parts):
            print(f"{len(parts)}")
        case Err(why):
            print(why)
"#);
    assert_eq!(out, vec!["42", "2", "order N", "3"]);
}

#[test]
fn a_pattern_that_matches_nothing_finds_nothing() {
    let out = run(r#"
use re

def main():
    match re.find("z+", "abc"):
        case Ok(hit):
            print(f"found: {hit}")
        case Err(why):
            print(f"no match: {why}")
    match re.matches("z+", "abc"):
        case Ok(hit):
            print(f"{hit}")
        case Err(why):
            print("err")
"#);
    assert_eq!(out, vec!["no match: no match", "False"]);
}

// --- csv ---

#[test]
fn a_csv_column_that_is_not_a_number_names_the_row() {
    let out = run(r#"
use csv

type Row:
    name: str
    amount: int

def main():
    match csv.parse("name,amount\nAda,ten\n", Row):
        case Ok(rows):
            print("unexpected ok")
        case Err(why):
            print(why)
"#);
    assert_eq!(out.len(), 1);
    assert!(
        out[0].contains("amount"),
        "the column belongs in it: {out:?}"
    );
}

#[test]
fn a_csv_missing_a_declared_column_says_which() {
    let out = run(r#"
use csv

type Row:
    name: str
    amount: int

def main():
    match csv.parse("name\nAda\n", Row):
        case Ok(rows):
            print("unexpected ok")
        case Err(why):
            print(why)
"#);
    assert!(out[0].contains("amount"), "got: {out:?}");
}

#[test]
fn a_good_csv_parses_into_declared_rows() {
    let out = run(r#"
use csv

type Row:
    name: str
    amount: int

def main():
    match csv.parse("name,amount\nAda,3\nGrace,4\n", Row):
        case Ok(rows):
            total = 0
            for row in rows:
                total = total + row.amount
            print(f"{len(rows)} rows, {total}")
        case Err(why):
            print(why)
"#);
    assert_eq!(out, vec!["2 rows, 7"]);
}

// --- fs ---

#[test]
fn reading_a_file_that_is_not_there_is_an_err() {
    let out = run(r#"
use fs

def main():
    match fs.read("definitely-not-a-file-here.txt"):
        case Ok(text):
            print("unexpected ok")
        case Err(why):
            print("err")
"#);
    assert_eq!(out, vec!["err"]);
}

#[test]
fn a_written_file_reads_back() {
    let dir = std::env::temp_dir().join(format!("kora-fs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("note.txt");
    let out = run(&format!(
        r#"
use fs

def main():
    match fs.write("{p}", "one\n"):
        case Ok(_):
            print("wrote")
        case Err(why):
            print(why)
    match fs.append("{p}", "two\n"):
        case Ok(_):
            print("appended")
        case Err(why):
            print(why)
    match fs.read("{p}"):
        case Ok(text):
            print(f"{{len(text)}}")
        case Err(why):
            print(why)
"#,
        p = path.to_str().unwrap()
    ));
    assert_eq!(out, vec!["wrote", "appended", "8"]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_path_that_came_out_of_a_file_is_refused() {
    // The same rule a URL follows. Contents read off disk are data from
    // outside this evaluation, and letting them name the next file to open
    // is how a program ends up reading something it never meant to.
    let dir = std::env::temp_dir().join(format!("kora-fs-path-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pointer.txt");
    std::fs::write(&path, "somewhere-else.txt").unwrap();

    let err = run_err(&format!(
        r#"
use fs

def main():
    match fs.read("{p}"):
        case Ok(text):
            match fs.read(text):
                case Ok(inner):
                    print("read")
                case Err(why):
                    print("err")
        case Err(why):
            print("outer err")
"#,
        p = path.to_str().unwrap()
    ));
    std::fs::remove_dir_all(&dir).ok();
    assert!(err.contains("came from outside the program"), "got: {err}");
}

#[test]
fn a_path_climbing_out_of_the_tree_is_refused() {
    // Refused rather than normalized: normalizing quietly changes what the
    // caller asked for, and the caller is the one who should know.
    let err = run_err(
        r#"
use fs

def main():
    match fs.read("../outside.txt"):
        case Ok(text):
            print("read")
        case Err(why):
            print("err")
"#,
    );
    assert!(
        err.contains(".."),
        "the message should show the path: {err}"
    );
}

// --- time ---

#[test]
fn time_now_and_format_and_elapsed() {
    let out = run(r#"
use time

def main():
    started: int = time.now()
    match time.format(started, "iso"):
        case Ok(text):
            print(f"{len(text) > 0}")
        case Err(why):
            print(why)
    gap: int = time.elapsed(started)
    print(f"{gap >= 0}")
"#);
    assert_eq!(out, vec!["True", "True"]);
}

// --- env ---

#[test]
fn an_unset_variable_is_an_err_rather_than_an_empty_string() {
    let out = run(r#"
use env

def main():
    match env.get("KORA_DEFINITELY_UNSET_VARIABLE"):
        case Ok(value):
            print("unexpectedly set")
        case Err(why):
            print("unset")
"#);
    assert_eq!(out, vec!["unset"]);
}

// --- the checker and the runtime must agree ---

#[test]
fn every_builtin_the_checker_knows_exists_at_runtime() {
    // The drift this catches is the one that costs most: a name the editor
    // offers and `kora check` accepts, which then fails when the line runs.
    // Calling each with no arguments is enough — a builtin that exists
    // complains about its arguments, and one that does not complains about
    // its name.
    for name in kora_types::builtin_names() {
        let src = format!("def main():\n    {name}()\n");
        let program = parse(&src).unwrap_or_else(|e| panic!("parse error for `{name}`: {e}"));
        let mut i = Interpreter::new();
        i.config = Config::parse(CONFIG).unwrap();
        i.program_name = "test.ko".into();
        if let Err(e) = i.run(&program) {
            assert!(
                !e.message.contains("is not defined"),
                "`{name}` is a builtin to the checker but not to the runtime: {}",
                e.message
            );
        }
    }
}

#[test]
fn every_stdlib_module_the_checker_lists_can_be_imported() {
    // Same drift, one level up: a module in the completion list that `use`
    // refuses is a worse experience than one that was never offered.
    for module in kora_types::module_names() {
        let src = format!("use {module}\n\ndef main():\n    print(\"ok\")\n");
        let program = parse(&src).unwrap_or_else(|e| panic!("parse error for `{module}`: {e}"));
        let mut i = Interpreter::new();
        i.config = Config::parse(CONFIG).unwrap();
        i.program_name = "test.ko".into();
        i.run(&program)
            .unwrap_or_else(|e| panic!("`use {module}` should work: {}", e.message));
    }
}
