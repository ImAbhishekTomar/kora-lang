//! Breakpoints and stepping, driven without an editor.
//!
//! The adapter in `kora-dap` is a thin translation layer; the behaviour worth
//! pinning down is here, where it can be tested without a protocol.

use kora_runtime::debug::{Controls, Debugger, Frame, Resume, StopReason};
use kora_runtime::{Config, Interpreter};
use kora_syntax::parse;
use std::sync::{Arc, Mutex};

const CONFIG: &str = "[models]\ndefault = \"local:test-model\"\n";

/// Where a stop happened: the reason, the stack as names, and the top frame's
/// line and variables.
#[derive(Debug, Clone, PartialEq)]
struct Stop {
    reason: String,
    stack: Vec<String>,
    line: u32,
    vars: Vec<(String, String)>,
}

/// A debugger that answers with a fixed script of resumes and records what it
/// saw. Running out of script means "continue".
struct Script {
    plan: Vec<Resume>,
    seen: Arc<Mutex<Vec<Stop>>>,
    printed: Arc<Mutex<Vec<String>>>,
}

impl Debugger for Script {
    fn stopped(
        &mut self,
        reason: StopReason,
        frames: &[Frame],
        _globals: &[(String, kora_runtime::value::Value)],
    ) -> Resume {
        let top = frames.last().expect("a stop always has a frame");
        self.seen.lock().unwrap().push(Stop {
            reason: reason.as_str().to_string(),
            stack: frames.iter().map(|f| f.name.clone()).collect(),
            line: top.line,
            vars: top
                .vars
                .iter()
                .map(|(k, v)| (k.clone(), v.to_string()))
                .collect(),
        });
        if self.plan.is_empty() {
            Resume::Continue
        } else {
            self.plan.remove(0)
        }
    }

    fn output(&mut self, line: &str) {
        self.printed.lock().unwrap().push(line.to_string());
    }
}

struct Session {
    stops: Vec<Stop>,
    printed: Vec<String>,
    error: Option<String>,
}

