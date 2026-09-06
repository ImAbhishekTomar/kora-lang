//! `csv`, `sql`, `env`, and the offline parts of `http`.
//!
//! Nothing here touches the network. The `http` tests cover URL validation
//! and the SSRF guard, which are the parts worth locking down.

use kora_runtime::{Config, Interpreter};
use kora_syntax::parse;

const CONFIG: &str = r#"
[models]
default = "local:test-model"

[sinks]
local_model = { allow = ["classified"] }
database = { allow = ["classified"] }
"#;

fn interp() -> Interpreter {
    let mut i = Interpreter::new();
    let config = Config::parse(CONFIG).unwrap();
    i.sinks = config.sinks.clone();
    i.config = config;
    i.program_name = "test.ko".into();
    i
}

fn run(src: &str) -> Vec<String> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = interp();
    i.run(&program)
        .unwrap_or_else(|e| panic!("runtime error: {}\n{src}", e.message));
    i.output
}

/// The error's message and hint together, since the safe-path guidance we
/// care about lives in the hint.
fn run_err_full(src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = interp();
    match i.run(&program) {
        Err(e) => format!("{} | {}", e.message, e.hint.unwrap_or_default()),
        Ok(_) => panic!("expected an error, program succeeded:\n{src}"),
    }
}

fn run_err(src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = interp();
    match i.run(&program) {
        Err(e) => e.message,
        Ok(_) => panic!("expected an error, program succeeded:\n{src}"),
    }
}

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "kora-std2-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// A path safe to paste into Kora source.
    ///
    /// Windows paths contain backslashes, and a backslash starts an escape
    /// sequence in a Kora string literal -- `C:\Users` reads as an unknown
    /// escape `\U`. Escaping here keeps these tests honest on every platform
    /// rather than only where paths happen to use forward slashes.
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().replace('\\', "\\\\")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

const PERSON: &str = r#"type Person:
    name: str
    zip: str
    amount: float
"#;

// --- csv ---

#[test]
fn csv_keeps_leading_zeros_in_string_columns() {
    // The pandas defect: a zip code column is inferred as an integer and
    // 01234 becomes 1234. Nothing downstream can recover the zero.
    let out = run(&format!(
        r#"{PERSON}
use csv
def main():
    text = "name,zip,amount\nada,01234,42.50\n"
    match csv.parse(text, Person):
        case Ok(people):
            for p in people:
                print(p.zip)
        case Err(why):
            print(why)
"#
    ));
    assert_eq!(out, vec!["01234"]);
}

#[test]
fn csv_type_errors_name_the_row_and_column() {
    let out = run(&format!(
        r#"{PERSON}
use csv
def main():
    text = "name,zip,amount\nada,01234,not-a-number\n"
    match csv.parse(text, Person):
        case Ok(people):
            print("unreachable")
        case Err(why):
            print(why)
"#
    ));
    assert_eq!(
        out[0],
        "row 2, column `amount`: expected a number, got `not-a-number`"
    );
}

#[test]
fn csv_ragged_rows_are_an_error_not_a_silent_shift() {
    let out = run(&format!(
        r#"{PERSON}
use csv
def main():
    text = "name,zip,amount\nada,01234\n"
    match csv.parse(text, Person):
        case Ok(p):
            print("unreachable")
        case Err(why):
            print(why)
"#
    ));
    assert!(out[0].contains("row 2"), "got: {}", out[0]);
    assert!(out[0].contains("2 field(s)"), "got: {}", out[0]);
}

#[test]
fn csv_missing_column_lists_what_the_file_has() {
    let out = run(&format!(
        r#"{PERSON}
use csv
def main():
    text = "name,zip\nada,01234\n"
    match csv.parse(text, Person):
        case Ok(p):
            print("unreachable")
        case Err(why):
            print(why)
"#
    ));
    assert!(out[0].contains("`amount` is missing"), "got: {}", out[0]);
    assert!(out[0].contains("name, zip"), "got: {}", out[0]);
}

