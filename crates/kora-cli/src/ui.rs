//! Small terminal helpers: color, the startup banner, and status lines.
//!
//! Kept dependency-free — raw ANSI codes, gated on `NO_COLOR` and whether
//! stderr/stdout are actually a terminal, so piped output and CI logs stay
//! plain text.

use std::io::IsTerminal;
use std::sync::OnceLock;

const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const MAGENTA: &str = "\x1b[35m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

fn color_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("NO_COLOR").is_none()
            && std::io::stdout().is_terminal()
            && std::io::stderr().is_terminal()
    })
}

fn paint(code: &str, text: &str) -> String {
    if color_enabled() {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

pub fn err() -> String {
    paint(&format!("{BOLD}{RED}"), "error:")
}

pub fn warn() -> String {
    paint(&format!("{BOLD}{YELLOW}"), "warning:")
}

pub fn ok(text: &str) -> String {
    paint(GREEN, text)
}

pub fn bold(text: &str) -> String {
    paint(BOLD, text)
}

pub fn dim(text: &str) -> String {
    paint(DIM, text)
}

pub fn cyan(text: &str) -> String {
    paint(CYAN, text)
}

/// A left-padded status verb, colored by what it means — the same shape
/// `cargo` uses (`   Compiling`, `    Finished`), so the eye already knows
/// how to scan it.
pub fn status(verb: &str, rest: &str) {
    let color = match verb {
        "pass" | "fetched" | "added" | "vendored" | "removed" | "recorded" => GREEN,
        "FAIL" | "unfetched" | "tampered" => RED,
        "unchanged" | "repointed" | "updated" | "skipped" => YELLOW,
        _ => CYAN,
    };
    println!(
        "  {}{}",
        paint(color, &format!("{verb:>9}")),
        if rest.is_empty() {
            String::new()
        } else {
            format!(" {rest}")
        }
    );
}

const LOGO: &str = r" _  __  ___   ____      _
| |/ / / _ \ |  _ \    / \
| ' / | | | || |_) |  / _ \
| . \ | |_| ||  _ <  / ___ \
|_|\_\ \___/ |_| \_\/_/   \_\";

/// Printed once, on `kora` with no arguments and on `kora --version` — the
/// character-art banner every CLI worth its salt shows on the way in.
pub fn banner(version: &str) {
    if color_enabled() {
        for (i, line) in LOGO.lines().enumerate() {
            let code = [MAGENTA, MAGENTA, CYAN, CYAN, CYAN][i % 5];
            println!("{code}{line}{RESET}");
        }
    } else {
        println!("{LOGO}");
    }
    println!("  {} — an agent-first programming language", bold("kora"));
    println!("  {}", dim(&format!("v{version}")));
    println!();
}

/// A single-line spinner-style summary printed after work that has no
/// per-item progress to show, e.g. a batch of fetches that already reported
/// each one as it landed.
pub fn done(summary: &str) {
    println!("{} {}", ok("✓"), summary);
}
