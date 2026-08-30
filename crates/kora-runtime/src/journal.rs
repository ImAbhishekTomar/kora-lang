//! Durable execution: the journal.
//!
//! A tree-walking interpreter keeps its state in the Rust call stack, and a
//! Rust stack cannot be serialized. So durability here does not snapshot the
//! stack — it **replays**. Every effect (model call, tool result, human
//! answer, clock read) is appended to a journal on disk. To resume, the
//! program runs again from the top with its effects served from the journal:
//! execution fast-forwards through work already done and arrives at exactly
//! the point where it stopped, with locals rebuilt along the way.
//!
//! This is how Temporal and Restate do it, and it is why the earlier design
//! decisions matter: agents share nothing, so each one replays independently,
//! and there is no shared mutable heap whose interleaving would have to be
//! reproduced.
//!
//! The contract this places on programs: code between effects must be
//! deterministic. Anything non-deterministic must go through the journal.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where a journal entry sits in the execution tree.
///
/// `parallel for` branches run concurrently, so a single global sequence
/// would record a different order on every run. Each branch instead gets its
/// own path, and sequences are counted within it — so each branch replays
/// deterministically no matter how the threads interleaved.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Scope(pub Vec<usize>);

impl Scope {
    pub fn root() -> Scope {
        Scope(Vec::new())
    }

    pub fn child(&self, index: usize) -> Scope {
        let mut path = self.0.clone();
        path.push(index);
        Scope(path)
    }

    fn label(&self) -> String {
        if self.0.is_empty() {
            "main".to_string()
        } else {
            self.0
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join("/")
        }
    }
}

/// One recorded effect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub scope: Scope,
    /// Position within the scope; replay matches on this.
    pub seq: usize,
    /// `file:line`, checked on replay to catch a changed program.
    pub site: String,
    pub effect: Effect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Effect {
    /// A completed model call, stored as its JSON result.
    Model {
        outcome: crate::cassette::RecordedOutcome,
    },
    /// A tool the model asked for, and what it returned.
    Tool { name: String, result_json: String },
    /// A `with context(...)` pruning decision made before one tool-loop
    /// turn: which whole exchanges were retained.
    ///
    /// The estimate itself is a pure function of already-journaled tool
    /// results and the lexical thresholds in the program text, so replaying
    /// it would in principle recompute the same answer. It is journaled
    /// anyway, for the same reason a `Model` outcome is: the contract is
    /// that every effect on the request material is replayed from the
    /// journal, not recomputed, so a future change to the estimate (a real
    /// tokenizer, say) cannot silently change what an old run resumes to.
    Context {
        retained: Vec<RecordedExchange>,
        dropped: usize,
    },
    /// A question put to a person, and their answer once given.
    Human { question: String, answer: String },
    /// A line already shown to the user.
    ///
    /// Output is journaled so a resumed run continues the story instead of
    /// retelling it. Suppressing during catch-up alone is not enough: the
    /// journal cannot know whether the line before a crash was printed, so
    /// the only way to get exactly-once output is to record it.
    Output { text: String },
}

/// A journaled copy of one tool call and its result, kept by a
/// `with context(...)` pruning decision.
///
/// A plain struct rather than `kora_models::ToolExchange` directly: the
/// journal is a serialization boundary, and giving it its own type keeps
/// that boundary from reaching back into `kora-models`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedExchange {
    pub name: String,
    pub arguments_json: String,
    pub result_json: String,
}

/// A question waiting on a person.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingQuestion {
    pub scope: Scope,
    pub seq: usize,
    pub site: String,
    pub question: String,
    pub context: String,
}

/// What happened to a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    /// Waiting on a human answer. The process may exit; the run survives.
    Suspended,
    Completed,
    Failed,
}

/// The durable record of one program run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub program: String,
    pub status: RunStatus,
    pub entries: Vec<Entry>,
    /// Set while suspended.
    pub pending: Option<PendingQuestion>,
    pub created: String,
    pub updated: String,
}

impl Run {
    pub fn new(id: String, program: String) -> Run {
        let now = timestamp();
        Run {
            id,
            program,
            status: RunStatus::Running,
            entries: Vec::new(),
            pending: None,
            created: now.clone(),
            updated: now,
        }
    }
}

