//! Record/replay of model calls.
//!
//! Cassettes make CI deterministic and free (DECISIONS.md): the same program
//! re-runs without touching a provider. Keys cover everything that could
//! change an answer — call site, model, prompt, and resolved input data.
//!
//! Format is human-readable JSON on disk so cassettes diff in pull requests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What the runtime does with model calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Call the provider every time; do not read or write cassettes.
    #[default]
    Live,
    /// Call the provider, then append each interaction to the cassette.
    Record,
    /// Never call a provider. A missing entry is an error, not a fallback.
    Replay,
}

/// One recorded model interaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub key: String,
    /// Kept for human readability when diffing; not part of the key.
    pub site: String,
    pub model: String,
    pub prompt: String,
    pub data: String,
    /// Fingerprint of the images sent with the call. Empty for a text-only
    /// call, and defaulted so cassettes recorded before images existed still
    /// load.
    #[serde(default)]
    pub media: String,
    pub outcome: RecordedOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordedOutcome {
    Ok {
        fields: serde_json::Map<String, serde_json::Value>,
        tokens_in: u64,
        tokens_out: u64,
    },
    Uncertain {
        reason: String,
        tokens_in: u64,
        tokens_out: u64,
    },
    /// The provider did not answer.
    ///
    /// Journaled, never recorded to a cassette. A durable run that took the
    /// failure branch must take it again on resume, or the replay diverges
    /// from the history it is replaying. A cassette is the opposite case: it
    /// is a fixture for a test suite, and freezing one afternoon's outage
    /// into it would make every later run fail for a reason that no longer
    /// exists.
    Failed {
        reason: String,
        tokens_in: u64,
        tokens_out: u64,
    },
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CassetteFile {
    entries: Vec<Entry>,
}

pub struct Cassette {
    pub mode: Mode,
    path: PathBuf,
    entries: HashMap<String, Entry>,
    /// Insertion-ordered keys, so a re-recorded file keeps a stable layout.
    order: Vec<String>,
    dirty: bool,
}

impl Cassette {
    /// Open (or start) the cassette for a program file.
    pub fn open(mode: Mode, program_path: &Path) -> Cassette {
        let path = cassette_path(program_path);
        let mut entries = HashMap::new();
        let mut order = Vec::new();
        if mode != Mode::Live {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(file) = serde_json::from_str::<CassetteFile>(&text) {
                    for entry in file.entries {
                        order.push(entry.key.clone());
                        entries.insert(entry.key.clone(), entry);
                    }
                }
            }
        }
        Cassette {
            mode,
            path,
            entries,
            order,
            dirty: false,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.get(key)
    }

    pub fn insert(&mut self, entry: Entry) {
        if !self.entries.contains_key(&entry.key) {
            self.order.push(entry.key.clone());
        }
        self.entries.insert(entry.key.clone(), entry);
        self.dirty = true;
    }

    /// Write the cassette back to disk (no-op unless recording changed it).
    pub fn save(&self) -> std::io::Result<()> {
        if self.mode != Mode::Record || !self.dirty {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = CassetteFile {
            entries: self
                .order
                .iter()
                .filter_map(|k| self.entries.get(k).cloned())
                .collect(),
        };
        let text = serde_json::to_string_pretty(&file)?;
        std::fs::write(&self.path, format!("{text}\n"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// `foo.ko` -> `cassettes/foo.json` beside the program.
fn cassette_path(program_path: &Path) -> PathBuf {
    let stem = program_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "program".to_string());
    program_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("cassettes")
        .join(format!("{stem}.json"))
}

/// Stable key for one call: everything that could change the answer.
///
/// A call with no images hashes the same four fields it always did, so
/// cassettes recorded before images existed keep replaying. The media field
/// is *absent* rather than empty for those calls — appending an empty field
/// would still change the hash and silently invalidate every committed
/// cassette in every project.
pub fn key_for(site: &str, model: &str, prompt: &str, data: &str, media: &str) -> String {
    // FNV-1a over the joined fields: short, stable across runs and platforms,
    // and good enough since collisions only affect a local cache.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut parts = vec![site, model, prompt, data];
    if !media.is_empty() {
        parts.push(media);
    }
    for part in parts {
        for byte in part.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Fingerprint the images attached to a call.
///
/// Keyed on the bytes rather than the path: re-recording after a file moves
/// should still hit the cassette, and editing the image behind an unchanged
/// path must miss it. A path-keyed cassette gets both of those backwards.
pub fn media_key(images: &[(&str, &[u8])]) -> String {
    images
        .iter()
        .map(|(mime, bytes)| format!("{mime}:{}", fnv1a(bytes)))
        .collect::<Vec<_>>()
        .join(",")
}

fn fnv1a(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(key: &str) -> Entry {
        Entry {
            key: key.to_string(),
            site: "demo.ko:12".into(),
            model: "local:llama3.1:8b".into(),
            prompt: "summarize".into(),
            data: "{\"a\":1}".into(),
            media: String::new(),
            outcome: RecordedOutcome::Ok {
                fields: serde_json::Map::new(),
                tokens_in: 5,
                tokens_out: 7,
            },
        }
    }

    #[test]
    fn key_is_stable_and_sensitive() {
        let a = key_for("f.ko:1", "m", "p", "d", "");
        assert_eq!(a, key_for("f.ko:1", "m", "p", "d", ""));
        assert_ne!(a, key_for("f.ko:2", "m", "p", "d", ""));
        assert_ne!(a, key_for("f.ko:1", "other", "p", "d", ""));
        assert_ne!(a, key_for("f.ko:1", "m", "other", "d", ""));
        assert_ne!(a, key_for("f.ko:1", "m", "p", "other", ""));
        assert_ne!(a, key_for("f.ko:1", "m", "p", "d", "image/png:1"));
    }

    #[test]
    fn key_has_no_field_boundary_ambiguity() {
        // Without a separator these two would hash identically.
        assert_ne!(
            key_for("ab", "c", "", "", ""),
            key_for("a", "bc", "", "", "")
        );
    }

    /// Committed cassettes are the whole point of replay being free, so the
    /// key for a text-only call must not drift when a new field is added to
    /// the key. This hash comes from a cassette recorded before images
    /// existed; if it changes, every project's cassettes go stale at once.
    #[test]
    fn a_text_only_key_never_drifts() {
        assert_eq!(
            key_for(
                "examples/01_expense_check.ko:19",
                "ollama:qwen3:8b",
                "extract the expense; policy_violation is True if a single meal is over $200",
                "\"AWS invoice, $4200, cloud hosting for production\"",
                "",
            ),
            "68d6915cc49a32af"
        );
    }

    /// A cassette must miss when the picture changes, even though the call
    /// site, prompt, and file path are all identical.
    #[test]
    fn media_key_follows_the_bytes() {
        let png = b"\x89PNG one".as_slice();
        let edited = b"\x89PNG two".as_slice();
        assert_eq!(
            media_key(&[("image/png", png)]),
            media_key(&[("image/png", png)])
        );
        assert_ne!(
            media_key(&[("image/png", png)]),
            media_key(&[("image/png", edited)])
        );
        // Order is part of the call: two receipts swapped is a different ask.
        assert_ne!(
            media_key(&[("image/png", png), ("image/png", edited)]),
            media_key(&[("image/png", edited), ("image/png", png)])
        );
        assert_eq!(media_key(&[]), "");
    }

    #[test]
    fn cassette_path_layout() {
        let p = cassette_path(Path::new("/tmp/proj/demo.ko"));
        assert!(p.ends_with("cassettes/demo.json"), "{p:?}");
    }

    #[test]
    fn round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!("kora-cassette-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("demo.ko");

        let mut recording = Cassette::open(Mode::Record, &program);
        recording.insert(sample_entry("k1"));
        recording.save().unwrap();

        let replaying = Cassette::open(Mode::Replay, &program);
        let entry = replaying.get("k1").expect("entry should replay");
        assert_eq!(entry.site, "demo.ko:12");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn live_mode_ignores_existing_cassette() {
        let dir = std::env::temp_dir().join(format!("kora-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("demo.ko");

        let mut recording = Cassette::open(Mode::Record, &program);
        recording.insert(sample_entry("k1"));
        recording.save().unwrap();

        let live = Cassette::open(Mode::Live, &program);
        assert!(live.get("k1").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_is_noop_outside_record_mode() {
        let dir = std::env::temp_dir().join(format!("kora-noop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("demo.ko");

        let mut replaying = Cassette::open(Mode::Replay, &program);
        replaying.insert(sample_entry("k1"));
        replaying.save().unwrap();
        assert!(
            !replaying.path().exists(),
            "replay must not write cassettes"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
