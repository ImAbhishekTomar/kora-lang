//! kora-helper: the boundary a package carries heavy work across.
//!
//! Some things a package needs cannot be written in Kora and should not be
//! written into the compiler either — rasterizing a PDF page, decoding a
//! video, anything with a large C library behind it. Kora's answer is a
//! *helper*: a separate program the package declares, which Kora starts,
//! talks to over a pipe, and kills.
//!
//! The shape is deliberate, and it is the same reasoning that keeps Python a
//! sidecar rather than an embedding (see `kora-python`):
//!
//! - **It is a process, not a library.** A `.so` loaded into the interpreter
//!   runs with the interpreter's rights and never passes a capability check.
//!   A helper has its own address space: a crash in it is a value, not a dead
//!   run, and a memory bug in it cannot reach the labels, the journal, or the
//!   budget.
//! - **It gets bytes, not paths.** Kora opens the file and sends the
//!   contents. The helper is never told where anything lives, so a helper
//!   cannot read what the program did not hand it.
//! - **It is timed.** A helper that stops answering is killed and the call
//!   fails. An in-process library that loops forever cannot be stopped at
//!   all.
//! - **Nothing is installed by running anything.** A helper is a declared
//!   artifact with a recorded hash, fetched by `kora install`. There is no
//!   script to run at install time, here as everywhere else.
//!
//! # The protocol — `stdio/v1`
//!
//! One request per call, one response, framed so that binary payloads do not
//! have to be base64'd through JSON — a rendered page is megabytes, and
//! paying a third again in encoding on every page adds up.
//!
//! Each frame, in both directions:
//!
//! ```text
//! 4 bytes, big-endian   length of the JSON header
//! N bytes               the JSON header
//! then, for each entry in the header's "blobs" array, that many raw bytes
//! ```
//!
//! A request header is `{"id": 1, "call": "render", "args": [...],
//! "blobs": [12345]}`, with the input bytes following. A response header is
//! `{"id": 1, "ok": true, "result": {...}, "blobs": [4096, 4096]}`, or
//! `{"id": 1, "ok": false, "error": "..."}`.
//!
//! Blobs are referred to from the result by index, so a helper can return one
//! value describing many payloads: `{"images": [{"blob": 0}, {"blob": 1}]}`.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use serde_json::{json, Value};

/// A frame larger than this is refused rather than allocated. A helper is a
/// separate program, and a separate program can lie about how much it is
/// about to send.
const MAX_HEADER_BYTES: usize = 1024 * 1024;
const MAX_BLOB_BYTES: usize = 64 * 1024 * 1024;
const MAX_BLOBS: usize = 4096;

#[derive(Debug)]
pub struct HelperError {
    pub message: String,
}