/// Live journal for one run: the recorded effects plus a replay cursor per
/// scope.
#[derive(Debug)]
pub struct Journal {
    run: Run,
    path: PathBuf,
    /// Recorded effects, indexed for replay lookup.
    recorded: HashMap<(Scope, usize), Entry>,
    /// Next sequence number to use in each scope.
    cursors: HashMap<Scope, usize>,
    /// True when this process must not perform new effects (unused today;
    /// reserved for a strict replay mode).
    pub durable: bool,
    /// How many recorded steps are still ahead of the cursors. While this is
    /// non-zero the program is re-executing work it already did, so output
    /// and other user-visible side effects are suppressed — otherwise a
    /// resumed run would print its whole history again.
    unconsumed: usize,
}

/// Outcome of asking the journal for an effect.
#[derive(Debug)]
pub enum Lookup {
    /// Already done on an earlier run; here is what it produced.
    Replayed(Effect),
    /// Not yet done. Perform it, then call `record`.
    Fresh { scope: Scope, seq: usize },
}

impl Journal {
    /// A journal that records nothing — the default for a plain `kora run`.
    pub fn disabled() -> Journal {
        Journal {
            run: Run::new(String::new(), String::new()),
            path: PathBuf::new(),
            recorded: HashMap::new(),
            cursors: HashMap::new(),
            durable: false,
            unconsumed: 0,
        }
    }

    /// Start or reopen a durable run.
    pub fn open(run: Run, path: PathBuf) -> Journal {
        let mut recorded = HashMap::new();
        for entry in &run.entries {
            recorded.insert((entry.scope.clone(), entry.seq), entry.clone());
        }
        let unconsumed = recorded.len();
        Journal {
            run,
            path,
            recorded,
            cursors: HashMap::new(),
            durable: true,
            unconsumed,
        }
    }

    pub fn run(&self) -> &Run {
        &self.run
    }

    pub fn is_durable(&self) -> bool {
        self.durable
    }

    /// True while the program is re-executing steps it already completed.
    /// Callers use this to stay silent during catch-up.
    pub fn is_replaying(&self) -> bool {
        self.unconsumed > 0
    }

    /// Claim the next slot in `scope`. Returns a recorded effect when this
    /// step already happened on an earlier run.
    pub fn next(&mut self, scope: &Scope, site: &str) -> Result<Lookup, JournalError> {
        let seq = *self.cursors.get(scope).unwrap_or(&0);
        self.cursors.insert(scope.clone(), seq + 1);

        match self.recorded.get(&(scope.clone(), seq)) {
            Some(entry) => {
                // A different site at the same position means the program
                // changed, or something non-deterministic moved. Replay would
                // silently produce wrong answers, so refuse instead.
                if entry.site != site {
                    return Err(JournalError::Diverged {
                        scope: scope.label(),
                        seq,
                        recorded: entry.site.clone(),
                        found: site.to_string(),
                    });
                }
                self.unconsumed = self.unconsumed.saturating_sub(1);
                Ok(Lookup::Replayed(entry.effect.clone()))
            }
            None => {
                // Reaching unrecorded work means catch-up is over.
                self.unconsumed = 0;
                Ok(Lookup::Fresh {
                    scope: scope.clone(),
                    seq,
                })
            }
        }
    }

    /// Append a completed effect and persist immediately, so a crash on the
    /// next instruction still resumes correctly.
    pub fn record(
        &mut self,
        scope: Scope,
        seq: usize,
        site: &str,
        effect: Effect,
    ) -> Result<(), JournalError> {
        let entry = Entry {
            scope: scope.clone(),
            seq,
            site: site.to_string(),
            effect,
        };
        self.recorded.insert((scope, seq), entry.clone());
        self.run.entries.push(entry);
        self.persist()
    }

    /// Park the run on a question and persist. The process may now exit.
    pub fn suspend(&mut self, pending: PendingQuestion) -> Result<(), JournalError> {
        self.run.status = RunStatus::Suspended;
        self.run.pending = Some(pending);
        self.persist()
    }

    pub fn finish(&mut self, status: RunStatus) -> Result<(), JournalError> {
        self.run.status = status;
        self.run.pending = None;
        self.persist()
    }