#[test]
fn csv_handles_quoted_fields_and_embedded_commas() {
    let out = run(r#"use csv
def main():
    text = "a,b\n\"x,1\",2\n"
    match csv.rows(text):
        case Ok(rows):
            for r in rows:
                print(r["a"])
        case Err(why):
            print(why)
"#);
    assert_eq!(out, vec!["x,1"]);
}

#[test]
fn csv_needs_a_declared_type() {
    let err = run_err("use csv\ndef main():\n    csv.parse(\"a\\nb\\n\", \"Person\")\n");
    assert!(err.contains("declared type"), "got: {err}");
}

#[test]
fn csv_round_trips_through_write() {
    let out = run(&format!(
        r#"{PERSON}
use csv
def main():
    text = "name,zip,amount\nada,01234,42.5\n"
    match csv.parse(text, Person):
        case Ok(people):
            match csv.write(people):
                case Ok(back):
                    print(back)
                case Err(w):
                    print(w)
        case Err(w):
            print(w)
"#
    ));
    assert!(out[0].contains("name,zip,amount"), "got: {}", out[0]);
    assert!(out[0].contains("ada,01234,42.5"), "got: {}", out[0]);
}

// --- sql ---

#[test]
fn sql_refuses_a_statement_built_from_outside_data() {
    // The whole point: interpolating outside data into a statement is the
    // easy path everywhere else, and it is not available here.
    let scratch = Scratch::new("inject");
    let db = scratch.path("t.db");
    let evil = scratch.path("evil.txt");
    std::fs::write(&evil, "1 or 1=1").unwrap();

    let err = run_err_full(&format!(
        r#"use sql
use fs
def main():
    match fs.read("{evil}"):
        case Ok(user_id):
            stmt = f"select * from t where id = {{user_id}}"
            sql.query("{db}", stmt)
        case Err(w):
            print(w)
"#
    ));
    assert!(err.contains("built from outside data"), "got: {err}");
    assert!(
        err.contains("parameter"),
        "the hint should show the safe path"
    );
}

#[test]
fn sql_binds_outside_data_safely_as_a_parameter() {
    // Binding is the path the language wants, so unverified parameters are
    // allowed: the driver keeps them out of the statement.
    let scratch = Scratch::new("bind");
    let db = scratch.path("t.db");
    let input = scratch.path("id.txt");
    std::fs::write(&input, "1").unwrap();

    let out = run(&format!(
        r#"use sql
use fs
def main():
    sql.execute("{db}", "create table t (id integer, name text)")
    sql.execute("{db}", "insert into t values (?, ?)", [1, "ada"])
    match fs.read("{input}"):
        case Ok(raw):
            match sql.query("{db}", "select name from t where id = ?", [raw]):
                case Ok(rows):
                    print(len(rows))
                case Err(w):
                    print(w)
        case Err(w):
            print(w)
"#
    ));
    assert_eq!(out, vec!["1"], "a bound parameter should match normally");
}

#[test]
fn sql_errors_include_the_statement() {
    let scratch = Scratch::new("sqlerr");
    let db = scratch.path("t.db");
    let out = run(&format!(
        r#"use sql
def main():
    match sql.query("{db}", "select * from nonexistent_table"):
        case Ok(rows):
            print("unreachable")
        case Err(why):
            print(why)
"#
    ));
    assert!(out[0].contains("nonexistent_table"), "got: {}", out[0]);
    assert!(out[0].contains("in:"), "the statement should be shown");
}

#[test]
fn sql_refuses_classified_parameters_without_a_release() {
    let scratch = Scratch::new("sqlsecret");
    let db = scratch.path("t.db");
    let err = run_err(&format!(
        r#"use sql
def main():
    classified ssn = "123-45-6789"
    sql.execute("{db}", "insert into t values (?)", [ssn])
"#
    ));
    assert!(err.contains("classified data"), "got: {err}");
}

// --- env ---

#[test]
fn env_marks_credentials_as_classified() {
    // The defect: a key read from the environment is an ordinary string, so
    // it reaches a log line without anyone noticing.
    std::env::set_var("KORA_TEST_API_KEY", "sk-secret");
    let scratch = Scratch::new("envleak");
    let out = scratch.path("leak.txt");
    let err = run_err(&format!(
        r#"use env
use fs
def main():
    match env.get("KORA_TEST_API_KEY"):
        case Ok(key):
            fs.write("{out}", key)
        case Err(w):
            print(w)
"#
    ));
    assert!(err.contains("classified data"), "got: {err}");
    std::env::remove_var("KORA_TEST_API_KEY");
}

#[test]
fn env_leaves_ordinary_variables_usable() {
    std::env::set_var("KORA_TEST_REGION", "eu-west-1");
    let out = run(r#"use env
def main():
    match env.get("KORA_TEST_REGION"):
        case Ok(v):
            print(v)
        case Err(w):
            print(w)
"#);
    assert_eq!(out, vec!["eu-west-1"]);
    std::env::remove_var("KORA_TEST_REGION");
}

#[test]
fn env_missing_variable_is_a_value() {
    let out = run(r#"use env
def main():
    match env.get("KORA_DEFINITELY_UNSET_XYZ"):
        case Ok(v):
            print("unreachable")
        case Err(why):
            print(why)
"#);
    assert_eq!(out, vec!["`KORA_DEFINITELY_UNSET_XYZ` is not set"]);
}

// --- http (offline paths) ---

#[test]
fn http_refuses_private_addresses_by_default() {
    // 169.254.169.254 is the cloud metadata endpoint, the SSRF target that
    // keeps turning up in real incidents.
    let out = run(r#"use http
def main():
    match http.get("http://169.254.169.254/latest/meta-data/"):
        case Ok(r):
            print("unreachable")
        case Err(why):
            print(why)
"#);
    assert!(out[0].contains("private address"), "got: {}", out[0]);
}

#[test]
fn http_refuses_non_http_schemes() {
    let out = run(r#"use http
def main():
    match http.get("file:///etc/passwd"):
        case Ok(r):
            print("unreachable")
        case Err(why):
            print(why)
"#);
    assert!(out[0].contains("not an http"), "got: {}", out[0]);
}

#[test]
fn http_refuses_a_url_that_came_from_outside() {
    // A URL assembled from a fetched document is how a request ends up
    // pointed at an internal service.
    let scratch = Scratch::new("ssrf-url");
    let file = scratch.path("url.txt");
    std::fs::write(&file, "http://169.254.169.254/").unwrap();
    let err = run_err(&format!(
        r#"use http
use fs
def main():
    match fs.read("{file}"):
        case Ok(url):
            http.get(url)
        case Err(w):
            print(w)
"#
    ));
    assert!(err.contains("came from outside"), "got: {err}");
}

#[test]
fn http_private_hosts_can_be_allowed_explicitly() {
    // The escape hatch exists, and it is visible in configuration.
    let config = Config::parse("[http]\nallow_private = true\n").unwrap();
    assert!(config.http_allow_private);
    assert_eq!(
        config.http_timeout_secs, 30,
        "a default timeout always exists"
    );
}

#[test]
fn http_timeout_cannot_be_disabled() {
    // "No timeout" is the defect being fixed, so zero is clamped rather than
    // honoured.
    let config = Config::parse("[http]\ntimeout_secs = 0\n").unwrap();
    assert!(config.http_timeout_secs >= 1);
}
