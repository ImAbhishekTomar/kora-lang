//! `kora.toml`, and what it does when it is wrong.
//!
//! Configuration is where a project's decisions live — which model, which
//! sink may see a secret, whether a request may reach a private address. A
//! setting that is silently ignored is worse than one that fails: the program
//! runs, and does the thing the file was written to prevent.

use kora_runtime::Config;

fn parse(text: &str) -> Config {
    Config::parse(text).unwrap_or_else(|e| panic!("should have parsed: {}\n{text}", e.message))
}

#[test]
fn an_empty_file_is_a_valid_configuration() {
    // A program with no models and no sinks is a legitimate program: not
    // everything calls a model.
    let config = parse("");
    assert!(config.models.is_empty());
    assert!(config.default_model().is_err(), "but there is no default");
}

#[test]
fn a_missing_default_says_what_to_add() {
    let config = parse("[models]\nsmart = \"openai:gpt-4o\"\n");
    let err = config
        .default_model()
        .expect_err("no default is configured");
    assert!(
        err.message.contains("default"),
        "the message should name the key: {}",
        err.message
    );
    assert!(
        err.message.contains("kora.toml"),
        "and the file to put it in: {}",
        err.message
    );
}

#[test]
fn broken_toml_is_refused_by_name() {
    let err = Config::parse("[models\ndefault = \"x\"").expect_err("this is not TOML");
    assert!(
        err.message.contains("kora.toml"),
        "the message should say which file: {}",
        err.message
    );
}

#[test]
fn a_name_resolves_to_the_spec_it_was_given() {
    let config = parse(
        r#"
[models]
default = "local:llama3.1:8b"
smart = "openai:gpt-4o"
"#,
    );
    let smart = config.resolve_model("smart").unwrap();
    assert_eq!(smart.model, "gpt-4o");
    let default = config.default_model().unwrap();
    assert_eq!(default.model, "llama3.1:8b");
}

#[test]
fn an_unknown_name_is_tried_as_a_spec_and_then_refused() {
    // The fallthrough exists so a spec still works where a name is expected;
    // something that is neither has to fail rather than resolve to nothing.
    let config = parse("[models]\ndefault = \"local:m\"\n");
    assert!(
        config.resolve_model("local:other").is_ok(),
        "a spec resolves"
    );
    assert!(
        config.resolve_model("not a model at all").is_err(),
        "and nonsense does not"
    );
}

#[test]
fn model_timeout_and_retries_reach_the_resolved_model() {
    // Both are settings rather than constants because a local vision model
    // and a hosted text model want different numbers.
    let config = parse(
        r#"
[models]
default = "local:llama3.1:8b"
timeout_secs = 300
max_retries = 0
"#,
    );
    let model = config.default_model().unwrap();
    assert_eq!(model.timeout_secs, 300);
    assert_eq!(model.max_retries, 0);
}

#[test]
fn defaults_apply_when_nothing_is_said() {
    let config = parse("[models]\ndefault = \"local:m\"\n");
    let model = config.default_model().unwrap();
    assert!(model.timeout_secs > 0, "a timeout always exists");
    assert!(
        !config.http_allow_private,
        "private addresses are off unless asked for"
    );
    assert!(config.http_timeout_secs > 0, "so does an http timeout");
    assert!(
        !config.classified_placeholder.is_empty(),
        "redaction needs something to print"
    );
}

#[test]
fn a_local_endpoint_overrides_the_default_host() {
    let config = parse(
        r#"
[models]
default = "local:m"

[models.local]
endpoint = "http://elsewhere:1234"
"#,
    );
    let model = config.default_model().unwrap();
    assert_eq!(model.endpoint.as_deref(), Some("http://elsewhere:1234"));
}

#[test]
fn an_openai_output_cap_applies_only_to_openai() {
    let config = parse(
        r#"
[models]
default = "openai:gpt-4o"
other = "local:m"

[models.openai]
max_output_tokens = 77
"#,
    );
    assert_eq!(config.default_model().unwrap().max_output_tokens, 77);
    // The local model keeps its own default: a cap written under `[models.openai]`
    // that silently applied everywhere would be a setting doing something its
    // name denies.
    assert_ne!(config.resolve_model("other").unwrap().max_output_tokens, 77);
}

