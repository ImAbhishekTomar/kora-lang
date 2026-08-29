//! A server that misbehaves must not take the program with it.
//!
//! These spawn a real child process, because the failure being covered is a
//! property of the pipe: a blocking read on a pipe cannot be given a deadline,
//! and a fake transport would never have hung in the first place.

use std::collections::HashMap;
use std::time::Instant;

/// Seconds a request waits in these tests. Short, so a wedged server is caught
/// quickly; long enough that a loaded CI machine does not trip it by accident.
const TIMEOUT_SECS: u64 = 2;
/// The late-reply test waits out two of these in sequence, so it gets its own.
/// `awkward_server.py` sleeps `LATE_TIMEOUT_SECS * 3 / 2` before the stale
/// answer; the two must be changed together.
const LATE_TIMEOUT_SECS: u64 = 4;

/// Whether a Python interpreter is available. The fixture server is a Python
/// script, so a machine without one skips these rather than failing -- the
/// same gate `python_test.rs` already uses.
fn python_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn config(mode: &str, tally: Option<&std::path::Path>) -> kora_mcp::ServerConfig {
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/awkward_server.py");
    let mut args = vec![fixture.to_string_lossy().into_owned(), mode.to_string()];
    if let Some(path) = tally {
        args.push(path.to_string_lossy().into_owned());
    }
    kora_mcp::ServerConfig {
        command: "python3".to_string(),
        args,
        env: HashMap::new(),
        timeout_secs: TIMEOUT_SECS,
        // The handshake is fine in every one of these; retrying it would only
        // add a second spawn to each test.
        max_retries: 0,
    }
}

/// The second call in the late-reply test waits on the stale one, so it needs
/// more room than the deliberately short default.
fn connect_with_timeout(
    mode: &str,
    timeout_secs: u64,
    tally: Option<&std::path::Path>,
) -> kora_mcp::Server {
    let config = kora_mcp::ServerConfig {
        timeout_secs,
        ..config(mode, tally)
    };
    kora_mcp::Server::connect("awkward", &config).expect("the handshake succeeds")
}

fn connect(mode: &str, tally: Option<&std::path::Path>) -> kora_mcp::Server {
    kora_mcp::Server::connect("awkward", &config(mode, tally)).expect("the handshake succeeds")
}

#[test]
fn a_server_that_never_answers_times_out_instead_of_hanging() {
    if !python_available() {
        return;
    }
    let mut server = connect("wedge", None);
    let started = Instant::now();
    let error = server
        .call("act", serde_json::json!({}))
        .expect_err("a call that is never answered is an error");

    assert!(
        error.message.contains("did not answer"),
        "the message should say what happened, got: {}",
        error.message
    );
    // The bug this covers is unbounded waiting, so the bound is the assertion.
    assert!(
        started.elapsed().as_secs() < TIMEOUT_SECS + 8,
        "the call took {:?}, which is not a timeout",
        started.elapsed()
    );
}

#[test]
fn a_server_that_exits_mid_call_reports_the_closed_connection() {
    if !python_available() {
        return;
    }
    let mut server = connect("die", None);
    let error = server
        .call("act", serde_json::json!({}))
        .expect_err("a server that exits cannot answer");
    assert!(
        error.message.contains("closed the connection"),
        "got: {}",
        error.message
    );
}

#[test]
fn a_tool_call_is_never_repeated() {
    if !python_available() {
        return;
    }
    // The safety property. A model call may be retried because generating
    // twice costs tokens and nothing else. A tool call may have sent a
    // message or charged a card, and a timeout is exactly the case where
    // whether it ran is unknown -- so it must be attempted once.
    let tally = std::env::temp_dir().join(format!(
        "kora-mcp-tally-{}-{:?}.log",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&tally);

    let mut server = kora_mcp::Server::connect(
        "awkward",
        &kora_mcp::ServerConfig {
            // Retries are *on*, so a client that retried tool calls would
            // show three of them in the tally rather than one.
            max_retries: 2,
            ..config("wedge", Some(&tally))
        },
    )
    .expect("the handshake succeeds");
    server
        .call("act", serde_json::json!({}))
        .expect_err("the server never answers");

    let ran = std::fs::read_to_string(&tally).unwrap_or_default();
    assert_eq!(
        ran.lines().count(),
        1,
        "the side effect ran {} times, not once",
        ran.lines().count()
    );
    let _ = std::fs::remove_file(&tally);
}

#[test]
fn a_late_answer_is_not_read_as_the_reply_to_the_next_call() {
    if !python_available() {
        return;
    }
    // After a timeout the server may still answer. That stale reply must not
    // be handed to whatever the program asked next, which would be a wrong
    // answer rather than an error -- the worse of the two failures.
    // Both waits need room on a loaded machine. The fixture sleeps half again
    // as long as this timeout, which leaves the same margin either side: the
    // first call gives up well before the stale answer, and the second still
    // has time left when that answer arrives.
    let mut server = connect_with_timeout("late", LATE_TIMEOUT_SECS, None);
    server
        .call("act", serde_json::json!({}))
        .expect_err("the first call times out");

    // The fixture holds this answer back until the stale one has been sent,
    // so the reply being skipped has definitely arrived first.
    let second = server
        .call("act", serde_json::json!({}))
        .expect("the server is still up and answers the second call");
    assert_eq!(second, "answer to the second call");
}

#[test]
fn starting_a_server_is_retried() {
    // The other half of the rule. A tool call is attempted once because it
    // may have had an effect; starting a server has had none, so a process
    // that dies on the first try is worth another.
    if !python_available() {
        return;
    }
    let attempts = std::env::temp_dir().join(format!("kora-mcp-starts-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&attempts);

    let server = kora_mcp::Server::connect(
        "awkward",
        &kora_mcp::ServerConfig {
            max_retries: 2,
            ..config("flaky", Some(&attempts))
        },
    );
    assert!(
        server.is_ok(),
        "the third start should succeed: {:?}",
        server.err().map(|e| e.message)
    );
    assert_eq!(
        std::fs::read_to_string(&attempts)
            .unwrap_or_default()
            .lines()
            .count(),
        3,
        "three starts, the first two of which died"
    );
    let _ = std::fs::remove_file(&attempts);
}

#[test]
fn a_server_that_never_starts_gives_up_rather_than_looping() {
    let error = kora_mcp::Server::connect(
        "missing",
        &kora_mcp::ServerConfig {
            command: "kora-no-such-command".to_string(),
            args: Vec::new(),
            env: HashMap::new(),
            timeout_secs: TIMEOUT_SECS,
            max_retries: 1,
        },
    )
    .expect_err("there is no such program");
    assert!(
        error.message.contains("could not start"),
        "got: {}",
        error.message
    );
}
