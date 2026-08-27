//! The `kora` command-line tool.
//!
//! Phase 1: `kora run <file.ko>` and `--version`. More subcommands
//! (`test`, `audit`, `trace`) arrive with their phases — see DECISIONS.md.

use std::process::ExitCode;

use kora_runtime::Interpreter;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") | Some("version") => {
            println!("kora {VERSION}");
            ExitCode::SUCCESS
        }
        Some("run") => match args.get(1) {
            Some(path) => run_file(path),
            None => {
                eprintln!("usage: kora run <file.ko>");
                ExitCode::from(2)
            }
        },
        Some(path) if path.ends_with(".ko") => run_file(path),
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

const USAGE: &str = "\
usage:
  kora run <file.ko>    run a program
  kora <file.ko>        same as `kora run`
  kora --version        print version";

fn run_file(path: &str) -> ExitCode {
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

    let mut interp = Interpreter::new();
    interp.direct_stdout = true;
    match interp.run(&program) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprint!("{}", e.render(&source, path));
            ExitCode::from(1)
        }
    }
}
