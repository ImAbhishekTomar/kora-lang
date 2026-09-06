//! `kora.toml` loading.
//!
//! Only the parts Phase 2 needs: model definitions and their settings.
//! Budgets, sinks, and telemetry sections are parsed but unused until their
//! phases (see DECISIONS.md) — unknown keys are ignored, never an error.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kora_models::{ModelConfig, ModelError};

use crate::label::SinkPolicy;

#[derive(Debug, Clone)]
pub struct Config {
    /// Named model aliases, e.g. "default" -> "local:llama3.1:8b".
    pub models: HashMap<String, String>,
    /// Per-provider settings.
    pub openai_max_output_tokens: Option<u32>,
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
    pub fn discover(start: &Path) -> Config {
        let mut dir = if start.is_dir() {
            Some(start.to_path_buf())
        } else {
            start.parent().map(PathBuf::from)
        };
        while let Some(d) = dir {
            let candidate = d.join("kora.toml");
            if candidate.is_file() {
                if let Ok(text) = std::fs::read_to_string(&candidate) {
                    if let Ok(cfg) = Config::parse(&text) {
                        return cfg;
                    }
                }
            }
            dir = d.parent().map(PathBuf::from);
        }
        Config::default()
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
                        config.models.insert(key.clone(), spec.clone());
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
                    // `[models.openai]` / `[models.local]` sub-tables
                    toml::Value::Table(table) => {
                        if key == "openai" {
                            config.openai_max_output_tokens = table
                                .get("max_output_tokens")
                                .and_then(|v| v.as_integer())
                                .map(|v| v as u32);
                        } else if key == "local" {
                            config.local_endpoint = table
                                .get("endpoint")
                                .and_then(|v| v.as_str())
                                .map(str::to_string);
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(config)
    }

    /// Resolve a model reference: either an alias from `[models]` or a direct
    /// spec like `openai:gpt-4o`. Applies provider settings from config.
    pub fn resolve_model(&self, reference: &str) -> Result<ModelConfig, ModelError> {
        let spec = self
            .models
            .get(reference)
            .map(String::as_str)
            .unwrap_or(reference);
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
            Some(spec) => self.resolve_model(spec),
            None => Err(ModelError::new(
                "no default model configured — add `[models] default = \"local:llama3.1:8b\"` to kora.toml",
            )),
        }
    }
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
        assert_eq!(c.models.get("default").unwrap(), "local:llama3.1:8b");
        assert_eq!(c.models.get("smart").unwrap(), "openai:gpt-4o");
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
