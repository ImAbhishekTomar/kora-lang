//! The adapter's state machine: requests in, events out.

use std::collections::HashMap;
use std::error::Error;
use std::io::{BufReader, Write};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value as J};

use kora_runtime::debug::{Controls, Debugger, Frame, Resume, StopReason};
use kora_runtime::value::Value;
use kora_runtime::{Cassette, Config, Interpreter, Mode};

use crate::protocol::{self, Seq};
use crate::variables::Arena;

/// Anything the main loop has to react to, from either direction.
enum Incoming {
    /// A DAP message from the editor.
    Client(J),
    /// The program stopped and is waiting for a decision.
    Stopped(Box<Snapshot>),
    /// The program printed a line.
    Output(String),
    /// The program finished, with a message if it failed.
    Exited(Option<String>),
}

/// Everything the editor may ask about while the program is paused.
pub struct Snapshot {
    reason: StopReason,
    frames: Vec<FrameView>,
    arena: Arena,
}

struct FrameView {
    name: String,
    file: String,
    line: u32,
    locals: usize,
    globals: usize,
}

/// The interpreter's side of the conversation.
///
/// It converts a stop into a snapshot, hands it over, and blocks. Blocking
/// *is* the pause: there is no separate "paused" flag that could disagree with
/// what the interpreter is actually doing.
struct Bridge {
    to_main: Sender<Incoming>,
    resume: Receiver<Resume>,
}

impl Debugger for Bridge {
    fn stopped(
        &mut self,
        reason: StopReason,
        frames: &[Frame],
        globals: &[(String, Value)],
    ) -> Resume {
        let snapshot = snapshot(reason, frames, globals);
        if self
            .to_main
            .send(Incoming::Stopped(Box::new(snapshot)))
            .is_err()
        {
            // The editor is gone; nothing will ever resume us.
            return Resume::Terminate;
        }
        self.resume.recv().unwrap_or(Resume::Terminate)
    }

    fn output(&mut self, line: &str) {
        let _ = self.to_main.send(Incoming::Output(line.to_string()));
    }
}

/// Flatten a stop into something the main thread can answer questions from.
fn snapshot(reason: StopReason, frames: &[Frame], globals: &[(String, Value)]) -> Snapshot {
    let mut arena = Arena::default();
    // Innermost first, which is the order DAP renders a stack in. Scopes are
    // reserved before anything is put in them, so the innermost frame's
    // Locals is handle 1 no matter what the program is holding.
    let mut views: Vec<FrameView> = frames
        .iter()
        .rev()
        .map(|frame| FrameView {
            name: frame.name.clone(),
            file: absolute(&frame.file),
            line: frame.line,
            locals: arena.reserve_scope("Locals"),
            globals: arena.reserve_scope("File"),
        })
        .collect();
    for (view, frame) in views.iter_mut().zip(frames.iter().rev()) {
        arena.fill_scope(view.locals, &frame.vars);
        arena.fill_scope(view.globals, globals);
    }
    Snapshot {
        reason,
        frames: views,
        arena,
    }
}