impl HelperError {
    fn new(message: impl Into<String>) -> HelperError {
        HelperError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for HelperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for HelperError {}

/// How to start a helper.
#[derive(Debug, Clone)]
pub struct Config {
    /// The executable, as installed by `kora install`.
    pub program: PathBuf,
    pub args: Vec<String>,
    /// How long one call may take before the helper is killed. A helper that
    /// hangs must not hang the run.
    pub timeout_secs: u64,
}

impl Config {
    pub fn new(program: PathBuf) -> Config {
        Config {
            program,
            args: Vec::new(),
            timeout_secs: 120,
        }
    }
}

/// What a helper answered: one JSON value, and the payloads it refers to.
#[derive(Debug, Default)]
pub struct Response {
    pub value: Value,
    pub blobs: Vec<Vec<u8>>,
}

/// One request in, one response out.
pub trait Transport: Send {
    fn call(&mut self, header: Value, blobs: Vec<Vec<u8>>) -> Result<Response, HelperError>;
}

/// A running helper.
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
    pub fn start(config: &Config) -> Result<Worker, HelperError> {
        Ok(Worker {
            transport: Box::new(ProcessTransport::spawn(config)?),
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

    /// Call `function(args...)` with optional binary input.
    ///
    /// The outer `Result` is the boundary failing — the helper is gone, it
    /// timed out, it spoke nonsense. The inner one is the helper reporting a
    /// failure it understood, which the program should see and handle.
    pub fn call(
        &mut self,
        function: &str,
        args: Vec<Value>,
        blobs: Vec<Vec<u8>>,
    ) -> Result<Result<Response, HelperError>, HelperError> {
        let id = self.next_id;
        self.next_id += 1;

        let header = json!({
            "id": id,
            "call": function,
            "args": args,
        });
        let response = self.transport.call(header, blobs)?;

        if response
            .value
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(Ok(Response {
                value: response.value.get("result").cloned().unwrap_or(Value::Null),
                blobs: response.blobs,
            }));
        }
        Ok(Err(HelperError::new(
            response
                .value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("the helper failed without saying why"),
        )))
    }
}

/// Framed messages over a child process's stdin and stdout.
///
/// Reading happens on its own thread so a call can time out: a blocking read
/// on a pipe cannot be interrupted, and a helper that stops answering would
/// otherwise stop the run with it.
struct ProcessTransport {
    child: Child,
    stdin: ChildStdin,
    frames: Receiver<Result<Response, HelperError>>,
    timeout: Duration,
    /// Once the far end is gone or out of step, every later call fails the
    /// same way rather than blocking on a pipe nobody is reading.
    broken: Option<String>,
}

impl ProcessTransport {
    fn spawn(config: &Config) -> Result<ProcessTransport, HelperError> {
        let mut child = Command::new(&config.program)
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The helper's own stderr goes to the terminal, so a helper that
            // explains itself is heard rather than swallowed.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                HelperError::new(format!(
                    "could not start the helper `{}`: {e}",
                    config.program.display()
                ))
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HelperError::new("the helper has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HelperError::new("the helper has no stdout"))?;

        let (sender, frames) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = stdout;
            loop {
                let frame = read_frame(&mut stdout);
                let stop = frame.is_err();
                if sender.send(frame).is_err() || stop {
                    return;
                }
            }
        });

        Ok(ProcessTransport {
            child,
            stdin,
            frames,
            timeout: Duration::from_secs(config.timeout_secs.max(1)),
            broken: None,
        })
    }

    /// Stop the helper and remember why, so later calls say the same thing.
    fn kill(&mut self, why: impl Into<String>) -> HelperError {
        let why = why.into();
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.broken = Some(why.clone());
        HelperError::new(why)
    }
}

impl Transport for ProcessTransport {
    fn call(&mut self, mut header: Value, blobs: Vec<Vec<u8>>) -> Result<Response, HelperError> {
        if let Some(why) = &self.broken {
            return Err(HelperError::new(why.clone()));
        }

        let lengths: Vec<usize> = blobs.iter().map(Vec::len).collect();
        header["blobs"] = json!(lengths);
        let encoded = serde_json::to_vec(&header)
            .map_err(|e| HelperError::new(format!("could not encode the request: {e}")))?;

        let write = (|| -> std::io::Result<()> {
            self.stdin
                .write_all(&(encoded.len() as u32).to_be_bytes())?;
            self.stdin.write_all(&encoded)?;
            for blob in &blobs {
                self.stdin.write_all(blob)?;
            }
            self.stdin.flush()
        })();
        if let Err(e) = write {
            return Err(self.kill(format!("the helper stopped listening: {e}")));
        }

        match self.frames.recv_timeout(self.timeout) {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(e)) => Err(self.kill(e.message)),
            Err(RecvTimeoutError::Timeout) => Err(self.kill(format!(
                "the helper did not answer within {} seconds",
                self.timeout.as_secs()
            ))),
            Err(RecvTimeoutError::Disconnected) => {
                Err(self.kill("the helper exited; see its output above"))
            }
        }
    }
}

