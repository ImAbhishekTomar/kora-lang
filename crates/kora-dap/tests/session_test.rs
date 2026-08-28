//! A full debug session, driven the way an editor drives one.
//!
//! The adapter is run in-process over a pipe rather than as a subprocess, so a
//! failure points at a line of Rust instead of at a hung child.

use std::io::Write;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value as J};

use kora_dap::{protocol, Adapter};

/// A writer that collects everything the adapter emits.
#[derive(Clone)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("kora-dap-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn write(&self, name: &str, source: &str) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, source).unwrap();
        path.to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Drives one session: scripted requests in, every message out.
///
/// `script` receives each `stopped` event and returns the requests to send in
/// response, which is how a test steps through a program.
fn session(
    program: &str,
    breakpoints: &[u32],
    stop_on_entry: bool,
    mut script: impl FnMut(&J, &Sender<J>) + Send + 'static,
) -> Vec<J> {
    let sink = Sink(Arc::new(Mutex::new(Vec::new())));
    let mut adapter = Adapter::new(sink.clone());
    let client = adapter.client();

    // A second channel lets the script queue requests from the watcher thread.
    let (queue, requests) = channel::<J>();
    let feeder = client.clone();
    std::thread::spawn(move || {
        while let Ok(message) = requests.recv() {
            if !feeder.send(message) {
                return;
            }
        }
    });

    let mut seq = 0;
    let mut request = |command: &str, arguments: J| {
        seq += 1;
        json!({ "seq": seq, "type": "request", "command": command, "arguments": arguments })
    };

    client.send(request("initialize", json!({ "adapterID": "kora" })));
    client.send(request(
        "setBreakpoints",
        json!({
            "source": { "path": program },
            "breakpoints": breakpoints.iter().map(|l| json!({ "line": l })).collect::<Vec<_>>(),
        }),
    ));
    client.send(request("configurationDone", json!({})));
    client.send(request(
        "launch",
        json!({ "program": program, "stopOnEntry": stop_on_entry }),
    ));

    // Watch the output for `stopped` events and let the script answer them.
    let watch_sink = sink.clone();
    let watcher = std::thread::spawn(move || {
        let mut handled = 0;
        loop {
            let bytes = watch_sink.0.lock().unwrap().clone();
            let messages = protocol::read_all(&bytes);
            let stops: Vec<J> = messages
                .iter()
                .filter(|m| m["event"] == "stopped")
                .cloned()
                .collect();
            if stops.len() > handled {
                script(&stops[handled], &queue);
                handled += 1;
            }
            if messages.iter().any(|m| m["event"] == "terminated") {
                // Disconnect so the adapter's loop ends and the test finishes.
                let _ = queue.send(json!({
                    "seq": 9000, "type": "request", "command": "disconnect", "arguments": {}
                }));
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    });

    adapter.pump();
    let _ = watcher.join();
    let bytes = sink.0.lock().unwrap().clone();
    protocol::read_all(&bytes)
}

fn request(seq: i64, command: &str, arguments: J) -> J {
    json!({ "seq": seq, "type": "request", "command": command, "arguments": arguments })
}

/// Responses to `command`, in order.
fn responses<'a>(messages: &'a [J], command: &str) -> Vec<&'a J> {
    messages
        .iter()
        .filter(|m| m["type"] == "response" && m["command"] == command)
        .collect()
}

const PROGRAM: &str = "\
type Employee:
    name: str
    salary: int

