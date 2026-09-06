//! What confinement actually stops, checked against the operating system.
//!
//! These run a shell rather than a helper. A helper speaks `stdio/v1` and
//! would need to be built first; what is under test here is the confinement
//! itself — whether the process Kora starts can open a socket or write a file
//! — and a shell can be asked to try both in one line. The mechanism is
//! identical: `sandbox::command` is what `Worker::start` calls.
//!
//! On a platform with no mechanism (Windows) every case is skipped rather
//! than asserted, because there is nothing to assert.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use kora_helper::sandbox::{self, Applied, Needs};

/// Run `/bin/sh -c script` under `needs`, and say whether it succeeded.
fn shell(script: &str, needs: Needs) -> Option<bool> {
    if !Path::new("/bin/sh").exists() {
        return None;
    }
    let (mut command, applied) = sandbox::command(
        Path::new("/bin/sh"),
        &["-c".to_string(), script.to_string()],
        needs,
    );
    if applied == Applied::None {
        // Nothing was applied, so there is nothing this test can claim.
        return None;
    }
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the shell starts");
    Some(status.success())
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kora-sandbox-{name}-{}", std::process::id()))
}

#[test]
fn a_confined_helper_cannot_write_a_file() {
    let target = scratch("write");
    let _ = std::fs::remove_file(&target);
    let script = format!("echo written > {}", target.display());

    let Some(succeeded) = shell(&script, Needs::default()) else {
        return;
    };
    assert!(!succeeded, "the write was allowed");
    assert!(
        !target.exists() || std::fs::read(&target).unwrap().is_empty(),
        "a confined helper left {} behind",
        target.display()
    );
    let _ = std::fs::remove_file(&target);
}

/// The importer granting `fs` is what turns the write back on. Without this
/// the feature would be a switch nobody can reach.
#[test]
fn granting_fs_lets_the_write_through() {
    let target = scratch("granted");
    let _ = std::fs::remove_file(&target);
    let script = format!("echo written > {}", target.display());

    let Some(succeeded) = shell(
        &script,
        Needs {
            net: false,
            fs: true,
        },
    ) else {
        return;
    };
    assert!(succeeded, "the write was refused despite an `fs` grant");
    assert!(target.exists(), "nothing was written");
    let _ = std::fs::remove_file(&target);
}

/// The claim is that a helper cannot reach the network at all, so the test
/// asks for a connection to a host that certainly exists rather than to
/// something local that might be firewalled anyway.
#[test]
fn a_confined_helper_cannot_open_a_connection() {
    // `sh` has no socket builtin, so this leans on whichever of these the
    // machine has. If none is present the case is skipped, not passed.
    let probes = [
        "command -v curl >/dev/null && curl -s -m 5 -o /dev/null http://example.com",
        "command -v python3 >/dev/null && python3 -c \"import socket; socket.create_connection(('example.com', 80), 5)\"",
    ];
    for probe in probes {
        let Some(unconfined_succeeded) = shell(
            probe,
            Needs {
                net: true,
                fs: true,
            },
        ) else {
            return;
        };
        if !unconfined_succeeded {
            // No tool, or no network on this machine. Either way the confined
            // run proves nothing.
            continue;
        }
        let Some(confined_succeeded) = shell(
            probe,
            Needs {
                net: false,
                fs: true,
            },
        ) else {
            return;
        };
        assert!(
            !confined_succeeded,
            "a confined helper reached the network with `{probe}`"
        );
        return;
    }
}

/// A helper that asked for everything is started directly. Wrapping it would
/// cost a process and change nothing, and the caller is told plainly that
/// nothing was applied rather than being left to assume it was.
#[test]
fn asking_for_everything_applies_nothing_and_says_so() {
    let (_, applied) = sandbox::command(
        Path::new("/bin/sh"),
        &[],
        Needs {
            net: true,
            fs: true,
        },
    );
    assert_eq!(applied, Applied::None);
    assert!(!applied.describes_confinement());
}
