//! The `kora` command-line tool.
//!
//! Phase 2: `kora run <file.ko>` with record/replay of model calls.
//! `test`, `audit`, and `trace` arrive with their phases — see DECISIONS.md.

use std::path::Path;
use std::process::ExitCode;

use kora_runtime::{Cassette, Config, Interpreter, Mode};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
usage:
  kora run <file.ko>           run a program
  kora <file.ko>               same as `kora run`
  kora --version               print version

flags for `run`:
  --record                     call models, then save the calls to a cassette
  --replay                     use only recorded calls; never reach a provider
  --report                     print token usage after the run";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") | Some("version") => {
            println!("kora {VERSION}");
            ExitCode::SUCCESS
        }
        Some("run") => run_args(&args[1..]),
        Some(first) if first.ends_with(".ko") => run_args(&args),
        Some(other) => {
            eprintln!("kora: unknown command `{other}`");
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        None => {
            println!("Kora — an agent-first programming language");
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
    }
}

fn run_args(args: &[String]) -> ExitCode {
    let mut path: Option<&str> = None;
    let mut mode = Mode::Live;
    let mut report = false;

    for arg in args {
        match arg.as_str() {
            "--record" => mode = Mode::Record,
            "--replay" => mode = Mode::Replay,
            "--report" => report = true,
            other if other.starts_with("--") => {
                eprintln!("kora: unknown flag `{other}`");
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
            other => path = Some(other),
        }
    }

    let Some(path) = path else {
        eprintln!("usage: kora run <file.ko>");
        return ExitCode::from(2);
    };
    run_file(path, mode, report)
}

fn run_file(path: &str, mode: Mode, report: bool) -> ExitCode {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read `{path}`: {e}");
            return ExitCode::from(1);
        }
    };

    let program = match kora_syntax::parse(&source) {
        Ok(p) => p,
        Err(e) => {
            eprint!("{}", e.render(&source, path));
            return ExitCode::from(1);
        }
    };

    let program_path = Path::new(path);
    let mut interp = Interpreter::new();
    interp.direct_stdout = true;
    interp.program_name = path.to_string();
    interp.config = Config::discover(program_path);
    interp.cassette = Some(std::sync::Arc::new(std::sync::Mutex::new(Cassette::open(
        mode,
        program_path,
    ))));

    let result = interp.run(&program);

    if let Some(cassette) = &interp.cassette {
        let cassette = cassette.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = cassette.save() {
            eprintln!("warning: could not write cassette: {e}");
        }
    }

    if report {
        eprintln!(
            "\n  tokens: {} in / {} out    model calls: {}",
            interp.tokens_in, interp.tokens_out, interp.model_calls
        );
    }

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprint!("{}", e.render(&source, path));
            ExitCode::from(1)
        }
    }
}
