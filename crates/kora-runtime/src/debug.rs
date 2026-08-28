//! Debugger support: breakpoints, stepping, and a view of the stack.
//!
//! The interpreter knows nothing about any protocol. It maintains a frame
//! stack and, before each statement, asks a [`Debugger`] whether to stop. The
//! adapter that speaks to an editor lives in `kora-dap`.
//!
//! Everything here costs nothing when no debugger is attached: the hook is a
//! single `Option` check, and no snapshot is taken.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::value::Value;

/// One frame of the call stack, as the debugger sees it.
///
/// `vars` is a snapshot taken before the frame's current statement, not a live
/// view. A frame that has called into another shows the names as they stood at
/// the call — which is what a paused stack should show.
#[derive(Debug, Clone)]
pub struct Frame {
    /// What to call this frame: a function name, or a file's top level.
    pub name: String,
    /// File the frame is executing, as the runtime displays it.
    pub file: String,
    /// Line of the statement about to run.
    pub line: u32,
    /// Local names, sorted, snapshotted before the current statement.
    pub vars: Vec<(String, Value)>,
}

/// Why execution stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Stopped before the first statement, because the client asked to.
    Entry,
    Breakpoint,
    Step,
    /// The client asked for a pause while the program was running.
    Pause,
}

impl StopReason {
    /// The word the Debug Adapter Protocol uses for this reason.
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::Entry => "entry",
            StopReason::Breakpoint => "breakpoint",
            StopReason::Step => "step",
            StopReason::Pause => "pause",
        }
    }
}

/// What the client wants to happen next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume {
    /// Run until a breakpoint or the end.
    Continue,
    /// Stop at the next statement in this frame or a shallower one.
    Over,
    /// Stop at the very next statement, wherever it is.
    Into,
    /// Run until this frame returns.
    Out,
    /// Stop the program.
    Terminate,
}

/// What the interpreter calls when it needs a decision.
///
/// `stopped` blocks for as long as the program is paused: that is the whole
/// mechanism. The implementation is free to serve requests from another thread
/// while it waits, which is what the adapter does.
pub trait Debugger {
    fn stopped(
        &mut self,
        reason: StopReason,
        frames: &[Frame],
        globals: &[(String, Value)],
    ) -> Resume;

    /// A line the program printed, forwarded as it happens rather than at the
    /// end, so the debug console keeps pace with execution.
    fn output(&mut self, line: &str);
}

/// Breakpoint lines by file.
///
/// Files are keyed by canonical path, because the editor and the runtime
/// spell the same file differently often enough that comparing the strings
/// would silently lose breakpoints.
#[derive(Debug, Default)]
pub struct Breakpoints {
    by_file: HashMap<String, HashSet<u32>>,
}

impl Breakpoints {
    /// Replace the breakpoints for one file, as `setBreakpoints` does.
    pub fn set(&mut self, file: &str, lines: impl IntoIterator<Item = u32>) {
        self.by_file
            .insert(canonical(file), lines.into_iter().collect());
    }

    pub fn is_set(&self, file: &str, line: u32) -> bool {
        self.by_file
            .get(&canonical(file))
            .is_some_and(|lines| lines.contains(&line))
    }

    pub fn is_empty(&self) -> bool {
        self.by_file.values().all(HashSet::is_empty)
    }
}

/// One spelling of a path, so two spellings of one file compare equal.
fn canonical(file: &str) -> String {
    Path::new(file)
        .canonicalize()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| file.to_string())
}

/// Shared with whatever drives the debugger, so breakpoints can be added and
/// a pause requested while the program is running.
#[derive(Clone, Default)]
pub struct Controls {
    pub breakpoints: Arc<Mutex<Breakpoints>>,
    pause: Arc<AtomicBool>,
}

impl Controls {
    /// Ask the program to stop at the next statement it reaches.
    pub fn request_pause(&self) {
        self.pause.store(true, Ordering::Relaxed);
    }

    fn take_pause(&self) -> bool {
        self.pause.swap(false, Ordering::Relaxed)
    }
}

/// The interpreter's own bookkeeping. Not public API: it is driven entirely by
/// the hook in `exec`.
#[derive(Default)]
pub(crate) struct Session {
    pub(crate) frames: Vec<Frame>,
    pub(crate) controls: Controls,
    /// What the last resume asked for.
    mode: Mode,
    /// Whether the very first statement should stop.
    pub(crate) stop_on_entry: bool,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Mode {
    #[default]
    Running,
    /// Stop at the next statement at this depth or shallower.
    Over(usize),
    Into,
    /// Stop at the next statement shallower than this depth.
    Out(usize),
    Terminating,
}

impl Session {
    /// Whether the statement about to run should stop the program, and why.
    pub(crate) fn should_stop(&mut self, file: &str, line: u32) -> Option<StopReason> {
        if self.mode == Mode::Terminating {
            return None;
        }
        if std::mem::take(&mut self.stop_on_entry) {
            return Some(StopReason::Entry);
        }
        if self.controls.take_pause() {
            return Some(StopReason::Pause);
        }
        let depth = self.frames.len();
        let stepped = match self.mode {
            Mode::Into => true,
            Mode::Over(at) => depth <= at,
            Mode::Out(at) => depth < at,
            Mode::Running | Mode::Terminating => false,
        };
        if stepped {
            return Some(StopReason::Step);
        }
        // Checking the map is the per-statement cost of an attached debugger,
        // so skip it entirely while no breakpoints are set.
        let breakpoints = self
            .controls
            .breakpoints
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !breakpoints.is_empty() && breakpoints.is_set(file, line) {
            return Some(StopReason::Breakpoint);
        }
        None
    }

    /// Record what the client asked for, relative to the current depth.
    pub(crate) fn resume(&mut self, resume: Resume) {
        let depth = self.frames.len();
        self.mode = match resume {
            Resume::Continue => Mode::Running,
            Resume::Over => Mode::Over(depth),
            Resume::Into => Mode::Into,
            // A step out of the outermost frame is a continue: there is
            // nothing shallower to return to.
            Resume::Out => Mode::Out(depth.max(1)),
            Resume::Terminate => Mode::Terminating,
        };
    }

    pub(crate) fn is_terminating(&self) -> bool {
        self.mode == Mode::Terminating
    }
}
