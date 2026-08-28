//! MCP integration at the language level.
//!
//! No server is spawned: these cover configuration, error messages, and the
//! security boundary. The protocol itself is tested in `kora-mcp`.

use kora_runtime::{Config, Interpreter};
use kora_syntax::parse;

const CONFIG: &str = r#"
[models]
default = "local:test-model"

[sinks]
local_model = { allow = ["classified"] }
files = { allow = ["classified"] }

[mcp.files]
command = "echo"
args = ["placeholder"]

[mcp.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_TOKEN = "$KORA_TEST_TOKEN" }
"#;

fn interp() -> Interpreter {
    let mut i = Interpreter::new();
    let config = Config::parse(CONFIG).unwrap();
    i.sinks = config.sinks.clone();
    i.config = config;
    i.program_name = "test.ko".into();
    i
}

fn run_err(src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut i = interp();
    match i.run(&program) {
        Err(e) => format!("{} | {}", e.message, e.hint.unwrap_or_default()),
        Ok(_) => panic!("expected an error, program succeeded:\n{src}"),
    }
}

// --- configuration ---

#[test]
fn servers_are_read_from_config() {
    let config = Config::parse(CONFIG).unwrap();
    let github = config.mcp_servers.get("github").expect("github configured");
    assert_eq!(github.command, "npx");
    assert_eq!(github.args[0], "-y");
}

#[test]
fn credentials_come_from_the_environment_not_the_file() {
    // `$VAR` is resolved at load, so a token lives in the environment rather
    // than in a committed config file.
    std::env::set_var("KORA_TEST_TOKEN", "ghp_secret");
    let config = Config::parse(CONFIG).unwrap();
    let github = config.mcp_servers.get("github").unwrap();
    assert_eq!(github.env.get("GITHUB_TOKEN").unwrap(), "ghp_secret");
    std::env::remove_var("KORA_TEST_TOKEN");
}

#[test]
fn a_literal_env_value_is_left_alone() {
    let config =
        Config::parse("[mcp.x]\ncommand = \"true\"\nenv = { MODE = \"debug\" }\n").unwrap();
    assert_eq!(config.mcp_servers["x"].env.get("MODE").unwrap(), "debug");
}

// --- errors before anything is spawned ---

#[test]
fn an_unconfigured_server_names_the_ones_that_exist() {
    let err = run_err("use mcp nowhere as n\ndef main():\n    pass\n");
    assert!(err.contains("no MCP server named `nowhere`"), "got: {err}");
    assert!(
        err.contains("files"),
        "the hint should list what is configured: {err}"
    );
    assert!(err.contains("github"), "got: {err}");
}

#[test]
fn a_server_with_no_command_is_rejected() {
    let program = parse("use mcp broken as b\ndef main():\n    pass\n").unwrap();
    let mut i = Interpreter::new();
    i.config = Config::parse("[mcp.broken]\nargs = [\"x\"]\n").unwrap();
    let err = i.run(&program).unwrap_err();
    assert!(err.message.contains("no `command`"), "{}", err.message);
}

#[test]
fn an_empty_configuration_suggests_how_to_add_one() {
    let program = parse("use mcp github as gh\ndef main():\n    pass\n").unwrap();
    let mut i = Interpreter::new();
    let err = i.run(&program).unwrap_err();
    assert!(
        err.message.contains("no MCP server named"),
        "{}",
        err.message
    );
    assert!(
        err.hint.as_deref().unwrap_or("").contains("[mcp.github]"),
        "the hint should show the shape of the config"
    );
}

// --- syntax ---

#[test]
fn use_mcp_parses_with_and_without_an_alias() {
    parse("use mcp github as gh\n").expect("aliased");
    parse("use mcp github\n").expect("bare");
}

