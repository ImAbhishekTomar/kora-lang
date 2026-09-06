//! The checksum log: what a version has always meant.
//!
//! A lockfile protects a project after its own first fetch. It cannot help
//! with the fetch that creates it — at that moment there is nothing to check
//! against, so whatever the repository serves is what gets recorded. An
//! attacker who publishes a backdoor, waits for a few first-time fetches, and
//! deletes it again leaves no trace in anybody's lockfile.
//!
//! The log closes that window. The first time any commit is seen its hash is
//! recorded permanently; every fetch afterwards is checked against the record
//! rather than re-trusting the source. Publish-then-delete stops working,
//! because deleting the release does not delete the record, and republishing
//! under the same commit cannot produce different bytes without being caught.
//!
//! There are two logs and a fetch is checked against both. The project's own
//! `kora.sums` is committed, so it is reviewable in a diff and shared with
//! everyone who clones the repository. A machine-level log under the user's
//! home directory is shared across *every* project on that machine, so a
//! package fetched honestly in one project protects the next project that
//! reaches for it — which the lockfile, being per-project, cannot do.
//!
//! What this is not: a hosted transparency log. Two machines that have never
//! fetched the same package cannot cross-check each other, and a first fetch
//! by everyone at once is still a first fetch. Closing that needs a log
//! somebody runs. What is here narrows the window from "every project, every
//! time" to "the first time anyone on this machine, or in this repository,
//! ever saw it".
//!
//! Entries are append-only: an existing line is never rewritten, only read.
//! A disagreement is refused, never resolved.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One immutable record: this commit of this repository had these bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub url: String,
    pub commit: String,
    pub hash: String,
}

impl Entry {
    fn key(&self) -> String {
        format!("{} {}", self.url, self.commit)
    }
}

/// What checking a fetch against the log concluded.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Not seen before. Recording it fixes what this commit means from now on.
    New,
    /// Seen before, with the same bytes.
    Known,
    /// Seen before, with *different* bytes. The identity was reused.
    Conflict { recorded: String },
}

/// An append-only record of what each commit's contents were.
#[derive(Debug, Clone, Default)]
pub struct SumLog {
    entries: BTreeMap<String, Entry>,
    /// Entries added since loading, so a write appends rather than rewrites.
    added: Vec<Entry>,
}

impl SumLog {
    pub const FILE: &'static str = "kora.sums";

