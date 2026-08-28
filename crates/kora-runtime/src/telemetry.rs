//! OpenTelemetry tracing.
//!
//! The runtime already knows everything worth tracing — it owns every model
//! call, every agent, every budget meter, and every declassification — so
//! spans are produced by the scheduler rather than by hand-written
//! instrumentation that drifts from the code.
//!
//! Two things here are unusual and deliberate.
//!
//! **The exporter is a labeled sink.** Prompt text leaking into an
//! observability vendor is a real, current incident category. Here a
//! classified value cannot become a span attribute, because the same label
//! machinery that guards a model call guards the exporter.
//!
//! **No SDK.** OTLP over HTTP is a JSON POST; emitting it directly keeps an
//! async runtime and a large dependency tree out of a synchronous
//! interpreter. Attribute names follow the OTel GenAI semantic conventions,
//! so existing dashboards understand the output without translation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value as J};

use crate::label::Label;

/// How much detail to export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Level {
    Off,
    /// Agents only.
    Agents,
    /// Agents and model calls.
    #[default]
    Calls,
    /// Adds prompts and results, subject to redaction.
    Full,
}

impl Level {
    pub fn parse(name: &str) -> Level {
        match name {
            "off" => Level::Off,
            "agents" => Level::Agents,
            "full" => Level::Full,
            _ => Level::Calls,
        }
    }

    fn records_calls(self) -> bool {
        matches!(self, Level::Calls | Level::Full)
    }
}

/// Where spans go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exporter {
    /// Append to a local file. The zero-configuration default: observability
    /// with no infrastructure to stand up first.
    File(String),
    /// POST to an OTLP/HTTP collector.
    Otlp(String),
    None,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub level: Level,
    pub exporter: Exporter,
    /// Refuse to export values carrying a label. On by default: telemetry is
    /// an export like any other.
    pub redact: bool,
    pub service_name: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            level: Level::Calls,
            exporter: Exporter::None,
            redact: true,
            service_name: "kora".to_string(),
        }
    }
}

/// A span attribute: key and value.
type Attribute = (String, J);

/// A timestamped event attached to a span.
type Event = (String, u128, Vec<Attribute>);

/// One recorded span.
#[derive(Debug, Clone)]
pub struct Span {
    trace_id: String,
    span_id: String,
    parent: Option<String>,
    name: String,
    start_nanos: u128,
    end_nanos: u128,
    attributes: Vec<Attribute>,
    events: Vec<Event>,
    error: Option<String>,
}

/// A span that is still open.
pub struct Active {
    span_id: String,
    parent: Option<String>,
    name: String,
    start_nanos: u128,
    attributes: Vec<Attribute>,
    events: Vec<Event>,
}

/// Collects spans for one program run.
#[derive(Debug)]
pub struct Tracer {
    config: Config,
    trace_id: String,
    finished: Mutex<Vec<Span>>,
    counter: AtomicU64,
}

impl Tracer {
    pub fn new(config: Config) -> Tracer {
        Tracer {
            trace_id: random_hex(16),
            config,
            finished: Mutex::new(Vec::new()),
            counter: AtomicU64::new(0),
        }
    }

