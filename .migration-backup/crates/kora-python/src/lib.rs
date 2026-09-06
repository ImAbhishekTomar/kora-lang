//! kora-python: the Python escape hatch, as a sidecar rather than an embed.
//!
//! Python runs in its own process. Values cross as JSON: data in, data out,
//! no live object handles and no callbacks back into Kora. That boundary is
//! the whole design, and it is what keeps the four thesis pillars intact
//! (DECISIONS.md):
//!
//! - **Threading.** Embedding CPython would reintroduce the GIL into Kora.
//!   A sidecar keeps Kora GIL-free, and N workers give real parallelism that
//!   embedding cannot.
//! - **Durability.** A CPython call stack cannot be checkpointed. As an RPC,
//!   a Python call is atomic: checkpoint before and after, never mid-frame.
//! - **Labels.** An explicit boundary is a sink the compiler can see.
//!   Embedded objects would be opaque and labels would vanish into them.
//! - **Packaging.** No `use python` means no Python needed; the binary stays
//!   a single download.
//!
//! The cost, accepted knowingly: per-call serialization, and no live-object
//! interop such as `df.groupby().apply(lambda ...)`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

/// The worker, embedded so there is no file to install or find.
const WORKER: &str = include_str!("worker.py");

#[derive(Debug)]
pub struct PythonError {
    pub message: String,
    /// The Python traceback, when the failure came from Python itself.
    pub traceback: Option<String>,
}

impl PythonError {
    fn new(message: impl Into<String>) -> PythonError {
        PythonError {
            message: message.into(),
            traceback: None,
        }
    }
}

impl std::fmt::Display for PythonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for PythonError {}

/// How to start the interpreter.
#[derive(Debug, Clone)]
pub struct Config {
    /// Defaults to `python3`; a virtualenv's interpreter goes here.
    pub command: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            command: "python3".to_string(),
        }
    }
}

/// One request into the worker, one response out.
pub trait Transport: Send {
    fn call(&mut self, request: Value) -> Result<Value, PythonError>;
}

/// A running Python worker.
pub struct Worker {
    transport: Box<dyn Transport>,
    next_id: u64,
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker").finish()
    }
}

impl Worker {
    /// Start an interpreter running the embedded worker.
    pub fn start(config: &Config) -> Result<Worker, PythonError> {
        let transport = ProcessTransport::spawn(config)?;
        Ok(Worker {
            transport: Box::new(transport),
            next_id: 1,
        })
    }

    /// A worker over a supplied transport. Tests use this.
    pub fn with_transport(transport: Box<dyn Transport>) -> Worker {
        Worker {
            transport,
            next_id: 1,
        }
    }

    /// Call `module.function(*args)`.
    ///
    /// The outer `Result` is the boundary failing — Python missing, the
    /// process gone. The inner one is Python raising, which the program
    /// should see and handle.
    pub fn call(
        &mut self,
        module: &str,
        function: &str,
        args: Vec<Value>,
    ) -> Result<Result<Value, PythonError>, PythonError> {
        let id = self.next_id;
        self.next_id += 1;

        let response = self.transport.call(json!({
            "id": id,
            "module": module,
            "func": function,
            "args": args,
        }))?;

        if response.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            return Ok(Ok(response.get("result").cloned().unwrap_or(Value::Null)));
        }
        Ok(Err(PythonError {
            message: response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown Python error")
                .to_string(),
            traceback: response
                .get("traceback")
                .and_then(Value::as_str)
                .map(str::to_string),
        }))
    }
}

