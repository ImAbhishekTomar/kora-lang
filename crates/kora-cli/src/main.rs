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
  kora test <file.ko>          run the `test` blocks in a file
  kora audit <file.ko>         list every declassification site
  kora runs <file.ko>          list durable runs and their status
  kora answer <file.ko> <id> <text>
                               answer a suspended run and resume it
  kora --version               print version

flags for `run`:
  --record                     call models, then save the calls to a cassette
  --replay                     use only recorded calls; never reach a provider
  --report                     print token usage after the run
  --durable                    journal every effect; survives being killed
                               and can suspend on ask_human
  --resume <run-id>            continue a durable run that was interrupted";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") | Some("version") => {
            println!("kora {VERSION}");
            ExitCode::SUCCESS
        }
        Some("run") => run_args(&args[1..]),
        Some("test") => match args.get(1) {
            Some(path) => test_file(path),
            None => {
                eprintln!("usage: kora test <file.ko>");
                ExitCode::from(2)
            }
        },
        Some("runs") => match args.get(1) {
            Some(path) => list_runs(path),
            None => {
                eprintln!("usage: kora runs <file.ko>");
                ExitCode::from(2)
            }
        },
        Some("answer") => match (args.get(1), args.get(2)) {
            (Some(path), Some(id)) => {
                let text = args[3..].join(" ");
                if text.is_empty() {
                    eprintln!("usage: kora answer <file.ko> <run-id> <text>");
                    return ExitCode::from(2);
                }
                answer_run(path, id, &text)
            }
            _ => {
                eprintln!("usage: kora answer <file.ko> <run-id> <text>");
                ExitCode::from(2)
            }
        },
        Some("audit") => match args.get(1) {
            Some(path) => audit_file(path),
            None => {
                eprintln!("usage: kora audit <file.ko>");
                ExitCode::from(2)
            }
        },
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
    let mut durable = false;
    let mut resume: Option<String> = None;

    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--resume" => match iter.next() {
                Some(id) => {
                    resume = Some(id.clone());
                    durable = true;
                }
                None => {
                    eprintln!("kora: --resume needs a run id");
                    return ExitCode::from(2);
                }
            },
            "--record" => mode = Mode::Record,
            "--replay" => mode = Mode::Replay,
            "--report" => report = true,
            "--durable" => durable = true,
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
    run_file(path, mode, report, durable, resume)
}

/// `kora test` — run the `test` blocks in a file.
///
/// Model calls replay from the cassette, so a suite costs nothing and gives
/// the same answer every time. A test that wants a live model is the
/// exception, not the default.
fn test_file(path: &str) -> ExitCode {
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
    let config = Config::discover(program_path);

    // Collect the tests by running the file's top level once.
    let mut collector = Interpreter::new();
    collector.collecting_tests = true;
    collector.program_name = path.to_string();
    collector.config = config.clone();
    collector.sinks = config.sinks.clone();
    if let Err(e) = collector.run_top_level(&program) {
        eprint!("{}", e.render(&source, path));
        return ExitCode::from(1);
    }

    let tests = collector.tests.clone();
    if tests.is_empty() {
        println!("no tests found in {path}");
        println!("write one with: test \"it works\":");
        return ExitCode::SUCCESS;
    }

    let mut passed = 0;
    let mut failed = Vec::new();
    for (name, body) in &tests {
        let mut interp = Interpreter::new();
        interp.program_name = path.to_string();
        interp.config = config.clone();
        interp.sinks = config.sinks.clone();
        interp.allow_private_hosts = config.http_allow_private;
        interp.http_timeout_secs = config.http_timeout_secs;
        interp.cassette = Some(std::sync::Arc::new(std::sync::Mutex::new(Cassette::open(
            Mode::Replay,
            program_path,
        ))));
        // Each test re-runs the file's definitions, then its own body, so
        // tests cannot leak state into one another.
        let outcome = interp
            .run_top_level(&program)
            .and_then(|()| interp.run_block(body));

        match outcome {
            Ok(()) => {
                println!("  pass  {name}");
                passed += 1;
            }
            Err(e) => {
                println!("  FAIL  {name}");
                println!("        {}", e.message);
                failed.push((name.clone(), e));
            }
        }
    }

    println!();
    if failed.is_empty() {
        println!("{passed} passed");
        ExitCode::SUCCESS
    } else {
        println!("{passed} passed, {} failed", failed.len());
        ExitCode::from(1)
    }
}

/// `kora audit` — the complete inventory of declassification sites.
///
/// Complete because every release goes through a `declassify` block, so the
/// parser can enumerate them all. No Python framework can promise this list.
fn audit_file(path: &str) -> ExitCode {
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
    let sites = kora_runtime::audit::audit(&program, path);
    print!("{}", kora_runtime::audit::render(&sites));
    ExitCode::SUCCESS
}

