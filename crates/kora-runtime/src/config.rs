//! `kora.toml` loading.
//!
//! Only the parts Phase 2 needs: model definitions and their settings.
//! Budgets, sinks, and telemetry sections are parsed but unused until their
//! phases (see DECISIONS.md) — unknown keys are ignored, never an error.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kora_models::{ModelConfig, ModelError};

use crate::label::SinkPolicy;

#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Named model aliases, e.g. "default" -> "local:llama3.1:8b".
    pub models: HashMap<String, String>,
    /// Per-provider settings.
    pub openai_max_output_tokens: Option<u32>,
    pub local_endpoint: Option<String>,
    /// Which sinks may receive which labels, from `[sinks]`.
    pub sinks: SinkPolicy,
    /// `[http] allow_private` — permit loopback and private address ranges.
    pub http_allow_private: bool,
    /// `[http] timeout_secs` — applied to every request; there is no "off".
    pub http_timeout_secs: u64,
    /// `[telemetry]` settings.
    pub telemetry: crate::telemetry::Config,
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
            ..Default::default()
        };
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
}