    pub fn at(root: &Path) -> Result<SumLog, String> {
        let path = Self::path(root);
        if !path.is_file() {
            return Ok(SumLog::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Ok(SumLog::parse(&text))
    }

    pub fn path(root: &Path) -> PathBuf {
        root.join(SumLog::FILE)
    }

    /// The machine-level log, shared by every project run by this user.
    ///
    /// `None` when there is no home directory to put it in, in which case
    /// only the project's own log applies.
    pub fn user_dir() -> Option<PathBuf> {
        let home = std::env::var_os("KORA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))?;
        Some(home.join(".kora"))
    }

    /// Load the project log and the machine log as one view.
    ///
    /// Records from either are believed, and a new record is written to both:
    /// the project's, so the team sees it in a diff, and the machine's, so
    /// the next project on this computer inherits it.
    pub fn shared(root: &Path) -> SumLog {
        let mut log = SumLog::at(root).unwrap_or_default();
        let Some(user) = SumLog::user_dir() else {
            return log;
        };
        let theirs = SumLog::at(&user).unwrap_or_default();
        for entry in theirs.entries.into_values() {
            // The project's own record wins on a disagreement: it is the one
            // committed alongside the code being built.
            log.entries.entry(entry.key()).or_insert(entry);
        }
        log
    }

    /// Append new records to the project log and to the machine log.
    pub fn append_shared(&mut self, root: &Path) -> Result<(), String> {
        let pending = self.added.clone();
        self.append(root)?;
        let Some(user) = SumLog::user_dir() else {
            return Ok(());
        };
        if pending.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(&user)
            .map_err(|e| format!("cannot create {}: {e}", user.display()))?;
        let mut theirs = SumLog::at(&user).unwrap_or_default();
        for entry in pending {
            theirs.record(&entry.url, &entry.commit, &entry.hash);
        }
        theirs.append(&user)
    }

    /// One record per line: `<repository> <commit> <hash>`.
    ///
    /// A line format rather than TOML, because the file only ever grows and a
    /// format that appends without reparsing keeps that honest.
    pub fn parse(text: &str) -> SumLog {
        let mut log = SumLog::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (Some(url), Some(commit), Some(hash)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let entry = Entry {
                url: url.to_string(),
                commit: commit.to_string(),
                hash: hash.to_string(),
            };
            // First writing wins. A later line claiming different bytes for
            // the same commit does not overwrite the record it disagrees
            // with — that is the whole point of the file.
            log.entries.entry(entry.key()).or_insert(entry);
        }
        log
    }

    /// What the log says about a fetch.
    pub fn check(&self, url: &str, commit: &str, hash: &str) -> Verdict {
        let key = format!("{url} {commit}");
        match self.entries.get(&key) {
            None => Verdict::New,
            Some(entry) if entry.hash == hash => Verdict::Known,
            Some(entry) => Verdict::Conflict {
                recorded: entry.hash.clone(),
            },
        }
    }

    /// Record a fetch. A commit already recorded is left exactly as it was.
    pub fn record(&mut self, url: &str, commit: &str, hash: &str) {
        let entry = Entry {
            url: url.to_string(),
            commit: commit.to_string(),
            hash: hash.to_string(),
        };
        let key = entry.key();
        if self.entries.contains_key(&key) {
            return;
        }
        self.entries.insert(key, entry.clone());
        self.added.push(entry);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn pending(&self) -> usize {
        self.added.len()
    }

    /// Append what is new, creating the file if it does not exist.
    ///
    /// Appending rather than rewriting is deliberate: a bug that rewrote the
    /// file could quietly drop history, and history is the only thing this
    /// file is for.
    pub fn append(&mut self, root: &Path) -> Result<(), String> {
        if self.added.is_empty() {
            return Ok(());
        }
        let path = Self::path(root);
        let fresh = !path.exists();
        let mut out = String::new();
        if fresh {
            out.push_str(
                "# Generated by kora, append-only. Committed, and never edited.\n\
                 #\n\
                 # Each line is what one commit's contents were the first time it was\n\
                 # seen. A later fetch that disagrees is refused rather than recorded.\n",
            );
        }
        // Sorted, so the bytes a parallel install appends do not depend on
        // which fetch finished first.
        self.added.sort_by_key(Entry::key);
        for entry in &self.added {
            out.push_str(&format!("{} {} {}\n", entry.url, entry.commit, entry.hash));
        }

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        file.write_all(out.as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        self.added.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_sighting_is_new_and_a_repeat_is_known() {
        let mut log = SumLog::default();
        assert_eq!(log.check("a/b", "c1", "sha256:x"), Verdict::New);
        log.record("a/b", "c1", "sha256:x");
        assert_eq!(log.check("a/b", "c1", "sha256:x"), Verdict::Known);
    }

    #[test]
    fn different_bytes_for_a_recorded_commit_are_a_conflict() {
        // The identity was reused. Publish-then-delete, or a rewritten
        // repository, both land here.
        let mut log = SumLog::default();
        log.record("a/b", "c1", "sha256:honest");
        assert_eq!(
            log.check("a/b", "c1", "sha256:evil"),
            Verdict::Conflict {
                recorded: "sha256:honest".to_string()
            }
        );
    }

    #[test]
    fn recording_twice_does_not_rewrite_the_first_record() {
        let mut log = SumLog::default();
        log.record("a/b", "c1", "sha256:honest");
        log.record("a/b", "c1", "sha256:evil");
        assert_eq!(
            log.check("a/b", "c1", "sha256:honest"),
            Verdict::Known,
            "the first record stands"
        );
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn a_later_line_never_overrides_an_earlier_one() {
        let log = SumLog::parse("a/b c1 sha256:honest\na/b c1 sha256:evil\n");
        assert_eq!(log.check("a/b", "c1", "sha256:honest"), Verdict::Known);
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn two_commits_of_one_repository_are_separate_records() {
        let mut log = SumLog::default();
        log.record("a/b", "c1", "sha256:one");
        log.record("a/b", "c2", "sha256:two");
        assert_eq!(log.len(), 2);
        assert_eq!(log.check("a/b", "c2", "sha256:two"), Verdict::Known);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let log = SumLog::parse("# a header\n\n  \na/b c1 sha256:x\n");
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn appending_keeps_what_was_already_there() {
        let dir = std::env::temp_dir().join("kora-sumlog-append");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut first = SumLog::default();
        first.record("a/b", "c1", "sha256:one");
        first.append(&dir).unwrap();

        let mut second = SumLog::at(&dir).unwrap();
        assert_eq!(second.check("a/b", "c1", "sha256:one"), Verdict::Known);
        second.record("a/c", "c2", "sha256:two");
        second.append(&dir).unwrap();

        let reloaded = SumLog::at(&dir).unwrap();
        assert_eq!(reloaded.len(), 2, "the earlier record survived");
        assert_eq!(reloaded.check("a/b", "c1", "sha256:one"), Verdict::Known);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn appending_nothing_does_not_create_a_file() {
        let dir = std::env::temp_dir().join("kora-sumlog-empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        SumLog::default().append(&dir).unwrap();
        assert!(!SumLog::path(&dir).is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
