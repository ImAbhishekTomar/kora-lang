//! Killing a durable run must not make its writes happen twice.
//!
//! The other durability tests drive the journal directly, which proves the
//! bookkeeping but not the promise: what a user actually does is `kill -9` a
//! half-finished pipeline and resume it. This test does exactly that against
//! the real binary, and then counts the rows the program wrote.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("kora-crash-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Eight rows, each appending one line and then burning enough time that the
/// process is reliably still running when the test kills it.
const PIPELINE: &str = r#"
use fs

def main():
    rows = ["r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7"]
    for row in rows:
        match fs.append("OUT", f"{row}\n"):
            case Ok(_):
                total = 0
                for i in range(120000):
                    total = total + i
            case Err(why):
                print(why)
    print("done")
"#;

/// A path as a Kora string literal.
///
/// Windows separators are backslashes, and a backslash in Kora source starts
/// an escape — `C:\Users\...` is `\U`, which is not one. Forward slashes are
/// accepted by the Windows APIs underneath, so this is a change of spelling
/// rather than of meaning.
fn ko_path(path: &Path) -> String {
    path.to_str().expect("a UTF-8 path").replace('\\', "/")
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.to_string())
        .collect()
}

/// The run id `kora runs` reports for the program's single run.
fn only_run_id(program: &Path) -> String {
    let listing = Command::new(env!("CARGO_BIN_EXE_kora"))
        .arg("runs")
        .arg(program)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&listing.stdout).to_string();
    let ids: Vec<String> = text
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|word| word.len() > 8 && word.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|w| w.to_string())
        .collect();
    assert_eq!(ids.len(), 1, "expected one run, got: {text}");
    ids[0].clone()
}