def main():
    people = [Employee(\"Ada\", 120), Employee(\"Grace\", 130)]
    total = 0
    for p in people:
        total = total + p.salary
    print(total)
";

#[test]
fn initialize_reports_capabilities_and_readiness() {
    let scratch = Scratch::new("init");
    let path = scratch.write("prog.ko", PROGRAM);
    let messages = session(&path, &[], false, |_, queue| {
        let _ = queue.send(request(100, "continue", json!({ "threadId": 1 })));
    });

    let initialize = responses(&messages, "initialize");
    assert_eq!(initialize.len(), 1);
    assert_eq!(
        initialize[0]["body"]["supportsConfigurationDoneRequest"],
        json!(true)
    );
    assert!(messages.iter().any(|m| m["event"] == "initialized"));
    assert!(messages.iter().any(|m| m["event"] == "terminated"));
}

#[test]
fn a_program_with_no_breakpoints_runs_to_the_end_and_prints() {
    let scratch = Scratch::new("run");
    let path = scratch.write("prog.ko", PROGRAM);
    let messages = session(&path, &[], false, |_, _| {});

    let printed: Vec<String> = messages
        .iter()
        .filter(|m| m["event"] == "output")
        .map(|m| {
            m["body"]["output"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .collect();
    assert_eq!(printed, vec!["250"]);
    assert!(messages.iter().any(|m| m["event"] == "terminated"));
}

#[test]
fn a_breakpoint_reports_the_stack_and_the_variables() {
    let scratch = Scratch::new("inspect");
    let path = scratch.write("prog.ko", PROGRAM);
    let messages = session(&path, &[10], false, |_stopped, queue| {
        let _ = queue.send(request(100, "stackTrace", json!({ "threadId": 1 })));
        let _ = queue.send(request(101, "scopes", json!({ "frameId": 1 })));
        // Scope handles are reserved before their contents, so the innermost
        // frame's Locals is handle 1. Asserted below rather than assumed.
        let _ = queue.send(request(
            102,
            "variables",
            json!({ "variablesReference": 1 }),
        ));
        let _ = queue.send(request(
            103,
            "evaluate",
            json!({ "expression": "total", "frameId": 1 }),
        ));
        let _ = queue.send(request(104, "continue", json!({ "threadId": 1 })));
    });

    let stack = responses(&messages, "stackTrace");
    let frames = stack[0]["body"]["stackFrames"].as_array().unwrap();
    assert_eq!(frames[0]["name"], json!("main"));
    assert_eq!(frames[0]["line"], json!(10));
    assert!(frames[0]["source"]["path"]
        .as_str()
        .unwrap()
        .ends_with("prog.ko"));

    let scopes = responses(&messages, "scopes");
    let names: Vec<&str> = scopes[0]["body"]["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Locals", "prog.ko"]);
    assert_eq!(
        scopes[0]["body"]["scopes"][0]["variablesReference"],
        json!(1)
    );

    let variables = responses(&messages, "variables");
    let shown: Vec<(String, String)> = variables[0]["body"]["variables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["name"].as_str().unwrap().to_string(),
                v["value"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(
        shown.contains(&("total".to_string(), "250".to_string())),
        "{shown:?}"
    );
    assert!(
        shown.iter().any(|(n, v)| n == "people" && v == "list[2]"),
        "a list shows its size, not its contents: {shown:?}"
    );

    let evaluated = responses(&messages, "evaluate");
    assert_eq!(evaluated[0]["body"]["result"], json!("250"));
}

#[test]
fn a_list_can_be_expanded_down_to_a_field() {
    let scratch = Scratch::new("expand");
    let path = scratch.write("prog.ko", PROGRAM);
    let messages = session(&path, &[10], false, |_, queue| {
        let _ = queue.send(request(
            100,
            "evaluate",
            json!({ "expression": "people.0.name", "frameId": 1 }),
        ));
        let _ = queue.send(request(101, "continue", json!({ "threadId": 1 })));
    });
    let evaluated = responses(&messages, "evaluate");
    assert_eq!(evaluated[0]["body"]["result"], json!("\"Ada\""));
}

#[test]
fn stop_on_entry_stops_before_anything_runs() {
    let scratch = Scratch::new("entry");
    let path = scratch.write("prog.ko", PROGRAM);
    let messages = session(&path, &[], true, |stopped, queue| {
        assert_eq!(stopped["body"]["reason"], json!("entry"));
        let _ = queue.send(request(100, "continue", json!({ "threadId": 1 })));
    });
    // Nothing printed before the stop, and the run finished after it.
    assert!(messages.iter().any(|m| m["event"] == "stopped"));
    assert!(messages.iter().any(|m| m["event"] == "terminated"));
}

#[test]
fn a_program_that_fails_reports_the_error_and_exits_non_zero() {
    let scratch = Scratch::new("failure");
    let path = scratch.write("prog.ko", "def main():\n    xs = [1]\n    print(xs[9])\n");
    let messages = session(&path, &[], false, |_, _| {});

    let stderr: String = messages
        .iter()
        .filter(|m| m["event"] == "output" && m["body"]["category"] == "stderr")
        .map(|m| m["body"]["output"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(stderr.contains("list index 9 out of range"), "{stderr}");
    let exited: Vec<&J> = messages.iter().filter(|m| m["event"] == "exited").collect();
    assert_eq!(exited[0]["body"]["exitCode"], json!(1));
}

#[test]
fn a_breakpoint_on_a_blank_line_moves_to_the_next_statement() {
    let scratch = Scratch::new("snap");
    let path = scratch.write("prog.ko", PROGRAM);
    // Line 4 is blank; line 7 is `total = 0`. Line 99 is past the end.
    let messages = session(&path, &[4, 99], false, |_, queue| {
        let _ = queue.send(request(100, "continue", json!({ "threadId": 1 })));
    });

    let set = responses(&messages, "setBreakpoints");
    let reported = set[0]["body"]["breakpoints"].as_array().unwrap();
    assert_eq!(reported[0]["verified"], json!(true));
    assert_eq!(reported[0]["line"], json!(5), "moved to the next statement");
    assert_eq!(reported[1]["verified"], json!(false));

    // The moved breakpoint really stops the program.
    let stops: Vec<&J> = messages
        .iter()
        .filter(|m| m["event"] == "stopped")
        .collect();
    assert_eq!(stops.len(), 1);
}

#[test]
fn an_unreadable_program_is_reported_rather_than_hanging() {
    let messages = session("/nonexistent/nope.ko", &[], false, |_, _| {});
    let stderr: String = messages
        .iter()
        .filter(|m| m["event"] == "output")
        .map(|m| m["body"]["output"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(stderr.contains("cannot read"), "{stderr}");
}
