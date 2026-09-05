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
//!
//! ## The file
//!
//! One run is one append-only file, `.kora/runs/<id>.jsonl`: a header line,
//! then one line per effect, then one line per status change. Appending is
//! why the format is line-oriented rather than a single JSON object — the
//! object form had to re-serialize and rewrite the whole run on every
//! effect, which is quadratic in the number of effects and made a pipeline
//! over a few thousand rows slower than the model calls it was journaling.
//!
//! Every append is flushed and `fsync`ed before the effect it describes is
//! handed back to the program. That is the promise the word durable is
//! making: the answer is on disk before anything can act on it, so a power
//! cut cannot lose a model call the program already spent tokens on. A
//! partial final line — the one crash a fsync cannot prevent, since the
//! machine can die mid-write — is discarded on load, because a half-written
//! line is by definition an effect whose result nobody saw.
//!
//! A run is also locked while a process holds it open, so two `--resume`s of
//! the same id cannot interleave their appends. The lock is an OS advisory
//! lock on a sidecar file, not a flag written into one: a durable run's
//! normal ending is being killed, and a lock the operating system releases
//! on process death is the only kind that does not leave every crashed run
//! needing to be unstuck by hand.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write as _;
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
    /// A world-changing call that was started and never finished: the
    /// process died between performing it and recording what it returned.
    ///
    /// This is the one honest answer available. Re-running it could append
    /// the same rows twice or charge the same card twice; assuming it
    /// succeeded could drop a row silently. So the slot keeps the attempt,
    /// and a resume that reaches it stops and says which call is in doubt —
    /// the same rule the tool loop already applies to a timed-out tool call.
    Attempted { name: String },
    /// A read of something outside the program — a file, a directory
    /// listing, a database query — recorded as a digest of what it returned
    /// rather than as the data itself.
    ///
    /// Deliberately not replayed the way a model call is. Content is
    /// unbounded (a pipeline's input CSV is routinely larger than every
    /// other effect in the run put together), and a journal that has to hold
    /// it stops being a log of decisions. A digest keeps the property that
    /// actually matters: a resumed run re-reads live, and if the source
    /// changed underneath it, the run stops with a sentence naming the file
    /// instead of continuing against data the first attempt never saw.
    Input { name: String, digest: String },
    /// A `notes.read(key)` and the value it returned.
    ///
    /// The notes store itself (`.kora/notes/<run-id>.json`) outlives this
    /// journal and is not replayed the way `Model`/`Tool`/`Human`/`Output`
    /// effects are — a `notes.write` goes straight to that file, live, every
    /// time. But the *read* inside one run is journaled, the same way
    /// `time.now()` is: without it, a replay would see whatever the store
    /// holds at replay time rather than what the live run actually read,
    /// which could differ if another process wrote to the same store meanwhile.
    Memory { key: String, value_json: String },
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

/// One line of a run file.
///
/// A run file is not a serialized `Run`; it is the sequence of events that
/// produced one. `Run` is what you get by folding these back together, which
/// is why appending an effect costs one line rather than a whole rewrite.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum Record {
    /// Written once, when the file is created.
    Header {
        id: String,
        program: String,
        created: String,
        /// Bumped if the line format ever changes shape, so an old file is
        /// refused with a sentence rather than misread.
        format: u32,
    },
    Entry {
        scope: Scope,
        seq: usize,
        site: String,
        effect: Effect,
    },
    Status {
        status: RunStatus,
        pending: Option<PendingQuestion>,
        updated: String,
    },
}

/// The line format this build writes and reads.
/// Bumped to 2 when effect identity stopped being `file:line` and became
/// structural (`kora_syntax::ops`). A run journaled under the old spelling
/// cannot be replayed by this build: the sites recorded in it name lines,
/// and nothing in the file recovers which call each one was.
const FORMAT: u32 = 2;

