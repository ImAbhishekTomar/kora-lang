//! Tracing, end to end through the interpreter.
//!
//! The test that matters most is the last one: a secret must not reach the
//! exported trace. Prompt text leaking into an observability vendor is a real
//! incident category, and the whole point of making the exporter a labeled
//! sink is that this cannot happen by accident.

use std::sync::Arc;

use kora_runtime::telemetry::{Config as TelemetryConfig, Exporter, Level};
use kora_runtime::{Config, Interpreter, Tracer};
use kora_syntax::parse;

const CONFIG: &str = r#"
[models]
default = "local:test-model"

[sinks]
local_model = { allow = ["classified"] }
"#;

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "kora-otel-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn trace_file(&self) -> String {
        self.0.join("trace.json").to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Run a program with tracing on, returning the exported trace as JSON.
fn trace(src: &str, level: Level, redact: bool) -> serde_json::Value {
    let scratch = Scratch::new("run");
    let path = scratch.trace_file();

    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut interp = Interpreter::new();
    let config = Config::parse(CONFIG).unwrap();
    interp.sinks = config.sinks.clone();
    interp.config = config;
    interp.program_name = "test.ko".into();
    interp.tracer = Arc::new(Tracer::new(TelemetryConfig {
        level,
        exporter: Exporter::File(path.clone()),
        redact,
        service_name: "test".into(),
    }));

    let _ = interp.run(&program);
    interp.tracer.flush().expect("spans should be written");

    let text = std::fs::read_to_string(&path).expect("a trace file");
    serde_json::from_str(&text).expect("valid OTLP json")
}

fn spans(payload: &serde_json::Value) -> Vec<serde_json::Value> {
    payload["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn names(payload: &serde_json::Value) -> Vec<String> {
    spans(payload)
        .iter()
        .map(|s| s["name"].as_str().unwrap_or("?").to_string())
        .collect()
}

const AGENT: &str = r#"agent worker(n: int) -> int:
    return n * 2

def main():
    worker(1)
    worker(2)
"#;

#[test]
fn agents_become_spans() {
    let payload = trace(AGENT, Level::Calls, true);
    let names = names(&payload);
    assert_eq!(
        names.iter().filter(|n| *n == "worker").count(),
        2,
        "{names:?}"
    );
}

#[test]
fn spans_share_one_trace_id() {
    let payload = trace(AGENT, Level::Calls, true);
    let ids: std::collections::HashSet<String> = spans(&payload)
        .iter()
        .map(|s| s["traceId"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(ids.len(), 1, "one run is one trace");
}

#[test]
fn nested_agents_nest_their_spans() {
    let src = r#"agent inner() -> int:
    return 1

agent outer() -> int:
    return inner()

def main():
    outer()
"#;
    let payload = trace(src, Level::Calls, true);
    let all = spans(&payload);
    let outer = all.iter().find(|s| s["name"] == "outer").unwrap();
    let inner = all.iter().find(|s| s["name"] == "inner").unwrap();
    assert_eq!(
        inner["parentSpanId"].as_str(),
        outer["spanId"].as_str(),
        "the inner agent should hang off the outer one"
    );
    assert!(
        outer.get("parentSpanId").is_none(),
        "the outer agent is a root span"
    );
}

#[test]
fn declassification_is_recorded() {
    let src = r#"def main():
    classified secret = "hunter2"
    declassify secret as plain for local_model:
        print(plain)
"#;
    let payload = trace(src, Level::Calls, true);
    let span = spans(&payload)
        .into_iter()
        .find(|s| s["name"] == "declassify")
        .expect("a declassify span");
    let attributes: Vec<(&str, &str)> = span["attributes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["key"].as_str().unwrap_or(""),
                a["value"]["stringValue"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert!(
        attributes.contains(&("kora.sink", "local_model")),
        "{attributes:?}"
    );
    assert!(
        attributes.iter().any(|(k, _)| *k == "kora.site"),
        "the site should be recorded, {attributes:?}"
    );
}

#[test]
fn a_failing_agent_marks_its_span() {
    let src = r#"agent boom() -> int:
    return 1 / 0

def main():
    boom()
"#;
    let payload = trace(src, Level::Calls, true);
    let span = spans(&payload)
        .into_iter()
        .find(|s| s["name"] == "boom")
        .expect("a span for the failing agent");
    assert_eq!(span["status"]["code"], 2);
    assert!(span["status"]["message"]
        .as_str()
        .unwrap_or("")
        .contains("division by zero"));
}

#[test]
fn the_agents_level_omits_model_calls() {
    let payload = trace(AGENT, Level::Agents, true);
    assert!(
        !names(&payload).iter().any(|n| n.starts_with("analyze")),
        "at `agents` level only agents are recorded"
    );
}

#[test]
fn tracing_off_writes_nothing() {
    let scratch = Scratch::new("off");
    let path = scratch.trace_file();
    let program = parse(AGENT).unwrap();
    let mut interp = Interpreter::new();
    interp.tracer = Arc::new(Tracer::new(TelemetryConfig {
        level: Level::Off,
        exporter: Exporter::File(path.clone()),
        redact: true,
        service_name: "test".into(),
    }));
    interp.run(&program).unwrap();
    interp.tracer.flush().unwrap();
    assert!(
        !std::path::Path::new(&path).exists(),
        "a disabled tracer should not create a file"
    );
}

#[test]
fn a_secret_never_reaches_the_exported_trace() {
    // The guarantee the whole design exists for. Even at `full`, where
    // prompts are exported, a classified value must not appear.
    let src = r#"type Assessment:
    band: str

agent review() -> str:
    classified salary = "165000-SECRET-VALUE"
    declassify salary as pay for local_model:
        a: Assessment = analyze(pay, "assess this")
    return "done"

def main():
    review()
"#;
    let payload = trace(src, Level::Full, true);
    let text = serde_json::to_string(&payload).unwrap();
    assert!(
        !text.contains("165000-SECRET-VALUE"),
        "the secret reached the trace:\n{text}"
    );
}
