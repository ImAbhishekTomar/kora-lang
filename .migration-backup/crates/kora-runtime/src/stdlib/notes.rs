//! `notes` — a durable run's own scratch space, made visible outside it.
//!
//! A long tool loop already keeps state in local variables — a plan, partial
//! results, things learned two turns ago — but a plain variable dies with the
//! interpreter and nothing outside the running process can read it. `notes`
//! is that same scratch space made durable (it survives a crash) and visible
//! (a different `kora` invocation can read it) without becoming a general
//! filesystem escape hatch: it is a single key-value store scoped to exactly
//! one identity, the current durable run, at `.kora/notes/<run-id>.json`.
//!
//! Two things distinguish it from a plain `fs.write`:
//!
//! - **Label propagation.** A classified value written with `notes.write`
//!   keeps its label coming back out of `notes.read` — the same transitivity
//!   a value already has crossing a function return or a `parallel for`
//!   boundary. A value read out of notes is additionally `unverified`, since
//!   the store is external to this evaluation, the same rule `fs.read`
//!   already follows.
//! - **Journaled reads.** `notes.write` goes straight to the file, live,
//!   every time — there is nothing to replay about it. But `notes.read` is
//!   journaled (`Effect::Memory`) the same way `time.now()` is: without that,
//!   a resumed run would see whatever the store holds at replay time, not
//!   what the live run actually read, which can differ if something else
//!   wrote to the same store meanwhile.
//!
//! Requires a durable run (`kora run --durable`) for the same reason
//! `ask_human` does: there is no run id, and so no store to address, without
//! one.

use std::collections::BTreeMap;

use kora_syntax::token::Span;

use super::json::{json_to_value, value_to_json};
use super::{err, ok};
use crate::interp::{Interpreter, RuntimeError};
use crate::label::{Label, Secrecy, Trust};
use crate::value::Value;

pub const EXPORTS: super::Exports = &[("read", read), ("write", write)];

/// One stored entry: the value as JSON, and the label it was written with.
#[derive(serde::Serialize, serde::Deserialize)]
struct Entry {
    value: serde_json::Value,
    secrecy: String,
    released: Option<String>,
}

fn store_path(interp: &Interpreter, span: Span) -> Result<std::path::PathBuf, RuntimeError> {
    let run_id = &interp.journal_run_id();
    if run_id.is_empty() {
        return Err(RuntimeError::new("notes needs a durable run", span)
            .with_hint("run with `kora run --durable <file.ko>`"));
    }
    Ok(crate::journal::notes_path(
        std::path::Path::new(&interp.program_name),
        run_id,
    ))
}

fn load_store(path: &std::path::Path) -> BTreeMap<String, Entry> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_store(path: &std::path::Path, store: &BTreeMap<String, Entry>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(store).unwrap_or_default();
    // Same partial-write defence as `fs.write`: a crash mid-write must never
    // leave a half-written store behind for the next run to trip over.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}

/// `notes.write(key, value) -> Ok(None) | Err(reason)`
fn write(interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let key = match args.first().map(|v| v.unlabeled()) {
        Some(Value::Str(s)) => s.to_string(),
        _ => return Err(RuntimeError::new("notes.write() needs a string key", span)),
    };
    let Some(value) = args.get(1) else {
        return Err(RuntimeError::new("notes.write() needs a value", span));
    };
    let Some(json) = value_to_json(value.unlabeled()) else {
        return Ok(err("notes.write() cannot store this value"));
    };
    let path = store_path(interp, span)?;
    let mut store = load_store(&path);
    let label = value.label();
    store.insert(
        key,
        Entry {
            value: json,
            secrecy: match label.secrecy {
                Secrecy::Classified => "classified".to_string(),
                Secrecy::Public => "public".to_string(),
            },
            released: label.released.map(|s| s.to_string()),
        },
    );
    match save_store(&path, &store) {
        Ok(()) => Ok(ok(Value::None)),
        Err(e) => Ok(err(format!("notes.write() failed: {e}"))),
    }
}

/// `notes.read(key, default) -> value`
///
/// `default` is positional, not `default=`: keyword arguments outside
/// `analyze()` are not accepted by the parser yet.
///
/// Unlike every other stdlib function here, this does not return `Ok`/`Err`:
/// a missing key is not a failure, it is simply the default, the same way a
/// dict lookup with a fallback is not.
fn read(interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let key = match args.first().map(|v| v.unlabeled()) {
        Some(Value::Str(s)) => s.to_string(),
        _ => return Err(RuntimeError::new("notes.read() needs a string key", span)),
    };
    let default = args.get(1).cloned().unwrap_or(Value::None);
    let path = store_path(interp, span)?;
    let site_key = key.clone();

    interp.journal_memory_read(&site_key, span, move || {
        let store = load_store(&path);
        match store.get(&key) {
            Some(entry) => {
                let secrecy = if entry.secrecy == "classified" {
                    Secrecy::Classified
                } else {
                    Secrecy::Public
                };
                let label = Label {
                    secrecy,
                    trust: Trust::Unverified,
                    released: entry.released.clone().map(|s| s.into_boxed_str()),
                };
                json_to_value(&entry.value).with_label(label)
            }
            None => default.clone(),
        }
    })
}