/// Live journal for one run: the recorded effects plus a replay cursor per
/// scope.
#[derive(Debug)]
pub struct Journal {
    run: Run,
    /// Open in append mode for the life of the run. Kept rather than
    /// reopened per effect so an append is one write and one fsync.
    file: Option<File>,
    /// Held, not read: dropping it releases the run to another process.
    _lock: Option<lock::RunLock>,
    /// Recorded effects, indexed for replay lookup.
    recorded: HashMap<(Scope, usize), Entry>,
    /// Where each slot sits in `run.entries`, so recording an outcome over
    /// an attempt replaces it rather than appending a second one.
    positions: HashMap<(Scope, usize), usize>,
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
            file: None,
            _lock: None,
            recorded: HashMap::new(),
            positions: HashMap::new(),
            cursors: HashMap::new(),
            durable: false,
            unconsumed: 0,
        }
    }

    /// Start or reopen a durable run, taking the run's lock for as long as
    /// this journal lives.
    ///
    /// Fails rather than waits when another process holds the run: two
    /// processes appending to one journal would each replay to a different
    /// point and then record effects the other never saw, and a queue would
    /// only delay that until the first one exits.
    pub fn open(run: Run, path: PathBuf) -> Result<Journal, JournalError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(JournalError::Io)?;
        }
        let held = lock::acquire(&lock_path(&path))
            .map_err(JournalError::Io)?
            .ok_or_else(|| JournalError::Locked { id: run.id.clone() })?;

        let existed = path.exists();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(JournalError::Io)?;
        if !existed {
            // A directory entry is itself only durable once the directory is
            // synced; without this a power cut can leave every effect fsynced
            // into a file that no longer has a name.
            sync_dir(path.parent());
            let header = Record::Header {
                id: run.id.clone(),
                program: run.program.clone(),
                created: run.created.clone(),
                format: FORMAT,
            };
            append(&mut file, &header)?;
        }

        let mut recorded = HashMap::new();
        let mut positions = HashMap::new();
        for (index, entry) in run.entries.iter().enumerate() {
            let key = (entry.scope.clone(), entry.seq);
            recorded.insert(key.clone(), entry.clone());
            positions.insert(key, index);
        }
        let unconsumed = recorded.len();
        Ok(Journal {
            run,
            file: Some(file),
            _lock: Some(held),
            recorded,
            positions,
            cursors: HashMap::new(),
            durable: true,
            unconsumed,
        })
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

    /// The slot `next` will claim in `scope` on its next call. The slot it
    /// just claimed is one below this.
    pub fn cursor(&self, scope: &Scope) -> usize {
        *self.cursors.get(scope).unwrap_or(&0)
    }

    /// Consume every recorded entry sitting immediately after the cursor in
    /// `scope`, and report how many there were.
    ///
    /// Used for exactly one thing: an interrupted streamed model call. The
    /// pieces a stream wrote before the process died are journaled effects
    /// like any other, and they sit after the call that produced them. The
    /// call itself never came back, so nothing else can be under them --
    /// which makes "everything after this slot" an exact description of the
    /// stream's fallout rather than a guess. Skipping them keeps the rest of
    /// the scope's numbering intact, so the resumed run's own next effect
    /// lands where the journal expects it.
    pub fn skip_after_cursor(&mut self, scope: &Scope) -> usize {
        let mut skipped = 0;
        loop {
            let seq = self.cursor(scope);
            if !self.recorded.contains_key(&(scope.clone(), seq)) {
                break;
            }
            self.cursors.insert(scope.clone(), seq + 1);
            self.unconsumed = self.unconsumed.saturating_sub(1);
            skipped += 1;
        }
        skipped
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
        let key = (scope, seq);
        self.recorded.insert(key.clone(), entry.clone());
        // A slot recorded twice is an attempt being replaced by its outcome:
        // the file keeps both lines, in order, and the later one wins.
        match self.positions.get(&key) {
            Some(index) => self.run.entries[*index] = entry.clone(),
            None => {
                self.positions.insert(key, self.run.entries.len());
                self.run.entries.push(entry.clone());
            }
        }
        self.write(&Record::Entry {
            scope: entry.scope,
            seq: entry.seq,
            site: entry.site,
            effect: entry.effect,
        })
    }

    /// Answer the question this run is parked on, and mark it runnable again.
    ///
    /// The answer is an ordinary journal entry at the slot the suspended step
    /// claimed, so the resumed run replays into it exactly as it would any
    /// other recorded effect.
    pub fn answer(&mut self, text: &str) -> Result<(), JournalError> {
        let Some(pending) = self.run.pending.clone() else {
            return Err(JournalError::NotWaiting {
                id: self.run.id.clone(),
            });
        };
        self.record(
            pending.scope.clone(),
            pending.seq,
            &pending.site,
            Effect::Human {
                question: pending.question,
                answer: text.to_string(),
            },
        )?;
        self.run.pending = None;
        self.run.status = RunStatus::Running;
        self.write(&Record::Status {
            status: RunStatus::Running,
            pending: None,
            updated: timestamp(),
        })
    }

    /// Park the run on a question and persist. The process may now exit.
    pub fn suspend(&mut self, pending: PendingQuestion) -> Result<(), JournalError> {
        self.run.status = RunStatus::Suspended;
        self.run.pending = Some(pending.clone());
        self.write(&Record::Status {
            status: RunStatus::Suspended,
            pending: Some(pending),
            updated: timestamp(),
        })
    }

    pub fn finish(&mut self, status: RunStatus) -> Result<(), JournalError> {
        self.run.status = status;
        self.run.pending = None;
        self.write(&Record::Status {
            status,
            pending: None,
            updated: timestamp(),
        })
    }

    /// Append one line and put it on the disk before returning.
    fn write(&mut self, record: &Record) -> Result<(), JournalError> {
        if !self.durable {
            return Ok(());
        }
        self.run.updated = timestamp();
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        append(file, record)
    }
}