    /// A tracer that records nothing, for ordinary runs.
    pub fn disabled() -> Tracer {
        Tracer::new(Config {
            level: Level::Off,
            exporter: Exporter::None,
            ..Default::default()
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.config.level != Level::Off && self.config.exporter != Exporter::None
    }

    pub fn level(&self) -> Level {
        self.config.level
    }

    pub fn records_calls(&self) -> bool {
        self.is_enabled() && self.config.level.records_calls()
    }

    /// Open a span. The caller finishes it with [`Tracer::end`].
    pub fn start(&self, name: &str, parent: Option<String>) -> Active {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        Active {
            // Unique per span without pulling in a random number generator.
            span_id: format!("{:016x}", mix(n, now_nanos() as u64)),
            parent,
            name: name.to_string(),
            start_nanos: now_nanos(),
            attributes: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn end(&self, span: Active, error: Option<String>) {
        let finished = Span {
            trace_id: self.trace_id.clone(),
            span_id: span.span_id,
            parent: span.parent,
            name: span.name,
            start_nanos: span.start_nanos,
            end_nanos: now_nanos(),
            attributes: span.attributes,
            events: span.events,
            error,
        };
        self.finished
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(finished);
    }

    /// Attach an attribute, refusing anything labeled.
    ///
    /// Returns false when the value was withheld, so a caller can substitute
    /// a placeholder rather than silently dropping the field.
    pub fn set(&self, span: &mut Active, key: &str, value: J, label: Label) -> bool {
        if self.config.redact && !label.is_plain() {
            span.attributes
                .push((key.to_string(), json!(format!("<{}>", label.name()))));
            return false;
        }
        span.attributes.push((key.to_string(), value));
        true
    }

    /// Attach an attribute that carries no data from the program.
    pub fn set_plain(&self, span: &mut Active, key: &str, value: J) {
        span.attributes.push((key.to_string(), value));
    }

    pub fn event(&self, span: &mut Active, name: &str, attributes: Vec<Attribute>) {
        span.events
            .push((name.to_string(), now_nanos(), attributes));
    }

    pub fn span_id_of(span: &Active) -> String {
        span.span_id.clone()
    }

    /// Write everything collected to the configured destination.
    pub fn flush(&self) -> Result<(), String> {
        if !self.is_enabled() {
            return Ok(());
        }
        let spans = self.finished.lock().unwrap_or_else(|e| e.into_inner());
        if spans.is_empty() {
            return Ok(());
        }
        let payload = self.to_otlp(&spans);
        match &self.config.exporter {
            Exporter::File(path) => {
                let text = serde_json::to_string_pretty(&payload)
                    .map_err(|e| format!("could not encode spans: {e}"))?;
                // Create the directory: a configured path may point somewhere
                // that does not exist yet, and losing a trace to that is a
                // silly way to lose one.
                if let Some(parent) = std::path::Path::new(path).parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
                    }
                }
                std::fs::write(path, format!("{text}\n"))
                    .map_err(|e| format!("could not write {path}: {e}"))
            }
            Exporter::Otlp(endpoint) => {
                let url = format!("{}/v1/traces", endpoint.trim_end_matches('/'));
                ureq::AgentBuilder::new()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                    .post(&url)
                    .set("Content-Type", "application/json")
                    .send_json(payload)
                    .map(|_| ())
                    .map_err(|e| format!("could not send spans to {url}: {e}"))
            }
            Exporter::None => Ok(()),
        }
    }

    /// OTLP/HTTP JSON. The shape any collector already understands.
    fn to_otlp(&self, spans: &[Span]) -> J {
        json!({
            "resourceSpans": [{
                "resource": {
                    "attributes": [attribute("service.name", json!(self.config.service_name))]
                },
                "scopeSpans": [{
                    "scope": { "name": "kora", "version": env!("CARGO_PKG_VERSION") },
                    "spans": spans.iter().map(|s| self.span_json(s)).collect::<Vec<_>>()
                }]
            }]
        })
    }

    fn span_json(&self, span: &Span) -> J {
        let mut out = json!({
            "traceId": span.trace_id,
            "spanId": span.span_id,
            "name": span.name,
            "kind": 1,
            "startTimeUnixNano": span.start_nanos.to_string(),
            "endTimeUnixNano": span.end_nanos.to_string(),
            "attributes": span
                .attributes
                .iter()
                .map(|(k, v)| attribute(k, v.clone()))
                .collect::<Vec<_>>(),
            "events": span
                .events
                .iter()
                .map(|(name, at, attrs)| json!({
                    "name": name,
                    "timeUnixNano": at.to_string(),
                    "attributes": attrs
                        .iter()
                        .map(|(k, v)| attribute(k, v.clone()))
                        .collect::<Vec<_>>()
                }))
                .collect::<Vec<_>>(),
        });
        if let Some(parent) = &span.parent {
            out["parentSpanId"] = json!(parent);
        }
        out["status"] = match &span.error {
            Some(message) => json!({ "code": 2, "message": message }),
            None => json!({ "code": 1 }),
        };
        out
    }

    /// A short human-readable summary, for `kora trace last`.
    pub fn summary(&self) -> String {
        let spans = self.finished.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = String::new();
        let mut sorted: Vec<&Span> = spans.iter().collect();
        sorted.sort_by_key(|s| s.start_nanos);
        for span in sorted {
            let millis = (span.end_nanos.saturating_sub(span.start_nanos)) / 1_000_000;
            let depth = if span.parent.is_some() { "  " } else { "" };
            let status = if span.error.is_some() { " FAILED" } else { "" };
            out.push_str(&format!("{depth}{:<28} {millis:>6}ms{status}\n", span.name));
        }
        out
    }
}

/// OTLP encodes attribute values by type.
fn attribute(key: &str, value: J) -> J {
    let encoded = match &value {
        J::String(s) => json!({ "stringValue": s }),
        J::Bool(b) => json!({ "boolValue": b }),
        J::Number(n) if n.is_i64() || n.is_u64() => json!({ "intValue": n.to_string() }),
        J::Number(n) => json!({ "doubleValue": n.as_f64().unwrap_or(0.0) }),
        other => json!({ "stringValue": other.to_string() }),
    };
    json!({ "key": key, "value": encoded })
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Span and trace ids need to be unique, not unpredictable, so a cheap mix of
/// a counter and the clock is enough and avoids a dependency.
fn mix(counter: u64, nanos: u64) -> u64 {
    let mut x = counter
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(nanos);
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn random_hex(bytes: usize) -> String {
    let mut out = String::with_capacity(bytes * 2);
    for i in 0..bytes.div_ceil(8) {
        out.push_str(&format!("{:016x}", mix(i as u64, now_nanos() as u64)));
    }
    out.truncate(bytes * 2);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracer(redact: bool) -> Tracer {
        Tracer::new(Config {
            level: Level::Full,
            exporter: Exporter::File("/dev/null".into()),
            redact,
            service_name: "test".into(),
        })
    }

    #[test]
    fn classified_values_never_become_attributes() {
        // Prompt text reaching an observability vendor is a real incident
        // category; the exporter is a sink like any other.
        let t = tracer(true);
        let mut span = t.start("analyze", None);
        let accepted = t.set(
            &mut span,
            "gen_ai.prompt",
            json!("the secret is hunter2"),
            Label::CLASSIFIED,
        );
        assert!(!accepted, "a classified value must be withheld");
        t.end(span, None);

        let text = serde_json::to_string(&t.to_otlp(&t.finished.lock().unwrap())).unwrap();
        assert!(!text.contains("hunter2"), "the secret leaked: {text}");
        assert!(text.contains("<classified>"), "a placeholder should remain");
    }

    #[test]
    fn unverified_values_are_withheld_too() {
        let t = tracer(true);
        let mut span = t.start("http", None);
        let accepted = t.set(
            &mut span,
            "body",
            json!("from the internet"),
            Label::UNVERIFIED,
        );
        assert!(!accepted);
        t.end(span, None);
        let text = serde_json::to_string(&t.to_otlp(&t.finished.lock().unwrap())).unwrap();
        assert!(text.contains("<unverified>"));
    }

    #[test]
    fn plain_values_are_exported() {
        let t = tracer(true);
        let mut span = t.start("analyze", None);
        assert!(t.set(
            &mut span,
            "gen_ai.request.model",
            json!("qwen3:8b"),
            Label::PUBLIC
        ));
        t.end(span, None);
        let text = serde_json::to_string(&t.to_otlp(&t.finished.lock().unwrap())).unwrap();
        assert!(text.contains("qwen3:8b"));
    }

    #[test]
    fn redaction_can_be_turned_off_deliberately() {
        let t = tracer(false);
        let mut span = t.start("analyze", None);
        assert!(t.set(&mut span, "prompt", json!("hunter2"), Label::CLASSIFIED));
        t.end(span, None);
        let text = serde_json::to_string(&t.to_otlp(&t.finished.lock().unwrap())).unwrap();
        assert!(text.contains("hunter2"), "an explicit opt-out should work");
    }

    #[test]
    fn spans_nest_through_parent_ids() {
        let t = tracer(true);
        let root = t.start("main", None);
        let root_id = Tracer::span_id_of(&root);
        let child = t.start("triage", Some(root_id.clone()));
        t.end(child, None);
        t.end(root, None);

        let spans = t.finished.lock().unwrap();
        let child = spans.iter().find(|s| s.name == "triage").unwrap();
        assert_eq!(child.parent.as_deref(), Some(root_id.as_str()));
        // One trace covers the whole run.
        assert!(spans.iter().all(|s| s.trace_id == spans[0].trace_id));
    }

    #[test]
    fn otlp_shape_is_what_a_collector_expects() {
        let t = tracer(true);
        let mut span = t.start("analyze", None);
        t.set_plain(&mut span, "gen_ai.usage.input_tokens", json!(120));
        t.event(
            &mut span,
            "declassify",
            vec![("sink".into(), json!("local_model"))],
        );
        t.end(span, None);

        let payload = t.to_otlp(&t.finished.lock().unwrap());
        let span = &payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(span["name"], "analyze");
        // Timestamps are strings in OTLP JSON, since they exceed 2^53.
        assert!(span["startTimeUnixNano"].is_string());
        // Integers are tagged, not bare.
        assert_eq!(span["attributes"][0]["value"]["intValue"], "120");
        assert_eq!(span["events"][0]["name"], "declassify");
        assert_eq!(span["status"]["code"], 1);
    }

    #[test]
    fn failures_are_marked_on_the_span() {
        let t = tracer(true);
        let span = t.start("analyze", None);
        t.end(span, Some("model refused".into()));
        let payload = t.to_otlp(&t.finished.lock().unwrap());
        let span = &payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(span["status"]["code"], 2);
        assert_eq!(span["status"]["message"], "model refused");
    }

    #[test]
    fn a_disabled_tracer_records_nothing() {
        let t = Tracer::disabled();
        assert!(!t.is_enabled());
        assert!(t.flush().is_ok());
    }

    #[test]
    fn span_ids_are_unique() {
        let t = tracer(true);
        let ids: std::collections::HashSet<String> = (0..200)
            .map(|_| Tracer::span_id_of(&t.start("s", None)))
            .collect();
        assert_eq!(ids.len(), 200, "span ids must not collide");
    }

    #[test]
    fn levels_parse_from_config() {
        assert_eq!(Level::parse("off"), Level::Off);
        assert_eq!(Level::parse("agents"), Level::Agents);
        assert_eq!(Level::parse("full"), Level::Full);
        // Anything unrecognised falls back to the useful default rather than
        // silently disabling telemetry.
        assert_eq!(Level::parse("nonsense"), Level::Calls);
    }
}