/// Run `source` with breakpoints on `lines`, answering each stop from `plan`.
fn debug(source: &str, lines: &[u32], plan: Vec<Resume>) -> Session {
    let dir = std::env::temp_dir().join(format!(
        "kora-debug-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("prog.ko");
    std::fs::write(&path, source).unwrap();

    let controls = Controls::default();
    controls
        .breakpoints
        .lock()
        .unwrap()
        .set(&path.to_string_lossy(), lines.iter().copied());

    let seen = Arc::new(Mutex::new(Vec::new()));
    let printed = Arc::new(Mutex::new(Vec::new()));
    let mut interp = Interpreter::new();
    interp.config = Config::parse(CONFIG).unwrap();
    interp.program_name = path.to_string_lossy().to_string();
    interp.attach_debugger(
        Box::new(Script {
            plan,
            seen: seen.clone(),
            printed: printed.clone(),
        }),
        controls,
        false,
    );

    let program = parse(source).expect("should parse");
    let error = interp.run(&program).err().map(|e| e.message);
    std::fs::remove_dir_all(&dir).ok();

    let stops = seen.lock().unwrap().clone();
    let printed = printed.lock().unwrap().clone();
    Session {
        stops,
        printed,
        error,
    }
}

const PROGRAM: &str = "\
def double(n: int) -> int:
    doubled = n * 2
    return doubled

def main():
    total = 0
    for n in [1, 2]:
        total = total + double(n)
    print(total)
";

#[test]
fn a_breakpoint_stops_on_its_line_with_the_names_in_scope() {
    let session = debug(PROGRAM, &[9], vec![]);
    assert_eq!(session.stops.len(), 1);
    let stop = &session.stops[0];
    assert_eq!(stop.reason, "breakpoint");
    assert_eq!(stop.line, 9);
    // `main()` is called after the file's top level has finished, so there is
    // no module frame under it — the top level really has returned.
    assert_eq!(stop.stack, vec!["main"]);
    assert_eq!(
        stop.vars,
        vec![("n".into(), "2".into()), ("total".into(), "6".into())]
    );
    assert_eq!(session.printed, vec!["6"]);
}

#[test]
fn a_breakpoint_in_a_loop_body_stops_once_per_iteration() {
    let session = debug(PROGRAM, &[8], vec![]);
    assert_eq!(session.stops.len(), 2);
    assert_eq!(
        session.stops[0].vars,
        vec![("n".into(), "1".into()), ("total".into(), "0".into())]
    );
    assert_eq!(
        session.stops[1].vars,
        vec![("n".into(), "2".into()), ("total".into(), "2".into())]
    );
}

#[test]
fn step_over_stays_in_the_frame_it_started_in() {
    let session = debug(PROGRAM, &[8], vec![Resume::Over]);
    // The step lands on the next statement in main, not inside double().
    assert_eq!(session.stops[1].reason, "step");
    assert_eq!(session.stops[1].stack, vec!["main"]);
}

#[test]
fn step_into_enters_the_called_function() {
    let session = debug(PROGRAM, &[8], vec![Resume::Into]);
    let inner = &session.stops[1];
    assert_eq!(inner.stack, vec!["main", "double"]);
    assert_eq!(inner.line, 2);
    assert_eq!(inner.vars, vec![("n".into(), "1".into())]);
}

#[test]
fn a_paused_parent_frame_shows_its_names_as_they_stood_at_the_call() {
    let session = debug(PROGRAM, &[2], vec![]);
    let stop = &session.stops[0];
    assert_eq!(stop.stack, vec!["main", "double"]);
    assert_eq!(stop.line, 2);
}

#[test]
fn step_out_runs_to_the_end_of_the_frame() {
    let session = debug(PROGRAM, &[2], vec![Resume::Out]);
    // Back in main, at a depth shallower than the frame we stepped out of.
    assert_eq!(session.stops[1].reason, "step");
    assert_eq!(session.stops[1].stack, vec!["main"]);
}

#[test]
fn terminate_stops_the_program_without_reporting_an_error() {
    let session = debug(PROGRAM, &[6], vec![Resume::Terminate]);
    assert_eq!(session.stops.len(), 1);
    assert_eq!(session.error.as_deref(), Some("stopped by the debugger"));
    // Nothing after the stop ran.
    assert!(session.printed.is_empty(), "{:?}", session.printed);
}

#[test]
fn a_frame_is_pushed_for_each_imported_file() {
    let dir = std::env::temp_dir().join(format!("kora-debug-import-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lib.ko"), "def go() -> int:\n    return 5\n").unwrap();
    let main_src = "use \"./lib.ko\" as lib\n\ndef main():\n    print(lib.go())\n";
    let path = dir.join("main.ko");
    std::fs::write(&path, main_src).unwrap();

    let controls = Controls::default();
    controls
        .breakpoints
        .lock()
        .unwrap()
        .set(&dir.join("lib.ko").to_string_lossy(), [2]);

    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut interp = Interpreter::new();
    interp.config = Config::parse(CONFIG).unwrap();
    interp.program_name = path.to_string_lossy().to_string();
    interp.attach_debugger(
        Box::new(Script {
            plan: vec![],
            seen: seen.clone(),
            printed: Arc::new(Mutex::new(Vec::new())),
        }),
        controls,
        false,
    );
    interp.run(&parse(main_src).unwrap()).unwrap();
    std::fs::remove_dir_all(&dir).ok();

    let stops = seen.lock().unwrap().clone();
    assert_eq!(stops.len(), 1);
    assert_eq!(
        stops[0].stack,
        vec!["main", "go"],
        "the breakpoint is in the imported file"
    );
}

#[test]
fn stop_on_entry_stops_before_the_first_statement() {
    let dir = std::env::temp_dir().join(format!("kora-debug-entry-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("prog.ko");
    let source = "def main():\n    print(1)\n";
    std::fs::write(&path, source).unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut interp = Interpreter::new();
    interp.config = Config::parse(CONFIG).unwrap();
    interp.program_name = path.to_string_lossy().to_string();
    interp.attach_debugger(
        Box::new(Script {
            plan: vec![],
            seen: seen.clone(),
            printed: Arc::new(Mutex::new(Vec::new())),
        }),
        Controls::default(),
        true,
    );
    interp.run(&parse(source).unwrap()).unwrap();
    std::fs::remove_dir_all(&dir).ok();

    let stops = seen.lock().unwrap().clone();
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0].reason, "entry");
    assert_eq!(stops[0].line, 1);
}