    fn persist(&mut self) -> Result<(), JournalError> {
        if !self.durable {
            return Ok(());
        }
        self.run.updated = timestamp();
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(JournalError::Io)?;
        }
        let text = serde_json::to_string_pretty(&self.run).map_err(JournalError::Encode)?;
        // Write to a temporary file and rename, so a crash mid-write cannot
        // leave a truncated journal — the one file that must never be
        // corrupt.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, format!("{text}\n")).map_err(JournalError::Io)?;
        std::fs::rename(&tmp, &self.path).map_err(JournalError::Io)
    }
}

#[derive(Debug)]
pub enum JournalError {
    Io(std::io::Error),
    Encode(serde_json::Error),
    Diverged {
        scope: String,
        seq: usize,
        recorded: String,
        found: String,
    },
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JournalError::Io(e) => write!(f, "journal io error: {e}"),
            JournalError::Encode(e) => write!(f, "journal encode error: {e}"),
            JournalError::Diverged {
                scope,
                seq,
                recorded,
                found,
            } => write!(
                f,
                "this run does not match its journal: step {seq} of {scope} was `{recorded}`, but the program now reaches `{found}`"
            ),
        }
    }
}

/// Seconds since the Unix epoch, as a string. Good enough for ordering runs
/// in a listing; the journal never depends on wall-clock time for replay.
fn timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Where runs live: `.kora/runs/<id>.json` beside the program.
pub fn run_path(program: &Path, id: &str) -> PathBuf {
    program
        .parent()
        .unwrap_or(Path::new("."))
        .join(".kora")
        .join("runs")
        .join(format!("{id}.json"))
}

pub fn runs_dir(program: &Path) -> PathBuf {
    program
        .parent()
        .unwrap_or(Path::new("."))
        .join(".kora")
        .join("runs")
}

/// A short, sortable run id.
pub fn new_run_id() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{secs:x}{:04x}", nanos % 0xffff)
}