#[test]
fn the_alias_is_a_known_name_to_the_checker() {
    // Which servers exist is a runtime question, so the checker records the
    // alias rather than reporting a name it cannot verify.
    let program = parse("use mcp github as gh\ndef main():\n    x = gh\n").unwrap();
    let analysis = kora_types::analyze(&program);
    let undefined: Vec<&str> = analysis
        .diagnostics
        .iter()
        .filter(|d| d.message.contains("not defined"))
        .map(|d| d.message.as_str())
        .collect();
    assert!(undefined.is_empty(), "{undefined:?}");
}

#[test]
fn a_server_appears_in_the_outline() {
    let program = parse("use mcp github as gh\n").unwrap();
    let analysis = kora_types::analyze(&program);
    let symbol = analysis.symbols.get("gh").expect("the alias is a symbol");
    assert_eq!(symbol.detail, "use mcp github");
}

// --- the security boundary ---

/// Pre-connect a server with canned tools, so the checks around *offering*
/// them can be exercised without spawning a process.
fn with_fake_server(name: &str) -> Interpreter {
    let i = interp();
    i.mcp.lock().unwrap().insert(
        name.to_string(),
        kora_mcp::Server::for_testing(
            name,
            vec![kora_mcp::Tool {
                name: "read_file".into(),
                description: "Read a file.".into(),
                params: vec![("path".into(), kora_mcp::ParamType::Str)],
                required: vec!["path".into()],
            }],
        ),
    );
    i
}

#[test]
fn a_server_is_its_own_sink() {
    // Releasing a secret to the model does not release it to a server: a
    // server runs in its own process and is a separate destination.
    let src = r#"type R:
    ok: bool

def main():
    classified secret = "hunter2"
    declassify secret as s for local_model:
        r: R = analyze(s, "do something", tools=files.tools)
"#;
    let program = parse(src).unwrap();
    let mut i = with_fake_server("files");
    // Bind the alias the way `use mcp files` would.
    i.bind_global(
        "files",
        kora_runtime::Value::McpServer {
            alias: std::rc::Rc::new("files".to_string()),
        },
    );
    let e = i.run(&program).unwrap_err();
    let err = format!("{} | {}", e.message, e.hint.unwrap_or_default());
    assert!(
        err.contains("cannot reach MCP server `files`"),
        "got: {err}"
    );
    assert!(
        err.contains("its own process"),
        "the hint should say why: {err}"
    );
}

#[test]
fn kora_tools_do_not_trigger_the_server_check() {
    // Only MCP tools introduce a second process; a declared tool runs here.
    let src = r#"type R:
    ok: bool

tool helper(a: str) -> str:
    "Does a thing."
    return a

def main():
    classified secret = "hunter2"
    declassify secret as s for local_model:
        r: R = analyze(s, "do something", tools=[helper])
"#;
    // Fails on the model call rather than on a sink check, since there is no
    // model to reach in a test.
    let err = run_err(src);
    assert!(
        !err.contains("MCP server"),
        "a declared tool should not look like a server: {err}"
    );
}

#[test]
fn parallel_branches_share_the_server_connection() {
    // Regression. Workers are fresh interpreters seeded with what they need,
    // and the MCP registry was left out — so a `use mcp` at the top level was
    // invisible inside a `parallel for`, and the branch reported the server as
    // not connected. A server is a process; one per branch would be both slow
    // and wrong.
    let src = r#"type R:
    ok: bool

agent look(item: str) -> str:
    classified secret = "hunter2"
    declassify secret as s for local_model:
        r: R = analyze(s, "do something", tools=files.tools)
    return "unreachable"

def main():
    results = parallel for i in ["a"]:
        return look(i)
"#;
    let program = parse(src).unwrap();
    let mut i = with_fake_server("files");
    i.bind_global(
        "files",
        kora_runtime::Value::McpServer {
            alias: std::rc::Rc::new("files".to_string()),
        },
    );
    let e = i.run(&program).unwrap_err();
    // Reaching the sink check at all proves the branch saw the connection;
    // "not connected" would mean it did not.
    assert!(
        e.message.contains("cannot reach MCP server"),
        "the branch should see the server, got: {}",
        e.message
    );
}