#[test]
fn http_settings_are_read() {
    let config = parse(
        r#"
[http]
allow_private = true
timeout_secs = 5
"#,
    );
    assert!(config.http_allow_private);
    assert_eq!(config.http_timeout_secs, 5);
}

#[test]
fn a_zero_http_timeout_is_clamped_rather_than_honoured() {
    // Documented behaviour: there is no "off". A request with no deadline is
    // how a program hangs forever on a server that accepted and went quiet.
    let config = parse("[http]\ntimeout_secs = 0\n");
    assert!(
        config.http_timeout_secs > 0,
        "0 should be clamped, got {}",
        config.http_timeout_secs
    );
}

#[test]
fn the_classified_placeholder_can_be_chosen() {
    let config = parse("[output]\nclassified_placeholder = \"<redacted>\"\n");
    assert_eq!(config.classified_placeholder, "<redacted>");
}

#[test]
fn sinks_record_what_each_one_may_receive() {
    use kora_runtime::label::{Label, Secrecy, Trust};

    let classified = Label {
        secrecy: Secrecy::Classified,
        trust: Trust::Trusted,
        released: None,
    };
    let config = parse(
        r#"
[sinks]
log = { allow = ["classified"] }
openai = { deny = ["classified"] }
"#,
    );
    assert!(
        config.sinks.permits("log", classified.clone()),
        "an allowed sink should allow"
    );
    assert!(
        !config.sinks.permits("openai", classified.clone()),
        "a denied sink should deny"
    );
    // Unknown sinks are refused rather than allowed: a typo in a sink name
    // must not silently open a hole.
    assert!(
        !config.sinks.permits("never-mentioned", classified),
        "a sink nobody configured is not a sink that permits everything"
    );
    assert!(config.sinks.is_known_sink("log"));
    assert!(!config.sinks.is_known_sink("never-mentioned"));
}

#[test]
fn public_data_reaches_any_sink() {
    use kora_runtime::label::Label;

    let config = parse(
        "[sinks]
log = { deny = [\"classified\"] }
",
    );
    assert!(
        config.sinks.permits("log", Label::default()),
        "a sink policy governs secrets, not ordinary values"
    );
}

#[test]
fn an_mcp_server_is_read_with_its_command() {
    let config = parse(
        r#"
[mcp.github]
command = "gh-mcp"
args = ["--stdio"]
"#,
    );
    let server = config
        .mcp_servers
        .get("github")
        .expect("the server should be registered");
    assert_eq!(server.command, "gh-mcp");
    assert_eq!(server.args, vec!["--stdio".to_string()]);
}

#[test]
fn a_python_interpreter_can_be_chosen() {
    let config = parse("[python]\ncommand = \"python3.12\"\n");
    assert_eq!(config.python.command, "python3.12");
    // And the default is the one on PATH, so a project that says nothing
    // still runs.
    assert_eq!(parse("").python.command, "python3");
}

#[test]
fn discovery_finds_nothing_gracefully() {
    // A directory with no kora.toml above it anywhere is the ordinary case
    // for a scratch script, and must not fail.
    let dir = std::env::temp_dir().join(format!("kora-config-none-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = Config::discover(&dir.join("scratch.ko"));
    assert!(config.default_model().is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn discovery_reads_the_file_beside_the_program() {
    let dir = std::env::temp_dir().join(format!("kora-config-here-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("kora.toml"),
        "[models]\ndefault = \"local:found-it\"\n",
    )
    .unwrap();
    let config = Config::discover(&dir.join("program.ko"));
    assert_eq!(config.default_model().unwrap().model, "found-it");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn discovery_climbs_to_a_parent_directory() {
    // A program in `src/` belongs to the project whose `kora.toml` is at the
    // root; making every subdirectory carry its own copy would be the same
    // mistake as vendoring configuration.
    let dir = std::env::temp_dir().join(format!("kora-config-up-{}", std::process::id()));
    let nested = dir.join("src").join("deep");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        dir.join("kora.toml"),
        "[models]\ndefault = \"local:from-the-root\"\n",
    )
    .unwrap();
    let config = Config::discover(&nested.join("program.ko"));
    assert_eq!(config.default_model().unwrap().model, "from-the-root");
    std::fs::remove_dir_all(&dir).ok();
}