/// `kora runs` — durable runs for a program and where each one stands.
fn list_runs(path: &str) -> ExitCode {
    let dir = kora_runtime::journal::runs_dir(Path::new(path));
    let Ok(entries) = std::fs::read_dir(&dir) else {
        println!("no durable runs yet");
        return ExitCode::SUCCESS;
    };
    let mut runs: Vec<kora_runtime::Run> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| kora_runtime::journal::load_run(&e.path()).ok())
        .collect();
    runs.sort_by(|a, b| b.updated.cmp(&a.updated));

    if runs.is_empty() {
        println!("no durable runs yet");
        return ExitCode::SUCCESS;
    }
    for run in &runs {
        let status = match run.status {
            kora_runtime::RunStatus::Running => "running",
            kora_runtime::RunStatus::Suspended => "suspended",
            kora_runtime::RunStatus::Completed => "completed",
            kora_runtime::RunStatus::Failed => "failed",
        };
        println!("  {}  {:<10} {} steps", run.id, status, run.entries.len());
        if let Some(p) = &run.pending {
            println!("      waiting: {}", p.question);
            println!("      answer with: kora answer {path} {} <text>", run.id);
        }
    }
    ExitCode::SUCCESS
}

/// `kora answer` — record a human answer and continue the run.
fn answer_run(path: &str, id: &str, text: &str) -> ExitCode {
    let run_path = kora_runtime::journal::run_path(Path::new(path), id);
    let mut run = match kora_runtime::journal::load_run(&run_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: cannot read run `{id}`: {e}");
            return ExitCode::from(1);
        }
    };
    let Some(pending) = run.pending.clone() else {
        eprintln!("run `{id}` is not waiting for an answer");
        return ExitCode::from(1);
    };

    // Append the answer as the effect the suspended step was waiting for,
    // then let the program replay up to that point and carry on.
    run.entries.push(kora_runtime::journal::Entry {
        scope: pending.scope.clone(),
        seq: pending.seq,
        site: pending.site.clone(),
        effect: kora_runtime::journal::Effect::Human {
            question: pending.question.clone(),
            answer: text.to_string(),
        },
    });
    run.pending = None;
    run.status = kora_runtime::RunStatus::Running;

    if let Err(e) = std::fs::write(
        &run_path,
        format!("{}\n", serde_json::to_string_pretty(&run).unwrap()),
    ) {
        eprintln!("error: cannot update run `{id}`: {e}");
        return ExitCode::from(1);
    }

    run_file(path, Mode::Live, false, true, Some(id.to_string()))
}

fn run_file(
    path: &str,
    mode: Mode,
    report: bool,
    durable: bool,
    resume_id: Option<String>,
) -> ExitCode {
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
    interp.sinks = interp.config.sinks.clone();
    interp.allow_private_hosts = interp.config.http_allow_private;
    interp.http_timeout_secs = interp.config.http_timeout_secs;
    interp.cassette = Some(std::sync::Arc::new(std::sync::Mutex::new(Cassette::open(
        mode,
        program_path,
    ))));

    // A durable run journals every effect, so it survives being killed and
    // can park on `ask_human` for as long as the answer takes.
    let run_id = resume_id.unwrap_or_else(kora_runtime::journal::new_run_id);
    if durable {
        let run_path = kora_runtime::journal::run_path(program_path, &run_id);
        let run = kora_runtime::journal::load_run(&run_path)
            .unwrap_or_else(|_| kora_runtime::Run::new(run_id.clone(), path.to_string()));
        interp.journal = std::sync::Arc::new(std::sync::Mutex::new(kora_runtime::Journal::open(
            run, run_path,
        )));
    }

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
        Ok(()) => {
            if durable {
                let mut journal = interp.journal.lock().unwrap_or_else(|e| e.into_inner());
                let _ = journal.finish(kora_runtime::RunStatus::Completed);
            }
            ExitCode::SUCCESS
        }
        // Suspension is not a failure: the run is parked, waiting on a person.
        Err(e) if e.is_suspension() => {
            let journal = interp.journal.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(pending) = &journal.run().pending {
                println!("\nrun {run_id} is waiting for an answer:");
                println!("  {}", pending.question);
                if !pending.context.is_empty() {
                    println!("  context: {}", pending.context);
                }
                println!("\nanswer with:");
                println!("  kora answer {path} {run_id} <your answer>");
            }
            ExitCode::from(3)
        }
        Err(e) => {
            if durable {
                let mut journal = interp.journal.lock().unwrap_or_else(|e| e.into_inner());
                let _ = journal.finish(kora_runtime::RunStatus::Failed);
            }
            eprint!("{}", e.render(&source, path));
            ExitCode::from(1)
        }
    }
}