/// Serialize one record and append it.
///
/// Every append is a single `write_all`, so a crash can only ever truncate
/// the last line, never interleave two. What differs per record is whether
/// the line is also forced to the physical disk before the program is
/// allowed to continue — see [`must_sync`].
fn append(file: &mut File, record: &Record) -> Result<(), JournalError> {
    let mut line = serde_json::to_string(record).map_err(JournalError::Encode)?;
    line.push('\n');
    file.write_all(line.as_bytes()).map_err(JournalError::Io)?;
    if must_sync(record) {
        file.sync_data().map_err(JournalError::Io)?;
    }
    Ok(())
}

/// Whether this record has to be on the disk before the program continues.
///
/// A killed process loses nothing either way: the bytes are already in the
/// operating system's page cache, which outlives the process. The question
/// `fsync` answers is the harder one — a power cut, where the tail of the
/// file can vanish and the run then resumes to an earlier point and does
/// something a second time.
///
/// For a model call that is money, for a write it is a duplicated row, for a
/// human answer it is asking a person again. Those are worth the milliseconds
/// every time. For an output line it is a repeated `print`, which is
/// cosmetic — and output is also the one effect a chatty program produces
/// thousands of, where a synced write per line would make `--durable` cost
/// more than the work it is protecting.
fn must_sync(record: &Record) -> bool {
    !matches!(
        record,
        Record::Entry {
            effect: Effect::Output { .. },
            ..
        }
    )
}

/// Best-effort directory fsync: not every platform or filesystem supports it,
/// and on the ones that do not, the file's own fsync is already the whole
/// guarantee available.
fn sync_dir(dir: Option<&Path>) {
    if let Some(dir) = dir {
        if let Ok(handle) = File::open(dir) {
            let _ = handle.sync_all();
        }
    }
}

