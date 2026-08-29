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
  kora check <file.ko>...      parse and check without running
    --syntax                   parse only; skip name resolution
  kora test <file.ko>          run the `test` blocks in a file
  kora audit <file.ko>         list every declassification site
  kora tree <file.ko>          show the packages the program actually uses
  kora install <file.ko>       fetch the dependencies the program uses
    --jobs <n>                 how many fetches at once
  kora runs <file.ko>          list durable runs and their status
  kora answer <file.ko> <id> <text>
                               answer a suspended run and resume it
  kora trace <file.ko>         show the spans from the last run
  kora lsp                     run the language server (used by editors)
  kora dap                     run the debug adapter (used by editors)
  kora --version               print version

flags for `run`:
  --record                     call models, then save the calls to a cassette
  --replay                     use only recorded calls; never reach a provider
  --report                     print token usage after the run
  --trace                      record OpenTelemetry spans for this run
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
        // Started by the editor over stdio, not by a person.
        Some("lsp") => match kora_lsp::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("kora lsp: {e}");
                ExitCode::from(1)
            }
        },
        // Also started by the editor: breakpoints, stepping, and the stack.
        Some("dap") => match kora_dap::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("kora dap: {e}");
                ExitCode::from(1)
            }
        },
        Some("check") => {
            let syntax_only = args.iter().any(|a| a == "--syntax");
            let paths: Vec<String> = args[1..]
                .iter()
                .filter(|a| !a.starts_with("--"))
                .cloned()
                .collect();
            if paths.is_empty() {
                eprintln!("usage: kora check [--syntax] <file.ko>...");
                return ExitCode::from(2);
            }
            check_files(&paths, syntax_only)
        }
        Some("test") => match args.get(1) {
            Some(path) => test_file(path),
            None => {
                eprintln!("usage: kora test <file.ko>");
                ExitCode::from(2)
            }
        },
        Some("trace") => match args.get(1) {
            Some(path) => show_trace(path),
            None => {
                eprintln!("usage: kora trace <file.ko>");
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
        Some("install") => {
            let jobs = flag_value(&args, "--jobs").and_then(|v| v.parse().ok());
            match args[1..].iter().find(|a| !a.starts_with("--")) {
                Some(path) => install_packages(path, jobs),
                None => {
                    eprintln!("usage: kora install [--jobs <n>] <file.ko>");
                    ExitCode::from(2)
                }
            }
        }
        Some("tree") => match args.get(1) {
            Some(path) => package_tree(path),
            None => {
                eprintln!("usage: kora tree <file.ko>");
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
    let mut trace = false;
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
            "--trace" => trace = true,
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
    run_file(path, mode, report, durable, resume, trace)
}

/// `kora check` — parse and check files without running them.
///
/// The same analysis the editor shows, as a command: useful in CI, and the
/// only way to check a file that needs resources this machine does not have.
/// The value after a `--flag`, when one was given.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let index = args.iter().position(|a| a == flag)?;
    args.get(index + 1)
        .filter(|v| !v.starts_with("--"))
        .cloned()
}

/// `kora install` — fetch the dependencies the program actually uses.
///
/// Only what the source imports is fetched: a dependency declared and never
/// imported is not downloaded, so a typo'd name never reaches the disk.
fn install_packages(path: &str, jobs: Option<usize>) -> ExitCode {
    let program_path = Path::new(path);
    if !program_path.is_file() {
        eprintln!("error: cannot read `{path}`");
        return ExitCode::from(1);
    }
    let config = Config::discover(program_path);
    let jobs = jobs.unwrap_or(config.install_jobs);

    let outcome = kora_pkg::install(program_path, jobs, true);
    for url in &outcome.fetched {
        println!("  fetched  {url}");
    }
    for (url, why) in &outcome.failed {
        eprintln!("error: cannot fetch {url}");
        for line in why.lines().take(4) {
            eprintln!("   {}", line.trim());
        }
        eprintln!();
    }
    if outcome.lock_changed {
        println!("  updated  {}", kora_pkg::Lock::FILE);
    }

    let used = outcome.resolution.needed().len();
    if outcome.failed.is_empty() {
        println!("{used} package{} in use", if used == 1 { "" } else { "s" });
    }

    let problems = report_package_problems(&outcome.resolution);
    if outcome.failed.is_empty() && !problems {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `kora tree` — the packages a program actually uses, and why.
///
/// The graph is derived from the source, so this is the list that would be
/// fetched and shipped, not the list `kora.toml` declares. A package marked
/// `(dev)` is reached only through `test` blocks and stays out of a shipped
/// program. Nobody declares that: it is derived, and the line says which rule
/// produced it, because a classification nobody can explain is the part of
/// this that frustrates people elsewhere.
fn package_tree(path: &str) -> ExitCode {
    let program_path = Path::new(path);
    if !program_path.is_file() {
        eprintln!("error: cannot read `{path}`");
        return ExitCode::from(1);
    }
    let resolution = kora_pkg::resolve(program_path);

    println!("{path}");
    let needed = resolution.needed();
    if needed.is_empty() {
        println!("  (no packages used)");
    }
    for package in &needed {
        let name = package.name.as_deref().unwrap_or("?");
        let version = package
            .manifest
            .version
            .as_deref()
            .map(|v| format!(" {v}"))
            .unwrap_or_default();
        let dev = if resolution.dev_only.contains(&package.id) {
            "  (dev — reached only through test blocks)"
        } else {
            ""
        };
        println!("  {name}{version}{dev}");
        println!("      grants: {}", package.grants.describe());
    }

    for unused in &resolution.unused {
        let who = resolution.packages[unused.declared_by.0]
            .name
            .as_deref()
            .unwrap_or("this program");
        println!("  {} — declared by {who}, never imported", unused.name);
    }

    for (file, why) in &resolution.unreadable {
        eprintln!("warning: skipped {}: {why}", file.display());
    }

    let mut failed = false;
    for missing in &resolution.missing {
        eprintln!(
            "error: no package named `{}` ({}:{})",
            missing.name,
            missing.file.display(),
            missing.span.line
        );
        failed = true;
    }
    failed |= report_package_problems(&resolution);

    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Report anything wrong with the authority in the graph.
///
/// A shortfall means a package asked for something nobody gave it, which is
/// better said before the run than at whichever call first needs it. A
/// conflict means one package was granted two different ways: taking the
/// union would let a permissive importer widen what a careful one withheld,
/// and taking the intersection would break the permissive one's working code.
fn report_package_problems(resolution: &kora_pkg::Resolution) -> bool {
    let mut failed = false;

    for unfetched in &resolution.unfetched {
        eprintln!(
            "error: package `{}` ({} at {}) is not fetched",
            unfetched.name, unfetched.url, unfetched.reference
        );
        eprintln!("   = hint: run `kora install <file.ko>`");
        eprintln!();
        failed = true;
    }

    for (url, expected, actual) in &resolution.tampered {
        // The lockfile is what the bytes were when they were fetched and
        // verified. Different bytes under the same name is the shape of a
        // moved tag, a rewritten repository, or an edited cache.
        eprintln!("error: {url} does not match the lockfile");
        eprintln!("   expected {expected}");
        eprintln!("   found    {actual}");
        eprintln!("   = hint: re-fetch with `kora install`, or restore the checkout");
        eprintln!();
        failed = true;
    }

    for (path, why) in &resolution.unverifiable {
        // Silently treating this as an empty manifest would read as "this
        // package has no dependencies and asked for nothing".
        eprintln!("error: cannot read {}: {why}", path.display());
        failed = true;
    }

    for shortfall in &resolution.shortfalls {
        eprintln!(
            "error: package `{}` requires {}, but was granted {}",
            shortfall.package,
            shortfall.missing.join(", "),
            shortfall.granted
        );
        eprintln!(
            "   = hint: grant it in kora.toml under `[dependencies.{}]`",
            shortfall.package
        );
        eprintln!();
        failed = true;
    }

    for conflict in &resolution.ref_conflicts {
        eprintln!(
            "error: {} is required at two revisions: {} and {}",
            conflict.url, conflict.first, conflict.second
        );
        eprintln!("   = hint: a repository has one entry in the lockfile; pin one revision");
        eprintln!();
        failed = true;
    }

    for conflict in &resolution.grant_conflicts {
        eprintln!(
            "error: package `{}` is granted two different ways: {} and {}",
            conflict.package, conflict.first, conflict.second
        );
        eprintln!("   = hint: grant it the same way everywhere that depends on it");
        eprintln!();
        failed = true;
    }

    failed
}

fn check_files(paths: &[String], syntax_only: bool) -> ExitCode {
    let mut problems = 0;
    let mut checked = 0;
    // Manifest directory -> (package name, dependencies not yet seen used).
    let mut dependency_use: std::collections::BTreeMap<
        std::path::PathBuf,
        (Option<String>, std::collections::BTreeSet<String>),
    > = std::collections::BTreeMap::new();

    for path in paths {
        let Ok(source) = std::fs::read_to_string(path) else {
            eprintln!("error: cannot read `{path}`");
            problems += 1;
            continue;
        };
        checked += 1;

        match kora_syntax::parse(&source) {
            Err(e) => {
                eprint!("{}", e.render(&source, path));
                problems += 1;
            }
            Ok(program) if syntax_only => {
                let _ = program;
            }
            Ok(program) => {
                // Checking follows `use "./lib.ko"`, so a name that only
                // exists in an imported file resolves here too.
                for d in kora_types::analyze_file(&program, Path::new(path)).diagnostics {
                    let line = d.span.line as usize;
                    let src_line = source.lines().nth(line.saturating_sub(1)).unwrap_or("");
                    eprintln!("error: {}", d.message);
                    eprintln!("  --> {path}:{line}:{}", d.span.col);
                    eprintln!("   |");
                    eprintln!(" {line} | {src_line}");
                    if let Some(hint) = &d.hint {
                        eprintln!("   = hint: {hint}");
                    }
                    eprintln!();
                    problems += 1;
                }
                // Whether a dependency is used is a question about the whole
                // program, not about one file: checking fifteen files of a
                // project must not accuse it of never importing what a
                // sixteenth imports. Findings accumulate per manifest and are
                // reported once, after every file is in.
                let resolution = kora_pkg::resolve(Path::new(path));
                for package in &resolution.packages {
                    let entry = dependency_use
                        .entry(package.root.clone())
                        .or_insert_with(|| {
                            (
                                package.name.clone(),
                                package.manifest.deps.keys().cloned().collect(),
                            )
                        });
                    for declared in package.manifest.deps.keys() {
                        let still_unused = resolution
                            .unused
                            .iter()
                            .any(|u| u.declared_by == package.id && &u.name == declared);
                        if !still_unused {
                            entry.1.remove(declared);
                        }
                    }
                }
                if report_package_problems(&resolution) {
                    problems += 1;
                }
                for missing in &resolution.missing {
                    eprintln!("error: no package named `{}`", missing.name);
                    eprintln!(
                        "  --> {}:{}:{}",
                        missing.file.display(),
                        missing.span.line,
                        missing.span.col
                    );
                    eprintln!("   = hint: declare it under `[dependencies]` in kora.toml");
                    eprintln!();
                    problems += 1;
                }
            }
        }
    }

    // A dependency the source never names is not fetched and not shipped.
    // Reporting it keeps kora.toml honest, as a warning rather than an error
    // because a dependency added a minute ago has not been imported yet.
    for (name, unused) in dependency_use.values() {
        let who = name.as_deref().unwrap_or("this program");
        for dep in unused {
            eprintln!("warning: `{dep}` is declared by {who} but never imported");
            eprintln!("   = hint: remove it from kora.toml, or write `use pkg {dep}`");
            eprintln!();
        }
    }

    if problems == 0 {
        println!(
            "checked {checked} file{}: no problems",
            if checked == 1 { "" } else { "s" }
        );
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "{problems} problem{} in {checked} file{}",
            if problems == 1 { "" } else { "s" },
            if checked == 1 { "" } else { "s" }
        );
        ExitCode::from(1)
    }
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
    let resolution = kora_pkg::resolve(program_path);
    if report_package_problems(&resolution) {
        return ExitCode::from(1);
    }
    let packages = std::sync::Arc::new(resolution);

    // Collect the tests by running the file's top level once.
    let mut collector = Interpreter::new();
    collector.collecting_tests = true;
    collector.program_name = path.to_string();
    collector.packages = packages.clone();
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
        interp.packages = packages.clone();
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
    // Every file the program imports is part of the program, so the audit
    // covers them too: an inventory that stopped at the entry file would not
    // be the complete list it claims to be.
    let packages = kora_pkg::resolve(Path::new(path));
    // And an audit over a graph with holes in it is worse than no audit.
    // "No declassification sites" is a claim about code that was read; a
    // dependency that was never fetched has not been read, so the command
    // has to refuse rather than report a clean bill for it.
    if report_package_problems(&packages) {
        eprintln!("the audit was not run: it would not be the complete list it promises");
        return ExitCode::from(1);
    }
    let sites = kora_runtime::audit::audit_program(&program, path, &packages);
    print!("{}", kora_runtime::audit::render(&sites));
    ExitCode::SUCCESS
}

/// Where a run's spans are written.
///
/// Honours `[telemetry] path` when the project configures one, so `kora trace`
/// and the run itself never disagree about where the file is.
fn trace_path(program: &Path) -> String {
    if let kora_runtime::telemetry::Exporter::File(path) =
        &Config::discover(program).telemetry.exporter
    {
        return path.clone();
    }
    program
        .parent()
        .unwrap_or(Path::new("."))
        .join(".kora")
        .join("last.trace.json")
        .to_string_lossy()
        .to_string()
}

/// `kora trace` — show the spans from the most recent traced run.
fn show_trace(path: &str) -> ExitCode {
    let file = trace_path(Path::new(path));
    let Ok(text) = std::fs::read_to_string(&file) else {
        println!("no trace yet — run it with `kora run --trace {path}`");
        return ExitCode::SUCCESS;
    };
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&text) else {
        eprintln!("error: {file} is not readable as a trace");
        return ExitCode::from(1);
    };
    let spans = &payload["resourceSpans"][0]["scopeSpans"][0]["spans"];
    let Some(spans) = spans.as_array() else {
        println!("the trace has no spans");
        return ExitCode::SUCCESS;
    };

    let mut rows: Vec<(u128, String)> = Vec::new();
    for span in spans {
        let start = span["startTimeUnixNano"]
            .as_str()
            .and_then(|s| s.parse::<u128>().ok())
            .unwrap_or(0);
        let end = span["endTimeUnixNano"]
            .as_str()
            .and_then(|s| s.parse::<u128>().ok())
            .unwrap_or(0);
        let name = span["name"].as_str().unwrap_or("?");
        let nested = span.get("parentSpanId").is_some();
        let failed = span["status"]["code"] == serde_json::json!(2);
        rows.push((
            start,
            format!(
                "{}{name:<32}{:>7}ms{}",
                if nested { "  " } else { "" },
                end.saturating_sub(start) / 1_000_000,
                if failed { "  FAILED" } else { "" }
            ),
        ));
    }
    rows.sort_by_key(|(start, _)| *start);
    for (_, line) in rows {
        println!("{line}");
    }
    println!("\n{} spans in {file}", spans.len());
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

    run_file(path, Mode::Live, false, true, Some(id.to_string()), false)
}

#[allow(clippy::too_many_arguments)]
fn run_file(
    path: &str,
    mode: Mode,
    report: bool,
    durable: bool,
    resume_id: Option<String>,
    trace: bool,
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
    // Verified before anything runs: an unfetched or tampered dependency
    // must stop the program, not surface at whichever import reaches it
    // first — by then the top level of other packages has already run.
    let packages = kora_pkg::resolve(program_path);
    if report_package_problems(&packages) {
        return ExitCode::from(1);
    }
    interp.packages = std::sync::Arc::new(packages);
    interp.config = Config::discover(program_path);
    interp.sinks = interp.config.sinks.clone();
    interp.allow_private_hosts = interp.config.http_allow_private;
    interp.http_timeout_secs = interp.config.http_timeout_secs;

    // `--trace` turns tracing on for a run that has no telemetry configured,
    // writing beside the program so there is nothing to set up first.
    let mut telemetry = interp.config.telemetry.clone();
    if trace && telemetry.exporter == kora_runtime::telemetry::Exporter::None {
        telemetry.exporter = kora_runtime::telemetry::Exporter::File(trace_path(program_path));
    }
    interp.tracer = std::sync::Arc::new(kora_runtime::Tracer::new(telemetry));
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

    if let Err(e) = interp.tracer.flush() {
        eprintln!("warning: {e}");
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
