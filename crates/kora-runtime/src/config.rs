//! `kora.toml` loading.
//!
//! Unknown top-level keys remain forward-compatible, but a configuration file
//! that exists and cannot be read or parsed is never treated as no config.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kora_models::{ModelConfig, ModelError, Provider};

use crate::label::SinkPolicy;

/// One `[models]` entry, written out rather than encoded in a string.
///
/// The model is named exactly as its provider's own documentation names it
/// — `openrouter/free`, `gpt-4o`, `qwen2.5vl:3b` — because a name a program
/// has to respell is a name somebody gets wrong. Everything the runtime
/// needs beyond the name is said here instead of inferred from it.
#[derive(Debug, Clone)]
pub struct DeclaredModel {
    /// Verbatim. Never parsed, never split.
    pub name: String,
    /// Where the request goes. `None` means the wire format's own default.
    pub endpoint: Option<String>,
    /// Which environment variable holds the key. `None` means no key is
    /// configured, and for an endpoint of one's own that means the request
    /// is sent without an `Authorization` header — a local vLLM or
    /// llama.cpp server usually wants none.
    pub api_key_env: Option<String>,
    /// Which request shape to build: the one thing about a model the runtime
    /// cannot read off a URL, because `/chat/completions` and `/api/chat`
    /// take different bodies. Two values, and it is the only Kora-specific
    /// word in the entry.
    pub api: Provider,
    pub max_output_tokens: Option<u32>,
    pub timeout_secs: Option<u64>,
    pub max_retries: Option<u32>,
}

/// How a role in `[models]` names its model.
#[derive(Debug, Clone)]
pub enum ModelEntry {
    /// `smart = "openai:gpt-4o"` — the older shorthand, where the wire
    /// format rides in the string and the endpoint is that format's default.
    /// Kept working because it is in every existing project and cassette.
    Spec(String),
    /// `smart = { name = "gpt-4o", endpoint = "...", api_key_env = "..." }`
    Declared(DeclaredModel),
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Named model roles, e.g. "default" -> `local:llama3.1:8b`.
    pub models: HashMap<String, ModelEntry>,
    /// Per-provider settings.
    pub openai_max_output_tokens: Option<u32>,
    /// `[models.openai] endpoint` — the base URL for OpenAI-wire calls.
    /// `None` means OpenAI itself; anything else is a compatible gateway.
    pub openai_endpoint: Option<String>,
    /// `[models.openai] api_key_env` — which variable holds that key. The
    /// name is configured, never the key: a secret in a checked-in file is
    /// the leak this avoids.
    pub openai_api_key_env: Option<String>,
    pub local_endpoint: Option<String>,
    /// `[models] timeout_secs` — how long one model call may take. A vision
    /// call on a local model runs far longer than a text one, so this is a
    /// setting rather than a constant.
    pub model_timeout_secs: Option<u64>,
    /// `[models] max_retries` — how many times a model call is retried when
    /// the provider does not answer. `0` disables retrying, which is a
    /// legitimate choice for a local model on the same machine; it is not
    /// the default, because a hosted provider under load is ordinary.
    pub model_max_retries: Option<u32>,
    /// Which sinks may receive which labels, from `[sinks]`.
    pub sinks: SinkPolicy,
    /// `[output] classified_placeholder` — what a terminal or captured output
    /// line shows in place of classified data. Output is intentionally a
    /// redacting boundary, not a declassification sink.
    pub classified_placeholder: String,
    /// `[http] allow_private` — permit loopback and private address ranges.
    pub http_allow_private: bool,
    /// `[http] timeout_secs` — applied to every request; there is no "off".
    pub http_timeout_secs: u64,
    /// `[telemetry]` settings.
    pub telemetry: crate::telemetry::Config,
    /// `[mcp.<name>]` server definitions: how to launch each one.
    pub mcp_servers: HashMap<String, kora_mcp::ServerConfig>,
    /// `[python]` — which interpreter the sidecar uses.
    pub python: kora_python::Config,
    /// `[install] jobs` — how many dependency fetches run at once. Zero
    /// means the default, which suits IO rather than core count.
    pub install_jobs: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            models: HashMap::new(),
            openai_max_output_tokens: None,
            openai_endpoint: None,
            openai_api_key_env: None,
            local_endpoint: None,
            model_timeout_secs: None,
            model_max_retries: None,
            sinks: SinkPolicy::default(),
            classified_placeholder: "__CLASSIFIED__".to_string(),
            http_allow_private: false,
            http_timeout_secs: 30,
            telemetry: crate::telemetry::Config::default(),
            mcp_servers: HashMap::new(),
            python: kora_python::Config::default(),
            install_jobs: 0,
        }
    }
}

