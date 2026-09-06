//! `kora dap` starts and speaks the protocol.
//!
//! The adapter's behaviour is covered in `kora-dap`, in process. What is only
//! testable here is the wiring: that the subcommand exists, that the binary
//! launches as an editor launches it, and that the first thing an editor sends
//! gets an answer. That wiring has no other test, and an editor's report of it
//! being broken is "we could not find a debugger for this language".

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

/// Frame a DAP message the way the protocol requires.
fn frame(body: &str) -> Vec<u8> {
    format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

/// Read one framed message.
fn read(input: &mut impl BufRead) -> String {
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        input.read_line(&mut line).expect("a header line");
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            length = value.trim().parse().expect("a length");
        }
    }
    let mut body = vec![0u8; length];
    input.read_exact(&mut body).expect("a body");
    String::from_utf8(body).expect("utf-8")
}

#[test]
fn the_debug_adapter_answers_an_initialize_request() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kora"))
        .arg("dap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("kora dap should start");

    let mut stdin = child.stdin.take().expect("stdin");
    stdin
        .write_all(&frame(
            r#"{"seq":1,"type":"request","command":"initialize","arguments":{"adapterID":"kora"}}"#,
        ))
        .expect("the adapter should accept a request");

    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    let response = read(&mut stdout);
    assert!(
        response.contains("\"command\":\"initialize\""),
        "{response}"
    );
    assert!(response.contains("\"success\":true"), "{response}");
    assert!(
        response.contains("supportsConfigurationDoneRequest"),
        "the editor needs to know what the adapter can do: {response}"
    );

    // `initialized` follows the response, and is what tells the editor it may
    // send breakpoints.
    let event = read(&mut stdout);
    assert!(event.contains("\"event\":\"initialized\""), "{event}");

    stdin
        .write_all(&frame(
            r#"{"seq":2,"type":"request","command":"disconnect","arguments":{}}"#,
        ))
        .expect("the adapter should accept a disconnect");
    drop(stdin);

    let status = child.wait().expect("the adapter should exit");
    assert!(status.success(), "kora dap exited with {status}");
}