#[test]
fn a_killed_pipeline_resumes_without_writing_any_row_twice() {
    let scratch = Scratch::new("resume");
    let out = scratch.0.join("out.txt");
    let program = scratch.write("pipeline.ko", &PIPELINE.replace("OUT", &ko_path(&out)));

    let mut child = Command::new(env!("CARGO_BIN_EXE_kora"))
        .arg("run")
        .arg(&program)
        .arg("--durable")
        .spawn()
        .unwrap();

    // Kill once the pipeline is underway but not finished. Waiting on the
    // file the program writes, rather than on a timer, keeps this from
    // depending on how fast the machine is.
    let start = std::time::Instant::now();
    loop {
        let written = lines(&out).len();
        if (2..=6).contains(&written) {
            break;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(60),
            "pipeline never reached the middle: {} lines written",
            written
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();

    let killed_at = lines(&out);
    assert!(
        killed_at.len() < 8,
        "the test killed the run too late to prove anything"
    );

    let id = only_run_id(&program);
    let resumed = Command::new(env!("CARGO_BIN_EXE_kora"))
        .arg("run")
        .arg(&program)
        .arg("--durable")
        .arg("--resume")
        .arg(&id)
        .output()
        .unwrap();
    let finished = lines(&out);
    // No row is ever written twice. That holds on both endings: the usual
    // one, where the run finishes all eight rows, and the narrow one where
    // the kill landed between a write and the line recording it — there the
    // resume refuses, naming the call whose fate nobody can know.
    for row in 0..8 {
        let name = format!("r{row}");
        assert!(
            finished.iter().filter(|line| **line == name).count() <= 1,
            "{name} was written more than once: {finished:?}"
        );
    }
    if resumed.status.success() {
        assert_eq!(
            finished.len(),
            8,
            "a resume that succeeds finishes the pipeline, got {finished:?}"
        );
    } else {
        let complaint = String::from_utf8_lossy(&resumed.stderr).to_string();
        assert!(
            complaint.contains("whether it finished is unknown"),
            "the only allowed failure is an interrupted write: {complaint}"
        );
    }
    // The rows written before the crash are still the ones on disk, in order:
    // a resumed run continues the file rather than starting it again.
    assert_eq!(&finished[..killed_at.len()], killed_at.as_slice());
}

#[test]
fn a_run_cannot_be_resumed_twice_at_once() {
    let scratch = Scratch::new("lock");
    let out = scratch.0.join("out.txt");
    let program = scratch.write("pipeline.ko", &PIPELINE.replace("OUT", &ko_path(&out)));

    let mut first = Command::new(env!("CARGO_BIN_EXE_kora"))
        .arg("run")
        .arg(&program)
        .arg("--durable")
        .spawn()
        .unwrap();

    let start = std::time::Instant::now();
    while lines(&out).is_empty() {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(60),
            "pipeline never started writing"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let id = only_run_id(&program);

    let second = Command::new(env!("CARGO_BIN_EXE_kora"))
        .arg("run")
        .arg(&program)
        .arg("--durable")
        .arg("--resume")
        .arg(&id)
        .output()
        .unwrap();
    let complaint = String::from_utf8_lossy(&second.stderr).to_string();

    first.kill().unwrap();
    first.wait().unwrap();

    assert!(
        !second.status.success(),
        "a second process must not join a live run"
    );
    assert!(
        complaint.contains("already open in another process"),
        "the refusal should say why: {complaint}"
    );
}

/// Reads are not replayed from the journal, so the guarantee is the other
/// one: a resume that would read different data stops instead.
const READER: &str = r#"
use fs

def main():
    match fs.read("IN"):
        case Ok(text):
            rows = ["r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7"]
            for row in rows:
                match fs.append("OUT", f"{row}:{text}"):
                    case Ok(_):
                        total = 0
                        for i in range(120000):
                            total = total + i
                    case Err(why):
                        print(why)
        case Err(why):
            print(why)
    print("done")
"#;

#[test]
fn a_resume_refuses_input_that_changed_since_the_run_started() {
    let scratch = Scratch::new("input");
    let input = scratch.write("input.txt", "first\n");
    let out = scratch.0.join("out.txt");
    let program = scratch.write(
        "reader.ko",
        &READER
            .replace("IN", &ko_path(&input))
            .replace("OUT", &ko_path(&out)),
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_kora"))
        .arg("run")
        .arg(&program)
        .arg("--durable")
        .spawn()
        .unwrap();

    let start = std::time::Instant::now();
    loop {
        if (2..=6).contains(&lines(&out).len()) {
            break;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(60),
            "pipeline never reached the middle"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();

    // Someone edits the input before the resume. Continuing would mix two
    // different inputs into one run's output.
    std::fs::write(&input, "second\n").unwrap();

    let id = only_run_id(&program);
    let resumed = Command::new(env!("CARGO_BIN_EXE_kora"))
        .arg("run")
        .arg(&program)
        .arg("--durable")
        .arg("--resume")
        .arg(&id)
        .output()
        .unwrap();
    let complaint = String::from_utf8_lossy(&resumed.stderr).to_string();

    assert!(
        !resumed.status.success(),
        "a resume against changed input must stop"
    );
    assert!(
        complaint.contains("different data than when this run first read it"),
        "the refusal should name what changed: {complaint}"
    );
}

// --- a streamed answer, killed half-written ---

use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A provider that streams an answer slowly enough to be killed in the
/// middle of it, and counts the requests that reach it.
fn spawn_slow_streaming_provider() -> (String, Arc<AtomicUsize>) {
    use std::io::{BufRead, BufReader, Read};

    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&requests);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = rest.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            let _ = reader.read_exact(&mut body);
            seen.fetch_add(1, Ordering::SeqCst);

            // Chunked, so the body can be written a piece at a time with the
            // client already reading. Without it the whole answer would
            // arrive at once and there would be no middle to kill.
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            );
            let _ = stream.flush();
            let frames = [
                r#"{"ans"#,
                r#"wer":"one "#,
                "two ",
                "three ",
                "four ",
                r#"five"}"#,
            ];
            for frame in frames {
                let event =
                    serde_json::json!({"message": {"content": frame}, "done": false}).to_string();
                let line = format!("{event}\n");
                if stream
                    .write_all(format!("{:x}\r\n{line}\r\n", line.len()).as_bytes())
                    .is_err()
                {
                    break;
                }
                let _ = stream.flush();
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            let done = serde_json::json!({
                "message": {"content": ""},
                "done": true,
                "prompt_eval_count": 3,
                "eval_count": 2,
            })
            .to_string();
            let line = format!("{done}\n");
            let _ = stream.write_all(format!("{:x}\r\n{line}\r\n0\r\n\r\n", line.len()).as_bytes());
            let _ = stream.flush();
        }
    });

    (format!("http://127.0.0.1:{port}"), requests)
}

/// Every piece the handler saw, appended one per line so the test can watch
/// the answer arrive from outside the process.
const STREAM_PROGRAM: &str = r#"
use fs

def main():
    answer: str = analyze("q", "count") on token(t):
        fs.append("OUT", f"{t}\n")
    match answer:
        case Ok(text):
            fs.append("OUT", f"ok: {text}\n")
        case Failed(why):
            fs.append("OUT", "failed\n")
    fs.append("OUT", "after\n")
"#;

#[test]
fn a_killed_stream_resumes_without_a_second_request_or_a_second_piece() {
    // The promise streaming makes durable runs is narrow and worth testing
    // against the real binary: a killed stream is never sent again, and the
    // half of the answer the user already saw is never written twice.
    let scratch = Scratch::new("stream");
    let (endpoint, requests) = spawn_slow_streaming_provider();
    scratch.write(
        "kora.toml",
        &format!(
            "[models]\ndefault = \"local:test-model\"\nmax_retries = 0\n\n[models.local]\nendpoint = \"{endpoint}\"\n"
        ),
    );
    let out = scratch.0.join("out.txt");
    let program = scratch.write("stream.ko", &STREAM_PROGRAM.replace("OUT", &ko_path(&out)));

    let mut child = Command::new(env!("CARGO_BIN_EXE_kora"))
        .arg("run")
        .arg(&program)
        .arg("--durable")
        .spawn()
        .unwrap();

    let start = std::time::Instant::now();
    loop {
        if (2..=4).contains(&lines(&out).len()) {
            break;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(60),
            "the stream never reached the middle: {:?}",
            lines(&out)
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();

    let killed_at = lines(&out);
    assert!(
        !killed_at.iter().any(|l| l == "after"),
        "the test killed the run too late to prove anything: {killed_at:?}"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    let id = only_run_id(&program);
    let resumed = Command::new(env!("CARGO_BIN_EXE_kora"))
        .arg("run")
        .arg(&program)
        .arg("--durable")
        .arg("--resume")
        .arg(&id)
        .output()
        .unwrap();
    assert!(
        resumed.status.success(),
        "the resume should finish: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );

    let finished = lines(&out);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "an interrupted stream must not reach the provider again"
    );
    assert_eq!(
        &finished[..killed_at.len()],
        killed_at.as_slice(),
        "the pieces already written stay as they were"
    );
    assert_eq!(
        finished[killed_at.len()..],
        ["failed".to_string(), "after".to_string()],
        "the program sees the interrupted stream as a failure and carries on"
    );
}
