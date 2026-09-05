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
        /// The answer as it was written, piece by piece, when the call
        /// streamed. Empty for an ordinary call.
        ///
        /// Recorded because the boundaries are observable: an `on token`
        /// handler that counts pieces, or writes a separator between them,
        /// gives a different answer for the same text delivered in one lump.
        /// Defaulted so cassettes recorded before streaming existed still
        /// load, replaying as a single piece.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        chunks: Vec<String>,
    },
    Uncertain {
        reason: String,
        tokens_in: u64,
        tokens_out: u64,
        /// Anything the stream had already written before the refusal was
        /// known. Usually empty -- the refusal field is sent first -- but a
        /// provider is free to change its mind after emitting.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        chunks: Vec<String>,
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
        /// The pieces that reached the program before the stream broke.
        ///
        /// A failed stream is still output: the characters are on the user's
        /// terminal and the handler has already run over them. Keeping them
        /// is what lets a resume replay that output silently instead of
        /// finding those lines where its own next effect should be.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        chunks: Vec<String>,
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
/// SHA-256 over the length-prefixed fields. The length prefixes are what make
/// the key unambiguous: without them a prompt ending in a fragment of the
/// next field would hash the same as the pair split differently, and the two
/// calls would share one recorded answer.
///
/// Cryptographic rather than a fast 64-bit mix, because a cassette is not
/// only a local cache. It is committed, diffed, and replayed in CI, so a
/// collision would not be a slow run -- it would be a test passing against
/// an answer recorded for a different question. That is worth a hash nobody
/// has to reason about the odds of.
pub fn key_for(site: &str, model: &str, prompt: &str, data: &str, media: &str) -> String {
    use sha2::Digest as _;

    let mut hasher = sha2::Sha256::new();
    for part in [site, model, prompt, data, media] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hex(&hasher.finalize())
}

/// Fingerprint the images attached to a call.
///
/// Keyed on the bytes rather than the path: re-recording after a file moves
/// should still hit the cassette, and editing the image behind an unchanged
/// path must miss it. A path-keyed cassette gets both of those backwards.
pub fn media_key(images: &[(&str, &[u8])]) -> String {
    images
        .iter()
        .map(|(mime, bytes)| format!("{mime}:{}", sha256(bytes)))
        .collect::<Vec<_>>()
        .join(",")
}

/// The key algorithm used before SHA-256, kept for lookup only.
///
/// Cassettes are committed files, and the image ones cannot be regenerated
/// without the model that recorded them. Recomputing a key from the fields
/// an entry stores would migrate the text-only ones, but not those: an
/// image's fingerprint is stored, and the bytes it was taken from are not.
/// So a miss on the current key falls back to this one, and everything
/// recorded from now on carries the stronger key. Nothing is written with
/// it.
pub fn legacy_key_for(site: &str, model: &str, prompt: &str, data: &str, media: &str) -> String {
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

/// The media fingerprint that goes with [`legacy_key_for`].
pub fn legacy_media_key(images: &[(&str, &[u8])]) -> String {
    images
        .iter()
        .map(|(mime, bytes)| format!("{mime}:{}", legacy_fnv1a(bytes)))
        .collect::<Vec<_>>()
        .join(",")
}

fn legacy_fnv1a(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex(&sha2::Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
                chunks: Vec::new(),
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
    /// key for a text-only call must not drift. This hash comes from a
    /// cassette recorded before images existed; if it changes, every
    /// project's cassettes go stale at once.
    #[test]
    fn the_legacy_key_still_matches_committed_cassettes() {
        assert_eq!(
            legacy_key_for(
                "examples/01_expense_check.ko:19",
                "ollama:qwen3:8b",
                "extract the expense; policy_violation is True if a single meal is over $200",
                "\"AWS invoice, $4200, cloud hosting for production\"",
                "",
            ),
            "68d6915cc49a32af"
        );
    }

    /// The same call under the current algorithm. Pinned for the same
    /// reason: a cassette recorded today has to replay tomorrow.
    #[test]
    fn the_current_key_never_drifts() {
        assert_eq!(
            key_for(
                "examples/01_expense_check.ko:19",
                "ollama:qwen3:8b",
                "extract the expense; policy_violation is True if a single meal is over $200",
                "\"AWS invoice, $4200, cloud hosting for production\"",
                "",
            ),
            "84c4e9df68fa1c5f781cd16d82a888ef1d5af74ff4e67353edf77f6de6bf5dcd"
        );
    }

    /// A key is a full SHA-256 digest, not a truncation of one. Truncating
    /// would put the collision odds back within reach of a large cassette
    /// set for no benefit -- the key is never typed by a person.
    #[test]
    fn a_key_is_a_whole_digest() {
        let key = key_for("f.ko:1", "m", "p", "d", "");
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The old algorithm is for reading existing files, never for writing
    /// new ones. If these ever agreed, the fallback would be pointless and
    /// the weaker hash would still be in the write path.
    #[test]
    fn the_two_algorithms_are_distinguishable() {
        assert_ne!(
            key_for("f.ko:1", "m", "p", "d", ""),
            legacy_key_for("f.ko:1", "m", "p", "d", "")
        );
    }

    /// The legacy media fingerprint has to keep matching what is committed,
    /// because an image cassette cannot be regenerated without the model
    /// that recorded it.
    #[test]
    fn the_legacy_media_fingerprint_still_matches() {
        assert_eq!(
            legacy_media_key(&[("image/png", b"\x89PNG one".as_slice())])
                .split(':')
                .count(),
            2
        );
        // The width is the tell: 16 hex characters for the old fingerprint,
        // 64 for the current one.
        let legacy = legacy_media_key(&[("image/png", b"\x89PNG one".as_slice())]);
        let current = media_key(&[("image/png", b"\x89PNG one".as_slice())]);
        assert_eq!(legacy.split(':').nth(1).unwrap().len(), 16);
        assert_eq!(current.split(':').nth(1).unwrap().len(), 64);
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
