//! Debug Adapter Protocol framing.
//!
//! The wire format is the same as the language server protocol: a
//! `Content-Length` header, a blank line, then a JSON body. Writing it out
//! here rather than taking a dependency keeps the adapter's only third-party
//! crate `serde_json`, which is worth more than the fifty lines it saves.

use std::io::{BufRead, Write};

use serde_json::{json, Value as J};

/// Read one message. `Ok(None)` means the client closed the stream.
pub fn read(input: &mut impl BufRead) -> std::io::Result<Option<J>> {
    let mut length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse().ok();
        }
    }
    let Some(length) = length else {
        return Ok(None);
    };
    let mut body = vec![0u8; length];
    input.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body).ok())
}

/// Write one message, header and all.
pub fn write(output: &mut impl Write, message: &J) -> std::io::Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(output, "Content-Length: {}\r\n\r\n", body.len())?;
    output.write_all(&body)?;
    output.flush()
}

/// Sequence numbers, which every message carries.
#[derive(Default)]
pub struct Seq(i64);

impl Seq {
    pub fn take(&mut self) -> i64 {
        self.0 += 1;
        self.0
    }
}

/// A successful response to `request`.
pub fn response(seq: i64, request: &J, body: J) -> J {
    json!({
        "seq": seq,
        "type": "response",
        "request_seq": request["seq"].as_i64().unwrap_or(0),
        "success": true,
        "command": request["command"].as_str().unwrap_or(""),
        "body": body,
    })
}

/// A failed response, with a message the client shows to the user.
pub fn error(seq: i64, request: &J, message: &str) -> J {
    json!({
        "seq": seq,
        "type": "response",
        "request_seq": request["seq"].as_i64().unwrap_or(0),
        "success": false,
        "command": request["command"].as_str().unwrap_or(""),
        "message": message,
    })
}

pub fn event(seq: i64, name: &str, body: J) -> J {
    json!({ "seq": seq, "type": "event", "event": name, "body": body })
}

/// Read the whole of a reader as messages. Used by tests.
pub fn read_all(bytes: &[u8]) -> Vec<J> {
    let mut cursor = std::io::BufReader::new(bytes);
    let mut out = Vec::new();
    while let Ok(Some(message)) = read(&mut cursor) {
        out.push(message);
    }
    out
}