/// The sidecar the run's lock is taken on. Deliberately not the journal
/// itself: a lock on the file being appended to is easy to lose to a
/// reopen, and the lock's lifetime is the process's, not the handle's.
fn lock_path(run_path: &Path) -> PathBuf {
    run_path.with_extension("lock")
}

/// An advisory, whole-file lock released by the operating system when the
/// process holding it exits — including when it is killed, which for a
/// durable run is the ordinary case rather than the exceptional one.
mod lock {
    use std::fs::File;
    use std::path::Path;

    #[derive(Debug)]
    pub struct RunLock {
        /// `None` only on a platform with no file locking at all, where the
        /// guard exists so the rest of the runtime needs no special case.
        _file: Option<File>,
    }

    /// `Ok(None)` means another process holds the run.
    #[cfg(unix)]
    pub fn acquire(path: &Path) -> std::io::Result<Option<RunLock>> {
        use std::os::unix::io::AsRawFd;

        let file = File::create(path)?;
        // SAFETY: `flock` takes a file descriptor this process owns and keeps
        // open for as long as the returned guard lives.
        let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if taken == 0 {
            return Ok(Some(RunLock { _file: Some(file) }));
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(None),
            _ => Err(err),
        }
    }

    /// Windows has no `flock`, but it has something stronger: a file opened
    /// with no sharing cannot be opened again at all, and the handle is
    /// closed by the kernel when the process ends.
    #[cfg(windows)]
    pub fn acquire(path: &Path) -> std::io::Result<Option<RunLock>> {
        use std::os::windows::fs::OpenOptionsExt;

        match std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .share_mode(0)
            .open(path)
        {
            Ok(file) => Ok(Some(RunLock { _file: Some(file) })),
            // A sharing violation *is* the lock working: another process
            // holds the handle. Windows reports it as raw error 32 (and 33
            // for a byte-range conflict), neither of which Rust maps to
            // `PermissionDenied` — so matching only on the kind let the
            // refusal surface as "journal io error: ... (os error 32)"
            // instead of the sentence naming what happened.
            Err(e)
                if e.kind() == std::io::ErrorKind::PermissionDenied
                    || matches!(e.raw_os_error(), Some(32) | Some(33)) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    #[cfg(not(any(unix, windows)))]
    pub fn acquire(_path: &Path) -> std::io::Result<Option<RunLock>> {
        Ok(Some(RunLock { _file: None }))
    }
}

#[derive(Debug)]
pub enum JournalError {
    Io(std::io::Error),
    Encode(serde_json::Error),
    /// Another process is running or resuming this run.
    Locked {
        id: String,
    },
    /// `answer` on a run that is not parked on a question.
    NotWaiting {
        id: String,
    },
    /// A run file written by a newer build.
    UnknownFormat {
        found: u32,
        supported: u32,
    },
    /// A run file written before effect identity became structural.
    RetiredFormat {
        found: u32,
    },
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
            JournalError::Locked { id } => write!(
                f,
                "run `{id}` is already open in another process; wait for it to finish, or resume a different run"
            ),
            JournalError::NotWaiting { id } => {
                write!(f, "run `{id}` is not waiting for an answer")
            }
            JournalError::UnknownFormat { found, supported } => write!(
                f,
                "this run was written by a newer Kora (journal format {found}, this build reads {supported})"
            ),
            JournalError::RetiredFormat { found } => write!(
                f,
                "this run was journaled by an older Kora (format {found}), which recorded each effect by line number rather than by which call it was; it cannot be resumed by this build — start a new run"
            ),
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

/// Where runs live: `.kora/runs/<id>.jsonl` beside the program.
pub fn run_path(program: &Path, id: &str) -> PathBuf {
    runs_dir(program).join(format!("{id}.jsonl"))
}

/// Where runs lived before the format became append-only. Kept so an
/// interrupted run written by an older build can still be resumed rather
/// than quietly disappearing from `kora runs`.
pub fn legacy_run_path(program: &Path, id: &str) -> PathBuf {
    runs_dir(program).join(format!("{id}.json"))
}

/// Convert a run written in the old whole-file format, if that is what is
/// there, and remove the old file so the run is not listed twice.
///
/// Returns whether anything was converted.
pub fn migrate_legacy(program: &Path, id: &str) -> Result<bool, JournalError> {
    let new = run_path(program, id);
    let old = legacy_run_path(program, id);
    if new.exists() || !old.exists() {
        return Ok(false);
    }
    let run = load_run(&old)?;
    write_run(&new, &run)?;
    std::fs::remove_file(&old).map_err(JournalError::Io)?;
    Ok(true)
}

/// Write a whole `Run` as a fresh append-only file. Used by migration and by
/// nothing else: a live journal only ever appends.
fn write_run(path: &Path, run: &Run) -> Result<(), JournalError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(JournalError::Io)?;
    }
    let mut file = std::fs::File::create(path).map_err(JournalError::Io)?;
    append(
        &mut file,
        &Record::Header {
            id: run.id.clone(),
            program: run.program.clone(),
            created: run.created.clone(),
            format: FORMAT,
        },
    )?;
    for entry in &run.entries {
        append(
            &mut file,
            &Record::Entry {
                scope: entry.scope.clone(),
                seq: entry.seq,
                site: entry.site.clone(),
                effect: entry.effect.clone(),
            },
        )?;
    }
    append(
        &mut file,
        &Record::Status {
            status: run.status,
            pending: run.pending.clone(),
            updated: run.updated.clone(),
        },
    )?;
    sync_dir(path.parent());
    Ok(())
}

pub fn runs_dir(program: &Path) -> PathBuf {
    program
        .parent()
        .unwrap_or(Path::new("."))
        .join(".kora")
        .join("runs")
}

/// Where a run's notes store lives: `.kora/notes/<run-id>.json` beside the
/// program. A run's own scratch space, addressed by exactly one identity —
/// which run wrote it — the way `run_path` addresses that run's journal.
pub fn notes_path(program: &Path, run_id: &str) -> PathBuf {
    program
        .parent()
        .unwrap_or(Path::new("."))
        .join(".kora")
        .join("notes")
        .join(format!("{run_id}.json"))
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

/// Rebuild a `Run` from its file.
///
/// A `.json` file is the old whole-object format and is read as one value; a
/// `.jsonl` file is folded line by line. A final line that does not parse is
/// dropped rather than refused: the machine died partway through writing it,
/// so it describes an effect whose result never reached the program. Any
/// earlier bad line is real corruption and is reported.
pub fn load_run(path: &Path) -> Result<Run, JournalError> {
    let text = std::fs::read_to_string(path).map_err(JournalError::Io)?;
    if path.extension().is_some_and(|e| e == "json") {
        return serde_json::from_str(&text).map_err(JournalError::Encode);
    }

    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut run = Run::new(String::new(), String::new());
    let mut positions: HashMap<(Scope, usize), usize> = HashMap::new();
    for (index, line) in lines.iter().enumerate() {
        let record: Record = match serde_json::from_str(line) {
            Ok(record) => record,
            Err(e) => {
                if index + 1 == lines.len() {
                    break;
                }
                return Err(JournalError::Encode(e));
            }
        };
        match record {
            Record::Header {
                id,
                program,
                created,
                format,
            } => {
                if format > FORMAT {
                    return Err(JournalError::UnknownFormat {
                        found: format,
                        supported: FORMAT,
                    });
                }
                // Refused rather than replayed. An older run's sites name
                // lines, so every one of them would be reported as a changed
                // program -- true in the letter and misleading in the spirit,
                // since what changed is how an effect is named.
                if format < FORMAT {
                    return Err(JournalError::RetiredFormat { found: format });
                }
                run.id = id;
                run.program = program;
                run.updated.clone_from(&created);
                run.created = created;
            }
            Record::Entry {
                scope,
                seq,
                site,
                effect,
            } => {
                let entry = Entry {
                    scope: scope.clone(),
                    seq,
                    site,
                    effect,
                };
                // A slot written twice is an attempt and then its outcome.
                match positions.get(&(scope.clone(), seq)) {
                    Some(index) => run.entries[*index] = entry,
                    None => {
                        positions.insert((scope, seq), run.entries.len());
                        run.entries.push(entry);
                    }
                }
            }
            Record::Status {
                status,
                pending,
                updated,
            } => {
                run.status = status;
                run.pending = pending;
                run.updated = updated;
            }
        }
    }
    Ok(run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cassette::RecordedOutcome;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kora-journal-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("run.jsonl")
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
        let mut j = Journal::open(Run::new("r1".into(), "a.ko".into()), path.clone()).unwrap();
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

        // Resume: the same exchanges come back, not a fresh estimate. The
        // first journal is dropped first, since a run is locked while a
        // process holds it.
        drop(j);
        let reopened = load_run(&path).unwrap();
        let mut j2 = Journal::open(reopened, path.clone()).unwrap();
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
        let mut j = Journal::open(Run::new("r1".into(), "a.ko".into()), path.clone()).unwrap();
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
        drop(j);
        let reopened = load_run(&path).unwrap();
        let mut j2 = Journal::open(reopened, path.clone()).unwrap();
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
        let mut j = Journal::open(Run::new("r1".into(), "a.ko".into()), path.clone()).unwrap();
        let root = Scope::root();
        let Lookup::Fresh { scope, seq } = j.next(&root, "a.ko:1").unwrap() else {
            panic!()
        };
        j.record(scope, seq, "a.ko:1", model_effect("x")).unwrap();
        drop(j);

        let mut j2 = Journal::open(load_run(&path).unwrap(), path.clone()).unwrap();
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
        let mut j = Journal::open(Run::new("r1".into(), "a.ko".into()), path.clone()).unwrap();
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
        let mut j = Journal::open(Run::new("r1".into(), "a.ko".into()), path.clone()).unwrap();
        let Lookup::Fresh { scope, seq } = j.next(&Scope::root(), "a.ko:1").unwrap() else {
            panic!()
        };
        j.record(scope, seq, "a.ko:1", model_effect("x")).unwrap();

        assert_eq!(load_run(&path).unwrap().entries.len(), 1);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_run_is_locked_while_a_process_holds_it() {
        // Two processes appending to one journal would each replay to a
        // different point and then record effects the other never saw.
        let path = scratch("locked");
        let first = Journal::open(Run::new("r1".into(), "a.ko".into()), path.clone()).unwrap();
        let second = Journal::open(load_run(&path).unwrap(), path.clone());
        assert!(
            matches!(second, Err(JournalError::Locked { .. })),
            "a second open of a live run must be refused, got {second:?}"
        );

        // Once the holder is gone, the run is resumable again.
        drop(first);
        assert!(Journal::open(load_run(&path).unwrap(), path.clone()).is_ok());

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_half_written_last_line_is_dropped_not_refused() {
        // What a power cut looks like: the machine died partway through the
        // append. That effect's result never reached the program, so the run
        // resumes as though it had not happened.
        let path = scratch("torn");
        let mut j = Journal::open(Run::new("r1".into(), "a.ko".into()), path.clone()).unwrap();
        let Lookup::Fresh { scope, seq } = j.next(&Scope::root(), "a.ko:1").unwrap() else {
            panic!()
        };
        j.record(scope, seq, "a.ko:1", model_effect("kept"))
            .unwrap();
        drop(j);

        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"record\":\"entry\",\"scope\":[],\"seq\":1,\"si");
        std::fs::write(&path, text).unwrap();

        let run = load_run(&path).unwrap();
        assert_eq!(run.entries.len(), 1);
        assert_eq!(run.entries[0].site, "a.ko:1");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn corruption_before_the_last_line_is_reported() {
        let path = scratch("corrupt");
        let mut j = Journal::open(Run::new("r1".into(), "a.ko".into()), path.clone()).unwrap();
        let Lookup::Fresh { scope, seq } = j.next(&Scope::root(), "a.ko:1").unwrap() else {
            panic!()
        };
        j.record(scope, seq, "a.ko:1", model_effect("x")).unwrap();
        drop(j);

        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines[0] = "{not json";
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        assert!(matches!(load_run(&path), Err(JournalError::Encode { .. })));

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_run_written_by_a_newer_build_is_refused() {
        let path = scratch("future");
        std::fs::write(
            &path,
            format!(
                "{{\"record\":\"header\",\"id\":\"r1\",\"program\":\"a.ko\",\"created\":\"0\",\"format\":{}}}\n",
                FORMAT + 1
            ),
        )
        .unwrap();
        assert!(matches!(
            load_run(&path),
            Err(JournalError::UnknownFormat { .. })
        ));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_run_from_the_old_format_is_migrated_not_lost() {
        let dir = scratch("legacy").parent().unwrap().to_path_buf();
        let program = dir.join("prog.ko");
        // The old format: one pretty-printed `Run` object, at `<id>.json`.
        let mut run = Run::new("r1".into(), "prog.ko".into());
        run.entries.push(Entry {
            scope: Scope::root(),
            seq: 0,
            site: "prog.ko:1".into(),
            effect: model_effect("old"),
        });
        run.status = RunStatus::Suspended;
        run.pending = Some(PendingQuestion {
            scope: Scope::root(),
            seq: 1,
            site: "prog.ko:2".into(),
            question: "approve?".into(),
            context: String::new(),
        });
        let old_path = legacy_run_path(&program, "r1");
        std::fs::create_dir_all(old_path.parent().unwrap()).unwrap();
        std::fs::write(&old_path, serde_json::to_string_pretty(&run).unwrap()).unwrap();

        assert!(migrate_legacy(&program, "r1").unwrap());
        assert!(
            !old_path.exists(),
            "the old file is removed, not listed twice"
        );

        let migrated = load_run(&run_path(&program, "r1")).unwrap();
        assert_eq!(migrated.id, "r1");
        assert_eq!(migrated.entries.len(), 1);
        assert_eq!(migrated.status, RunStatus::Suspended);
        assert_eq!(migrated.pending.unwrap().question, "approve?");
        // Migration is idempotent: nothing left to convert.
        assert!(!migrate_legacy(&program, "r1").unwrap());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_answer_is_appended_and_unparks_the_run() {
        let path = scratch("answer");
        let mut j = Journal::open(Run::new("r1".into(), "a.ko".into()), path.clone()).unwrap();
        j.suspend(PendingQuestion {
            scope: Scope::root(),
            seq: 0,
            site: "a.ko:12".into(),
            question: "approve?".into(),
            context: String::new(),
        })
        .unwrap();
        j.answer("yes").unwrap();
        drop(j);

        let run = load_run(&path).unwrap();
        assert_eq!(run.status, RunStatus::Running);
        assert!(run.pending.is_none());
        match &run.entries[0].effect {
            Effect::Human { answer, .. } => assert_eq!(answer, "yes"),
            other => panic!("expected the human answer, got {other:?}"),
        }

        // The same journal replays it rather than asking again.
        let mut resumed = Journal::open(run, path.clone()).unwrap();
        assert!(matches!(
            resumed.next(&Scope::root(), "a.ko:12").unwrap(),
            Lookup::Replayed(Effect::Human { .. })
        ));

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn answering_a_run_that_is_not_waiting_is_an_error() {
        let path = scratch("notwaiting");
        let mut j = Journal::open(Run::new("r1".into(), "a.ko".into()), path.clone()).unwrap();
        assert!(matches!(
            j.answer("yes"),
            Err(JournalError::NotWaiting { .. })
        ));
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