impl Drop for ProcessTransport {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read one framed message: a JSON header, then the payloads it declares.
fn read_frame(source: &mut impl Read) -> Result<Response, HelperError> {
    let mut length = [0u8; 4];
    read_exact(source, &mut length, "the helper exited")?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_HEADER_BYTES {
        return Err(HelperError::new(format!(
            "the helper announced a {length}-byte header, over the {MAX_HEADER_BYTES}-byte limit"
        )));
    }

    let mut header = vec![0u8; length];
    read_exact(source, &mut header, "the helper stopped mid-header")?;
    let header: Value = serde_json::from_slice(&header)
        .map_err(|e| HelperError::new(format!("the helper sent a header that is not JSON: {e}")))?;

    let lengths: Vec<usize> = match header.get("blobs") {
        Some(Value::Array(items)) => {
            if items.len() > MAX_BLOBS {
                return Err(HelperError::new(format!(
                    "the helper announced {} payloads, over the {MAX_BLOBS} limit",
                    items.len()
                )));
            }
            items
                .iter()
                .map(|v| v.as_u64().unwrap_or_default() as usize)
                .collect()
        }
        _ => Vec::new(),
    };

    let mut blobs = Vec::with_capacity(lengths.len());
    for size in lengths {
        if size > MAX_BLOB_BYTES {
            return Err(HelperError::new(format!(
                "the helper announced a {size}-byte payload, over the {MAX_BLOB_BYTES}-byte limit"
            )));
        }
        let mut blob = vec![0u8; size];
        read_exact(source, &mut blob, "the helper stopped mid-payload")?;
        blobs.push(blob);
    }

    Ok(Response {
        value: header,
        blobs,
    })
}

fn read_exact(source: &mut impl Read, into: &mut [u8], cut_short: &str) -> Result<(), HelperError> {
    match source.read_exact(into) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Err(HelperError::new(cut_short)),
        Err(e) => Err(HelperError::new(format!(
            "could not read from the helper: {e}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame a response the way a helper would, so the reader can be tested
    /// without a helper to run.
    fn framed(header: Value, blobs: &[&[u8]]) -> Vec<u8> {
        let mut header = header;
        header["blobs"] = json!(blobs.iter().map(|b| b.len()).collect::<Vec<_>>());
        let encoded = serde_json::to_vec(&header).unwrap();
        let mut out = (encoded.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(&encoded);
        for blob in blobs {
            out.extend_from_slice(blob);
        }
        out
    }

    #[test]
    fn a_frame_carries_its_payloads() {
        let bytes = framed(
            json!({"ok": true, "result": {"pages": 2}}),
            &[b"one", b"two"],
        );
        let frame = read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(frame.value["result"]["pages"], 2);
        assert_eq!(frame.blobs, vec![b"one".to_vec(), b"two".to_vec()]);
    }

    #[test]
    fn a_frame_with_no_payloads_is_fine() {
        let bytes = framed(json!({"ok": true, "result": 7}), &[]);
        let frame = read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(frame.value["result"], 7);
        assert!(frame.blobs.is_empty());
    }

    /// A helper is a separate program: what it announces is a claim, not a
    /// fact, and an enormous one must be refused before it is allocated.
    #[test]
    fn an_oversized_header_is_refused_not_allocated() {
        let mut bytes = (MAX_HEADER_BYTES as u32 + 1).to_be_bytes().to_vec();
        bytes.extend_from_slice(b"{}");
        let err = read_frame(&mut bytes.as_slice()).unwrap_err();
        assert!(err.message.contains("over the"), "{}", err.message);
    }

    #[test]
    fn an_oversized_payload_is_refused_not_allocated() {
        let header = json!({"ok": true, "blobs": [MAX_BLOB_BYTES + 1]});
        let encoded = serde_json::to_vec(&header).unwrap();
        let mut bytes = (encoded.len() as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(&encoded);
        let err = read_frame(&mut bytes.as_slice()).unwrap_err();
        assert!(err.message.contains("over the"), "{}", err.message);
    }

    #[test]
    fn a_helper_that_stops_mid_frame_says_so() {
        let bytes = framed(json!({"ok": true}), &[b"payload"]);
        let truncated = &bytes[..bytes.len() - 3];
        let err = read_frame(&mut &truncated[..]).unwrap_err();
        assert!(err.message.contains("mid-payload"), "{}", err.message);
    }

    struct Canned(Vec<Response>);

    impl Transport for Canned {
        fn call(&mut self, _header: Value, _blobs: Vec<Vec<u8>>) -> Result<Response, HelperError> {
            Ok(self.0.remove(0))
        }
    }

    #[test]
    fn a_failure_the_helper_understood_is_a_value() {
        let mut worker = Worker::with_transport(Box::new(Canned(vec![Response {
            value: json!({"ok": false, "error": "page 3 is not an image"}),
            blobs: Vec::new(),
        }])));
        let outcome = worker.call("render", vec![], vec![]).unwrap();
        assert_eq!(outcome.unwrap_err().message, "page 3 is not an image");
    }
}