/// JSON lines over a child process's stdin and stdout.
struct ProcessTransport {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl ProcessTransport {
    fn spawn(config: &Config) -> Result<ProcessTransport, PythonError> {
        let mut child = Command::new(&config.command)
            // `-c` avoids writing the worker to disk and finding it again.
            .arg("-c")
            .arg(WORKER)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Let Python's own stderr through, so an import error is visible
            // rather than swallowed.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                PythonError::new(format!(
                    "could not start `{}`: {e}. Install Python, or set `[python] command` in kora.toml",
                    config.command
                ))
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| PythonError::new("the worker has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| PythonError::new("the worker has no stdout"))?;

        Ok(ProcessTransport {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }
}

impl Transport for ProcessTransport {
    fn call(&mut self, request: Value) -> Result<Value, PythonError> {
        let line = serde_json::to_string(&request)
            .map_err(|e| PythonError::new(format!("could not encode the request: {e}")))?;
        writeln!(self.stdin, "{line}")
            .and_then(|()| self.stdin.flush())
            .map_err(|e| PythonError::new(format!("could not write to the worker: {e}")))?;

        let mut response = String::new();
        let read = self
            .stdout
            .read_line(&mut response)
            .map_err(|e| PythonError::new(format!("could not read from the worker: {e}")))?;
        if read == 0 {
            return Err(PythonError::new(
                "the Python worker exited; see its output above",
            ));
        }
        serde_json::from_str(response.trim())
            .map_err(|e| PythonError::new(format!("the worker sent something unreadable: {e}")))
    }
}

impl Drop for ProcessTransport {
    fn drop(&mut self) {
        // The worker exits when its stdin closes; kill anything that does not,
        // so a run cannot leave an orphaned interpreter behind.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records requests and replays scripted responses.
    struct Fake {
        responses: Vec<Value>,
        pub seen: Vec<Value>,
    }

    impl Transport for Fake {
        fn call(&mut self, request: Value) -> Result<Value, PythonError> {
            self.seen.push(request);
            if self.responses.is_empty() {
                return Err(PythonError::new("no scripted response"));
            }
            Ok(self.responses.remove(0))
        }
    }

    #[test]
    fn a_successful_call_returns_its_result() {
        let worker = Worker::with_transport(Box::new(Fake {
            responses: vec![json!({ "id": 1, "ok": true, "result": 2.0 })],
            seen: Vec::new(),
        }));
        let mut worker = worker;
        let out = worker
            .call("statistics", "mean", vec![json!([1, 2, 3])])
            .unwrap()
            .unwrap();
        assert_eq!(out, json!(2.0));
    }

    #[test]
    fn the_request_names_the_module_and_function() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        struct Recorder(std::sync::Arc<std::sync::Mutex<Vec<Value>>>);
        impl Transport for Recorder {
            fn call(&mut self, request: Value) -> Result<Value, PythonError> {
                self.0.lock().unwrap().push(request);
                Ok(json!({ "ok": true, "result": null }))
            }
        }

        let mut worker = Worker::with_transport(Box::new(Recorder(seen.clone())));
        worker
            .call("json", "dumps", vec![json!({"a": 1})])
            .unwrap()
            .unwrap();

        let requests = seen.lock().unwrap();
        assert_eq!(requests[0]["module"], "json");
        assert_eq!(requests[0]["func"], "dumps");
        assert_eq!(requests[0]["args"][0]["a"], 1);
    }

    #[test]
    fn a_python_exception_is_a_result_not_a_boundary_failure() {
        // Python raising is something the program should handle; only the
        // boundary itself failing is an error.
        let mut worker = Worker::with_transport(Box::new(Fake {
            responses: vec![json!({
                "id": 1,
                "ok": false,
                "error": "ZeroDivisionError: division by zero",
                "traceback": "Traceback..."
            })],
            seen: Vec::new(),
        }));
        let outcome = worker.call("operator", "truediv", vec![json!(1), json!(0)]);
        let inner = outcome.expect("the boundary held").unwrap_err();
        assert!(inner.message.contains("ZeroDivisionError"));
        assert!(
            inner.traceback.is_some(),
            "a traceback helps across a process"
        );
    }

    #[test]
    fn a_dead_worker_is_a_boundary_failure() {
        struct Dead;
        impl Transport for Dead {
            fn call(&mut self, _: Value) -> Result<Value, PythonError> {
                Err(PythonError::new("the Python worker exited"))
            }
        }
        let mut worker = Worker::with_transport(Box::new(Dead));
        assert!(worker.call("json", "dumps", vec![]).is_err());
    }

    #[test]
    fn request_ids_increment() {
        let mut worker = Worker::with_transport(Box::new(Fake {
            responses: vec![
                json!({ "ok": true, "result": 1 }),
                json!({ "ok": true, "result": 2 }),
            ],
            seen: Vec::new(),
        }));
        worker.call("m", "f", vec![]).unwrap().unwrap();
        worker.call("m", "f", vec![]).unwrap().unwrap();
        assert_eq!(worker.next_id, 3);
    }

    #[test]
    fn the_worker_script_is_embedded() {
        // No file to install, find, or keep in sync with the binary.
        assert!(WORKER.contains("def main()"));
        assert!(WORKER.contains("importlib"));
    }
}