/// Editors want a path they can open, and a relative one is relative to a
/// working directory they do not necessarily share.
fn absolute(path: &str) -> String {
    Path::new(path)
        .canonicalize()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

/// What `launch` asked for.
#[derive(Default, Clone)]
struct Launch {
    program: String,
    stop_on_entry: bool,
    mode: Option<Mode>,
    /// `noDebug` means "just run it": no breakpoints, no stepping.
    no_debug: bool,
}

/// The adapter. Public so tests can drive it over a pair of pipes.
pub struct Adapter<W: Write> {
    out: W,
    seq: Seq,
    controls: Controls,
    to_main: Sender<Incoming>,
    from_all: Receiver<Incoming>,
    resume: Option<Sender<Resume>>,
    snapshot: Option<Snapshot>,
    launch: Option<Launch>,
    configured: bool,
    started: bool,
    running: Option<std::thread::JoinHandle<()>>,
    /// Breakpoints the editor set before the program existed, kept so the
    /// response can report them verified.
    lines: HashMap<String, Vec<u32>>,
    done: bool,
}

/// A handle for feeding client messages to a running adapter.
///
/// The queue itself is private: an adapter must never be told the program
/// stopped by anything other than the program.
#[derive(Clone)]
pub struct Client(Sender<Incoming>);

impl Client {
    /// Deliver one DAP message. `false` once the adapter has finished.
    pub fn send(&self, message: J) -> bool {
        self.0.send(Incoming::Client(message)).is_ok()
    }
}

/// Run the adapter over stdin and stdout. This is `kora dap`.
pub fn run() -> Result<(), Box<dyn Error>> {
    let mut adapter = Adapter::new(std::io::stdout());
    let client = adapter.client();

    // stdin becomes messages on the same queue the program thread uses, so
    // the main loop has exactly one thing to wait on.
    std::thread::spawn(move || {
        let mut input = BufReader::new(std::io::stdin());
        while let Ok(Some(message)) = protocol::read(&mut input) {
            if !client.send(message) {
                return;
            }
        }
    });

    adapter.pump();
    Ok(())
}

impl<W: Write> Adapter<W> {
    pub fn new(out: W) -> Adapter<W> {
        let (to_main, from_all) = channel();
        Adapter {
            out,
            seq: Seq::default(),
            controls: Controls::default(),
            to_main,
            from_all,
            resume: None,
            snapshot: None,
            launch: None,
            configured: false,
            started: false,
            running: None,
            lines: HashMap::new(),
            done: false,
        }
    }

    /// A handle for delivering client messages to this adapter.
    pub fn client(&self) -> Client {
        Client(self.to_main.clone())
    }

    /// Handle everything until the session ends.
    pub fn pump(&mut self) {
        while !self.done {
            let Ok(event) = self.from_all.recv() else {
                break;
            };
            self.handle(event);
        }
        if let Some(handle) = self.running.take() {
            let _ = handle.join();
        }
    }

    fn handle(&mut self, event: Incoming) {
        match event {
            Incoming::Client(message) => self.request(&message),
            Incoming::Output(line) => self.output(&line, "stdout"),
            Incoming::Stopped(snapshot) => {
                let reason = snapshot.reason;
                self.snapshot = Some(*snapshot);
                self.event(
                    "stopped",
                    json!({
                        "reason": reason.as_str(),
                        "threadId": THREAD,
                        "allThreadsStopped": true,
                    }),
                );
            }
            Incoming::Exited(error) => {
                if let Some(message) = &error {
                    self.output(message, "stderr");
                }
                let code = if error.is_some() { 1 } else { 0 };
                self.event("exited", json!({ "exitCode": code }));
                self.event("terminated", json!({}));
            }
        }
    }

    // --- requests ---

    fn request(&mut self, message: &J) {
        if message["type"] != "request" {
            return;
        }
        let command = message["command"].as_str().unwrap_or("").to_string();
        match command.as_str() {
            "initialize" => {
                self.reply(message, capabilities());
                self.event("initialized", json!({}));
            }
            "setBreakpoints" => self.set_breakpoints(message),
            // Kora has no exceptions to break on, so accept and report none.
            "setExceptionBreakpoints" => self.reply(message, json!({ "breakpoints": [] })),
            "launch" => self.launch(message),
            "attach" => self.fail(
                message,
                "kora dap launches programs; it cannot attach to one",
            ),
            "configurationDone" => {
                self.configured = true;
                self.reply(message, json!({}));
                self.start();
            }
            "threads" => self.reply(
                message,
                json!({ "threads": [{ "id": THREAD, "name": "main" }] }),
            ),
            "stackTrace" => self.stack_trace(message),
            "scopes" => self.scopes(message),
            "variables" => self.variables(message),
            "continue" => self.resume_with(
                message,
                Resume::Continue,
                json!({"allThreadsContinued": true}),
            ),
            "next" => self.resume_with(message, Resume::Over, json!({})),
            "stepIn" => self.resume_with(message, Resume::Into, json!({})),
            "stepOut" => self.resume_with(message, Resume::Out, json!({})),
            "pause" => {
                self.controls.request_pause();
                self.reply(message, json!({}));
            }
            "evaluate" => self.evaluate(message),
            "disconnect" | "terminate" => {
                // Let the program go before answering, so a paused run does
                // not keep the process alive after the editor has left.
                if let Some(resume) = &self.resume {
                    let _ = resume.send(Resume::Terminate);
                }
                self.reply(message, json!({}));
                self.done = true;
            }
            _ => self.fail(message, &format!("`{command}` is not supported")),
        }
    }

    fn set_breakpoints(&mut self, message: &J) {
        let file = message["arguments"]["source"]["path"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let lines: Vec<u32> = message["arguments"]["breakpoints"]
            .as_array()
            .map(|bps| {
                bps.iter()
                    .filter_map(|b| b["line"].as_u64())
                    .map(|l| l as u32)
                    .collect()
            })
            .unwrap_or_default();

        // A click in the gutter often lands on a blank line or a comment.
        // Snapping to the next statement is what makes such a breakpoint work
        // instead of silently never firing; the response reports the line it
        // moved to, so the editor draws the marker where it will actually stop.
        let statements = std::fs::read_to_string(&file)
            .ok()
            .and_then(|source| kora_syntax::parse(&source).ok())
            .map(|program| kora_syntax::statement_lines(&program));

        let placed: Vec<Option<u32>> = lines
            .iter()
            .map(|line| match &statements {
                Some(statements) => kora_syntax::snap(statements, *line),
                // The file did not parse, so take the line at its word rather
                // than refusing every breakpoint in a file being edited.
                None => Some(*line),
            })
            .collect();

        let effective: Vec<u32> = placed.iter().flatten().copied().collect();
        self.controls
            .breakpoints
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set(&file, effective.iter().copied());
        self.lines.insert(file.clone(), effective);

        let verified: Vec<J> = placed
            .iter()
            .map(|line| match line {
                Some(line) => {
                    json!({ "verified": true, "line": line, "source": { "path": file } })
                }
                None => json!({
                    "verified": false,
                    "message": "no statement on or after this line",
                }),
            })
            .collect();
        self.reply(message, json!({ "breakpoints": verified }));
    }

    fn launch(&mut self, message: &J) {
        let args = &message["arguments"];
        let Some(program) = args["program"].as_str() else {
            self.fail(message, "launch needs a `program`");
            return;
        };
        let mode = match (args["replay"].as_bool(), args["record"].as_bool()) {
            (Some(true), _) => Some(Mode::Replay),
            (_, Some(true)) => Some(Mode::Record),
            _ => None,
        };
        self.launch = Some(Launch {
            program: program.to_string(),
            stop_on_entry: args["stopOnEntry"].as_bool().unwrap_or(false),
            mode,
            no_debug: args["noDebug"].as_bool().unwrap_or(false),
        });
        self.reply(message, json!({}));
        // Most clients send configurationDone first; some never send it.
        if self.configured {
            self.start();
        }
    }

    fn stack_trace(&mut self, message: &J) {
        let Some(snapshot) = &self.snapshot else {
            self.reply(message, json!({ "stackFrames": [], "totalFrames": 0 }));
            return;
        };
        let frames: Vec<J> = snapshot
            .frames
            .iter()
            .enumerate()
            .map(|(i, frame)| {
                json!({
                    "id": i + 1,
                    "name": frame.name,
                    "line": frame.line,
                    "column": 1,
                    "source": {
                        "name": file_name(&frame.file),
                        "path": frame.file,
                    },
                })
            })
            .collect();
        let total = frames.len();
        self.reply(
            message,
            json!({ "stackFrames": frames, "totalFrames": total }),
        );
    }

    fn scopes(&mut self, message: &J) {
        let id = message["arguments"]["frameId"].as_i64().unwrap_or(1) as usize;
        let Some(frame) = self.snapshot.as_ref().and_then(|s| s.frames.get(id - 1)) else {
            self.reply(message, json!({ "scopes": [] }));
            return;
        };
        let (locals, globals) = (frame.locals, frame.globals);
        let file = file_name(&frame.file);
        self.reply(
            message,
            json!({ "scopes": [
                { "name": "Locals", "variablesReference": locals, "expensive": false },
                // The file's top-level names, which is what "global" means in
                // Kora: each file has its own.
                { "name": file, "variablesReference": globals, "expensive": false },
            ]}),
        );
    }

    fn variables(&mut self, message: &J) {
        let handle = message["arguments"]["variablesReference"]
            .as_i64()
            .unwrap_or(0) as usize;
        let Some(snapshot) = &self.snapshot else {
            self.reply(message, json!({ "variables": [] }));
            return;
        };
        let arena = &snapshot.arena;
        let variables: Vec<J> = arena
            .children_of(handle)
            .iter()
            .filter_map(|child| {
                let node = arena.get(*child)?;
                Some(json!({
                    "name": node.name,
                    "value": node.value,
                    "type": node.type_name,
                    "variablesReference": arena.reference(*child),
                }))
            })
            .collect();
        self.reply(message, json!({ "variables": variables }));
    }

    /// Watch and hover expressions.
    ///
    /// Names and field paths only — `total`, `employee.salary`, `rows.0`.
    /// Evaluating arbitrary Kora would mean running it, and a watch expression
    /// that calls a model or writes a file is not what anybody means by
    /// "inspect".
    fn evaluate(&mut self, message: &J) {
        let expression = message["arguments"]["expression"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        let id = message["arguments"]["frameId"].as_i64().unwrap_or(1) as usize;
        let Some(snapshot) = &self.snapshot else {
            self.fail(message, "the program is not paused");
            return;
        };
        let Some(frame) = snapshot.frames.get(id.saturating_sub(1)) else {
            self.fail(message, "no such frame");
            return;
        };

        let arena = &snapshot.arena;
        let mut parts = expression.split('.');
        let Some(head) = parts.next().filter(|h| !h.is_empty()) else {
            self.fail(message, "nothing to evaluate");
            return;
        };
        // A local shadows a top-level name, exactly as it does at runtime.
        let mut current =
            find(arena, frame.locals, head).or_else(|| find(arena, frame.globals, head));
        for part in parts {
            current = current.and_then(|handle| find(arena, handle, part));
        }
        match current.and_then(|handle| arena.get(handle).map(|n| (handle, n))) {
            Some((handle, node)) => self.reply(
                message,
                json!({
                    "result": node.value,
                    "type": node.type_name,
                    "variablesReference": arena.reference(handle),
                }),
            ),
            None => self.fail(message, &format!("`{expression}` is not in scope here")),
        }
    }

    fn resume_with(&mut self, message: &J, resume: Resume, body: J) {
        let Some(sender) = &self.resume else {
            self.fail(message, "the program is not running");
            return;
        };
        if sender.send(resume).is_err() {
            self.fail(message, "the program is no longer running");
            return;
        }
        self.snapshot = None;
        self.reply(message, body);
        self.event(
            "continued",
            json!({ "threadId": THREAD, "allThreadsContinued": true }),
        );
    }

    // --- running the program ---

    /// Start the program, once both `launch` and `configurationDone` have
    /// arrived. Called from both, so the order does not matter.
    fn start(&mut self) {
        if self.started || !self.configured {
            return;
        }
        let Some(launch) = self.launch.clone() else {
            return;
        };
        self.started = true;

        let source = match std::fs::read_to_string(&launch.program) {
            Ok(source) => source,
            Err(e) => {
                self.output(
                    &format!("cannot read `{}`: {e}\n", launch.program),
                    "stderr",
                );
                self.handle(Incoming::Exited(None));
                self.done = true;
                return;
            }
        };
        let program = match kora_syntax::parse(&source) {
            Ok(program) => program,
            Err(e) => {
                self.output(&e.render(&source, &launch.program), "stderr");
                self.handle(Incoming::Exited(None));
                self.done = true;
                return;
            }
        };

        self.event("thread", json!({ "reason": "started", "threadId": THREAD }));

        let (resume_tx, resume_rx) = channel();
        self.resume = Some(resume_tx);
        let to_main = self.to_main.clone();
        let controls = self.controls.clone();

        self.running = Some(std::thread::spawn(move || {
            let mut interp = build(&launch);
            if !launch.no_debug {
                interp.attach_debugger(
                    Box::new(Bridge {
                        to_main: to_main.clone(),
                        resume: resume_rx,
                    }),
                    controls,
                    launch.stop_on_entry,
                );
            }
            let outcome = interp.run(&program);
            // Output captured before the debugger was attached, or produced by
            // a `parallel for` worker, arrives here rather than live.
            for line in &interp.output {
                let _ = to_main.send(Incoming::Output(format!("{line}\n")));
            }
            if let Some(cassette) = &interp.cassette {
                let cassette = cassette.lock().unwrap_or_else(|e| e.into_inner());
                let _ = cassette.save();
            }
            let error = match outcome {
                Ok(()) => None,
                // Stopping from the editor is not a failure to report.
                Err(e) if e.is_terminated() => None,
                Err(e) => Some(e.render(&source, &launch.program)),
            };
            let _ = to_main.send(Incoming::Exited(error));
        }));
    }

    // --- protocol plumbing ---

    fn reply(&mut self, request: &J, body: J) {
        let seq = self.seq.take();
        self.send(protocol::response(seq, request, body));
    }

    fn fail(&mut self, request: &J, message: &str) {
        let seq = self.seq.take();
        self.send(protocol::error(seq, request, message));
    }

    fn event(&mut self, name: &str, body: J) {
        let seq = self.seq.take();
        self.send(protocol::event(seq, name, body));
    }

    fn output(&mut self, text: &str, category: &str) {
        let text = if text.ends_with('\n') {
            text.to_string()
        } else {
            format!("{text}\n")
        };
        self.event("output", json!({ "category": category, "output": text }));
    }

    fn send(&mut self, message: J) {
        let _ = protocol::write(&mut self.out, &message);
    }
}

/// Kora has one thread of user-visible execution; `parallel for` fans out
/// underneath it and is not separately debuggable.
const THREAD: i64 = 1;

/// An interpreter configured the way `kora run` configures one.
fn build(launch: &Launch) -> Interpreter {
    let path = Path::new(&launch.program);
    let mut interp = Interpreter::new();
    // Printed lines travel as `output` events, so nothing may also go to the
    // adapter's stdout: that is the protocol channel.
    interp.direct_stdout = false;
    interp.program_name = launch.program.clone();
    interp.config = Config::discover(path);
    interp.sinks = interp.config.sinks.clone();
    interp.allow_private_hosts = interp.config.http_allow_private;
    interp.http_timeout_secs = interp.config.http_timeout_secs;
    if let Some(mode) = launch.mode {
        interp.cassette = Some(Arc::new(Mutex::new(Cassette::open(mode, path))));
    }
    interp
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// The child of `parent` called `name`.
fn find(arena: &Arena, parent: usize, name: &str) -> Option<usize> {
    arena
        .children_of(parent)
        .iter()
        .copied()
        .find(|child| arena.get(*child).is_some_and(|n| n.name == name))
}

/// What this adapter can do, in the client's vocabulary.
fn capabilities() -> J {
    json!({
        "supportsConfigurationDoneRequest": true,
        "supportsTerminateRequest": true,
        "supportsEvaluateForHovers": true,
        "supportsSingleThreadExecutionRequests": false,
        "supportsStepBack": false,
        "supportsSetVariable": false,
        "supportsRestartRequest": false,
        "exceptionBreakpointFilters": [],
    })
}