impl Config {
    /// Look next to the program file, then walk up to the filesystem root.
    /// An unreadable or invalid file is an error, never the empty default, so
    /// a typo cannot silently change the model or sink policy.
    pub fn discover(start: &Path) -> Result<Config, ModelError> {
        let mut dir = if start.is_dir() {
            Some(start.to_path_buf())
        } else {
            start.parent().map(PathBuf::from)
        };
        while let Some(d) = dir {
            let candidate = d.join("kora.toml");
            if candidate.is_file() {
                let text = std::fs::read_to_string(&candidate).map_err(|error| {
                    ModelError::new(format!("cannot read {}: {error}", candidate.display()))
                })?;
                return Config::parse(&text)
                    .map_err(|error| ModelError::new(format!("{}: {error}", candidate.display())));
            }
            dir = d.parent().map(PathBuf::from);
        }
        Ok(Config::default())
    }

    pub fn parse(text: &str) -> Result<Config, ModelError> {
        let root: toml::Value = text
            .parse()
            .map_err(|e| ModelError::new(format!("kora.toml is not valid TOML: {e}")))?;

        let mut config = Config {
            sinks: SinkPolicy::from_toml(&root),
            http_timeout_secs: 30,
            classified_placeholder: "__CLASSIFIED__".to_string(),
            ..Default::default()
        };
        if let Some(section) = root.get("output").and_then(|v| v.as_table()) {
            if let Some(placeholder) = section
                .get("classified_placeholder")
                .and_then(|v| v.as_str())
            {
                config.classified_placeholder = placeholder.to_string();
            }
        }
        if let Some(section) = root.get("install").and_then(|v| v.as_table()) {
            if let Some(jobs) = section.get("jobs").and_then(|v| v.as_integer()) {
                config.install_jobs = jobs.max(0) as usize;
            }
        }
        if let Some(section) = root.get("python").and_then(|v| v.as_table()) {
            if let Some(command) = section.get("command").and_then(|v| v.as_str()) {
                config.python.command = command.to_string();
            }
        }
        if let Some(servers) = root.get("mcp").and_then(|v| v.as_table()) {
            // Scalars at the `[mcp]` level are settings for every server;
            // sub-tables are the servers themselves. Same shape as `[models]`,
            // so there is one thing to learn rather than two.
            let default_timeout = servers
                .get("timeout_secs")
                .and_then(|v| v.as_integer())
                // A zero timeout is how "wait forever" sneaks back in, which
                // is the failure this setting exists to prevent.
                .map(|secs| secs.clamp(1, 3600) as u64)
                .unwrap_or(kora_mcp::DEFAULT_TIMEOUT_SECS);
            // Zero is honoured, unlike a timeout: "do not retry starting it"
            // is a real answer for a server that is simply not installed.
            let default_retries = servers
                .get("max_retries")
                .and_then(|v| v.as_integer())
                .map(|times| times.clamp(0, 10) as u32)
                .unwrap_or(kora_mcp::DEFAULT_MAX_RETRIES);

            for (name, spec) in servers {
                let Some(spec) = spec.as_table() else {
                    continue;
                };
                let mut env = HashMap::new();
                if let Some(table) = spec.get("env").and_then(|v| v.as_table()) {
                    for (key, value) in table {
                        if let Some(text) = value.as_str() {
                            // `$VAR` reads from the environment, so a token
                            // lives there rather than in a committed file.
                            let resolved = match text.strip_prefix('$') {
                                Some(var) => std::env::var(var).unwrap_or_default(),
                                None => text.to_string(),
                            };
                            env.insert(key.clone(), resolved);
                        }
                    }
                }
                config.mcp_servers.insert(
                    name.clone(),
                    kora_mcp::ServerConfig {
                        command: spec
                            .get("command")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        args: spec
                            .get("args")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        env,
                        // A server that reaches a slow API can be given room
                        // without loosening the deadline on every other one.
                        timeout_secs: spec
                            .get("timeout_secs")
                            .and_then(|v| v.as_integer())
                            .map(|secs| secs.clamp(1, 3600) as u64)
                            .unwrap_or(default_timeout),
                        max_retries: spec
                            .get("max_retries")
                            .and_then(|v| v.as_integer())
                            .map(|times| times.clamp(0, 10) as u32)
                            .unwrap_or(default_retries),
                    },
                );
            }
        }
        if let Some(section) = root.get("telemetry").and_then(|v| v.as_table()) {
            let level = section
                .get("level")
                .and_then(|v| v.as_str())
                .map(crate::telemetry::Level::parse)
                .unwrap_or_default();
            let exporter = match section.get("exporter").and_then(|v| v.as_str()) {
                Some("otlp") => crate::telemetry::Exporter::Otlp(
                    section
                        .get("endpoint")
                        .and_then(|v| v.as_str())
                        .unwrap_or("http://localhost:4318")
                        .to_string(),
                ),
                // The zero-configuration default: a local file, so there is
                // no collector to stand up before seeing anything.
                Some("file") => crate::telemetry::Exporter::File(
                    section
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("kora.trace.json")
                        .to_string(),
                ),
                _ => crate::telemetry::Exporter::None,
            };
            config.telemetry = crate::telemetry::Config {
                level,
                exporter,
                // Redaction is on unless someone turns it off on purpose.
                redact: section
                    .get("redact")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
                service_name: section
                    .get("service_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("kora")
                    .to_string(),
            };
        }
        if let Some(http) = root.get("http").and_then(|v| v.as_table()) {
            config.http_allow_private = http
                .get("allow_private")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if let Some(secs) = http.get("timeout_secs").and_then(|v| v.as_integer()) {
                // A zero or negative timeout is how "no timeout" sneaks back
                // in, so it is clamped rather than honoured.
                config.http_timeout_secs = secs.clamp(1, 600) as u64;
            }
        }

        if let Some(models) = root.get("models").and_then(|v| v.as_table()) {
            for (key, value) in models {
                match value {
                    // `default = "local:llama3.1:8b"`
                    toml::Value::String(spec) => {
                        config
                            .models
                            .insert(key.clone(), ModelEntry::Spec(spec.clone()));
                    }
                    // `timeout_secs = 900`, clamped like the http one: a
                    // zero timeout is how "wait forever" sneaks back in.
                    toml::Value::Integer(secs) if key == "timeout_secs" => {
                        config.model_timeout_secs = Some((*secs).clamp(1, 3600) as u64);
                    }
                    // Unlike a timeout, zero is honoured here: "do not retry"
                    // is a real answer, and a local model that is simply not
                    // running should say so on the first attempt.
                    toml::Value::Integer(times) if key == "max_retries" => {
                        config.model_max_retries = Some((*times).clamp(0, 10) as u32);
                    }
                    // A table is either a model written out in full --
                    // which is the one with a `name` -- or the settings for
                    // one of the two built-in wire formats. Keying on `name`
                    // rather than on a list of reserved words is what lets a
                    // role be called `openai` if a project wants it to be.
                    toml::Value::Table(table) if table.contains_key("name") => {
                        let model = declared_model(key, table)?;
                        config
                            .models
                            .insert(key.clone(), ModelEntry::Declared(model));
                    }
                    // `[models.openai]` / `[models.local]` sub-tables
                    toml::Value::Table(table) => {
                        if key == "openai" {
                            config.openai_max_output_tokens = table
                                .get("max_output_tokens")
                                .and_then(|v| v.as_integer())
                                .map(|v| v as u32);
                            config.openai_endpoint = table
                                .get("endpoint")
                                .and_then(|v| v.as_str())
                                .map(str::to_string);
                            config.openai_api_key_env = table
                                .get("api_key_env")
                                .and_then(|v| v.as_str())
                                .map(str::to_string);
                        } else if key == "local" {
                            config.local_endpoint = table
                                .get("endpoint")
                                .and_then(|v| v.as_str())
                                .map(str::to_string);
                        } else {
                            return Err(ModelError::new(format!(
                                "[models.{key}] must declare a non-empty string `name`"
                            )));
                        }
                    }
                    _ => {
                        return Err(ModelError::new(format!(
                            "models.{key} must be a model string, a declared model table, or a supported numeric setting"
                        )));
                    }
                }
            }
        }
        Ok(config)
    }

    /// Resolve a model reference: either a role from `[models]` or a direct
    /// spec like `openai:gpt-4o`. Applies provider settings from config.
    pub fn resolve_model(&self, reference: &str) -> Result<ModelConfig, ModelError> {
        match self.models.get(reference) {
            Some(ModelEntry::Declared(declared)) => Ok(self.declared_config(declared)),
            Some(ModelEntry::Spec(spec)) => self.spec_config(spec),
            None => self.spec_config(reference),
        }
    }

    /// A model written out in full. Nothing here is inferred from the name,
    /// so the per-wire-format `[models.openai]` / `[models.local]` tables do
    /// not apply: an entry that says where it goes has already said it.
    fn declared_config(&self, declared: &DeclaredModel) -> ModelConfig {
        ModelConfig {
            provider: declared.api.clone(),
            model: declared.name.clone(),
            endpoint: declared.endpoint.clone(),
            api_key: None,
            api_key_env: declared.api_key_env.clone(),
            max_output_tokens: declared.max_output_tokens.unwrap_or(4096),
            timeout_secs: declared
                .timeout_secs
                .or(self.model_timeout_secs)
                .unwrap_or(kora_models::DEFAULT_TIMEOUT_SECS),
            max_retries: declared
                .max_retries
                .or(self.model_max_retries)
                .unwrap_or(kora_models::DEFAULT_MAX_RETRIES),
            deadline: None,
        }
    }

    /// The `provider:model` shorthand, where the endpoint and the key
    /// variable come from the per-format tables instead of the entry.
    fn spec_config(&self, spec: &str) -> Result<ModelConfig, ModelError> {
        let mut model = kora_models::parse_model_spec(spec)?;
        if let Some(secs) = self.model_timeout_secs {
            model.timeout_secs = secs;
        }
        if let Some(times) = self.model_max_retries {
            model.max_retries = times;
        }
        match model.provider {
            kora_models::Provider::OpenAI => {
                if let Some(max) = self.openai_max_output_tokens {
                    model.max_output_tokens = max;
                }
                model.endpoint.clone_from(&self.openai_endpoint);
                model.api_key_env.clone_from(&self.openai_api_key_env);
            }
            kora_models::Provider::Ollama => {
                model.endpoint.clone_from(&self.local_endpoint);
            }
        }
        Ok(model)
    }

    /// The model used when a call site names none.
    pub fn default_model(&self) -> Result<ModelConfig, ModelError> {
        match self.models.get("default") {
            Some(_) => self.resolve_model("default"),
            None => Err(ModelError::new(
                "no default model configured — add `[models] default = \"local:llama3.1:8b\"` to kora.toml",
            )),
        }
    }
}

/// Read one `[models]` entry written out in full. Once a table declares itself
/// as a model, malformed fields are errors rather than missing configuration.
fn declared_model(role: &str, table: &toml::value::Table) -> Result<DeclaredModel, ModelError> {
    let name = table
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ModelError::new(format!("models.{role}.name must be a string")))?;
    let name = name.trim();
    if name.is_empty() {
        return Err(ModelError::new(format!(
            "models.{role}.name must not be empty"
        )));
    }
    // Two request shapes exist, so an unrecognised one is a mistake worth
    // failing on rather than a silent fall back to the other.
    let api = match table.get("api") {
        Some(toml::Value::String(api)) if api == "ollama" => Provider::Ollama,
        Some(toml::Value::String(api)) if api == "openai" => Provider::OpenAI,
        None => Provider::OpenAI,
        // The OpenAI wire format is what nearly every hosted provider and
        // gateway speaks, so it is what an entry gets when it says nothing.
        Some(toml::Value::String(other)) => {
            return Err(ModelError::new(format!(
                "models.{role}.api is `{other}`; expected `openai` or `ollama`"
            )));
        }
        Some(_) => {
            return Err(ModelError::new(format!(
                "models.{role}.api must be the string `openai` or `ollama`"
            )));
        }
    };
    let optional_string = |field: &str| match table.get(field) {
        Some(toml::Value::String(value)) => Ok(Some(value.clone())),
        None => Ok(None),
        Some(_) => Err(ModelError::new(format!(
            "models.{role}.{field} must be a string"
        ))),
    };
    let optional_integer = |field: &str| match table.get(field) {
        Some(toml::Value::Integer(value)) => Ok(Some(*value)),
        None => Ok(None),
        Some(_) => Err(ModelError::new(format!(
            "models.{role}.{field} must be an integer"
        ))),
    };
    Ok(DeclaredModel {
        name: name.to_string(),
        endpoint: optional_string("endpoint")?,
        api_key_env: optional_string("api_key_env")?,
        api,
        max_output_tokens: optional_integer("max_output_tokens")?
            .map(|v| v.clamp(1, u32::MAX as i64) as u32),
        timeout_secs: optional_integer("timeout_secs")?.map(|v| v.clamp(1, 3600) as u64),
        max_retries: optional_integer("max_retries")?.map(|v| v.clamp(0, 10) as u32),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[models]
default = "local:llama3.1:8b"
smart = "openai:gpt-4o"

[models.openai]
max_output_tokens = 2048

[models.local]
endpoint = "http://box:11434"

[budget]
program_max_tokens = 2_000_000
"#;

    #[test]
    fn parses_models_and_settings() {
        let c = Config::parse(SAMPLE).unwrap();
        assert_eq!(c.resolve_model("default").unwrap().model, "llama3.1:8b");
        assert_eq!(c.resolve_model("smart").unwrap().model, "gpt-4o");
        assert_eq!(c.openai_max_output_tokens, Some(2048));
        assert_eq!(c.local_endpoint.as_deref(), Some("http://box:11434"));
    }

    /// A vision call on a local model runs far past the text-only default,
    /// and a timeout that fires on ordinary work is worse than no default.
    #[test]
    fn model_timeout_is_configurable_and_clamped() {
        let c = Config::parse("[models]\ndefault = \"local:m\"\ntimeout_secs = 900\n").unwrap();
        assert_eq!(c.model_timeout_secs, Some(900));
        assert_eq!(c.default_model().unwrap().timeout_secs, 900);

        // `timeout_secs` is a setting, not a model alias.
        assert!(!c.models.contains_key("timeout_secs"));

        // Zero is how "wait forever" sneaks back in.
        let zero = Config::parse("[models]\ndefault = \"local:m\"\ntimeout_secs = 0\n").unwrap();
        assert_eq!(zero.model_timeout_secs, Some(1));
    }

    #[test]
    fn the_default_timeout_survives_an_unset_config() {
        let c = Config::parse(SAMPLE).unwrap();
        assert_eq!(c.model_timeout_secs, None);
        assert_eq!(
            c.default_model().unwrap().timeout_secs,
            kora_models::DEFAULT_TIMEOUT_SECS
        );
    }

    #[test]
    fn alias_resolution_applies_settings() {
        let c = Config::parse(SAMPLE).unwrap();
        let smart = c.resolve_model("smart").unwrap();
        assert_eq!(smart.model, "gpt-4o");
        assert_eq!(smart.max_output_tokens, 2048);

        let local = c.default_model().unwrap();
        assert_eq!(local.model, "llama3.1:8b");
        assert_eq!(local.endpoint.as_deref(), Some("http://box:11434"));
    }

    #[test]
    fn direct_spec_works_without_alias() {
        let c = Config::parse(SAMPLE).unwrap();
        let m = c.resolve_model("openai:gpt-4o-mini").unwrap();
        assert_eq!(m.model, "gpt-4o-mini");
    }

    #[test]
    fn an_openai_compatible_gateway_is_configured_not_hardcoded() {
        // OpenRouter, Groq, Together, a local vLLM: all speak the OpenAI wire
        // format, so they are the `openai` provider with a different base URL
        // and a key of their own. Nothing about them is a new provider.
        let c = Config::parse(
            r#"
[models]
default = "openai:anthropic/claude-sonnet-4"

[models.openai]
endpoint = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
"#,
        )
        .unwrap();
        let m = c.default_model().unwrap();
        assert_eq!(m.model, "anthropic/claude-sonnet-4");
        assert_eq!(m.endpoint.as_deref(), Some("https://openrouter.ai/api/v1"));
        assert_eq!(m.api_key_env.as_deref(), Some("OPENROUTER_API_KEY"));
    }

    #[test]
    fn openai_without_a_gateway_keeps_its_defaults() {
        let c = Config::parse(SAMPLE).unwrap();
        let m = c.resolve_model("smart").unwrap();
        assert_eq!(m.endpoint, None);
        assert_eq!(m.api_key_env, None);
    }

    #[test]
    fn a_model_written_out_in_full_keeps_its_name_verbatim() {
        // The name is whatever the provider's docs print. Kora does not
        // parse it, so a slash, a colon, or both survive untouched.
        let c = Config::parse(
            r#"
[models]
default = { name = "openrouter/free", endpoint = "https://openrouter.ai/api/v1", api_key_env = "OPENROUTER_API_KEY" }
vision  = { name = "qwen2.5vl:3b", endpoint = "http://localhost:11434", api = "ollama" }
"#,
        )
        .unwrap();

        let d = c.default_model().unwrap();
        assert_eq!(d.model, "openrouter/free");
        assert_eq!(d.provider, Provider::OpenAI);
        assert_eq!(d.endpoint.as_deref(), Some("https://openrouter.ai/api/v1"));
        assert_eq!(d.api_key_env.as_deref(), Some("OPENROUTER_API_KEY"));

        let v = c.resolve_model("vision").unwrap();
        assert_eq!(v.model, "qwen2.5vl:3b");
        assert_eq!(v.provider, Provider::Ollama);
        assert_eq!(v.endpoint.as_deref(), Some("http://localhost:11434"));
    }

    #[test]
    fn a_declared_model_with_an_unknown_api_is_refused() {
        let error = Config::parse("[models]\ndefault = { name = \"m\", api = \"opneai\" }\n")
            .expect_err("a provider typo must not choose a different wire format");
        assert!(error.to_string().contains("expected `openai` or `ollama`"));
    }

    #[test]
    fn a_declared_model_with_a_malformed_field_is_refused() {
        let error = Config::parse("[models]\ndefault = { name = \"m\", endpoint = 3 }\n")
            .expect_err("a malformed endpoint must not be treated as absent");
        assert!(error.to_string().contains("endpoint must be a string"));
    }

    #[test]
    fn two_gateways_coexist_in_one_project() {
        // The reason an endpoint belongs to the entry rather than to the
        // provider: one program routing two roles at two services.
        let c = Config::parse(
            r#"
[models]
smart = { name = "gpt-4o", api_key_env = "OPENAI_API_KEY" }
cheap = { name = "openrouter/free", endpoint = "https://openrouter.ai/api/v1", api_key_env = "OPENROUTER_API_KEY" }
"#,
        )
        .unwrap();
        assert_eq!(c.resolve_model("smart").unwrap().endpoint, None);
        assert_eq!(
            c.resolve_model("cheap").unwrap().endpoint.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
    }

    #[test]
    fn a_model_with_no_key_variable_configures_no_key() {
        // A local server that wants no auth is a setup, not an oversight.
        let c = Config::parse(
            r#"
[models]
default = { name = "Qwen/Qwen2.5-7B", endpoint = "http://localhost:8000/v1" }
"#,
        )
        .unwrap();
        let m = c.default_model().unwrap();
        assert_eq!(m.api_key_env, None);
        assert_eq!(m.api_key, None);
    }

    #[test]
    fn the_old_string_form_still_resolves() {
        let c = Config::parse(SAMPLE).unwrap();
        let m = c.resolve_model("smart").unwrap();
        assert_eq!(m.model, "gpt-4o");
        assert_eq!(m.provider, Provider::OpenAI);
        assert_eq!(m.max_output_tokens, 2048);
    }

    #[test]
    fn unknown_sections_are_ignored() {
        // The [budget] block above is not an error today.
        assert!(Config::parse(SAMPLE).is_ok());
    }

    #[test]
    fn missing_default_is_a_clear_error() {
        let c = Config::parse("[models]\nsmart = \"openai:gpt-4o\"\n").unwrap();
        let err = c.default_model().unwrap_err();
        assert!(err.message.contains("no default model"), "{}", err.message);
    }

    #[test]
    fn classified_output_placeholder_defaults_and_can_be_configured() {
        assert_eq!(
            Config::parse("").unwrap().classified_placeholder,
            "__CLASSIFIED__"
        );
        let config = Config::parse("[output]\nclassified_placeholder = \"[secret]\"\n").unwrap();
        assert_eq!(config.classified_placeholder, "[secret]");
    }
}