pub fn load_run(path: &Path) -> Result<Run, JournalError> {
    let text = std::fs::read_to_string(path).map_err(JournalError::Io)?;
    serde_json::from_str(&text).map_err(JournalError::Encode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cassette::RecordedOutcome;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kora-journal-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("run.json")
    }

    fn model_effect(tag: &str) -> Effect {
        let mut fields = serde_json::Map::new();
        fields.insert("v".into(), serde_json::json!(tag));
        Effect::Model {
            outcome: RecordedOutcome::Ok {
                fields,
                tokens_in: 1,
                tokens_out: 1,
                chunks: Vec::new(),
            },
        }
    }

    #[test]
    fn fresh_journal_reports_every_step_as_new() {
        let mut j = Journal::disabled();
        let root = Scope::root();
        assert!(matches!(
            j.next(&root, "a.ko:1").unwrap(),
            Lookup::Fresh { seq: 0, .. }
        ));
        assert!(matches!(
            j.next(&root, "a.ko:2").unwrap(),
            Lookup::Fresh { seq: 1, .. }
        ));
    }

    #[test]
    fn a_context_pruning_decision_replays_the_same_retained_history() {
        let path = scratch("context");
        let mut j = Journal::open(Run::new("r1".into(), "a.ko".into()), path.clone());
        let root = Scope::root();

        let kept = RecordedExchange {
            name: "lookup".into(),
            arguments_json: "{\"id\":\"new\"}".into(),
            result_json: "unverified: kept unchanged".into(),
        };
        let Lookup::Fresh { scope, seq } = j.next(&root, "a.ko:1#context").unwrap() else {
            panic!("expected fresh")
        };
        j.record(
            scope,
            seq,
            "a.ko:1#context",
            Effect::Context {
                retained: vec![kept.clone()],
                dropped: 2,
            },
        )
        .unwrap();

        // Resume: the same exchanges come back, not a fresh estimate.
        let reopened = load_run(&path).unwrap();
        let mut j2 = Journal::open(reopened, path.clone());
        match j2.next(&root, "a.ko:1#context").unwrap() {
            Lookup::Replayed(Effect::Context { retained, dropped }) => {
                assert_eq!(retained.len(), 1);
                assert_eq!(retained[0].result_json, kept.result_json);
                assert_eq!(dropped, 2);
            }
            other => panic!("expected a replayed context decision, got {other:?}"),
        }

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn recorded_steps_replay_in_order() {
        let path = scratch("replay");
        let mut j = Journal::open(Run::new("r1".into(), "a.ko".into()), path.clone());
        let root = Scope::root();

        // First run: two fresh effects.
        let Lookup::Fresh { scope, seq } = j.next(&root, "a.ko:1").unwrap() else {
            panic!("expected fresh")
        };
        j.record(scope, seq, "a.ko:1", model_effect("first"))
            .unwrap();
        let Lookup::Fresh { scope, seq } = j.next(&root, "a.ko:2").unwrap() else {
            panic!("expected fresh")
        };
        j.record(scope, seq, "a.ko:2", model_effect("second"))
            .unwrap();

        // Resume: the same steps replay, in the same order, without redoing.
        let reopened = load_run(&path).unwrap();
        let mut j2 = Journal::open(reopened, path.clone());
        assert!(matches!(
            j2.next(&root, "a.ko:1").unwrap(),
            Lookup::Replayed(_)
        ));
        assert!(matches!(
            j2.next(&root, "a.ko:2").unwrap(),
            Lookup::Replayed(_)
        ));
        // Past the end of the journal, work is fresh again.
        assert!(matches!(
            j2.next(&root, "a.ko:3").unwrap(),
            Lookup::Fresh { .. }
        ));

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn parallel_branches_have_independent_sequences() {
        // Branch order across threads is not reproducible, so each branch
        // counts its own steps.
        let mut j = Journal::disabled();
        let a = Scope::root().child(0);
        let b = Scope::root().child(1);
        assert!(matches!(
            j.next(&a, "x.ko:5").unwrap(),
            Lookup::Fresh { seq: 0, .. }
        ));
        assert!(matches!(
            j.next(&b, "x.ko:5").unwrap(),
            Lookup::Fresh { seq: 0, .. }
        ));
        assert!(matches!(
            j.next(&a, "x.ko:6").unwrap(),
            Lookup::Fresh { seq: 1, .. }
        ));
    }

    #[test]
    fn a_changed_program_is_refused_not_silently_replayed() {
        let path = scratch("diverge");
        let mut j = Journal::open(Run::new("r1".into(), "a.ko".into()), path.clone());
        let root = Scope::root();
        let Lookup::Fresh { scope, seq } = j.next(&root, "a.ko:1").unwrap() else {
            panic!()
        };
        j.record(scope, seq, "a.ko:1", model_effect("x")).unwrap();

        let mut j2 = Journal::open(load_run(&path).unwrap(), path.clone());
        let err = j2.next(&root, "a.ko:99").unwrap_err();
        assert!(
            matches!(err, JournalError::Diverged { .. }),
            "replaying a changed program must fail loudly"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn suspension_survives_a_reload() {
        let path = scratch("suspend");
        let mut j = Journal::open(Run::new("r1".into(), "a.ko".into()), path.clone());
        j.suspend(PendingQuestion {
            scope: Scope::root(),
            seq: 0,
            site: "a.ko:12".into(),
            question: "approve?".into(),
            context: "details".into(),
        })
        .unwrap();

        let reloaded = load_run(&path).unwrap();
        assert_eq!(reloaded.status, RunStatus::Suspended);
        assert_eq!(reloaded.pending.unwrap().question, "approve?");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn every_record_is_persisted_immediately() {
        // A crash on the next instruction must still find the effect on disk.
        let path = scratch("durable");
        let mut j = Journal::open(Run::new("r1".into(), "a.ko".into()), path.clone());
        let Lookup::Fresh { scope, seq } = j.next(&Scope::root(), "a.ko:1").unwrap() else {
            panic!()
        };
        j.record(scope, seq, "a.ko:1", model_effect("x")).unwrap();

        assert_eq!(load_run(&path).unwrap().entries.len(), 1);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn disabled_journal_writes_nothing() {
        let mut j = Journal::disabled();
        let Lookup::Fresh { scope, seq } = j.next(&Scope::root(), "a.ko:1").unwrap() else {
            panic!()
        };
        j.record(scope, seq, "a.ko:1", model_effect("x")).unwrap();
        assert!(!j.is_durable());
    }
}
