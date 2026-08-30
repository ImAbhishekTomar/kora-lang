//! Tree-walking interpreter (execution Stage 1 per DECISIONS.md).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use kora_models::{
    AnalyzeOutcome, AnalyzeRequest, FieldType, Schema, SchemaField, Step, ToolExchange, ToolSpec,
};
use kora_syntax::ast::*;
use kora_syntax::token::Span;

use std::sync::{Arc, Mutex};

use crate::budget::Budget;
use crate::cassette::{self, Cassette, Mode, RecordedOutcome};
use crate::config::Config;
use crate::debug::{self, Debugger, Frame, Resume};
use crate::journal::{self, Effect, Journal, Lookup, PendingQuestion};
use crate::label::{DeclassifySite, Label, SinkPolicy};
use crate::modules::{self, ModuleId, ModuleSpace};
use crate::portable::Portable;
use crate::stdlib::require_verified;
use crate::telemetry::Tracer;
use crate::value::Value;

/// Why execution stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopKind {
    /// A real error: the program is wrong or the world misbehaved.
    Error,
    /// The program asked a person a question and is waiting. Not a failure —
    /// the run is parked and resumes when the answer arrives.
    Suspended,
    /// A debugger client asked for the run to stop. Not a failure either: the
    /// program did nothing wrong, somebody pressed stop.
    Terminated,
}

/// A runtime error, source-anchored like syntax errors.
///
/// Suspension rides the same channel: the language has no exception handling,
/// so nothing catches it on the way out, and the top level can tell the two
/// apart by `kind`.
#[derive(Debug, Clone)]
pub struct RuntimeError {
    pub message: String,
    pub hint: Option<String>,
    pub span: Span,
    pub kind: StopKind,
    /// The file the span belongs to, when it is not the entry file. Set as an
    /// error leaves an imported module, so `render` quotes the right line.
    pub file: Option<String>,
}

impl RuntimeError {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        RuntimeError {
            message: message.into(),
            hint: None,
            span,
            kind: StopKind::Error,
            file: None,
        }
    }

    /// The signal raised by `ask_human` when no answer is recorded yet.
    fn suspended(span: Span) -> Self {
        RuntimeError {
            message: "waiting for a human answer".into(),
            hint: None,
            span,
            kind: StopKind::Suspended,
            file: None,
        }
    }

    pub fn is_suspension(&self) -> bool {
        self.kind == StopKind::Suspended
    }

    /// Raised when a debugger client stops the run.
    fn terminated(span: Span) -> Self {
        RuntimeError {
            message: "stopped by the debugger".into(),
            hint: None,
            span,
            kind: StopKind::Terminated,
            file: None,
        }
    }

    pub fn is_terminated(&self) -> bool {
        self.kind == StopKind::Terminated
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Record which file the span is in, unless an inner frame already did.
    ///
    /// The innermost module wins: that is where the failing line actually is.
    fn in_file(mut self, file: &str) -> Self {
        if self.file.is_none() {
            self.file = Some(file.to_string());
        }
        self
    }

    pub fn render(&self, source: &str, filename: &str) -> String {
        // An error raised inside an imported module belongs to that file, so
        // read it rather than quoting the entry file's line by the same
        // number, which would point at unrelated code.
        let (owned, filename) = match &self.file {
            Some(file) if file != filename => (std::fs::read_to_string(file).ok(), file.as_str()),
            _ => (None, filename),
        };
        let source = owned.as_deref().unwrap_or(source);
        let line_no = self.span.line as usize;
        let col = self.span.col as usize;
        let src_line = source.lines().nth(line_no.saturating_sub(1)).unwrap_or("");
        let gutter = format!("{line_no}");
        let pad = " ".repeat(gutter.len());
        let caret_pad = " ".repeat(col.saturating_sub(1));
        let mut out = format!(
            "error: {msg}\n {pad}--> {filename}:{line_no}:{col}\n {pad} |\n {gutter} | {src_line}\n {pad} | {caret_pad}^\n",
            msg = self.message,
        );
        if let Some(hint) = &self.hint {
            out.push_str(&format!(" {pad} = hint: {hint}\n"));
        }
        out
    }
}

/// Non-error control flow escaping a block.
enum Flow {
    Normal,
    Break,
    Continue,
    Return(Value),
}

/// One lexical scope frame.
type Scope = HashMap<String, Value>;

pub struct Interpreter {
    /// Top-level names of the module currently executing. Swapped out with
    /// `modules[current_module].names` whenever execution crosses a file
    /// boundary, so each file sees only its own top level.
    globals: Scope,
    /// Every file loaded this run. Index 0 is the entry file; imports append.
    modules: Vec<ModuleSpace>,
    /// Which module `globals` currently belongs to.
    current_module: ModuleId,
    /// Modules whose top level is mid-execution, innermost last. Used to
    /// catch an import cycle before it recurses forever.
    loading: Vec<ModuleId>,
    /// User `type` declarations: name -> field list.
    types: HashMap<String, Vec<FieldDef>>,
    /// Where `print` writes (swappable for tests).
    pub output: Vec<String>,
    /// Text written by `write` that no newline has finished yet.
    ///
    /// A streamed answer arrives in pieces that are not lines, so `write`
    /// has to be able to leave one open. Kept separate from `output` rather
    /// than appended to its last entry, because a partial line is not a line
    /// and anything reading `output` would otherwise see it as one.
    pending_line: String,
    /// Print directly to stdout (true for `kora run`), or capture (tests).
    pub direct_stdout: bool,
    /// Model configuration from kora.toml.
    pub config: Config,
    /// Record/replay of model calls. Shared, because `parallel for` workers
    /// all read from and record into the same cassette.
    pub cassette: Option<Arc<Mutex<Cassette>>>,
    /// Program path, used for cassette keys and error sites.
    pub program_name: String,
    /// Total tokens consumed this run (budgets enforce these in Phase 3).
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Model calls actually sent to a provider (cassette hits excluded).
    pub model_calls: u64,
    /// Budget in force at the current point of execution.
    pub budget: Budget,
    /// How many worker threads `parallel for` may use at once.
    pub max_workers: usize,
    /// Which sinks may receive which labels, from `[sinks]` in kora.toml.
    pub sinks: SinkPolicy,
    /// Sinks currently unlocked by an enclosing `declassify` block.
    declassified_for: Vec<String>,
    /// Every declassification reached during this run, for `kora audit`.
    pub declassify_sites: Vec<DeclassifySite>,
    /// Durable execution journal. Disabled unless the run is durable.
    pub journal: Arc<Mutex<Journal>>,
    /// This agent's position in the execution tree, for journal ordering.
    pub scope: journal::Scope,
    /// Journal slot claimed for an in-flight model call.
    pending_slot: Option<(journal::Scope, usize)>,
    /// Whether `http` may reach loopback and private address ranges.
    pub allow_private_hosts: bool,
    /// Timeout applied to every outbound request.
    pub http_timeout_secs: u64,
    /// Value that `analyze` should return while a `with mock` block is active.
    mocked_analyze: Vec<Value>,
    /// Collected `test` blocks, when running under `kora test`.
    pub collecting_tests: bool,
    pub tests: Vec<(String, Vec<Stmt>)>,
    /// Emits OpenTelemetry spans. Shared, so parallel branches land in one
    /// trace.
    pub tracer: Arc<Tracer>,
    /// The span a new child should attach to.
    parent_span: Option<String>,
    /// The resolved package graph, from `[dependencies]` plus the `use pkg`
    /// statements the program actually writes. Shared and read-only, so a
    /// `parallel for` worker resolves imports exactly as its parent does.
    pub packages: Arc<kora_pkg::Resolution>,
    /// Connected MCP servers, by configured name. Shared so parallel branches
    /// reuse one connection rather than spawning a process per agent.
    pub mcp: Arc<Mutex<HashMap<String, kora_mcp::Server>>>,
    /// The Python sidecar, started on first use and shared thereafter. One
    /// worker per run rather than one per call, since starting an
    /// interpreter is the expensive part.
    pub python: Arc<Mutex<Option<kora_python::Worker>>>,
    /// Attached debugger, if any. `None` costs one branch per statement.
    ///
    /// Taken out of the interpreter while it is being called, so the debugger
    /// may be handed the frames it needs to describe.
    debugger: Option<Box<dyn Debugger>>,
    /// Frames, breakpoints, and step state. Untouched unless a debugger is
    /// attached.
    debug: debug::Session,
    /// Non-zero while a `case` guard is being evaluated.
    ///
    /// Guards are speculative -- an arm may be tried and rejected -- so a
    /// model call inside one would spend budget on a branch that never ran.
    /// The checker rejects the obvious spelling; this catches the rest,
    /// including a guard that reaches `analyze` through a helper.
    in_guard: u32,
}

/// Tags a program may construct directly: the outcomes of a model call and
/// the stdlib's result shape.
const OUTCOME_TAGS: &[&str] = &["Ok", "Err", "Uncertain", "Exhausted", "Failed"];

const BUILTINS: &[&str] = &[
    "print",
    "write",
    "len",
    "range",
    "str",
    "int",
    "float",
    "bool",
    "abs",
    "min",
    "max",
    "sum",
    "sorted",
    "append",
    "keys",
    "values",
    "tokens_spent",
    "tokens_remaining",
    "calls_spent",
    "redact",
    "ask_human",
];

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl Interpreter {
    pub fn new() -> Self {
        Interpreter {
            globals: HashMap::new(),
            modules: vec![ModuleSpace::new(
                "<input>".to_string(),
                PathBuf::from("<input>"),
                PathBuf::from("."),
                kora_pkg::ROOT,
            )],
            current_module: modules::ROOT,
            loading: Vec::new(),
            types: HashMap::new(),
            output: Vec::new(),
            pending_line: String::new(),
            direct_stdout: false,
            config: Config::default(),
            packages: Arc::new(kora_pkg::Resolution::default()),
            cassette: None,
            program_name: "<input>".to_string(),
            tokens_in: 0,
            tokens_out: 0,
            model_calls: 0,
            budget: Budget::unlimited(),
            max_workers: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            sinks: SinkPolicy::default(),
            declassified_for: Vec::new(),
            declassify_sites: Vec::new(),
            journal: Arc::new(Mutex::new(Journal::disabled())),
            scope: journal::Scope::root(),
            pending_slot: None,
            allow_private_hosts: false,
            http_timeout_secs: 30,
            mocked_analyze: Vec::new(),
            collecting_tests: false,
            tests: Vec::new(),
            tracer: Arc::new(Tracer::disabled()),
            parent_span: Option::None,
            mcp: Arc::new(Mutex::new(HashMap::new())),
            python: Arc::new(Mutex::new(Option::None)),
            debugger: Option::None,
            debug: debug::Session::default(),
            in_guard: 0,
        }
    }

    /// Execute a program's top level without calling `main()`.
    ///
    /// Used by the test runner: definitions and `use` statements must be in
    /// scope, but the program's entry point must not run.
    pub fn run_top_level(&mut self, program: &Program) -> Result<(), RuntimeError> {
        self.sync_root();
        // The entry file counts as loading while its top level runs, so a file
        // that imports it back is reported as the cycle it is rather than
        // silently seeing a half-built namespace.
        self.loading.push(modules::ROOT);
        self.debug_enter(&top_level_name(&self.program_name), 1);
        let mut scope = HashMap::new();
        let mut outcome = Ok(());
        for stmt in &program.items {
            match self.exec(stmt, &mut scope) {
                Ok(Flow::Return(_)) => break,
                Ok(_) => {}
                Err(e) => {
                    outcome = Err(e);
                    break;
                }
            }
        }
        self.debug_leave();
        self.loading.pop();
        self.globals.extend(scope);
        outcome
    }

    /// Run a sequence of statements in a fresh scope, for one test body.
    pub fn run_block(&mut self, body: &[Stmt]) -> Result<(), RuntimeError> {
        let mut scope = HashMap::new();
        self.exec_block(body, &mut scope).map(|_| ())
    }

    /// Run a whole program: execute top-level statements, then call `main()`
    /// if it was defined.
    pub fn run(&mut self, program: &Program) -> Result<(), RuntimeError> {
        self.run_top_level(program)?;

        if let Some(Value::Func { def: main_fn, .. }) = self.globals.get("main").cloned() {
            if !main_fn.params.is_empty() {
                return Err(RuntimeError::new(
                    "main() must take no parameters",
                    Span::new(0, 0, 1, 1),
                ));
            }
            self.call_function(&main_fn, modules::ROOT, vec![], Span::new(0, 0, 1, 1))?;
        }
        self.flush_pending();
        Ok(())
    }

    /// Close any line `write` left open, so a program that ends mid-line
    /// does not lose its last piece of output.
    pub fn flush_pending(&mut self) {
        if !self.pending_line.is_empty() {
            let line = std::mem::take(&mut self.pending_line);
            self.output.push(line);
        }
    }

    // --- statements ---

    fn exec(&mut self, stmt: &Stmt, scope: &mut Scope) -> Result<Flow, RuntimeError> {
        if self.debugger.is_some() {
            self.debug_before(stmt, scope)?;
        }
        match &stmt.kind {
            StmtKind::Expr(e) => {
                self.eval(e, scope)?;
                Ok(Flow::Normal)
            }
            StmtKind::Assign {
                target,
                ty,
                value,
                classified,
                on_token,
            } => {
                // `x: T = analyze(...)` is the one place the declared type is
                // available, and analyze needs it to build the model schema.
                let v = match (ty, analyze_args(value)) {
                    (Some(t), Some((args, kwargs))) => {
                        self.eval_analyze(t, args, kwargs, on_token.as_ref(), value.span, scope)?
                    }
                    // The handler is only meaningful on a call that streams,
                    // and only `analyze` streams. Caught here rather than
                    // ignored, because a block that silently never runs is
                    // worse than one that never parsed.
                    (_, None) if on_token.is_some() => {
                        return Err(RuntimeError::new(
                            "`on token` can only watch an `analyze()` call",
                            value.span,
                        )
                        .with_hint("write `answer: str = analyze(data, \"...\") on token(t):`"))
                    }
                    _ => {
                        let v = self.eval(value, scope)?;
                        if let Some(t) = ty {
                            self.check_annotation(&v, t, value.span)?;
                        }
                        v
                    }
                };
                let v = if *classified {
                    v.with_label(Label::CLASSIFIED)
                } else {
                    v
                };
                self.assign(target, v, scope)?;
                Ok(Flow::Normal)
            }
            StmtKind::AugAssign { target, op, value } => {
                let current = self.eval(target, scope)?;
                let rhs = self.eval(value, scope)?;
                let result = self.binop(*op, current, rhs, stmt.span)?;
                self.assign(target, result, scope)?;
                Ok(Flow::Normal)
            }
            StmtKind::If {
                branches,
                else_body,
            } => {
                for (cond, body) in branches {
                    if self.eval(cond, scope)?.truthy() {
                        return self.exec_block(body, scope);
                    }
                }
                if let Some(body) = else_body {
                    return self.exec_block(body, scope);
                }
                Ok(Flow::Normal)
            }
            StmtKind::While { cond, body } => {
                while self.eval(cond, scope)?.truthy() {
                    match self.exec_block(body, scope)? {
                        Flow::Break => break,
                        Flow::Continue | Flow::Normal => {}
                        ret @ Flow::Return(_) => return Ok(ret),
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::For { var, iter, body } => {
                let iterable = self.eval(iter, scope)?;
                let items = self.iterate(iterable, iter.span)?;
                for item in items {
                    scope.insert(var.clone(), item);
                    match self.exec_block(body, scope)? {
                        Flow::Break => break,
                        Flow::Continue | Flow::Normal => {}
                        ret @ Flow::Return(_) => return Ok(ret),
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::FuncDef(f) => {
                let func = Value::Func {
                    def: Rc::new(f.clone()),
                    home: self.current_module,
                };
                scope.insert(f.name.clone(), func.clone());
                // Register globally right away so recursion and forward calls
                // from other functions resolve.
                self.globals.insert(f.name.clone(), func);
                Ok(Flow::Normal)
            }
            StmtKind::TypeDef { name, fields } => {
                for field in fields {
                    self.validate_field_metadata(field)?;
                }
                // Types share one namespace across every module, so a value
                // built in one file is the same type everywhere. Two files
                // declaring the same name differently is a mistake, not a
                // pair of unrelated types, so say so.
                let qualified = self.qualify_type(name);
                if let Some(existing) = self.types.get(&qualified) {
                    if existing != fields {
                        return Err(RuntimeError::new(
                            format!("type `{name}` is declared twice with different fields"),
                            stmt.span,
                        )
                        .with_hint(
                            "types are shared across the files of one package; give one of them another name",
                        ));
                    }
                }
                self.types.insert(qualified.clone(), fields.clone());
                // Also bind it in this module's namespace, so an importer can
                // reach it as `alias.Name`.
                let type_ref = Value::TypeRef {
                    name: Rc::new(qualified),
                };
                scope.insert(name.clone(), type_ref.clone());
                self.globals.insert(name.clone(), type_ref);
                Ok(Flow::Normal)
            }
            StmtKind::Return(value) => {
                let v = match value {
                    Some(e) => self.eval(e, scope)?,
                    None => Value::None,
                };
                Ok(Flow::Return(v))
            }
            StmtKind::Test { name, body } => {
                // Under `kora run` a test block is inert; `kora test`
                // collects and runs them.
                if self.collecting_tests {
                    self.tests.push((name.clone(), body.clone()));
                }
                Ok(Flow::Normal)
            }
            StmtKind::Assert { condition, message } => {
                let holds = self.eval(condition, scope)?.truthy();
                if holds {
                    return Ok(Flow::Normal);
                }
                let detail = match message {
                    Some(expr) => self.eval(expr, scope)?.to_string(),
                    Option::None => "assertion failed".to_string(),
                };
                Err(RuntimeError::new(detail, stmt.span))
            }
            StmtKind::WithMock {
                target,
                result,
                body,
            } => {
                if target != "analyze" {
                    return Err(RuntimeError::new(
                        format!("`{target}` cannot be mocked"),
                        stmt.span,
                    )
                    .with_hint("today only `analyze` can be mocked"));
                }
                let value = self.eval(result, scope)?;
                self.mocked_analyze.push(value);
                let outcome = self.exec_block(body, scope);
                self.mocked_analyze.pop();
                outcome
            }
            StmtKind::UseFile { path, alias } => {
                let id = self.load_module(path, stmt.span)?;
                let value = Value::UserModule {
                    id,
                    alias: Rc::new(alias.clone()),
                };
                scope.insert(alias.clone(), value.clone());
                self.globals.insert(alias.clone(), value);
                Ok(Flow::Normal)
            }
            StmtKind::UsePython { module, alias } => {
                let value = Value::PyModule {
                    module: Rc::new(module.clone()),
                };
                scope.insert(alias.clone(), value.clone());
                self.globals.insert(alias.clone(), value);
                Ok(Flow::Normal)
            }
            StmtKind::UsePkg { package, alias } => {
                let id = self.load_package(package, stmt.span)?;
                let value = Value::UserModule {
                    id,
                    alias: Rc::new(alias.clone()),
                };
                scope.insert(alias.clone(), value.clone());
                self.globals.insert(alias.clone(), value);
                Ok(Flow::Normal)
            }
            StmtKind::UseMcp { server, alias } => {
                self.connect_mcp(server, stmt.span)?;
                let value = Value::McpServer {
                    alias: Rc::new(server.clone()),
                };
                scope.insert(alias.clone(), value.clone());
                self.globals.insert(alias.clone(), value);
                Ok(Flow::Normal)
            }
            StmtKind::Use { module, alias } => {
                if crate::stdlib::module(module).is_none() {
                    let mut e = RuntimeError::new(
                        format!("there is no module named `{module}`"),
                        stmt.span,
                    );
                    if let Some(close) = crate::stdlib::MODULE_NAMES
                        .iter()
                        .find(|m| close_enough(m, module))
                    {
                        e = e.with_hint(format!("did you mean `{close}`?"));
                    } else {
                        e = e.with_hint(format!(
                            "available modules: {}",
                            crate::stdlib::MODULE_NAMES.join(", ")
                        ));
                    }
                    return Err(e);
                }
                let value = Value::Module {
                    name: Rc::new(module.clone()),
                };
                scope.insert(alias.clone(), value.clone());
                self.globals.insert(alias.clone(), value);
                Ok(Flow::Normal)
            }
            StmtKind::Declassify {
                value,
                binding,
                sink,
                body,
            } => {
                let released = self.eval(value, scope)?;
                let label = released.label();

                // Releasing classified data is authority of its own. Off by
                // default, because adding a dependency must not become the
                // way to launder a secret out of a program.
                if !self.grants().allows_declassify() {
                    let package = self.current_package_name();
                    return Err(RuntimeError::new(
                        format!("package `{package}` is not allowed to declassify"),
                        stmt.span,
                    )
                    .with_hint(format!(
                        "if that is intended, grant it in kora.toml: `[dependencies.{package}]` with `grants = {{ declassify = true }}`"
                    )));
                }
                if !self.grants().allows_sink(sink) {
                    let package = self.current_package_name();
                    return Err(RuntimeError::new(
                        format!("package `{package}` is not allowed to release data to `{sink}`"),
                        stmt.span,
                    )
                    .with_hint(format!(
                        "grant it in kora.toml: `[dependencies.{package}]` with `grants = {{ sinks = [\"{sink}\"] }}`"
                    )));
                }

                if !self.sinks.is_known_sink(sink) {
                    let known = self.sinks.known_sinks();
                    let mut err =
                        RuntimeError::new(format!("`{sink}` is not a declared sink"), stmt.span);
                    err = if known.is_empty() {
                        err.with_hint(
                            "declare sinks in kora.toml, e.g. `[sinks] local_model = { allow = [\"classified\"] }`",
                        )
                    } else {
                        err.with_hint(format!("declared sinks: {}", known.join(", ")))
                    };
                    return Err(err);
                }

                if !self.sinks.permits(sink, label.clone()) {
                    let accepting = self.sinks.sinks_accepting_classified();
                    let hint = if accepting.is_empty() {
                        "no sink currently accepts classified data — check `[sinks]` in kora.toml"
                            .to_string()
                    } else {
                        format!(
                            "sinks allowed for classified data: {}",
                            accepting.join(", ")
                        )
                    };
                    return Err(RuntimeError::new(
                        format!(
                            "policy forbids {} data reaching sink `{sink}`",
                            label.name()
                        ),
                        stmt.span,
                    )
                    .with_hint(hint));
                }

                // Record the release before running the block: the audit trail
                // should show intent even if the body later fails.
                self.declassify_sites.push(DeclassifySite {
                    file: self.program_name.clone(),
                    line: stmt.span.line,
                    expression: binding.clone(),
                    sink: sink.clone(),
                });
                if self.tracer.is_enabled() {
                    // `kora audit` is the static inventory; this is the live
                    // record of what actually happened.
                    let mut span = self.tracer.start("declassify", self.parent_span.clone());
                    self.tracer
                        .set_plain(&mut span, "kora.sink", serde_json::json!(sink));
                    self.tracer
                        .set_plain(&mut span, "kora.value", serde_json::json!(binding));
                    self.tracer.set_plain(
                        &mut span,
                        "kora.site",
                        serde_json::json!(format!("{}:{}", self.program_name, stmt.span.line)),
                    );
                    self.tracer.end(span, Option::None);
                }

                // The value keeps its label and records the sink it was
                // released to. Stripping the label instead would release it
                // to *everything* inside the block, so a secret declassified
                // for a model could be written to a file three lines later.
                let shadowed = scope.get(binding).cloned();
                let approved = released
                    .unlabeled()
                    .clone()
                    .with_label(released.label().released_to(sink));
                scope.insert(binding.clone(), approved);
                self.declassified_for.push(sink.clone());

                let result = self.exec_block(body, scope);

                self.declassified_for.pop();
                match shadowed {
                    Some(previous) => scope.insert(binding.clone(), previous),
                    Option::None => scope.remove(binding),
                };
                result
            }
            StmtKind::WithBudget { budget, body } => {
                let outer = self.budget.clone();
                self.budget = outer.nested(budget);
                let result = self.exec_block(body, scope);
                self.budget = outer;
                result
            }
            StmtKind::ParallelFor {
                var,
                iter,
                body,
                collect_into,
            } => {
                let iterable = self.eval(iter, scope)?;
                let items = self.iterate(iterable, iter.span)?;
                let results = self.run_parallel(var, items, body, scope, stmt.span)?;
                if let Some(name) = collect_into {
                    scope.insert(name.clone(), results);
                }
                Ok(Flow::Normal)
            }
            StmtKind::Break => Ok(Flow::Break),
            StmtKind::Continue => Ok(Flow::Continue),
            StmtKind::Pass => Ok(Flow::Normal),
            StmtKind::Match { subject, arms } => {
                let value = self.eval(subject, scope)?;
                let mut guard_rejected = false;
                for arm in arms {
                    let Some(bindings) = match_pattern(&arm.pattern, &value) else {
                        continue;
                    };
                    // Bound before the guard runs, so the guard can read them.
                    // They are deliberately left bound when the guard fails:
                    // Kora scopes by function like Python, and a binder that
                    // vanished on a rejected arm would be the one place in the
                    // language where a block had its own scope.
                    for (name, v) in bindings {
                        scope.insert(name, v);
                    }
                    if let Some(guard) = &arm.guard {
                        self.in_guard += 1;
                        let verdict = self.eval(guard, scope);
                        self.in_guard -= 1;
                        if !verdict?.truthy() {
                            guard_rejected = true;
                            continue;
                        }
                    }
                    return self.exec_block(&arm.body, scope);
                }
                // Distinguishing the two is worth a branch: "nothing matched"
                // and "everything that matched was refused by its guard" have
                // different fixes, and guessing wrong sends the reader to the
                // wrong half of the `match`.
                if guard_rejected {
                    Err(RuntimeError::new(
                        format!(
                            "every `case` arm matching {} was rejected by its guard",
                            value.type_name()
                        ),
                        stmt.span,
                    )
                    .with_hint("add an unguarded arm for the same pattern, or `case _:`"))
                } else {
                    // `Failed` is newer than the three-arm `match` most
                    // programs were written against, so it is the one
                    // unmatched value with a known fix. Naming the arm beats
                    // the generic advice, which here would be actively bad:
                    // `case _:` turns a provider outage into whatever the
                    // catch-all happens to say.
                    let hint = if matches!(
                        value.unlabeled(),
                        Value::Variant { tag, .. } if tag.as_str() == "Failed"
                    ) {
                        "add `case Failed(why):` — the provider did not answer, \
                         which is not the model declining"
                    } else {
                        "add a catch-all arm: `case _:`"
                    };
                    Err(RuntimeError::new(
                        format!("no `case` arm matched {}", value.type_name()),
                        stmt.span,
                    )
                    .with_hint(hint))
                }
            }
            StmtKind::BindOrElse {
                name,
                ty,
                value,
                classified,
                reason,
                else_body,
            } => {
                // Same special case as an annotated assignment: `analyze` is
                // handed the declared type so it can build the model schema.
                let outcome = match (ty, analyze_args(value)) {
                    (Some(t), Some((args, kwargs))) => {
                        self.eval_analyze(t, args, kwargs, None, value.span, scope)?
                    }
                    _ => self.eval(value, scope)?,
                };
                match unwrap_outcome(&outcome, stmt.span)? {
                    Ok(payload) => {
                        if let Some(t) = ty {
                            self.check_annotation(&payload, t, value.span)?;
                        }
                        let payload = if *classified {
                            payload.with_label(Label::CLASSIFIED)
                        } else {
                            payload
                        };
                        scope.insert(name.clone(), payload);
                        Ok(Flow::Normal)
                    }
                    Err(why) => {
                        if let Some(reason) = reason {
                            scope.insert(reason.clone(), why);
                        }
                        match self.exec_block(else_body, scope)? {
                            // The checker proves the block diverges, so this
                            // is unreachable in a program that passed `check`.
                            // A program reaching it anyway must not continue
                            // with `name` unbound.
                            Flow::Normal => Err(RuntimeError::new(
                                format!("the `else` block of `{name}` fell through"),
                                stmt.span,
                            )
                            .with_hint("end it with `return`, `break`, or `continue`")),
                            other => Ok(other),
                        }
                    }
                }
            }
        }
    }

    fn exec_block(&mut self, body: &[Stmt], scope: &mut Scope) -> Result<Flow, RuntimeError> {
        for stmt in body {
            match self.exec(stmt, scope)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn assign(
        &mut self,
        target: &Expr,
        value: Value,
        scope: &mut Scope,
    ) -> Result<(), RuntimeError> {
        match &target.kind {
            ExprKind::Name(name) => {
                scope.insert(name.clone(), value);
                Ok(())
            }
            ExprKind::Attr { object, name } => {
                let obj = self.eval(object, scope)?;
                match obj.unlabeled() {
                    Value::Object { type_name, fields } => {
                        if let Some(field) = self
                            .types
                            .get(type_name.as_str())
                            .and_then(|declared| declared.iter().find(|field| field.name == *name))
                        {
                            self.validate_field_value(field, &value, target.span)?;
                        }
                        fields.borrow_mut().insert(name.clone(), value);
                        Ok(())
                    }
                    other => Err(RuntimeError::new(
                        format!("cannot set attribute on {}", other.type_name()),
                        target.span,
                    )),
                }
            }
            ExprKind::Index { object, index } => {
                let obj = self.eval(object, scope)?;
                let idx = self.eval(index, scope)?;
                match (obj, idx) {
                    (Value::List(items), Value::Int(i)) => {
                        let mut items = items.borrow_mut();
                        let len = items.len();
                        let real = normalize_index(i, len).ok_or_else(|| {
                            RuntimeError::new(
                                format!("list index {i} out of range (length {len})"),
                                target.span,
                            )
                        })?;
                        items[real] = value;
                        Ok(())
                    }
                    (Value::Dict(map), Value::Str(key)) => {
                        map.borrow_mut().insert(key.to_string(), value);
                        Ok(())
                    }
                    (obj, idx) => Err(RuntimeError::new(
                        format!("cannot index {} with {}", obj.type_name(), idx.type_name()),
                        target.span,
                    )),
                }
            }
            _ => Err(RuntimeError::new("invalid assignment target", target.span)),
        }
    }

    /// Phase 1 annotation check: verify the value's runtime type matches the
    /// annotation. The static checker (kora-types) will subsume this later.
    fn check_annotation(&self, v: &Value, ty: &TypeExpr, span: Span) -> Result<(), RuntimeError> {
        // A label does not change what type a value is.
        let v = v.unlabeled();
        let expected = match ty {
            TypeExpr::Name(n) => n.clone(),
            TypeExpr::Generic(n, _) => n.clone(),
        };
        let actual = v.type_name();
        let ok = match expected.as_str() {
            "int" => matches!(v, Value::Int(_)),
            "float" => matches!(v, Value::Float(_) | Value::Int(_)),
            "str" => matches!(v, Value::Str(_)),
            "bool" => matches!(v, Value::Bool(_)),
            "list" => matches!(v, Value::List(_)),
            "dict" => matches!(v, Value::Dict(_)),
            name => match v {
                Value::Object { type_name, .. } => type_name.as_str() == self.qualify_type(name),
                _ => {
                    // Unknown annotation on a non-object: only fail when we
                    // know the type exists (declared via `type`).
                    self.lookup_type(name).is_none()
                }
            },
        };
        if ok {
            Ok(())
        } else {
            // Two packages may each declare `Config`, so a bare mismatch
            // could read `expected Config, got Config`. When the short names
            // collide, say which package each one came from.
            let mut error = RuntimeError::new(
                format!("expected `{}`, got `{}`", ty.display(), actual),
                span,
            );
            if let Value::Object { type_name, .. } = v.unlabeled() {
                let want = self.qualify_type(&ty.display());
                if want != type_name.as_str()
                    && crate::value::short_type_name(type_name) == ty.display()
                {
                    error = error.with_hint(format!(
                        "{} is not {}; they are different types with the same name",
                        self.type_origin(type_name),
                        self.type_origin(&want)
                    ));
                }
            }
            Err(error)
        }
    }

    /// Name a type the way a reader can act on: its own name, plus the
    /// package it came from when that is not the program itself.
    fn type_origin(&self, qualified: &str) -> String {
        let short = crate::value::short_type_name(qualified);
        let Some((prefix, _)) = qualified.rsplit_once(crate::value::TYPE_QUALIFIER) else {
            return format!("`{short}` in this program");
        };
        let package = prefix
            .strip_prefix('#')
            .and_then(|id| id.parse::<usize>().ok())
            .and_then(|id| self.packages.packages.get(id))
            .and_then(|p| p.name.clone())
            .unwrap_or_else(|| "an imported package".to_string());
        format!("`{short}` from package `{package}`")
    }

    fn validate_field_metadata(&self, field: &FieldDef) -> Result<(), RuntimeError> {
        let Some(pattern) = &field.metadata.pattern else {
            return Ok(());
        };
        if field.ty.display() != "str" {
            return Err(RuntimeError::new(
                format!("field `{}` uses `pattern` but is not a `str`", field.name),
                field.span,
            )
            .with_hint("`pattern` is only valid on `str` fields"));
        }
        regex::Regex::new(pattern).map_err(|error| {
            RuntimeError::new(
                format!(
                    "field `{}` has an invalid pattern `{pattern}`: {error}",
                    field.name
                ),
                field.span,
            )
        })?;
        Ok(())
    }

    fn validate_field_value(
        &self,
        field: &FieldDef,
        value: &Value,
        span: Span,
    ) -> Result<(), RuntimeError> {
        let Some(pattern) = &field.metadata.pattern else {
            return Ok(());
        };
        let Value::Str(text) = value.unlabeled() else {
            return Ok(());
        };
        let regex = regex::Regex::new(pattern).expect("validated when the type is declared");
        if regex.is_match(text) {
            Ok(())
        } else {
            Err(RuntimeError::new(
                format!("field `{}` must match pattern `{pattern}`", field.name),
                span,
            ))
        }
    }

    // --- expressions ---

    fn eval(&mut self, expr: &Expr, scope: &mut Scope) -> Result<Value, RuntimeError> {
        match &expr.kind {
            ExprKind::Int(v) => Ok(Value::Int(*v)),
            ExprKind::Float(v) => Ok(Value::Float(*v)),
            ExprKind::Str(s) => Ok(Value::Str(Rc::new(s.clone()))),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::None => Ok(Value::None),
            ExprKind::Name(name) => self.lookup(name, scope, expr.span),
            ExprKind::FString { parts, exprs } => {
                let mut out = String::new();
                let mut label = Label::PUBLIC;
                for (i, part) in parts.iter().enumerate() {
                    out.push_str(part);
                    if i < exprs.len() {
                        let v = self.eval(&exprs[i], scope)?;
                        label = label.join(v.label());
                        out.push_str(&v.to_string());
                    }
                }
                Ok(Value::Str(Rc::new(out)).with_label(label))
            }
            ExprKind::List(items) => {
                let mut vals = Vec::with_capacity(items.len());
                for item in items {
                    vals.push(self.eval(item, scope)?);
                }
                Ok(Value::List(Rc::new(RefCell::new(vals))))
            }
            ExprKind::Dict(pairs) => {
                let mut map = HashMap::new();
                for (k, v) in pairs {
                    let key = match self.eval(k, scope)? {
                        Value::Str(s) => s.to_string(),
                        other => {
                            return Err(RuntimeError::new(
                                format!("dict keys must be strings, got {}", other.type_name()),
                                k.span,
                            ));
                        }
                    };
                    map.insert(key, self.eval(v, scope)?);
                }
                Ok(Value::Dict(Rc::new(RefCell::new(map))))
            }
            ExprKind::Unary { op, operand } => {
                let v = self.eval(operand, scope)?;
                let label = v.label();
                let v = v.unlabeled().clone();
                let result = match op {
                    UnaryOp::Not => Ok(Value::Bool(!v.truthy())),
                    UnaryOp::Neg => match v {
                        Value::Int(i) => Ok(Value::Int(-i)),
                        Value::Float(f) => Ok(Value::Float(-f)),
                        other => Err(RuntimeError::new(
                            format!("cannot negate {}", other.type_name()),
                            expr.span,
                        )),
                    },
                };
                result.map(|v| v.with_label(label))
            }
            ExprKind::Binary { op, left, right } => {
                // Short-circuit logic first.
                match op {
                    BinOp::And => {
                        let l = self.eval(left, scope)?;
                        if !l.truthy() {
                            return Ok(l);
                        }
                        return self.eval(right, scope);
                    }
                    BinOp::Or => {
                        let l = self.eval(left, scope)?;
                        if l.truthy() {
                            return Ok(l);
                        }
                        return self.eval(right, scope);
                    }
                    _ => {}
                }
                let l = self.eval(left, scope)?;
                let r = self.eval(right, scope)?;
                // Transitivity: a result computed from classified data is
                // classified. Without this, `f"{ssn}"` would launder it.
                let label = l.label().join(r.label());
                self.binop(*op, l, r, expr.span)
                    .map(|v| v.with_label(label))
            }
            ExprKind::Attr { object, name } => {
                let obj = self.eval(object, scope)?;
                // `gh.tools` and `gh.search_issues` resolve against the
                // connected server rather than a field.
                if let Value::McpServer { alias } = obj.unlabeled() {
                    let alias = alias.to_string();
                    return self.mcp_member(&alias, name, expr.span);
                }
                // `lib.helper` reads that file's top level.
                if let Value::UserModule { id, alias } = obj.unlabeled() {
                    let (id, alias) = (*id, alias.to_string());
                    return match self.module_member(id, name) {
                        Some(v) => Ok(v),
                        None => {
                            let exports = self.module_exports(id);
                            Err(RuntimeError::new(
                                format!("`{alias}` has no name `{name}`"),
                                expr.span,
                            )
                            .with_hint(if exports.is_empty() {
                                format!("{alias} defines nothing at its top level")
                            } else {
                                format!("{alias} provides: {}", exports.join(", "))
                            }))
                        }
                    };
                }
                let outer_label = obj.label();
                // A `classified` field marks values read from it, even when
                // the containing object is public.
                let field_label = match obj.unlabeled() {
                    Value::Object { type_name, .. } => self
                        .types
                        .get(type_name.as_str())
                        .and_then(|fields| fields.iter().find(|f| &f.name == name))
                        .filter(|f| f.classified)
                        .map(|_| Label::CLASSIFIED)
                        .unwrap_or_default(),
                    _ => Label::PUBLIC,
                };
                let label = outer_label.join(field_label);
                let obj = obj.unlabeled().clone();
                let result = match &obj {
                    Value::Object { fields, type_name } => {
                        fields.borrow().get(name).cloned().ok_or_else(|| {
                            let available: Vec<String> = fields.borrow().keys().cloned().collect();
                            RuntimeError::new(
                                format!("`{type_name}` has no field `{name}`"),
                                expr.span,
                            )
                            .with_hint(format!("available fields: {}", available.join(", ")))
                        })
                    }
                    // Method-style builtins on lists/dicts resolve at call time;
                    // represent as a bound marker the Call arm understands.
                    Value::List(_) | Value::Dict(_) | Value::Str(_) => Err(RuntimeError::new(
                        format!("`{}` has no attribute `{name}`", obj.type_name()),
                        expr.span,
                    )
                    .with_hint("method calls like xs.append(v) are written append(xs, v) for now")),
                    other => Err(RuntimeError::new(
                        format!("`{}` has no attribute `{name}`", other.type_name()),
                        expr.span,
                    )),
                };
                result.map(|v| v.with_label(label))
            }
            ExprKind::Index { object, index } => {
                let obj = self.eval(object, scope)?;
                let idx = self.eval(index, scope)?;
                let label = obj.label().join(idx.label());
                let obj = obj.unlabeled().clone();
                let idx = idx.unlabeled().clone();
                let result = match (&obj, &idx) {
                    (Value::List(items), Value::Int(i)) => {
                        let items = items.borrow();
                        let real = normalize_index(*i, items.len()).ok_or_else(|| {
                            RuntimeError::new(
                                format!("list index {i} out of range (length {})", items.len()),
                                expr.span,
                            )
                        })?;
                        Ok(items[real].clone())
                    }
                    (Value::Dict(map), Value::Str(key)) => {
                        map.borrow().get(key.as_str()).cloned().ok_or_else(|| {
                            RuntimeError::new(format!("key \"{key}\" not found"), expr.span)
                        })
                    }
                    (Value::Str(s), Value::Int(i)) => {
                        let chars: Vec<char> = s.chars().collect();
                        let real = normalize_index(*i, chars.len()).ok_or_else(|| {
                            RuntimeError::new(format!("string index {i} out of range"), expr.span)
                        })?;
                        Ok(Value::Str(Rc::new(chars[real].to_string())))
                    }
                    _ => Err(RuntimeError::new(
                        format!("cannot index {} with {}", obj.type_name(), idx.type_name()),
                        expr.span,
                    )),
                };
                result.map(|v| v.with_label(label))
            }
            ExprKind::Slice {
                object,
                start,
                stop,
            } => {
                let obj = self.eval(object, scope)?;
                let label = obj.label();
                let obj = obj.unlabeled().clone();
                let start_v = match start {
                    Some(e) => Some(self.expect_int(e, scope)?),
                    None => None,
                };
                let stop_v = match stop {
                    Some(e) => Some(self.expect_int(e, scope)?),
                    None => None,
                };
                let result = match obj {
                    Value::List(items) => {
                        let items = items.borrow();
                        let (a, b) = slice_bounds(start_v, stop_v, items.len());
                        Ok(Value::List(Rc::new(RefCell::new(items[a..b].to_vec()))))
                    }
                    Value::Str(s) => {
                        let chars: Vec<char> = s.chars().collect();
                        let (a, b) = slice_bounds(start_v, stop_v, chars.len());
                        Ok(Value::Str(Rc::new(chars[a..b].iter().collect())))
                    }
                    other => Err(RuntimeError::new(
                        format!("cannot slice {}", other.type_name()),
                        expr.span,
                    )),
                };
                result.map(|v| v.with_label(label))
            }
            ExprKind::Call {
                callee,
                args,
                kwargs,
            } => {
                if let Some((name, arg)) = kwargs.first() {
                    return Err(RuntimeError::new(
                        format!("`{name}` is not a keyword argument here"),
                        arg.span,
                    )
                    .with_hint("only analyze() takes keyword arguments today"));
                }
                let mut arg_vals = Vec::with_capacity(args.len());
                for a in args {
                    arg_vals.push(self.eval(a, scope)?);
                }
                // Type constructor? `Expense(...)` — not yet; Phase 1 uses
                // named-field dict later. Function or builtin:
                match &callee.kind {
                    ExprKind::Name(name) => {
                        if name == "analyze" {
                            // Untyped call site: we have no schema to constrain
                            // the model with, so refuse rather than guess.
                            return Err(RuntimeError::new(
                                "analyze() needs a declared result type",
                                expr.span,
                            )
                            .with_hint(
                                "annotate the assignment, e.g. `result: Insight = analyze(data, \"...\")`",
                            ));
                        }
                        if let Some((qualified, fields)) = self.lookup_type(name) {
                            return self.construct(&qualified, &fields, arg_vals, expr.span);
                        }
                        // Outcome constructors, so a test can build the value
                        // a model call would have produced. Restricted to the
                        // known tags, so a typo is an error rather than a
                        // silently-created variant nothing will ever match.
                        if OUTCOME_TAGS.contains(&name.as_str()) {
                            return Ok(Value::Variant {
                                tag: Rc::new(name.clone()),
                                payload: arg_vals,
                            });
                        }
                        match self.lookup(name, scope, callee.span)? {
                            Value::Func { def, home } => {
                                self.call_function(&def, home, arg_vals, expr.span)
                            }
                            Value::Builtin(b) => self.call_builtin(b, arg_vals, expr.span),
                            // `tax.Money(...)`: a type reached through the
                            // module that declared it.
                            Value::TypeRef { name } => match self.types.get(name.as_str()).cloned()
                            {
                                Some(fields) => self.construct(&name, &fields, arg_vals, expr.span),
                                Option::None => Err(RuntimeError::new(
                                    format!("there is no type named `{name}`"),
                                    expr.span,
                                )),
                            },
                            other => Err(RuntimeError::new(
                                format!("{} is not callable", other.type_name()),
                                expr.span,
                            )),
                        }
                    }
                    // `json.parse(...)` and friends: a module member call.
                    ExprKind::Attr { object, name } => {
                        // `stats.mean(xs)`: a call into the Python sidecar.
                        if let ExprKind::Name(alias) = &object.kind {
                            if let Ok(Value::PyModule { module }) =
                                self.lookup(alias, scope, object.span)
                            {
                                let module = module.to_string();
                                return self.call_python(&module, name, arg_vals, expr.span);
                            }
                        }
                        if let ExprKind::Name(module_alias) = &object.kind {
                            if let Ok(Value::Module { name: module_name }) =
                                self.lookup(module_alias, scope, object.span)
                            {
                                return self.call_module_fn(
                                    &module_name,
                                    name,
                                    arg_vals,
                                    expr.span,
                                );
                            }
                        }
                        let target = self.eval(callee, scope)?;
                        match target {
                            Value::Func { def, home } => {
                                self.call_function(&def, home, arg_vals, expr.span)
                            }
                            Value::Builtin(b) => self.call_builtin(b, arg_vals, expr.span),
                            // `tax.Money(...)`: a type reached through the
                            // module that declared it.
                            Value::TypeRef { name } => match self.types.get(name.as_str()).cloned()
                            {
                                Some(fields) => self.construct(&name, &fields, arg_vals, expr.span),
                                Option::None => Err(RuntimeError::new(
                                    format!("there is no type named `{name}`"),
                                    expr.span,
                                )),
                            },
                            other => Err(RuntimeError::new(
                                format!("{} is not callable", other.type_name()),
                                expr.span,
                            )),
                        }
                    }
                    _ => {
                        let target = self.eval(callee, scope)?;
                        match target {
                            Value::Func { def, home } => {
                                self.call_function(&def, home, arg_vals, expr.span)
                            }
                            Value::Builtin(b) => self.call_builtin(b, arg_vals, expr.span),
                            // `tax.Money(...)`: a type reached through the
                            // module that declared it.
                            Value::TypeRef { name } => match self.types.get(name.as_str()).cloned()
                            {
                                Some(fields) => self.construct(&name, &fields, arg_vals, expr.span),
                                Option::None => Err(RuntimeError::new(
                                    format!("there is no type named `{name}`"),
                                    expr.span,
                                )),
                            },
                            other => Err(RuntimeError::new(
                                format!("{} is not callable", other.type_name()),
                                expr.span,
                            )),
                        }
                    }
                }
            }
        }
    }

    fn expect_int(&mut self, e: &Expr, scope: &mut Scope) -> Result<i64, RuntimeError> {
        match self.eval(e, scope)? {
            Value::Int(i) => Ok(i),
            other => Err(RuntimeError::new(
                format!("expected an integer, got {}", other.type_name()),
                e.span,
            )),
        }
    }

    fn lookup(&self, name: &str, scope: &Scope, span: Span) -> Result<Value, RuntimeError> {
        if let Some(v) = scope.get(name) {
            return Ok(v.clone());
        }
        if let Some(v) = self.globals.get(name) {
            return Ok(v.clone());
        }
        // A declared type used as a value: `csv.parse(text, Expense)`. The
        // reference carries the qualified name, so it still means this
        // package's type after it crosses a package boundary.
        if let Some((qualified, _)) = self.lookup_type(name) {
            return Ok(Value::TypeRef {
                name: Rc::new(qualified),
            });
        }
        if BUILTINS.contains(&name) {
            return Ok(Value::Builtin(
                BUILTINS.iter().find(|b| **b == name).unwrap(),
            ));
        }
        let mut err = RuntimeError::new(format!("name `{name}` is not defined"), span);
        // Suggest close matches (simple case-insensitive + edit-distance-1).
        let candidates: Vec<&String> = scope
            .keys()
            .chain(self.globals.keys())
            .filter(|k| close_enough(k, name))
            .collect();
        if let Some(c) = candidates.first() {
            err = err.with_hint(format!("did you mean `{c}`?"));
        }
        Err(err)
    }

    fn construct(
        &mut self,
        type_name: &str,
        fields: &[FieldDef],
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, RuntimeError> {
        if args.len() != fields.len() {
            return Err(RuntimeError::new(
                format!(
                    "`{type_name}` takes {} field(s), got {} argument(s)",
                    fields.len(),
                    args.len()
                ),
                span,
            )
            .with_hint(format!(
                "fields in order: {}",
                fields
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let mut map = HashMap::new();
        for (fd, v) in fields.iter().zip(args) {
            self.validate_field_value(fd, &v, span)?;
            map.insert(fd.name.clone(), v);
        }
        Ok(Value::Object {
            type_name: Rc::new(type_name.to_string()),
            fields: Rc::new(RefCell::new(map)),
        })
    }

    fn call_function(
        &mut self,
        f: &FuncDef,
        home: ModuleId,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, RuntimeError> {
        if args.len() != f.params.len() {
            return Err(RuntimeError::new(
                format!(
                    "{}() takes {} argument(s), got {}",
                    f.name,
                    f.params.len(),
                    args.len()
                ),
                span,
            ));
        }
        // Agents are the unit of execution, so they are the unit of tracing.
        // An agent nested inside another must restore the outer span when it
        // finishes, or the trace loses its shape.
        let agent_span = if self.tracer.is_enabled() && f.kind == FuncKind::Agent {
            let outer_parent = self.parent_span.clone();
            let mut span = self.tracer.start(&f.name, outer_parent.clone());
            self.tracer
                .set_plain(&mut span, "kora.kind", serde_json::json!("agent"));
            self.parent_span = Some(Tracer::span_id_of(&span));
            Some((span, outer_parent))
        } else {
            Option::None
        };

        // An agent's declared budget wraps its whole body.
        let outer_budget = self.budget.clone();
        if let Some(spec) = &f.budget {
            self.budget = outer_budget.nested(spec);
        }

        let mut local: Scope = HashMap::new();
        for (p, v) in f.params.iter().zip(args) {
            if let Some(ty) = &p.ty {
                self.check_annotation(&v, ty, p.span).map_err(|e| {
                    RuntimeError::new(
                        format!("argument `{}` of {}(): {}", p.name, f.name, e.message),
                        span,
                    )
                })?;
            }
            local.insert(p.name.clone(), v);
        }
        // A function body reads its own file's top level, so entering one
        // defined elsewhere switches namespaces for the duration of the call.
        let outer_module = self.enter_module(home);
        self.debug_enter(&f.name, span.line);
        let flow = self
            .exec_block(&f.body, &mut local)
            .map_err(|e| self.blame_current_file(e));
        self.debug_leave();
        self.leave_module(outer_module);
        self.budget = outer_budget;

        if let Some((mut span, outer_parent)) = agent_span {
            self.tracer.set_plain(
                &mut span,
                "kora.tokens",
                serde_json::json!(self.budget.spent_tokens()),
            );
            self.parent_span = outer_parent;
            let error = match &flow {
                Err(e) if !e.is_suspension() => Some(e.message.clone()),
                _ => Option::None,
            };
            self.tracer.end(span, error);
        }

        match flow? {
            Flow::Return(v) => Ok(v),
            _ => Ok(Value::None),
        }
    }

    // --- operators ---

    fn binop(&self, op: BinOp, l: Value, r: Value, span: Span) -> Result<Value, RuntimeError> {
        use BinOp::*;
        use Value::*;
        // Operators work on the underlying value; the caller re-applies the
        // joined label to the result.
        let l = l.unlabeled().clone();
        let r = r.unlabeled().clone();
        match op {
            Add => match (l, r) {
                (Int(a), Int(b)) => Ok(Int(a + b)),
                (Float(a), Float(b)) => Ok(Float(a + b)),
                (Int(a), Float(b)) => Ok(Float(a as f64 + b)),
                (Float(a), Int(b)) => Ok(Float(a + b as f64)),
                (Str(a), Str(b)) => Ok(Str(Rc::new(format!("{a}{b}")))),
                (List(a), List(b)) => {
                    let mut out = a.borrow().clone();
                    out.extend(b.borrow().iter().cloned());
                    Ok(List(Rc::new(RefCell::new(out))))
                }
                (Str(a), other) => Err(RuntimeError::new(
                    format!("cannot add str and {}", other.type_name()),
                    span,
                )
                .with_hint(format!(
                    "convert first: \"{a}\" + str(value), or use an f-string"
                ))),
                (l, r) => Err(RuntimeError::new(
                    format!("cannot add {} and {}", l.type_name(), r.type_name()),
                    span,
                )),
            },
            Sub | Mul | Div | FloorDiv | Mod | Pow => self.arith(op, l, r, span),
            Eq => Ok(Bool(l.same(&r))),
            NotEq => Ok(Bool(!l.same(&r))),
            Lt | Gt | LtEq | GtEq => self.compare(op, l, r, span),
            In | NotIn => {
                let found = match &r {
                    List(items) => items.borrow().iter().any(|v| v.same(&l)),
                    Dict(map) => match &l {
                        Str(s) => map.borrow().contains_key(s.as_str()),
                        _ => false,
                    },
                    Str(hay) => match &l {
                        Str(needle) => hay.contains(needle.as_str()),
                        _ => {
                            return Err(RuntimeError::new(
                                "`in` on a string needs a string on the left",
                                span,
                            ));
                        }
                    },
                    other => {
                        return Err(RuntimeError::new(
                            format!("`in` needs a list, dict, or str, got {}", other.type_name()),
                            span,
                        ));
                    }
                };
                Ok(Bool(if op == In { found } else { !found }))
            }
            And | Or => unreachable!("short-circuited earlier"),
        }
    }

    fn arith(&self, op: BinOp, l: Value, r: Value, span: Span) -> Result<Value, RuntimeError> {
        use Value::*;
        let l = l.unlabeled().clone();
        let r = r.unlabeled().clone();
        // Special case: str * int repetition.
        if let (BinOp::Mul, Str(s), Int(n)) = (op, &l, &r) {
            return Ok(Str(Rc::new(s.repeat((*n).max(0) as usize))));
        }
        let as_pair = match (&l, &r) {
            (Int(a), Int(b)) => Some((*a as f64, *b as f64, true)),
            (Float(a), Float(b)) => Some((*a, *b, false)),
            (Int(a), Float(b)) => Some((*a as f64, *b, false)),
            (Float(a), Int(b)) => Some((*a, *b as f64, false)),
            _ => Option::None,
        };
        let (a, b, both_int) = as_pair.ok_or_else(|| {
            RuntimeError::new(
                format!(
                    "cannot apply `{}` to {} and {}",
                    op.symbol(),
                    l.type_name(),
                    r.type_name()
                ),
                span,
            )
        })?;
        if matches!(op, BinOp::Div | BinOp::FloorDiv | BinOp::Mod) && b == 0.0 {
            return Err(RuntimeError::new("division by zero", span));
        }
        Ok(match op {
            BinOp::Sub => {
                if both_int {
                    Int((a - b) as i64)
                } else {
                    Float(a - b)
                }
            }
            BinOp::Mul => {
                if both_int {
                    Int((a * b) as i64)
                } else {
                    Float(a * b)
                }
            }
            BinOp::Div => Float(a / b),
            BinOp::FloorDiv => {
                if both_int {
                    Int((a / b).floor() as i64)
                } else {
                    Float((a / b).floor())
                }
            }
            BinOp::Mod => {
                if both_int {
                    Int((a.rem_euclid(b)) as i64)
                } else {
                    Float(a.rem_euclid(b))
                }
            }
            BinOp::Pow => {
                if both_int && b >= 0.0 {
                    Int(a.powf(b) as i64)
                } else {
                    Float(a.powf(b))
                }
            }
            _ => unreachable!(),
        })
    }

    fn compare(&self, op: BinOp, l: Value, r: Value, span: Span) -> Result<Value, RuntimeError> {
        use Value::*;
        let l = l.unlabeled().clone();
        let r = r.unlabeled().clone();
        let ord = match (&l, &r) {
            (Int(a), Int(b)) => (*a as f64).partial_cmp(&(*b as f64)),
            (Float(a), Float(b)) => a.partial_cmp(b),
            (Int(a), Float(b)) => (*a as f64).partial_cmp(b),
            (Float(a), Int(b)) => a.partial_cmp(&(*b as f64)),
            (Str(a), Str(b)) => Some(a.cmp(b)),
            _ => Option::None,
        };
        let ord = ord.ok_or_else(|| {
            RuntimeError::new(
                format!(
                    "cannot compare {} and {} with `{}`",
                    l.type_name(),
                    r.type_name(),
                    op.symbol()
                ),
                span,
            )
        })?;
        use std::cmp::Ordering::*;
        Ok(Bool(match op {
            BinOp::Lt => ord == Less,
            BinOp::Gt => ord == Greater,
            BinOp::LtEq => ord != Greater,
            BinOp::GtEq => ord != Less,
            _ => unreachable!(),
        }))
    }

    // --- iteration & builtins ---

    fn iterate(&self, v: Value, span: Span) -> Result<Vec<Value>, RuntimeError> {
        let label = v.label();
        let v = v.unlabeled().clone();
        let items = self.iterate_inner(v, span)?;
        Ok(items
            .into_iter()
            .map(|i| i.with_label(label.clone()))
            .collect())
    }

    fn iterate_inner(&self, v: Value, span: Span) -> Result<Vec<Value>, RuntimeError> {
        match v {
            Value::List(items) => Ok(items.borrow().clone()),
            Value::Str(s) => Ok(s
                .chars()
                .map(|c| Value::Str(Rc::new(c.to_string())))
                .collect()),
            Value::Dict(map) => Ok(map
                .borrow()
                .keys()
                .map(|k| Value::Str(Rc::new(k.clone())))
                .collect()),
            other => Err(RuntimeError::new(
                format!("cannot loop over {}", other.type_name()),
                span,
            )
            .with_hint("loop over a list, string, dict, or range(...)")),
        }
    }

    fn call_builtin(
        &mut self,
        name: &str,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, RuntimeError> {
        // `redact` is the one builtin that must see labels: masking them is
        // its whole job. Everything else works on plain values and passes the
        // joined label through to its result.
        if name == "redact" {
            return match args.as_slice() {
                [value] => {
                    let types = self.types.clone();
                    Ok(redact_value(value, &types, false, &mut 0))
                }
                _ => Err(RuntimeError::new(
                    format!("redact() expects 1 argument, got {}", args.len()),
                    span,
                )),
            };
        }
        if name == "ask_human" {
            return self.ask_human(args, span);
        }
        let label = args
            .iter()
            .fold(Label::PUBLIC, |acc, v| acc.join(v.label()));
        let args: Vec<Value> = args.iter().map(|v| v.unlabeled().clone()).collect();
        self.call_builtin_inner(name, args, span)
            .map(|v| v.with_label(label))
    }

    fn call_builtin_inner(
        &mut self,
        name: &str,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, RuntimeError> {
        let argc = args.len();
        let wrong = |want: &str| {
            Err(RuntimeError::new(
                format!("{name}() expects {want}, got {argc} argument(s)"),
                span,
            ))
        };
        match name {
            "write" => {
                // Like `print` without the newline, for output that arrives
                // in pieces. The pieces of a streamed answer are not lines,
                // and a handler that could only `print` would break one
                // answer across as many lines as the model sent tokens.
                let text = args
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                if !self.record_output(&text, span)? {
                    return Ok(Value::None);
                }
                if self.direct_stdout {
                    use std::io::Write as _;
                    print!("{text}");
                    // Without this the answer appears all at once when the
                    // line ends, which is the thing streaming exists to
                    // avoid: stdout to a terminal is line-buffered.
                    let _ = std::io::stdout().flush();
                } else {
                    self.pending_line.push_str(&text);
                }
                Ok(Value::None)
            }
            "print" => {
                let line = args
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                // Anything `write` left open belongs to this line.
                let line = if self.pending_line.is_empty() {
                    line
                } else {
                    format!("{}{line}", std::mem::take(&mut self.pending_line))
                };
                // In a durable run, output is an effect: already-shown lines
                // replay silently, so resuming continues rather than repeats.
                if !self.record_output(&line, span)? {
                    return Ok(Value::None);
                }
                // A debugger wants the line as it happens, not at the end of
                // the run, or its console lags behind the highlighted stack.
                // It also takes ownership of the line: buffering it as well
                // would show every line twice.
                let shown = self.debug_output(&line);
                if self.direct_stdout {
                    println!("{line}");
                } else if !shown {
                    self.output.push(line);
                }
                Ok(Value::None)
            }
            "len" => match args.as_slice() {
                [Value::List(l)] => Ok(Value::Int(l.borrow().len() as i64)),
                [Value::Str(s)] => Ok(Value::Int(s.chars().count() as i64)),
                [Value::Dict(d)] => Ok(Value::Int(d.borrow().len() as i64)),
                [other] => Err(RuntimeError::new(
                    format!("len() does not work on {}", other.type_name()),
                    span,
                )),
                _ => wrong("1 argument"),
            },
            "range" => {
                let (start, stop) = match args.as_slice() {
                    [Value::Int(stop)] => (0, *stop),
                    [Value::Int(start), Value::Int(stop)] => (*start, *stop),
                    _ => return wrong("1 or 2 integers"),
                };
                Ok(Value::List(Rc::new(RefCell::new(
                    (start..stop).map(Value::Int).collect(),
                ))))
            }
            "str" => match args.as_slice() {
                [v] => Ok(Value::Str(Rc::new(v.to_string()))),
                _ => wrong("1 argument"),
            },
            "int" => {
                match args.as_slice() {
                    [Value::Int(v)] => Ok(Value::Int(*v)),
                    [Value::Float(v)] => Ok(Value::Int(*v as i64)),
                    [Value::Str(s)] => s.trim().parse::<i64>().map(Value::Int).map_err(|_| {
                        RuntimeError::new(format!("cannot convert \"{s}\" to int"), span)
                    }),
                    [Value::Bool(b)] => Ok(Value::Int(*b as i64)),
                    _ => wrong("1 argument"),
                }
            }
            "float" => match args.as_slice() {
                [Value::Int(v)] => Ok(Value::Float(*v as f64)),
                [Value::Float(v)] => Ok(Value::Float(*v)),
                [Value::Str(s)] => s.trim().parse::<f64>().map(Value::Float).map_err(|_| {
                    RuntimeError::new(format!("cannot convert \"{s}\" to float"), span)
                }),
                _ => wrong("1 argument"),
            },
            "bool" => match args.as_slice() {
                [v] => Ok(Value::Bool(v.truthy())),
                _ => wrong("1 argument"),
            },
            "abs" => match args.as_slice() {
                [Value::Int(v)] => Ok(Value::Int(v.abs())),
                [Value::Float(v)] => Ok(Value::Float(v.abs())),
                _ => wrong("1 number"),
            },
            "min" | "max" => {
                let items: Vec<Value> = match args.as_slice() {
                    [Value::List(l)] => l.borrow().clone(),
                    _ if argc >= 2 => args,
                    _ => return wrong("a list or 2+ arguments"),
                };
                if items.is_empty() {
                    return Err(RuntimeError::new(format!("{name}() of empty list"), span));
                }
                let mut best = items[0].clone();
                for item in &items[1..] {
                    let take = match self.compare(
                        if name == "min" { BinOp::Lt } else { BinOp::Gt },
                        item.clone(),
                        best.clone(),
                        span,
                    )? {
                        Value::Bool(b) => b,
                        _ => unreachable!(),
                    };
                    if take {
                        best = item.clone();
                    }
                }
                Ok(best)
            }
            "sum" => match args.as_slice() {
                [Value::List(l)] => {
                    let mut acc = Value::Int(0);
                    for item in l.borrow().iter() {
                        acc = self.binop(BinOp::Add, acc, item.clone(), span)?;
                    }
                    Ok(acc)
                }
                _ => wrong("a list"),
            },
            "sorted" => match args.as_slice() {
                [Value::List(l)] => {
                    let mut items = l.borrow().clone();
                    let mut err = None;
                    items.sort_by(|a, b| {
                        match self.compare(BinOp::Lt, a.clone(), b.clone(), span) {
                            Ok(Value::Bool(true)) => std::cmp::Ordering::Less,
                            Ok(_) => std::cmp::Ordering::Greater,
                            Err(e) => {
                                err.get_or_insert(e);
                                std::cmp::Ordering::Equal
                            }
                        }
                    });
                    if let Some(e) = err {
                        return Err(e);
                    }
                    Ok(Value::List(Rc::new(RefCell::new(items))))
                }
                _ => wrong("a list"),
            },
            "append" => match args.as_slice() {
                [Value::List(l), v] => {
                    l.borrow_mut().push(v.clone());
                    Ok(Value::None)
                }
                _ => wrong("a list and a value"),
            },
            // Budget introspection, so `if tokens_remaining() < 1000:` is
            // just an ordinary condition in user code.
            "tokens_spent" => Ok(Value::Int(self.budget.spent_tokens() as i64)),
            "calls_spent" => Ok(Value::Int(self.budget.spent_calls() as i64)),
            "tokens_remaining" => Ok(match self.budget.remaining_tokens() {
                Some(v) => Value::Int(v as i64),
                Option::None => Value::None,
            }),
            "keys" => match args.as_slice() {
                [Value::Dict(d)] => Ok(Value::List(Rc::new(RefCell::new(
                    d.borrow()
                        .keys()
                        .map(|k| Value::Str(Rc::new(k.clone())))
                        .collect(),
                )))),
                _ => wrong("a dict"),
            },
            "values" => match args.as_slice() {
                [Value::Dict(d)] => Ok(Value::List(Rc::new(RefCell::new(
                    d.borrow().values().cloned().collect(),
                )))),
                _ => wrong("a dict"),
            },
            other => Err(RuntimeError::new(
                format!("unknown builtin `{other}`"),
                span,
            )),
        }
    }
}

fn normalize_index(i: i64, len: usize) -> Option<usize> {
    let len = len as i64;
    let real = if i < 0 { len + i } else { i };
    if real >= 0 && real < len {
        Some(real as usize)
    } else {
        None
    }
}

fn slice_bounds(start: Option<i64>, stop: Option<i64>, len: usize) -> (usize, usize) {
    let len_i = len as i64;
    let clamp = |v: i64| -> usize {
        let v = if v < 0 { len_i + v } else { v };
        v.clamp(0, len_i) as usize
    };
    let a = start.map(clamp).unwrap_or(0);
    let b = stop.map(clamp).unwrap_or(len);
    (a, b.max(a))
}

fn close_enough(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    let (al, bl) = (a.len(), b.len());
    if al.abs_diff(bl) > 1 || al < 3 {
        return false;
    }
    // Cheap edit-distance <= 1 check.
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut i = 0;
    let mut j = 0;
    let mut edits = 0;
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
        } else {
            edits += 1;
            if edits > 1 {
                return false;
            }
            if a.len() > b.len() {
                i += 1;
            } else if b.len() > a.len() {
                j += 1;
            } else {
                i += 1;
                j += 1;
            }
        }
    }
    edits + (a.len() - i) + (b.len() - j) <= 1
}

/// Try to match a pattern against a value. Returns the bindings it introduces,
/// or None when the arm does not apply.
/// Split an outcome into its successful payload or the reason it was not.
///
/// `Ok(v)` / `Err(why)` / `Uncertain(why)` / `Exhausted(meter)` / `Failed(why)`
/// are the shapes
/// a model call and the stdlib produce. Anything else is refused rather than
/// guessed at: silently treating a bare value as success would make
/// `x = f() else:` quietly skip its own failure path.
///
/// A label on the outcome rides onto whichever side comes out, so unwrapping
/// classified data cannot launder it.
fn unwrap_outcome(value: &Value, span: Span) -> Result<Result<Value, Value>, RuntimeError> {
    let label = value.label();
    let relabel = |v: Value| v.with_label(label.clone());
    match value.unlabeled() {
        Value::Variant { tag, payload } => match tag.as_str() {
            "Ok" => Ok(Ok(relabel(payload.first().cloned().unwrap_or(Value::None)))),
            "Err" | "Uncertain" | "Exhausted" | "Failed" => Ok(Err(relabel(
                payload.first().cloned().unwrap_or(Value::None),
            ))),
            other => Err(RuntimeError::new(
                format!("`else` binding expects an outcome, found `{other}(...)`"),
                span,
            )
            .with_hint("outcomes are `Ok`, `Err`, `Uncertain`, `Exhausted`, and `Failed`")),
        },
        other => Err(RuntimeError::new(
            format!(
                "`else` binding expects an outcome, found {}",
                other.type_name()
            ),
            span,
        )
        .with_hint("use a plain `=` for a value that cannot fail")),
    }
}

/// Test `value` against `pattern`, returning what the pattern binds.
///
/// Structure is read through any label wrapper, and every binding is re-wrapped
/// with the label the subject carried. Without that, `match` on a classified
/// outcome silently skipped every `Ok(...)` arm -- the classified value took a
/// different branch than the same value unclassified. Looking through the
/// wrapper without re-applying the label would be worse: it would launder the
/// label off the payload.
fn match_pattern(pattern: &Pattern, value: &Value) -> Option<Vec<(String, Value)>> {
    let label = value.label();
    let bindings = match_structure(pattern, value.unlabeled())?;
    if label.is_plain() {
        return Some(bindings);
    }
    Some(
        bindings
            .into_iter()
            .map(|(name, v)| (name, v.with_label(label.clone())))
            .collect(),
    )
}

fn match_structure(pattern: &Pattern, value: &Value) -> Option<Vec<(String, Value)>> {
    match pattern {
        Pattern::Wildcard => Some(vec![]),
        Pattern::Bind(name) => Some(vec![(name.clone(), value.clone())]),
        Pattern::Ctor(tag, binders) => match value {
            Value::Variant {
                tag: value_tag,
                payload,
            } if value_tag.as_str() == tag => {
                if binders.len() != payload.len() {
                    return None;
                }
                Some(
                    binders
                        .iter()
                        .cloned()
                        .zip(payload.iter().cloned())
                        .collect(),
                )
            }
            // Allow `case TypeName(...)` to match an object of that type,
            // binding fields positionally is not supported; bare name only.
            Value::Object { type_name, .. } if type_name.as_str() == tag && binders.is_empty() => {
                Some(vec![])
            }
            _ => None,
        },
        Pattern::LiteralInt(want) => match value {
            Value::Int(got) if got == want => Some(vec![]),
            _ => None,
        },
        Pattern::LiteralStr(want) => match value {
            Value::Str(got) if got.as_str() == want => Some(vec![]),
            _ => None,
        },
        Pattern::LiteralBool(want) => match value {
            Value::Bool(got) if got == want => Some(vec![]),
            _ => None,
        },
    }
}

/// Positional and keyword arguments of an `analyze` call.
type AnalyzeArgs<'a> = (&'a [Expr], &'a [(String, Expr)]);

/// If `expr` is a call to `analyze`, return its positional and keyword args.
fn analyze_args(expr: &Expr) -> Option<AnalyzeArgs<'_>> {
    match &expr.kind {
        ExprKind::Call {
            callee,
            args,
            kwargs,
        } => match &callee.kind {
            ExprKind::Name(name) if name == "analyze" => Some((args.as_slice(), kwargs.as_slice())),
            _ => None,
        },
        _ => None,
    }
}

impl Interpreter {
    /// The `analyze(data, "prompt")` primitive.
    ///
    /// The declared type becomes a JSON schema the model must satisfy, so the
    /// result is ordinary typed data the rest of the program can branch on.
    fn eval_analyze(
        &mut self,
        ty: &TypeExpr,
        args: &[Expr],
        kwargs: &[(String, Expr)],
        on_token: Option<&TokenHandler>,
        span: Span,
        scope: &mut Scope,
    ) -> Result<Value, RuntimeError> {
        // A guard may be evaluated for an arm that is then rejected, so a
        // model call inside one would spend budget on a branch that never
        // ran. The checker catches `case P if analyze(...)`; this catches the
        // same call reached through a helper, which it cannot see.
        if self.in_guard > 0 {
            return Err(RuntimeError::new(
                "a `case` guard cannot call a model",
                span,
            )
            .with_hint(
                "guards are tried against arms that may not run; call `analyze` before the `match` and guard on its result",
            ));
        }
        if args.len() != 2 {
            return Err(RuntimeError::new(
                format!(
                    "analyze() takes 2 arguments (data, prompt), got {}",
                    args.len()
                ),
                span,
            )
            .with_hint("example: `result: Insight = analyze(rows, \"find anomalies\")`"));
        }
        for (name, arg) in kwargs {
            if name != "tools" && name != "model" {
                return Err(RuntimeError::new(
                    format!("analyze() has no keyword argument `{name}`"),
                    arg.span,
                )
                .with_hint("the keyword arguments are `tools=[...]` and `model=\"name\"`"));
            }
        }

        let data = self.eval(&args[0], scope)?;

        // The enforcement point. Classified data may only reach a model when
        // an enclosing `declassify ... for <sink>:` unlocked that sink and
        // policy permits the label there.
        let data_label = data.label();
        if data_label.is_classified() {
            let sink_name = self.model_sink_name();
            // The value must have been released to *this* sink, not merely
            // released to something.
            if !data_label.may_reach(&sink_name) {
                let accepting = self.sinks.sinks_accepting_classified();
                let hint = if accepting.is_empty() {
                    "wrap it in `declassify <value> for <sink>:` and allow that sink in kora.toml"
                        .to_string()
                } else {
                    format!(
                        "wrap it in `declassify <value> for {}:`",
                        accepting.join("` or `declassify <value> for ")
                    )
                };
                return Err(RuntimeError::new(
                    format!(
                        "classified data cannot reach model sink `{sink_name}` (no declassify in scope)"
                    ),
                    args[0].span,
                )
                .with_hint(hint));
            }
            if !self.sinks.permits(&sink_name, data_label.clone()) {
                return Err(RuntimeError::new(
                    format!("policy forbids classified data reaching sink `{sink_name}`"),
                    args[0].span,
                ));
            }
        }
        let prompt_value = self.eval(&args[1], scope)?;
        if prompt_value.label().is_classified() {
            return Err(
                RuntimeError::new("the prompt contains classified data", args[1].span).with_hint(
                    "declassify it first, or keep sensitive values in the data argument",
                ),
            );
        }
        let prompt = match prompt_value.unlabeled().clone() {
            Value::Str(s) => s.to_string(),
            other => {
                return Err(RuntimeError::new(
                    format!(
                        "analyze() prompt must be a string, got {}",
                        other.type_name()
                    ),
                    args[1].span,
                ))
            }
        };

        // Qualified once here, so the schema, the mock check, and the
        // resulting object all agree on which package's type this is.
        let type_name = self.qualify_type(match ty {
            TypeExpr::Name(n) => n,
            TypeExpr::Generic(n, _) => n,
        });
        let schema = self.schema_for(&type_name, span)?;

        // Streaming is only offered where it means something. For a declared
        // type the wire carries JSON, so the pieces of a "stream" are
        // fragments of syntax -- `{"merch` -- which no program wants to
        // print and no reader wants to see. Refused with the reason rather
        // than allowed to disappoint at runtime.
        if on_token.is_some() && !schema.text {
            return Err(RuntimeError::new(
                format!("`on token` needs a `str` result, but this call asks for `{}`", crate::value::short_type_name(&type_name)),
                span,
            )
            .with_hint(
                "a declared type arrives as JSON, so its pieces are syntax, not prose; annotate the call `: str` to stream an answer, or drop the handler to keep the typed result",
            ));
        }

        // A mock stands in for the whole call. It is checked against the
        // declared type, so a mock of the wrong shape fails the test instead
        // of passing it — which is the failure mode of untyped mocking.
        if let Some(mocked) = self.mocked_analyze.last().cloned() {
            self.check_mock(&mocked, &type_name, span)?;
            // A mocked stream still runs the handler, once, over the whole
            // mocked answer -- as one piece rather than none, since a test
            // has no way to script the pieces a real provider would choose.
            // Without this, `with mock` would make the handler body dead
            // code under every test that uses it.
            if let (Some(handler), Value::Variant { tag, payload }) = (on_token, mocked.unlabeled())
            {
                if tag.as_str() == "Ok" {
                    if let Some(Value::Str(text)) = payload.first().map(Value::unlabeled) {
                        self.run_token_handler(handler, text.as_str(), scope)?;
                    }
                }
            }
            return Ok(mocked);
        }

        // `tools=[...]`: the tools the model may call.
        let tool_funcs = match kwargs.iter().find(|(n, _)| n == "tools") {
            Some((_, arg)) => self.tool_list(arg, scope)?,
            Option::None => Vec::new(),
        };

        // An MCP server is a separate process, so offering its tools is a
        // second destination for the data — distinct from the model itself.
        // Releasing a secret to the model does not release it to GitHub.
        let data_label_for_servers = self.deep_label(&data);
        if data_label_for_servers.is_classified() {
            let mut servers: Vec<&str> = tool_funcs
                .iter()
                .filter_map(|h| match h {
                    ToolHandle::Mcp { server, .. } => Some(server.as_str()),
                    ToolHandle::Kora { .. } => Option::None,
                })
                .collect();
            servers.sort();
            servers.dedup();
            for server in servers {
                if !data_label_for_servers.may_reach(server) {
                    return Err(RuntimeError::new(
                        format!(
                            "classified data cannot reach MCP server `{server}` (no declassify in scope)"
                        ),
                        args[0].span,
                    )
                    .with_hint(format!(
                        "a server runs in its own process, so it is its own sink: wrap it in `declassify <value> for {server}:` and allow that sink in kora.toml"
                    )));
                }
            }
        }
        let tools: Vec<ToolSpec> = tool_funcs
            .iter()
            .map(|handle| match handle {
                ToolHandle::Kora { def, .. } => self.tool_spec(def),
                ToolHandle::Mcp { server, name } => self.mcp_tool_spec(server, name, span),
            })
            .collect::<Result<_, _>>()?;

        let data_json = value_to_json(data.unlabeled());
        let data_text = serde_json::to_string(&data_json).unwrap_or_else(|_| "null".to_string());

        // Images ride alongside the JSON. They are part of the question, so
        // they are part of the cassette key too: the same prompt over a
        // different picture is a different call.
        let mut images = Vec::new();
        collect_images(&data, &mut images);
        let media_key = cassette::media_key(
            &images
                .iter()
                .map(|i| (i.mime.as_str(), i.bytes.as_slice()))
                .collect::<Vec<_>>(),
        );

        let model = match kwargs.iter().find(|(n, _)| n == "model") {
            Some((_, arg)) => self.named_model(arg, scope)?,
            Option::None => self
                .config
                .default_model()
                .map_err(|e| RuntimeError::new(e.message, span))?,
        };
        let model_label = format!("{:?}:{}", model.provider, model.model).to_lowercase();
        let site = format!("{}:{}", self.program_name, span.line);
        let key = cassette::key_for(&site, &model_label, &prompt, &data_text, &media_key);

        // Budget check before spending: an exhausted budget stops the call
        // rather than discovering the overrun afterwards. Exhaustion is a
        // value, so partial work upstream survives.
        if let Some(meter) = self.budget.check() {
            // Recorded even though nothing was sent. `x = analyze(...) else:`
            // can swallow an `Exhausted` on its way to a fallback, and a
            // budget that stopped a call has to stay visible to whoever reads
            // the trace afterwards -- otherwise the cheapest-looking run is
            // the one that quietly did the least.
            self.trace_refused_call(&type_name, meter.name());
            return Ok(Value::Variant {
                tag: Rc::new("Exhausted".to_string()),
                payload: vec![Value::Str(Rc::new(meter.name().to_string()))],
            });
        }

        // A durable run replays its own journal first: resuming must return
        // exactly what the earlier attempt returned.
        let journal_site = format!("{site}#model");
        if let Some((outcome, chunks)) = self.journal_model_call(&journal_site, span)? {
            self.trace_replayed_call(&type_name, "journal");
            self.replay_chunks(on_token, &chunks, scope)?;
            return self.outcome_to_value(outcome, &type_name, span);
        }

        // Replay next: a cassette hit costs nothing and keeps CI deterministic.
        let recorded = self.cassette.as_ref().and_then(|c| {
            c.lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key)
                .map(|e| e.outcome.clone())
        });
        if let Some(outcome) = recorded {
            self.trace_replayed_call(&type_name, "cassette");
            self.replay_chunks(on_token, &chunks_of(&outcome), scope)?;
            return self.outcome_to_value(outcome_from_record(outcome), &type_name, span);
        }
        let mode = self
            .cassette
            .as_ref()
            .map(|c| c.lock().unwrap_or_else(|e| e.into_inner()).mode);
        if mode == Some(Mode::Replay) {
            return Err(RuntimeError::new(
                format!("no recorded model call for {site} (replay mode)"),
                span,
            )
            .with_hint("re-record with `kora run --record <file.ko>`"));
        }

        let (outcome, chunks) = match on_token {
            Some(handler) => self.run_stream(
                &model, &prompt, &data_text, &images, &schema, handler, scope, span,
            )?,
            None => (
                self.run_tool_loop(
                    &model,
                    &prompt,
                    &data_text,
                    &images,
                    &schema,
                    &tools,
                    &tool_funcs,
                    span,
                )?,
                Vec::new(),
            ),
        };
        // Journal before anything else: a crash after this point must resume
        // without paying for the call again.
        self.journal_record_model(&journal_site, &outcome, &chunks, span)?;

        // A cassette is a fixture. Recording an outage into one would make
        // every later replay fail for a reason that was over by the afternoon,
        // and `--replay` is meant to be the deterministic half of the runtime.
        let recordable = !matches!(outcome, AnalyzeOutcome::Failed { .. });
        if let Some(c) = self.cassette.as_ref().filter(|_| recordable) {
            let mut c = c.lock().unwrap_or_else(|e| e.into_inner());
            if c.mode == Mode::Record {
                c.insert(cassette::Entry {
                    key,
                    site,
                    model: model_label,
                    prompt,
                    data: data_text,
                    media: media_key,
                    outcome: record_from_outcome_with(&outcome, &chunks),
                });
            }
        }

        self.outcome_to_value(outcome, &type_name, span)
    }

    /// Resolve `model="name"` against `[models]` in kora.toml.
    ///
    /// A *name* from the config, never a provider spec. A program says which
    /// role it needs — `vision`, `smart`, `cheap` — and the config says which
    /// model fills it. Accepting `"openai:gpt-4o"` here would put a vendor's
    /// model name in source files, which is how a program ends up needing an
    /// environment variable to choose between two providers.
    fn named_model(
        &mut self,
        arg: &Expr,
        scope: &mut Scope,
    ) -> Result<kora_models::ModelConfig, RuntimeError> {
        let value = self.eval(arg, scope)?;
        // The model is a destination. A name that came from outside the
        // program — model output, a file, an HTTP body — could redirect the
        // call to somewhere the program never intended.
        require_verified(&value, "analyze", "a model name", arg.span)?;
        let Value::Str(name) = value.unlabeled() else {
            return Err(RuntimeError::new(
                format!(
                    "analyze(model=...) must be a string, got {}",
                    value.type_name()
                ),
                arg.span,
            ));
        };
        if !self.config.models.contains_key(name.as_str()) {
            let mut known: Vec<&str> = self.config.models.keys().map(String::as_str).collect();
            known.sort();
            let hint = if known.is_empty() {
                "add one to kora.toml, e.g. `[models] vision = \"local:gemma4:12b\"`".to_string()
            } else {
                format!("kora.toml declares: {}", known.join(", "))
            };
            return Err(RuntimeError::new(
                format!("no model named `{name}` in kora.toml"),
                arg.span,
            )
            .with_hint(hint));
        }
        self.config
            .resolve_model(name)
            .map_err(|e| RuntimeError::new(e.message, arg.span))
    }

    /// Build the model schema from a Kora `type` declaration.
    fn schema_for(&self, type_name: &str, span: Span) -> Result<Schema, RuntimeError> {
        // `str` is the one result type that is not a declared shape: the
        // answer is prose, and asking for it should not require inventing a
        // one-field type to hold it. It still travels as an object on the
        // wire so `Uncertain` survives -- see `Schema::for_text`.
        if type_name == "str" {
            return Ok(Schema::for_text());
        }
        let mut seen = HashSet::new();
        self.schema_for_inner(type_name, span, &mut seen)
    }

    /// `seen` holds the qualified names of types on the current path from the
    /// root result type down to this call, so a type that (directly or
    /// through another type) contains itself is a clear error instead of a
    /// stack overflow.
    fn schema_for_inner(
        &self,
        type_name: &str,
        span: Span,
        seen: &mut HashSet<String>,
    ) -> Result<Schema, RuntimeError> {
        let written = crate::value::short_type_name(type_name);
        let fields = self.types.get(type_name).ok_or_else(|| {
            RuntimeError::new(format!("`{written}` is not a declared type"), span)
                .with_hint("declare it first, e.g. `type Insight:` with typed fields below")
        })?;
        if !seen.insert(type_name.to_string()) {
            return Err(RuntimeError::new(
                format!("type `{written}` cannot be requested from analyze() because it refers to itself"),
                span,
            )
            .with_hint("analyze() builds the whole result shape upfront, so a type nested inside itself has no fixed size to ask for"));
        }
        let mut out = Vec::new();
        for field in fields.clone() {
            let ft = self.analyze_field_type(&field, seen)?;
            out.push(SchemaField {
                name: field.name.clone(),
                field_type: ft,
                description: field.metadata.description.clone(),
                pattern: field.metadata.pattern.clone(),
            });
        }
        seen.remove(type_name);
        Ok(Schema {
            type_name: written.to_string(),
            fields: out,
            text: false,
        })
    }

    /// A field's type as it should be requested from a model: a scalar, a
    /// list of strings, another declared type nested inline, or a list of
    /// that type.
    fn analyze_field_type(
        &self,
        field: &FieldDef,
        seen: &mut HashSet<String>,
    ) -> Result<FieldType, RuntimeError> {
        let unsupported_hint = "analyze result fields must be str, int, float, bool, \
                                 list[str], another declared type, or list[<declared type>]";
        match &field.ty {
            TypeExpr::Name(n) => match n.as_str() {
                "str" => Ok(FieldType::Str),
                "int" => Ok(FieldType::Int),
                "float" => Ok(FieldType::Float),
                "bool" => Ok(FieldType::Bool),
                other => match self.lookup_type(other) {
                    Some((qualified, _)) => {
                        let nested = self.schema_for_inner(&qualified, field.span, seen)?;
                        Ok(FieldType::Object(Rc::new(nested)))
                    }
                    None => Err(RuntimeError::new(
                        format!(
                            "field `{}` has type `{other}`, which analyze() cannot request yet",
                            field.name
                        ),
                        field.span,
                    )
                    .with_hint(unsupported_hint)),
                },
            },
            TypeExpr::Generic(n, args) if n == "list" => match args.first() {
                Some(TypeExpr::Name(inner)) if inner == "str" => Ok(FieldType::ListOfStr),
                Some(TypeExpr::Name(inner)) => match self.lookup_type(inner) {
                    Some((qualified, _)) => {
                        let nested = self.schema_for_inner(&qualified, field.span, seen)?;
                        Ok(FieldType::ListOfObject(Rc::new(nested)))
                    }
                    None => Err(RuntimeError::new(
                        format!(
                            "field `{}` must be `list[str]` or `list[<declared type>]`",
                            field.name
                        ),
                        field.span,
                    )
                    .with_hint(unsupported_hint)),
                },
                _ => Err(RuntimeError::new(
                    format!(
                        "field `{}` must be `list[str]` or `list[<declared type>]`",
                        field.name
                    ),
                    field.span,
                )
                .with_hint(unsupported_hint)),
            },
            other => Err(RuntimeError::new(
                format!(
                    "field `{}` has type `{}`, which analyze() cannot request yet",
                    field.name,
                    other.display()
                ),
                field.span,
            )
            .with_hint(unsupported_hint)),
        }
    }

    /// Wrap a provider outcome as `Ok(Object)` or `Uncertain(reason)`.
    fn outcome_to_value(
        &mut self,
        outcome: AnalyzeOutcome,
        type_name: &str,
        span: Span,
    ) -> Result<Value, RuntimeError> {
        match outcome {
            AnalyzeOutcome::Ok {
                fields_json,
                tokens_in,
                tokens_out,
            } => {
                self.tokens_in += tokens_in;
                self.tokens_out += tokens_out;
                self.budget.charge_call(tokens_in, tokens_out);
                // A `str` result is handed back as the string itself. The
                // single-field object it travelled in is a wire detail; a
                // program that asked for prose should not have to reach
                // through a field it never declared to read it.
                if type_name == "str" {
                    let text = fields_json
                        .get(kora_models::TEXT_KEY)
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    return Ok(Value::Variant {
                        tag: Rc::new("Ok".to_string()),
                        payload: vec![Value::Str(Rc::new(text))],
                    });
                }
                // Reconstructed from the original `TypeExpr`s, not the
                // model-facing `Schema`, so a nested declared-type field
                // becomes a `Value::Object` under its qualified name -- the
                // same identity a directly-constructed value would carry --
                // rather than the display name `Schema` uses for prompts.
                let declared_fields = self.types.get(type_name).cloned().unwrap_or_default();
                let mut fields = HashMap::new();
                for field in &declared_fields {
                    let raw = fields_json.get(&field.name).ok_or_else(|| {
                        RuntimeError::new(
                            format!("model result is missing field `{}`", field.name),
                            span,
                        )
                    })?;
                    fields.insert(field.name.clone(), self.json_to_value_typed(raw, &field.ty));
                }
                Ok(Value::Variant {
                    tag: Rc::new("Ok".to_string()),
                    payload: vec![Value::Object {
                        type_name: Rc::new(type_name.to_string()),
                        fields: Rc::new(RefCell::new(fields)),
                    }],
                })
            }
            AnalyzeOutcome::Uncertain {
                reason,
                tokens_in,
                tokens_out,
            } => {
                self.tokens_in += tokens_in;
                self.tokens_out += tokens_out;
                self.budget.charge_call(tokens_in, tokens_out);
                Ok(Value::Variant {
                    tag: Rc::new("Uncertain".to_string()),
                    payload: vec![Value::Str(Rc::new(reason))],
                })
            }
            AnalyzeOutcome::Failed {
                reason,
                tokens_in,
                tokens_out,
            } => {
                // Zero for a live failure -- the turns that did complete were
                // charged as they happened -- but still added rather than
                // skipped, because a `Failed` replayed from a journal carries
                // what the original attempt spent.
                self.tokens_in += tokens_in;
                self.tokens_out += tokens_out;
                self.budget.charge_call(tokens_in, tokens_out);
                Ok(Value::Variant {
                    tag: Rc::new("Failed".to_string()),
                    payload: vec![Value::Str(Rc::new(reason))],
                })
            }
        }
    }
}

/// The pieces a recorded outcome was written in, if it was streamed.
fn chunks_of(outcome: &RecordedOutcome) -> Vec<String> {
    match outcome {
        RecordedOutcome::Ok { chunks, .. } => chunks.clone(),
        _ => Vec::new(),
    }
}

fn record_from_outcome_with(outcome: &AnalyzeOutcome, chunks: &[String]) -> RecordedOutcome {
    match outcome {
        AnalyzeOutcome::Ok {
            fields_json,
            tokens_in,
            tokens_out,
        } => RecordedOutcome::Ok {
            fields: fields_json.clone(),
            tokens_in: *tokens_in,
            tokens_out: *tokens_out,
            chunks: chunks.to_vec(),
        },
        AnalyzeOutcome::Uncertain {
            reason,
            tokens_in,
            tokens_out,
        } => RecordedOutcome::Uncertain {
            reason: reason.clone(),
            tokens_in: *tokens_in,
            tokens_out: *tokens_out,
        },
        AnalyzeOutcome::Failed {
            reason,
            tokens_in,
            tokens_out,
        } => RecordedOutcome::Failed {
            reason: reason.clone(),
            tokens_in: *tokens_in,
            tokens_out: *tokens_out,
        },
    }
}

fn outcome_from_record(record: RecordedOutcome) -> AnalyzeOutcome {
    match record {
        RecordedOutcome::Ok {
            fields,
            tokens_in,
            tokens_out,
            ..
        } => AnalyzeOutcome::Ok {
            fields_json: fields,
            tokens_in,
            tokens_out,
        },
        RecordedOutcome::Uncertain {
            reason,
            tokens_in,
            tokens_out,
        } => AnalyzeOutcome::Uncertain {
            reason,
            tokens_in,
            tokens_out,
        },
        RecordedOutcome::Failed {
            reason,
            tokens_in,
            tokens_out,
        } => AnalyzeOutcome::Failed {
            reason,
            tokens_in,
            tokens_out,
        },
    }
}

/// Every image reachable from `value`, in the order the program wrote them.
///
/// Images are pulled out of the data rather than passed separately so that
/// `analyze({"front": front, "back": back}, ...)` works: the model sees the
/// structure in the JSON and the pixels in the same request.
fn collect_images(value: &Value, out: &mut Vec<Rc<crate::media::Image>>) {
    match value {
        Value::Image(image) => out.push(image.clone()),
        Value::List(items) => {
            for item in items.borrow().iter() {
                collect_images(item, out);
            }
        }
        // Dict and object fields are visited in name order, so the images
        // arrive in the same order on every run. A HashMap's own order is
        // not stable, and an unstable order would change the cassette key.
        Value::Dict(map) => {
            let map = map.borrow();
            let mut names: Vec<&String> = map.keys().collect();
            names.sort();
            for name in names {
                collect_images(&map[name], out);
            }
        }
        Value::Object { fields, .. } => {
            let fields = fields.borrow();
            let mut names: Vec<&String> = fields.keys().collect();
            names.sort();
            for name in names {
                collect_images(&fields[name], out);
            }
        }
        Value::Variant { payload, .. } => {
            for item in payload {
                collect_images(item, out);
            }
        }
        Value::Labeled { inner, .. } => collect_images(inner, out),
        _ => {}
    }
}

/// Kora value -> JSON, for sending data to a model.
fn value_to_json(value: &Value) -> serde_json::Value {
    use serde_json::Value as J;
    match value {
        Value::Int(v) => J::from(*v),
        Value::Float(v) => serde_json::Number::from_f64(*v)
            .map(J::Number)
            .unwrap_or(J::Null),
        Value::Str(s) => J::String(s.to_string()),
        Value::Bool(b) => J::Bool(*b),
        Value::None => J::Null,
        Value::List(items) => J::Array(items.borrow().iter().map(value_to_json).collect()),
        Value::Dict(map) => J::Object(
            map.borrow()
                .iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect(),
        ),
        Value::Object { fields, .. } => J::Object(
            fields
                .borrow()
                .iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect(),
        ),
        Value::Variant { tag, payload } => {
            if payload.is_empty() {
                J::String(tag.to_string())
            } else {
                let mut obj = serde_json::Map::new();
                obj.insert(
                    tag.to_string(),
                    J::Array(payload.iter().map(value_to_json).collect()),
                );
                J::Object(obj)
            }
        }
        // The pixels travel beside the JSON, not inside it. The marker holds
        // the image's place in the structure -- the Nth marker is the Nth
        // image in the request -- and deliberately carries neither the path
        // nor the size: the data text is part of the cassette key, and a key
        // that moves when a file is renamed would miss every recording.
        Value::Image(_) => J::String("<image>".to_string()),
        Value::Func { def, .. } => J::String(format!("<function {}>", def.name)),
        Value::Builtin(name) => J::String(format!("<builtin {name}>")),
        Value::Module { name } => J::String(format!("<module {name}>")),
        Value::UserModule { alias, .. } => J::String(format!("<module {alias}>")),
        // The qualifier is internal identity. Letting it reach a prompt would
        // put a package id in front of the model and move every cassette key.
        Value::TypeRef { name } => {
            J::String(format!("<type {}>", crate::value::short_type_name(name)))
        }
        Value::McpServer { alias } => J::String(format!("<mcp server {alias}>")),
        Value::PyModule { module } => J::String(format!("<python module {module}>")),
        Value::McpTool { server, name } => J::String(format!("<tool {server}.{name}>")),
        // Serialization is only reached after a label check has passed, so
        // the wrapper is transparent here.
        Value::Labeled { inner, .. } => value_to_json(inner),
    }
}

/// JSON -> Kora value, for reading a model result back.
fn json_to_value(json: &serde_json::Value) -> Value {
    use serde_json::Value as J;
    match json {
        J::Null => Value::None,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        J::String(s) => Value::Str(Rc::new(s.clone())),
        J::Array(items) => Value::List(Rc::new(RefCell::new(
            items.iter().map(json_to_value).collect(),
        ))),
        J::Object(map) => Value::Dict(Rc::new(RefCell::new(
            map.iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect(),
        ))),
    }
}

impl Interpreter {
    /// JSON -> Kora value, honoring a declared result type: a `type`-shaped
    /// field becomes a `Value::Object` under its *qualified* name (so
    /// `.attr` access, `match`, and package-scoped type identity all work
    /// the same as a directly-constructed value), rather than the untyped
    /// `Value::Dict` that plain `json_to_value` would otherwise produce.
    ///
    /// Driven by the original `TypeExpr`, not `kora_models::FieldType` --
    /// `Schema::type_name` is a display name meant for prompts, and does not
    /// carry the package qualifier `Value::Object` needs.
    fn json_to_value_typed(&self, json: &serde_json::Value, ty: &TypeExpr) -> Value {
        use serde_json::Value as J;
        match ty {
            TypeExpr::Name(n) if matches!(n.as_str(), "str" | "int" | "float" | "bool") => {
                json_to_value(json)
            }
            TypeExpr::Name(n) => match self.lookup_type(n) {
                Some((qualified, fields)) => {
                    let J::Object(map) = json else {
                        return json_to_value(json);
                    };
                    let mut out = HashMap::new();
                    for field in &fields {
                        let raw = map.get(&field.name).unwrap_or(&J::Null);
                        out.insert(field.name.clone(), self.json_to_value_typed(raw, &field.ty));
                    }
                    Value::Object {
                        type_name: Rc::new(qualified),
                        fields: Rc::new(RefCell::new(out)),
                    }
                }
                None => json_to_value(json),
            },
            TypeExpr::Generic(n, args) if n == "list" => match (json, args.first()) {
                (J::Array(items), Some(inner_ty)) => Value::List(Rc::new(RefCell::new(
                    items
                        .iter()
                        .map(|item| self.json_to_value_typed(item, inner_ty))
                        .collect(),
                ))),
                _ => json_to_value(json),
            },
            _ => json_to_value(json),
        }
    }

    /// Run a `parallel for` body across worker threads.
    ///
    /// Each worker is its own agent: a fresh interpreter with a private heap,
    /// seeded by copying the values it needs. Nothing is shared except the
    /// budget (an atomic pot) and the cassette (read-mostly, behind a lock),
    /// so there is no data race to reason about and no lock in user code.
    fn run_parallel(
        &mut self,
        var: &str,
        items: Vec<Value>,
        body: &[Stmt],
        scope: &Scope,
        span: Span,
    ) -> Result<Value, RuntimeError> {
        if items.is_empty() {
            return Ok(Value::List(Rc::new(RefCell::new(Vec::new()))));
        }

        // Snapshot everything a worker may read, as portable copies.
        let mut seed: Vec<(String, Portable)> = Vec::new();
        for (name, value) in scope.iter().chain(self.globals.iter()) {
            if name != var {
                seed.push((name.clone(), Portable::from_value(value)));
            }
        }
        // Imported modules travel with the worker: a function copied into a
        // branch still resolves the names of the file it was written in.
        let module_seed = self.snapshot_modules();
        let current_module = self.current_module;
        let types = self.types.clone();
        let config = self.config.clone();
        let sinks = self.sinks.clone();
        let journal = self.journal.clone();
        let parent_scope = self.scope.clone();
        let tracer = self.tracer.clone();
        let parent_span = self.parent_span.clone();
        // Connections are shared, not re-made per branch: a server or a
        // Python interpreter is a process, and one per agent would be both
        // slow and wrong.
        let mcp = self.mcp.clone();
        let python = self.python.clone();
        let program_name = self.program_name.clone();
        let body: Vec<Stmt> = body.to_vec();
        let budget = self.budget.clone();
        // Crosses the thread boundary as data, like everything else a worker
        // is seeded with.
        let mocked_analyze: Vec<Portable> = self
            .mocked_analyze
            .iter()
            .map(Portable::from_value)
            .collect();

        // Workers share one cassette handle: replay works inside parallel
        // bodies, and recordings from every worker land in the same file.
        let cassette = self.cassette.clone();
        // Read-only, so a worker resolves `use pkg` exactly as its parent.
        let packages = self.packages.clone();

        let portable_items: Vec<Portable> = items.iter().map(Portable::from_value).collect();
        let next = std::sync::atomic::AtomicUsize::new(0);
        let total = portable_items.len();
        let slots: Vec<std::sync::Mutex<Option<WorkerResult>>> =
            (0..total).map(|_| std::sync::Mutex::new(None)).collect();

        let worker_count = self.max_workers.min(total).max(1);
        std::thread::scope(|s| {
            for _ in 0..worker_count {
                s.spawn(|| loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if index >= total {
                        break;
                    }
                    let outcome = run_one(
                        index,
                        &portable_items[index],
                        var,
                        &body,
                        &seed,
                        &module_seed,
                        current_module,
                        &types,
                        &packages,
                        &config,
                        &sinks,
                        &program_name,
                        &budget,
                        &mocked_analyze,
                        cassette.as_ref(),
                        &journal,
                        &parent_scope,
                        &tracer,
                        &parent_span,
                        &mcp,
                        &python,
                    );
                    *slots[index].lock().unwrap() = Some(outcome);
                });
            }
        });

        // Fold worker results back in deterministic input order, so a parallel
        // run reads exactly like a sequential one.
        let mut collected = Vec::with_capacity(total);
        let mut first_error: Option<RuntimeError> = None;
        for slot in slots {
            let Some(result) = slot.into_inner().unwrap_or_else(|e| e.into_inner()) else {
                continue;
            };
            self.tokens_in += result.tokens_in;
            self.tokens_out += result.tokens_out;
            self.model_calls += result.model_calls;
            self.output.extend(result.output);
            self.declassify_sites.extend(result.declassify_sites);
            match result.value {
                Ok(v) => collected.push(v.into_value()),
                Err(e) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        }

        if let Some(mut e) = first_error {
            // Point at the loop, since the body ran on another thread.
            if e.span.line == 0 {
                e.span = span;
            }
            // A branch that errored is a bug in the program, not a failure the
            // program modelled -- an expected failure is an outcome and comes
            // back as a value in its own slot. So the loop still fails. But
            // the work that did finish was paid for, and saying how much of it
            // there was is the difference between "this is broken" and "this
            // is broken on one input out of two hundred".
            if collected.len() > 1 {
                e = e.with_hint(format!(
                    "{} of {total} branches had already finished; their results are lost with this error",
                    collected.len()
                ));
            }
            return Err(e);
        }
        Ok(Value::List(Rc::new(RefCell::new(collected))))
    }
}

/// What one worker produced.
struct WorkerResult {
    value: Result<Portable, RuntimeError>,
    output: Vec<String>,
    tokens_in: u64,
    tokens_out: u64,
    model_calls: u64,
    declassify_sites: Vec<DeclassifySite>,
}

/// Execute one iteration of a `parallel for` body in a private interpreter.
#[allow(clippy::too_many_arguments)]
fn run_one(
    index: usize,
    item: &Portable,
    var: &str,
    body: &[Stmt],
    seed: &[(String, Portable)],
    module_seed: &[ModuleSnapshot],
    current_module: ModuleId,
    types: &HashMap<String, Vec<FieldDef>>,
    packages: &Arc<kora_pkg::Resolution>,
    config: &Config,
    sinks: &SinkPolicy,
    program_name: &str,
    budget: &Budget,
    mocked_analyze: &[Portable],
    cassette: Option<&Arc<Mutex<Cassette>>>,
    journal: &Arc<Mutex<Journal>>,
    parent_scope: &journal::Scope,
    tracer: &Arc<Tracer>,
    parent_span: &Option<String>,
    mcp: &Arc<Mutex<HashMap<String, kora_mcp::Server>>>,
    python: &Arc<Mutex<Option<kora_python::Worker>>>,
) -> WorkerResult {
    let mut interp = Interpreter::new();
    interp.restore_modules(module_seed, current_module);
    interp.types = types.clone();
    interp.packages = packages.clone();
    interp.config = config.clone();
    interp.allow_private_hosts = config.http_allow_private;
    interp.http_timeout_secs = config.http_timeout_secs;
    interp.sinks = sinks.clone();
    interp.program_name = program_name.to_string();
    interp.budget = budget.clone();
    // A mock is part of the test that set it up, and a `parallel for` inside
    // that test is still inside it. Without this the fan-out reaches for a
    // real model, which makes the one path most worth testing the one path
    // that cannot be.
    interp.mocked_analyze = mocked_analyze
        .iter()
        .map(|m| m.clone().into_value())
        .collect();
    for (name, value) in seed {
        interp
            .globals
            .insert(name.clone(), value.clone().into_value());
    }
    interp.cassette = cassette.cloned();
    interp.journal = journal.clone();
    // One trace covers the whole run, so parallel branches hang off the span
    // that spawned them rather than starting traces of their own.
    interp.tracer = tracer.clone();
    interp.mcp = mcp.clone();
    interp.python = python.clone();
    interp.parent_span = parent_span.clone();
    // Each branch counts its own journal steps, so a resumed run replays
    // correctly no matter how the threads interleaved.
    interp.scope = parent_scope.child(index);

    let mut scope: Scope = HashMap::new();
    scope.insert(var.to_string(), item.clone().into_value());
    // The loop index is useful for diagnostics and is cheap to expose.
    scope.insert("__index__".to_string(), Value::Int(index as i64));

    let flow = interp.exec_block(body, &mut scope);

    let value = match flow {
        // `return` inside the body yields that value; otherwise the body's
        // last bound value for `var` is not meaningful, so yield None.
        Ok(Flow::Return(v)) => Ok(Portable::from_value(&v)),
        Ok(_) => Ok(Portable::None),
        Err(e) => Err(e),
    };

    // A branch that ended mid-line still wrote those characters.
    interp.flush_pending();
    WorkerResult {
        value,
        output: interp.output,
        tokens_in: interp.tokens_in,
        tokens_out: interp.tokens_out,
        model_calls: interp.model_calls,
        declassify_sites: interp.declassify_sites,
    }
}

impl Interpreter {
    /// Evaluate the `tools=[...]` argument into the functions it names.
    fn tool_list(
        &mut self,
        arg: &Expr,
        scope: &mut Scope,
    ) -> Result<Vec<ToolHandle>, RuntimeError> {
        let value = self.eval(arg, scope)?;
        let items = match value {
            Value::List(items) => items.borrow().clone(),
            other => {
                return Err(RuntimeError::new(
                    format!("analyze() tools must be a list, got {}", other.type_name()),
                    arg.span,
                )
                .with_hint("write `tools=[lookup_customer]`"))
            }
        };
        let mut out = Vec::new();
        for item in items {
            match item {
                Value::Func { def, home } if def.kind == FuncKind::Tool => {
                    out.push(ToolHandle::Kora { def, home })
                }
                Value::McpTool { server, name } => out.push(ToolHandle::Mcp {
                    server: server.to_string(),
                    name: name.to_string(),
                }),
                Value::Func { def, .. } => {
                    return Err(RuntimeError::new(
                        format!("`{}` is not a tool", def.name),
                        arg.span,
                    )
                    .with_hint(format!(
                        "declare it with `tool {}(...)` so the model may call it",
                        def.name
                    )))
                }
                other => {
                    return Err(RuntimeError::new(
                        format!("expected a tool, got {}", other.type_name()),
                        arg.span,
                    ))
                }
            }
        }
        Ok(out)
    }

    /// Describe a `tool` declaration for the model: parameters from the
    /// signature, description from the docstring. No boilerplate to write.
    fn tool_spec(&self, f: &FuncDef) -> Result<ToolSpec, RuntimeError> {
        let mut params = Vec::new();
        for p in &f.params {
            let ty = p.ty.as_ref().ok_or_else(|| {
                RuntimeError::new(
                    format!("tool `{}` needs a type on parameter `{}`", f.name, p.name),
                    p.span,
                )
                .with_hint("models need types to know what to pass, e.g. `email: str`")
            })?;
            params.push((p.name.clone(), field_type_of(ty, &p.name, p.span)?));
        }
        Ok(ToolSpec {
            name: f.name.clone(),
            description: f
                .doc
                .clone()
                .unwrap_or_else(|| format!("The {} tool.", f.name)),
            params,
        })
    }

    /// Drive the model until it produces a final answer, running any tools it
    /// asks for along the way. Every turn is charged against the budget, so a
    /// runaway loop stops rather than spending forever.
    #[allow(clippy::too_many_arguments)]
    /// Run one `on token` handler body over a piece of the answer.
    ///
    /// `break` and `continue` are refused rather than silently ignored: the
    /// handler is not a loop the program wrote, and a `break` here reads as
    /// though it would stop the stream, which it does not.
    fn run_token_handler(
        &mut self,
        handler: &TokenHandler,
        text: &str,
        scope: &mut Scope,
    ) -> Result<(), RuntimeError> {
        scope.insert(handler.var.clone(), Value::Str(Rc::new(text.to_string())));
        match self.exec_block(&handler.body, scope)? {
            Flow::Normal => Ok(()),
            Flow::Break | Flow::Continue => Err(RuntimeError::new(
                "`break` and `continue` have nothing to leave inside an `on token` handler",
                handler.span,
            )
            .with_hint("the handler runs once per piece of the answer, not in a loop")),
            Flow::Return(_) => Err(RuntimeError::new(
                "`return` cannot be used inside an `on token` handler",
                handler.span,
            )
            .with_hint("the call has not produced its outcome yet; return after matching on it")),
        }
    }

    /// A streamed `analyze()`: the answer is handed to the handler as it is
    /// written, and the outcome comes back exactly as a blocking call's would.
    ///
    /// Returns the pieces alongside the outcome so a recording keeps them.
    /// Replaying an answer as one lump when it was written as forty would
    /// make a handler that counts pieces disagree with the run it replays.
    #[allow(clippy::too_many_arguments)]
    fn run_stream(
        &mut self,
        model: &kora_models::ModelConfig,
        prompt: &str,
        data_text: &str,
        images: &[Rc<crate::media::Image>],
        schema: &Schema,
        handler: &TokenHandler,
        scope: &mut Scope,
        _span: Span,
    ) -> Result<(AnalyzeOutcome, Vec<String>), RuntimeError> {
        let request = AnalyzeRequest {
            prompt: prompt.to_string(),
            data_json: data_text.to_string(),
            images: images
                .iter()
                .map(|i| kora_models::ImagePart {
                    mime: i.mime.clone(),
                    bytes: i.bytes.clone(),
                })
                .collect(),
            schema: schema.clone(),
            tools: Vec::new(),
            tool_history: Vec::new(),
        };

        let mut chunks: Vec<String> = Vec::new();
        let mut handler_error: Option<RuntimeError> = None;
        let result = {
            let mut on_text = |text: &str| -> Result<kora_models::Flow, kora_models::ModelError> {
                chunks.push(text.to_string());
                match self.run_token_handler(handler, text, scope) {
                    Ok(()) => Ok(kora_models::Flow::Continue),
                    Err(e) => {
                        // The handler raising is the program failing, not the
                        // provider. Stop reading rather than paying for the
                        // rest of an answer nobody will look at, and carry the
                        // real error out past the transport's error type.
                        handler_error = Some(e);
                        Ok(kora_models::Flow::Stop)
                    }
                }
            };
            kora_models::analyze_streaming(model, &request, &mut on_text)
        };
        self.model_calls += 1;

        if let Some(e) = handler_error {
            return Err(e);
        }
        match result {
            Ok(outcome) => Ok((outcome, chunks)),
            // Same rule as the blocking path: a provider that does not answer
            // is an outcome the program decides about, not a crash.
            Err(e) => Ok((
                AnalyzeOutcome::Failed {
                    reason: e.message,
                    tokens_in: 0,
                    tokens_out: 0,
                },
                chunks,
            )),
        }
    }

    // The call is one thing described by many parts: which model, what to
    // ask, what to send, and what may be called back. Bundling them into a
    // struct used once here would move the same list somewhere further from
    // where it is read.
    #[allow(clippy::too_many_arguments)]
    fn run_tool_loop(
        &mut self,
        model: &kora_models::ModelConfig,
        prompt: &str,
        data_text: &str,
        images: &[Rc<crate::media::Image>],
        schema: &Schema,
        tools: &[ToolSpec],
        tool_funcs: &[ToolHandle],
        span: Span,
    ) -> Result<AnalyzeOutcome, RuntimeError> {
        // A hard ceiling so a model that keeps asking for tools cannot spin
        // forever when no `max_steps` budget was declared.
        const MAX_TURNS: usize = 12;
        let mut history: Vec<ToolExchange> = Vec::new();
        // Built once: the tool loop re-sends the same pictures each turn, and
        // a receipt scan is megabytes.
        let parts: Vec<kora_models::ImagePart> = images
            .iter()
            .map(|i| kora_models::ImagePart {
                mime: i.mime.clone(),
                bytes: i.bytes.clone(),
            })
            .collect();

        for _ in 0..MAX_TURNS {
            if let Some(meter) = self.budget.check() {
                return Err(RuntimeError::new(
                    format!("budget exhausted ({}) during tool loop", meter.name()),
                    span,
                ));
            }
            let request = AnalyzeRequest {
                prompt: prompt.to_string(),
                data_json: data_text.to_string(),
                images: parts.clone(),
                schema: schema.clone(),
                tools: tools.to_vec(),
                tool_history: history.clone(),
            };
            // A provider that does not answer is an outcome, not a crash.
            // The transport has already retried whatever was worth retrying,
            // so reaching here means the failure outlasted the backoff -- and
            // the program, not the runtime, decides what that is worth. The
            // tokens are zero because every turn that did complete was
            // charged as it happened, a few lines below.
            let step = match kora_models::step(model, &request) {
                Ok(step) => step,
                Err(e) => {
                    self.model_calls += 1;
                    return Ok(AnalyzeOutcome::Failed {
                        reason: e.message,
                        tokens_in: 0,
                        tokens_out: 0,
                    });
                }
            };
            self.model_calls += 1;

            match step {
                Step::Done(outcome) => return Ok(outcome),
                Step::CallTool {
                    name,
                    arguments_json,
                    tokens_in,
                    tokens_out,
                } => {
                    self.tokens_in += tokens_in;
                    self.tokens_out += tokens_out;
                    self.budget.charge_call(tokens_in, tokens_out);
                    if let Some(meter) = self.budget.charge_step() {
                        return Err(RuntimeError::new(
                            format!("budget exhausted ({}) during tool loop", meter.name()),
                            span,
                        ));
                    }
                    // A server that never answered is the same shape of
                    // failure as a provider that never answered, and ends the
                    // call the same way. Handing it back to the model instead
                    // would invite it to call the wedged tool again, paying
                    // the timeout on every remaining turn until the budget is
                    // gone -- and then reporting `Exhausted`, which names the
                    // wrong cause.
                    let result_json =
                        match self.run_tool(&name, &arguments_json, tool_funcs, span)? {
                            ToolRun::Result(text) => text,
                            ToolRun::Unavailable(reason) => {
                                return Ok(AnalyzeOutcome::Failed {
                                    reason,
                                    tokens_in: 0,
                                    tokens_out: 0,
                                })
                            }
                        };
                    history.push(ToolExchange {
                        name,
                        arguments_json,
                        result_json,
                    });
                }
            }
        }
        Err(RuntimeError::new(
            format!("model kept asking for tools after {MAX_TURNS} turns"),
            span,
        )
        .with_hint("add a `budget: max_steps = N` line, or simplify the prompt"))
    }

    /// Execute one model-requested tool call and return its JSON result.
    fn run_tool(
        &mut self,
        name: &str,
        arguments_json: &str,
        tool_funcs: &[ToolHandle],
        span: Span,
    ) -> Result<ToolRun, RuntimeError> {
        let handle = tool_funcs
            .iter()
            .find(|h| h.model_name() == name)
            .ok_or_else(|| {
                RuntimeError::new(format!("model asked for unknown tool `{name}`"), span)
            })?
            .clone();

        // An MCP tool runs in its server's process. What it returns is data
        // from outside the program, labeled accordingly by the caller.
        let (func, home) = match handle {
            ToolHandle::Mcp { server, name } => {
                return self.run_mcp_tool(&server, &name, arguments_json, span)
            }
            ToolHandle::Kora { def, home } => (def, home),
        };

        let parsed: serde_json::Value =
            serde_json::from_str(arguments_json).unwrap_or(serde_json::Value::Null);
        let mut args = Vec::new();
        for p in &func.params {
            let raw = parsed.get(&p.name).ok_or_else(|| {
                RuntimeError::new(
                    format!("model called `{name}` without argument `{}`", p.name),
                    span,
                )
            })?;
            args.push(json_to_value(raw));
        }

        let result = self.call_function(&func, home, args, span)?;
        Ok(ToolRun::Result(
            serde_json::to_string(&value_to_json(&result)).unwrap_or_else(|_| "null".to_string()),
        ))
    }
}

/// What running one model-requested tool produced.
enum ToolRun {
    /// JSON text to hand back to the model, including a tool that ran and
    /// reported its own failure -- that is an answer, and the model can act
    /// on it.
    Result(String),
    /// The server did not answer at all. Not something the model can route
    /// around, so the `analyze` call ends as `Failed(reason)`.
    Unavailable(String),
}

/// Map a Kora type annotation onto a model-visible field type.
fn field_type_of(ty: &TypeExpr, what: &str, span: Span) -> Result<FieldType, RuntimeError> {
    match ty {
        TypeExpr::Name(n) => match n.as_str() {
            "str" => Ok(FieldType::Str),
            "int" => Ok(FieldType::Int),
            "float" => Ok(FieldType::Float),
            "bool" => Ok(FieldType::Bool),
            other => Err(RuntimeError::new(
                format!("`{what}` has type `{other}`, which models cannot be given yet"),
                span,
            )
            .with_hint("supported types: str, int, float, bool, list[str]")),
        },
        TypeExpr::Generic(n, args) if n == "list" => match args.first() {
            Some(TypeExpr::Name(inner)) if inner == "str" => Ok(FieldType::ListOfStr),
            _ => Err(RuntimeError::new(
                format!("`{what}` must be `list[str]`"),
                span,
            )),
        },
        other => Err(RuntimeError::new(
            format!(
                "`{what}` has type `{}`, which models cannot be given yet",
                other.display()
            ),
            span,
        )),
    }
}

impl Interpreter {
    /// The sink name a model call flows to, derived from the configured
    /// provider: an Ollama model is `local_model`, an API model is its
    /// provider name. Policy in kora.toml is written against these names.
    pub fn model_sink_name(&self) -> String {
        match self.config.default_model() {
            Ok(model) => match model.provider {
                kora_models::Provider::Ollama => "local_model".to_string(),
                kora_models::Provider::OpenAI => "openai".to_string(),
            },
            Err(_) => "model".to_string(),
        }
    }
}

/// Replace sensitive leaf values with stable placeholders.
///
/// The model gets shape without secrets: `<STR_1> owes <NUM_2>`. Because the
/// result contains no classified content, it is public by construction and
/// needs no declassification.
fn redact_value(
    value: &Value,
    types: &HashMap<String, Vec<FieldDef>>,
    inherited: bool,
    counter: &mut usize,
) -> Value {
    if inherited
        && !matches!(
            value,
            Value::List(_) | Value::Dict(_) | Value::Object { .. }
        )
    {
        *counter += 1;
        return Value::Str(Rc::new(format!("<{}_{counter}>", tag_for(value))));
    }
    match value {
        Value::Labeled { inner, .. } => {
            // Containers keep their shape; only sensitive leaves are masked.
            match inner.unlabeled() {
                inner @ (Value::List(_) | Value::Dict(_) | Value::Object { .. }) => {
                    redact_value(inner, types, true, counter)
                }
                leaf => {
                    *counter += 1;
                    Value::Str(Rc::new(format!("<{}_{counter}>", tag_for(leaf))))
                }
            }
        }
        Value::List(items) => Value::List(Rc::new(RefCell::new(
            items
                .borrow()
                .iter()
                .map(|v| redact_value(v, types, inherited, counter))
                .collect(),
        ))),
        Value::Dict(map) => Value::Dict(Rc::new(RefCell::new(
            map.borrow()
                .iter()
                .map(|(k, v)| (k.clone(), redact_value(v, types, inherited, counter)))
                .collect(),
        ))),
        Value::Object { type_name, fields } => {
            // Per-field `classified` markers live on the type declaration, so
            // consult it: the field's value carries no wrapper of its own.
            let declared = types.get(type_name.as_str());
            Value::Object {
                type_name: type_name.clone(),
                fields: Rc::new(RefCell::new(
                    fields
                        .borrow()
                        .iter()
                        .map(|(k, v)| {
                            let sensitive = inherited
                                || declared.is_some_and(|fs| {
                                    fs.iter().any(|f| &f.name == k && f.classified)
                                });
                            (k.clone(), redact_value(v, types, sensitive, counter))
                        })
                        .collect(),
                )),
            }
        }
        other => other.clone(),
    }
}

fn tag_for(value: &Value) -> &'static str {
    match value.unlabeled() {
        Value::Str(_) => "STR",
        Value::Int(_) | Value::Float(_) => "NUM",
        Value::Bool(_) => "BOOL",
        _ => "VALUE",
    }
}

impl Interpreter {
    /// `ask_human(question, context)` — the suspension primitive.
    ///
    /// On the first pass this parks the run and unwinds; the process may then
    /// exit for minutes or days. When an answer arrives the program runs again
    /// from the top, every prior effect replays from the journal, and this
    /// call returns the answer as if it had simply blocked. That is why it
    /// reads like an ordinary function call in the middle of a function.
    fn ask_human(&mut self, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
        let question = match args.first() {
            Some(v) => v.unlabeled().to_string(),
            None => {
                return Err(RuntimeError::new("ask_human() needs a question", span)
                    .with_hint("example: `ask_human(\"approve this?\", details)`"))
            }
        };
        // Asking a person is not a model sink, but shipping secrets into a
        // prompt shown elsewhere still deserves an explicit release.
        if args.iter().any(|v| v.label().is_classified()) {
            return Err(
                RuntimeError::new("ask_human() was given classified data", span)
                    .with_hint("declassify it first, or pass a redact(...) version"),
            );
        }
        let context = args
            .get(1)
            .map(|v| v.unlabeled().to_string())
            .unwrap_or_default();

        let site = format!("{}:{}#human", self.program_name, span.line);
        let mut journal = self.journal.lock().unwrap_or_else(|e| e.into_inner());

        let lookup = journal
            .next(&self.scope, &site)
            .map_err(|e| RuntimeError::new(e.to_string(), span))?;

        match lookup {
            // The answer arrived on an earlier resume: hand it back.
            Lookup::Replayed(Effect::Human { answer, .. }) => Ok(Value::Str(Rc::new(answer))),
            Lookup::Replayed(other) => Err(RuntimeError::new(
                format!("journal step is {other:?}, but the program reached ask_human()"),
                span,
            )),
            Lookup::Fresh { scope, seq } => {
                if !journal.is_durable() {
                    return Err(RuntimeError::new("ask_human() needs a durable run", span)
                        .with_hint("start it with `kora run --durable <file.ko>`"));
                }
                journal
                    .suspend(PendingQuestion {
                        scope,
                        seq,
                        site,
                        question,
                        context,
                    })
                    .map_err(|e| RuntimeError::new(e.to_string(), span))?;
                Err(RuntimeError::suspended(span))
            }
        }
    }

    /// Route a model call through the journal so a resumed run does not pay
    /// for work already done.
    fn journal_model_call(
        &mut self,
        site: &str,
        span: Span,
    ) -> Result<Option<(AnalyzeOutcome, Vec<String>)>, RuntimeError> {
        let mut journal = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        if !journal.is_durable() {
            return Ok(None);
        }
        match journal
            .next(&self.scope, site)
            .map_err(|e| RuntimeError::new(e.to_string(), span))?
        {
            Lookup::Replayed(Effect::Model { outcome }) => {
                let chunks = chunks_of(&outcome);
                Ok(Some((outcome_from_record(outcome), chunks)))
            }
            Lookup::Replayed(other) => Err(RuntimeError::new(
                format!("journal step is {other:?}, but the program reached a model call"),
                span,
            )),
            Lookup::Fresh { scope, seq } => {
                self.pending_slot = Some((scope, seq));
                Ok(None)
            }
        }
    }

    /// Record the result of a model call that was just performed.
    fn journal_record_model(
        &mut self,
        site: &str,
        outcome: &AnalyzeOutcome,
        chunks: &[String],
        span: Span,
    ) -> Result<(), RuntimeError> {
        let Some((scope, seq)) = self.pending_slot.take() else {
            return Ok(());
        };
        let mut journal = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        journal
            .record(
                scope,
                seq,
                site,
                Effect::Model {
                    outcome: record_from_outcome_with(outcome, chunks),
                },
            )
            .map_err(|e| RuntimeError::new(e.to_string(), span))
    }

    /// Re-run an `on token` handler over a recorded answer.
    ///
    /// A replayed run has to look like the run it replays, and the handler's
    /// output is part of what the program did. An answer recorded before
    /// streaming existed has no pieces, so it arrives as one.
    fn replay_chunks(
        &mut self,
        on_token: Option<&TokenHandler>,
        chunks: &[String],
        scope: &mut Scope,
    ) -> Result<(), RuntimeError> {
        let Some(handler) = on_token else {
            return Ok(());
        };
        for chunk in chunks {
            self.run_token_handler(handler, chunk, scope)?;
        }
        Ok(())
    }
}

impl Interpreter {
    /// Journal one line of output. Returns false when the line was already
    /// shown on an earlier attempt and must not be shown again.
    fn record_output(&mut self, line: &str, span: Span) -> Result<bool, RuntimeError> {
        let mut journal = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        if !journal.is_durable() {
            return Ok(true);
        }
        let site = format!("{}:{}#output", self.program_name, span.line);
        match journal
            .next(&self.scope, &site)
            .map_err(|e| RuntimeError::new(e.to_string(), span))?
        {
            Lookup::Replayed(_) => Ok(false),
            Lookup::Fresh { scope, seq } => {
                journal
                    .record(
                        scope,
                        seq,
                        &site,
                        Effect::Output {
                            text: line.to_string(),
                        },
                    )
                    .map_err(|e| RuntimeError::new(e.to_string(), span))?;
                Ok(true)
            }
        }
    }
}

/// A module's namespace in a form that can cross a thread boundary.
struct ModuleSnapshot {
    path: String,
    key: PathBuf,
    dir: PathBuf,
    package: kora_pkg::PackageId,
    names: Vec<(String, Portable)>,
}

// --- debugger ---

impl Interpreter {
    /// Attach a debugger. The controls are shared, so the caller can add
    /// breakpoints and ask for a pause while the program runs.
    pub fn attach_debugger(
        &mut self,
        debugger: Box<dyn Debugger>,
        controls: crate::debug::Controls,
        stop_on_entry: bool,
    ) {
        self.debugger = Some(debugger);
        self.debug.controls = controls;
        self.debug.stop_on_entry = stop_on_entry;
    }

    /// Whether a debugger is listening. Used to skip work that only it needs.
    pub fn is_debugging(&self) -> bool {
        self.debugger.is_some()
    }

    /// Push a frame. A no-op with no debugger attached, so ordinary runs pay
    /// nothing for the bookkeeping.
    fn debug_enter(&mut self, name: &str, line: u32) {
        if self.debugger.is_none() {
            return;
        }
        self.debug.frames.push(Frame {
            name: name.to_string(),
            file: self.current_file(),
            line,
            vars: Vec::new(),
        });
    }

    fn debug_leave(&mut self) {
        if self.debugger.is_none() {
            return;
        }
        self.debug.frames.pop();
    }

    /// Decide whether to stop before `stmt`, and block while stopped.
    fn debug_before(&mut self, stmt: &Stmt, scope: &Scope) -> Result<(), RuntimeError> {
        let file = self.current_file();
        let line = stmt.span.line;

        // The snapshot is what a paused parent frame will show. Taking it
        // before every statement means a frame that has called into another
        // shows its names as they stood at the call, which is what a stack
        // view should show.
        if let Some(frame) = self.debug.frames.last_mut() {
            frame.file = file.clone();
            frame.line = line;
            frame.vars = sorted_vars(scope);
        }

        let Some(reason) = self.debug.should_stop(&file, line) else {
            if self.debug.is_terminating() {
                return Err(RuntimeError::terminated(stmt.span));
            }
            return Ok(());
        };

        let globals = sorted_vars(&self.globals);
        // The debugger is handed the frames, so it cannot be borrowed from
        // the interpreter at the same time: take it out for the call.
        let mut debugger = self.debugger.take().expect("checked by the caller");
        let resume = debugger.stopped(reason, &self.debug.frames, &globals);
        self.debugger = Some(debugger);
        self.debug.resume(resume);

        if resume == Resume::Terminate {
            return Err(RuntimeError::terminated(stmt.span));
        }
        Ok(())
    }

    /// Forward a printed line to the debugger. `true` if one took it.
    fn debug_output(&mut self, line: &str) -> bool {
        match &mut self.debugger {
            Some(debugger) => {
                debugger.output(line);
                true
            }
            Option::None => false,
        }
    }
}

/// What to call a file's top-level frame in a stack view.
fn top_level_name(path: &str) -> String {
    let name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());
    format!("{name} (top level)")
}

/// A scope as a sorted name/value list, so the editor shows a stable order.
fn sorted_vars(scope: &Scope) -> Vec<(String, Value)> {
    let mut out: Vec<(String, Value)> = scope
        .iter()
        .filter(|(name, _)| !name.starts_with("__"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

// --- file modules ---

impl Interpreter {
    /// Copy every loaded module, for seeding a `parallel for` worker.
    ///
    /// The active module's names live in `globals`, so they are read from
    /// there rather than from the table, which is empty for it.
    fn snapshot_modules(&self) -> Vec<ModuleSnapshot> {
        self.modules
            .iter()
            .enumerate()
            .map(|(id, space)| {
                let live = if id == self.current_module {
                    &self.globals
                } else {
                    &space.names
                };
                ModuleSnapshot {
                    path: space.path.clone(),
                    key: space.key.clone(),
                    dir: space.dir.clone(),
                    package: space.package,
                    names: live
                        .iter()
                        .map(|(k, v)| (k.clone(), Portable::from_value(v)))
                        .collect(),
                }
            })
            .collect()
    }

    /// Rebuild a module table from a snapshot. Ids are preserved, so a copied
    /// function still points at the module it was defined in.
    fn restore_modules(&mut self, snapshot: &[ModuleSnapshot], current: ModuleId) {
        self.modules = snapshot
            .iter()
            .map(|m| {
                let mut space =
                    ModuleSpace::new(m.path.clone(), m.key.clone(), m.dir.clone(), m.package);
                space.names = m
                    .names
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone().into_value()))
                    .collect();
                space
            })
            .collect();
        self.current_module = current.min(self.modules.len().saturating_sub(1));
        self.globals = std::mem::take(&mut self.modules[self.current_module].names);
    }

    /// Give the entry file its real identity in the module table.
    ///
    /// `program_name` is set after the interpreter is built, so the root entry
    /// starts as a placeholder. Fixing it up before any import means a file
    /// that imports the entry file back gets the same module rather than a
    /// second copy with its own state.
    fn sync_root(&mut self) {
        let path = Path::new(&self.program_name);
        let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let root = &mut self.modules[modules::ROOT];
        root.path = self.program_name.clone();
        root.key = key;
    }

    /// Directory that imports written in the current module resolve against.
    ///
    /// The entry file's directory comes from `program_name`, so a program run
    /// from anywhere still finds the files sitting next to it.
    fn current_dir(&self) -> PathBuf {
        if self.current_module == modules::ROOT {
            return Path::new(&self.program_name)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
        }
        self.modules[self.current_module].dir.clone()
    }

    /// The file the current module was loaded from, for error messages.
    pub fn current_file(&self) -> String {
        if self.current_module == modules::ROOT {
            return self.program_name.clone();
        }
        self.modules[self.current_module].path.clone()
    }

    /// The authority the package running right now holds.
    ///
    /// Read through `current_module`, so it follows execution across a
    /// package boundary automatically: a `tool` defined in a dependency runs
    /// under that dependency's grants even when a model called it.
    fn grants(&self) -> &kora_pkg::Grants {
        static UNRESTRICTED: std::sync::OnceLock<kora_pkg::Grants> = std::sync::OnceLock::new();
        let package = self.modules[self.current_module].package;
        self.packages
            .packages
            .get(package.0)
            .map(|p| &p.grants)
            // A program run without a resolution — an embedded interpreter,
            // or a test — is the root program, and unrestricted.
            .unwrap_or_else(|| UNRESTRICTED.get_or_init(kora_pkg::Grants::unrestricted))
    }

    /// The name of the package running right now, for error messages.
    fn current_package_name(&self) -> String {
        let package = self.modules[self.current_module].package;
        self.packages
            .packages
            .get(package.0)
            .and_then(|p| p.name.clone())
            .unwrap_or_else(|| "this program".to_string())
    }

    /// Refuse an effect the running package was never granted.
    fn require_capability(
        &self,
        capability: kora_pkg::Capability,
        what: &str,
        span: Span,
    ) -> Result<(), RuntimeError> {
        if self.grants().allows(capability) {
            return Ok(());
        }
        let package = self.current_package_name();
        Err(RuntimeError::new(
            format!(
                "package `{package}` is not allowed to use {what}: no `{}` capability",
                capability.name()
            ),
            span,
        )
        .with_hint(format!(
            "grant it in kora.toml: `[dependencies.{package}]` with `grants = {{ {} = true }}`",
            capability.name()
        )))
    }

    /// Qualify a bare type name with the package the current file belongs to.
    ///
    /// Types are shared across the *files* of one package, exactly as before,
    /// and are invisible across a package boundary unless reached through the
    /// module that declared them. Without this, two dependencies declaring
    /// `Config` would be a hard error the consumer could not fix, since it
    /// owns neither of them.
    fn qualify_type(&self, name: &str) -> String {
        self.qualify_type_in(self.modules[self.current_module].package, name)
    }

    fn qualify_type_in(&self, package: kora_pkg::PackageId, name: &str) -> String {
        if package == kora_pkg::ROOT {
            // The root program's types are unqualified, so every program
            // written before packages existed behaves identically.
            name.to_string()
        } else {
            format!("#{}{}{name}", package.0, crate::value::TYPE_QUALIFIER)
        }
    }

    /// Look up a type by the name as written, in the current package.
    ///
    /// A name that already carries a qualifier — one that arrived through a
    /// `TypeRef`, having been resolved in the package that declared it — is
    /// used as-is.
    fn lookup_type(&self, name: &str) -> Option<(String, Vec<FieldDef>)> {
        if name.contains(crate::value::TYPE_QUALIFIER) {
            return self.types.get(name).map(|f| (name.to_string(), f.clone()));
        }
        let qualified = self.qualify_type(name);
        self.types.get(&qualified).map(|f| (qualified, f.clone()))
    }

    /// Make `target` the active module, returning the one it replaced.
    ///
    /// The live namespace lives in `globals`; the table holds the others. A
    /// swap rather than a copy keeps this O(1) no matter how large a module's
    /// top level is.
    fn enter_module(&mut self, target: ModuleId) -> ModuleId {
        let previous = self.current_module;
        if target == previous {
            return previous;
        }
        self.modules[previous].names = std::mem::take(&mut self.globals);
        self.globals = std::mem::take(&mut self.modules[target].names);
        self.current_module = target;
        previous
    }

    /// Undo `enter_module`.
    fn leave_module(&mut self, previous: ModuleId) {
        self.enter_module(previous);
    }

    /// Attach the current file to an error, so a failure inside an imported
    /// module is reported against that file rather than the entry file.
    fn blame_current_file(&self, error: RuntimeError) -> RuntimeError {
        if error.is_suspension() {
            return error;
        }
        error.in_file(&self.current_file())
    }

    /// Read one module's top-level name, for `alias.name`.
    fn module_member(&self, id: ModuleId, name: &str) -> Option<Value> {
        if id == self.current_module {
            return self.globals.get(name).cloned();
        }
        self.modules[id].names.get(name).cloned()
    }

    /// Every name a module exports, for the "no such name" hint.
    fn module_exports(&self, id: ModuleId) -> Vec<String> {
        let names = if id == self.current_module {
            &self.globals
        } else {
            &self.modules[id].names
        };
        let mut out: Vec<String> = names.keys().cloned().collect();
        out.sort();
        out
    }

    /// Load `path` as a module and return its id, reusing an already-loaded
    /// copy. Top-level statements run exactly once per file per run.
    fn load_module(&mut self, written: &str, span: Span) -> Result<ModuleId, RuntimeError> {
        self.sync_root();
        let base = self.current_dir();
        // A file belongs to the package that imported it, so a `use pkg`
        // written inside it resolves against that package's manifest.
        let package = self.modules[self.current_module].package;
        let resolved = modules::resolve(written, &base).map_err(|e| {
            RuntimeError::new(e.message(), span)
                .with_hint(e.hint())
                .in_file(&self.current_file())
        })?;

        self.load_resolved(resolved, written, package, span)
    }

    /// Load a package's entry file, resolving its name against the manifest
    /// of the package that wrote the import.
    ///
    /// Every file of the loaded package carries that package's id, so its own
    /// `use pkg` statements resolve against its own `[dependencies]` rather
    /// than the importer's.
    fn load_package(&mut self, name: &str, span: Span) -> Result<ModuleId, RuntimeError> {
        self.sync_root();
        let from = self.modules[self.current_module].package;

        let Some(target) = self.packages.dep_of(from, name) else {
            let declared = self
                .packages
                .packages
                .get(from.0)
                .map(|p| {
                    let mut names: Vec<&str> = p.manifest.deps.keys().map(String::as_str).collect();
                    names.sort();
                    names
                })
                .unwrap_or_default();
            let hint = if declared.is_empty() {
                format!("add `{name} = {{ path = \"...\" }}` under `[dependencies]` in kora.toml")
            } else {
                format!("kora.toml declares: {}", declared.join(", "))
            };
            return Err(
                RuntimeError::new(format!("no package named `{name}`"), span)
                    .with_hint(hint)
                    .in_file(&self.current_file()),
            );
        };

        let package = target.id;
        let entry = target.entry.clone();
        let display = entry.display().to_string();
        let dir = entry
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let resolved = modules::Resolved {
            path: entry.clone(),
            key: entry,
            dir,
        };
        self.load_resolved(resolved, &display, package, span)
    }

    /// Run a resolved file's top level once, under the given package.
    fn load_resolved(
        &mut self,
        resolved: modules::Resolved,
        written: &str,
        package: kora_pkg::PackageId,
        span: Span,
    ) -> Result<ModuleId, RuntimeError> {
        if let Some(id) = self.modules.iter().position(|m| m.key == resolved.key) {
            // Already loaded, or being loaded: a file mid-import means the
            // graph has a cycle, and its names are not there yet.
            if self.loading.contains(&id) {
                let chain: Vec<String> = self
                    .loading
                    .iter()
                    .map(|m| self.modules[*m].path.clone())
                    .collect();
                return Err(RuntimeError::new(
                    modules::cycle_message(&chain, &self.modules[id].path),
                    span,
                )
                .with_hint("move the shared code into a third file both can import")
                .in_file(&self.current_file()));
            }
            return Ok(id);
        }

        let display = resolved.path.display().to_string();
        let source = std::fs::read_to_string(&resolved.path).map_err(|e| {
            RuntimeError::new(format!("cannot read `{written}`: {e}"), span)
                .in_file(&self.current_file())
        })?;
        let program = kora_syntax::parse(&source).map_err(|e| {
            RuntimeError::new(e.message.clone(), e.span)
                .with_hint(
                    e.hint
                        .clone()
                        .unwrap_or_else(|| format!("while importing {display}")),
                )
                .in_file(&display)
        })?;

        let id = self.modules.len();
        self.modules.push(ModuleSpace::new(
            display.clone(),
            resolved.key,
            resolved.dir,
            package,
        ));

        let outer = self.enter_module(id);
        self.loading.push(id);
        self.debug_enter(&top_level_name(&display), 1);
        let mut scope: Scope = HashMap::new();
        let mut outcome = Ok(());
        for stmt in &program.items {
            match self.exec(stmt, &mut scope) {
                // A bare `return` at a module's top level ends its loading,
                // exactly as it ends the entry file's top level.
                Ok(Flow::Return(_)) => break,
                Ok(_) => {}
                Err(e) => {
                    outcome = Err(self.blame_current_file(e));
                    break;
                }
            }
        }
        self.globals.extend(scope);
        self.debug_leave();
        self.loading.pop();
        self.leave_module(outer);
        outcome?;
        Ok(id)
    }
}

impl Interpreter {
    /// Call a stdlib function, e.g. `json.parse(text)`.
    fn call_module_fn(
        &mut self,
        module_name: &str,
        function: &str,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, RuntimeError> {
        let Some(module) = crate::stdlib::module(module_name) else {
            return Err(RuntimeError::new(
                format!("there is no module named `{module_name}`"),
                span,
            ));
        };
        let Some(native) = module.functions.get(function).copied() else {
            let mut available: Vec<&str> = module.functions.keys().copied().collect();
            available.sort();
            return Err(RuntimeError::new(
                format!("`{module_name}` has no function `{function}`"),
                span,
            )
            .with_hint(format!("{module_name} provides: {}", available.join(", "))));
        };
        // Every stdlib call passes through here, so one check covers the
        // network, the filesystem, the database, and the environment. The
        // modules with no entry — json, csv, re, time — compute over values
        // the caller already holds, and have nothing to gate.
        if let Some(capability) = kora_pkg::Capability::for_module(module_name) {
            self.require_capability(capability, &format!("`{module_name}`"), span)?;
        }
        native(self, args, span)
    }

    /// Read a value that is nondeterministic, journaling it in a durable run.
    ///
    /// A clock or a random number read live during a replay would send the
    /// program down a different branch than the run it is meant to be
    /// continuing, so the first attempt's answer is the one every replay sees.
    pub fn journal_scalar(
        &mut self,
        what: &str,
        span: Span,
        live: impl Fn() -> i64,
    ) -> Result<i64, RuntimeError> {
        let mut journal = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        if !journal.is_durable() {
            return Ok(live());
        }
        let site = format!("{}:{}#{what}", self.program_name, span.line);
        match journal
            .next(&self.scope, &site)
            .map_err(|e| RuntimeError::new(e.to_string(), span))?
        {
            Lookup::Replayed(Effect::Tool { result_json, .. }) => {
                Ok(result_json.parse::<i64>().unwrap_or_else(|_| live()))
            }
            Lookup::Replayed(other) => Err(RuntimeError::new(
                format!("journal step is {other:?}, but the program reached {what}"),
                span,
            )),
            Lookup::Fresh { scope, seq } => {
                let value = live();
                journal
                    .record(
                        scope,
                        seq,
                        &site,
                        Effect::Tool {
                            name: what.to_string(),
                            result_json: value.to_string(),
                        },
                    )
                    .map_err(|e| RuntimeError::new(e.to_string(), span))?;
                Ok(value)
            }
        }
    }
}

impl Interpreter {
    /// The strictest label anywhere inside a value.
    ///
    /// A `classified` field marker lives on the *type declaration*, so an
    /// object holding a secret looks public until the field is read. Any sink
    /// that consumes a whole value — serializing it, writing it, sending it —
    /// must look inside, or the marker is trivially bypassed by passing the
    /// container instead of the field.
    pub fn deep_label(&self, value: &Value) -> Label {
        self.deep_label_inner(value, false)
    }

    fn deep_label_inner(&self, value: &Value, inherited: bool) -> Label {
        let mut label = value.label();
        if inherited {
            label = label.join(Label::CLASSIFIED);
        }
        match value.unlabeled() {
            Value::List(items) => {
                for item in items.borrow().iter() {
                    label = label.join(self.deep_label_inner(item, inherited));
                }
            }
            Value::Dict(map) => {
                for item in map.borrow().values() {
                    label = label.join(self.deep_label_inner(item, inherited));
                }
            }
            Value::Object { type_name, fields } => {
                let declared = self.types.get(type_name.as_str());
                for (name, item) in fields.borrow().iter() {
                    let sensitive = inherited
                        || declared
                            .is_some_and(|fs| fs.iter().any(|f| &f.name == name && f.classified));
                    label = label.join(self.deep_label_inner(item, sensitive));
                }
            }
            Value::Variant { payload, .. } => {
                for item in payload {
                    label = label.join(self.deep_label_inner(item, inherited));
                }
            }
            _ => {}
        }
        label
    }
}

/// Support the stdlib needs from the interpreter.
impl Interpreter {
    /// Field names and types of a declared `type`, in declaration order.
    pub fn declared_fields(&self, type_name: &str) -> Option<Vec<(String, TypeExpr)>> {
        self.types.get(type_name).map(|fields| {
            fields
                .iter()
                .map(|f| (f.name.clone(), f.ty.clone()))
                .collect()
        })
    }

    /// Qualify a bare type name the way a *sibling* of `alongside` would be.
    ///
    /// A nested field type is written bare inside the declaration that uses
    /// it, so it belongs to the package that declaration came from — not to
    /// whichever package happens to be running when the value is parsed.
    pub fn qualify_alongside(&self, alongside: &str, bare: &str) -> String {
        match alongside.rsplit_once(crate::value::TYPE_QUALIFIER) {
            Some((prefix, _)) => format!("{prefix}{}{bare}", crate::value::TYPE_QUALIFIER),
            None => bare.to_string(),
        }
    }

    /// Whether an enclosing `declassify ... for <sink>:` block released data
    /// to this sink.
    pub fn declassified_for_sink(&self, sink: &str) -> bool {
        self.declassified_for.iter().any(|s| s == sink)
    }

    /// Replay a recorded effect for a stdlib call, if a durable run already
    /// performed it. Network calls and clocks must not happen twice.
    pub fn journal_lookup(
        &mut self,
        site: &str,
        span: Span,
    ) -> Result<Option<String>, RuntimeError> {
        let mut journal = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        if !journal.is_durable() {
            return Ok(None);
        }
        match journal
            .next(&self.scope, site)
            .map_err(|e| RuntimeError::new(e.to_string(), span))?
        {
            Lookup::Replayed(Effect::Tool { result_json, .. }) => Ok(Some(result_json)),
            Lookup::Replayed(other) => Err(RuntimeError::new(
                format!("journal step is {other:?}, but the program reached {site}"),
                span,
            )),
            Lookup::Fresh { scope, seq } => {
                self.pending_slot = Some((scope, seq));
                Ok(None)
            }
        }
    }

    /// Record what a stdlib effect produced.
    pub fn journal_record(
        &mut self,
        site: &str,
        name: &str,
        result_json: &str,
        span: Span,
    ) -> Result<(), RuntimeError> {
        let Some((scope, seq)) = self.pending_slot.take() else {
            return Ok(());
        };
        let mut journal = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        journal
            .record(
                scope,
                seq,
                site,
                Effect::Tool {
                    name: name.to_string(),
                    result_json: result_json.to_string(),
                },
            )
            .map_err(|e| RuntimeError::new(e.to_string(), span))
    }
}

impl Interpreter {
    /// Verify a mocked `analyze` result matches what the call site declared.
    ///
    /// Mocking frameworks elsewhere cannot do this: they have no idea what
    /// shape the caller expected, so a mock that drifts from reality keeps
    /// passing long after the code it stands for has changed.
    fn check_mock(&self, mocked: &Value, type_name: &str, span: Span) -> Result<(), RuntimeError> {
        let Value::Variant { tag, payload } = mocked.unlabeled() else {
            return Err(RuntimeError::new(
                format!(
                    "a mock for analyze() must be Ok(...), Uncertain(...), Exhausted(...), or Failed(...), got {}",
                    mocked.type_name()
                ),
                span,
            ));
        };
        match tag.as_str() {
            "Ok" if type_name == "str" => {
                let Some(inner) = payload.first() else {
                    return Err(RuntimeError::new("Ok(...) needs a value", span));
                };
                if !matches!(inner.unlabeled(), Value::Str(_)) {
                    return Err(RuntimeError::new(
                        format!(
                            "the mock returns Ok({}), but this call site declares `str`",
                            inner.type_name()
                        ),
                        span,
                    ));
                }
                Ok(())
            }
            "Ok" => {
                let Some(inner) = payload.first() else {
                    return Err(RuntimeError::new("Ok(...) needs a value", span));
                };
                let Value::Object {
                    type_name: actual,
                    fields,
                } = inner.unlabeled()
                else {
                    return Err(RuntimeError::new(
                        format!(
                            "the mock returns Ok({}), but this call site declares `{}`",
                            inner.type_name(),
                            crate::value::short_type_name(type_name)
                        ),
                        span,
                    ));
                };
                if actual.as_str() != type_name {
                    return Err(RuntimeError::new(
                        format!(
                            "the mock returns `{}`, but this call site declares `{}`",
                            crate::value::short_type_name(actual),
                            crate::value::short_type_name(type_name)
                        ),
                        span,
                    ));
                }
                // Every declared field must be present, so a mock cannot drift
                // from the type it stands for.
                if let Some(declared) = self.types.get(type_name) {
                    let present = fields.borrow();
                    for field in declared {
                        if !present.contains_key(&field.name) {
                            return Err(RuntimeError::new(
                                format!("the mock is missing field `{}`", field.name),
                                span,
                            )
                            .with_hint(format!(
                                "`{}` declares: {}",
                                crate::value::short_type_name(type_name),
                                declared
                                    .iter()
                                    .map(|f| f.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )));
                        }
                    }
                }
                Ok(())
            }
            "Uncertain" | "Exhausted" | "Failed" => Ok(()),
            other => Err(RuntimeError::new(
                format!("`{other}` is not a valid analyze() outcome"),
                span,
            )
            .with_hint("use Ok(...), Uncertain(reason), Exhausted(meter), or Failed(reason)")),
        }
    }
}

impl Interpreter {
    /// Record a model call that was served from a cassette or journal.
    ///
    /// Worth a span of its own: a trace where cached calls are simply absent
    /// makes a replayed run look like it did no work at all.
    fn trace_replayed_call(&mut self, type_name: &str, source: &str) {
        if !self.tracer.records_calls() {
            return;
        }
        let mut span = self
            .tracer
            .start(&format!("analyze {type_name}"), self.parent_span.clone());
        self.tracer.set_plain(
            &mut span,
            "gen_ai.operation.name",
            serde_json::json!("analyze"),
        );
        self.tracer
            .set_plain(&mut span, "kora.replayed", serde_json::json!(true));
        self.tracer
            .set_plain(&mut span, "kora.replay_source", serde_json::json!(source));
        self.tracer
            .set_plain(&mut span, "gen_ai.usage.input_tokens", serde_json::json!(0));
        self.tracer.end(span, None);
    }

    /// A call the budget stopped before it was sent.
    fn trace_refused_call(&mut self, type_name: &str, meter: &str) {
        if !self.tracer.records_calls() {
            return;
        }
        let mut span = self
            .tracer
            .start(&format!("analyze {type_name}"), self.parent_span.clone());
        self.tracer.set_plain(
            &mut span,
            "gen_ai.operation.name",
            serde_json::json!("analyze"),
        );
        self.tracer
            .set_plain(&mut span, "kora.exhausted", serde_json::json!(true));
        self.tracer
            .set_plain(&mut span, "kora.exhausted_meter", serde_json::json!(meter));
        self.tracer
            .set_plain(&mut span, "gen_ai.usage.input_tokens", serde_json::json!(0));
        self.tracer.end(span, None);
    }
}

/// MCP: connecting to servers, and calling their tools.
impl Interpreter {
    /// Start a configured server, or reuse one already connected.
    fn connect_mcp(&mut self, name: &str, span: Span) -> Result<(), RuntimeError> {
        // A server is a separate process with credentials of its own, so
        // reaching one is authority a dependency has to be given by name.
        if !self.grants().allows_mcp(name) {
            let package = self.current_package_name();
            return Err(RuntimeError::new(
                format!("package `{package}` is not allowed to reach MCP server `{name}`"),
                span,
            )
            .with_hint(format!(
                "grant it in kora.toml: `[dependencies.{package}]` with `grants = {{ mcp = [\"{name}\"] }}`"
            )));
        }
        let mut servers = self.mcp.lock().unwrap_or_else(|e| e.into_inner());
        if servers.contains_key(name) {
            return Ok(());
        }
        let Some(config) = self.config.mcp_servers.get(name).cloned() else {
            let mut known: Vec<&String> = self.config.mcp_servers.keys().collect();
            known.sort();
            let mut e =
                RuntimeError::new(format!("no MCP server named `{name}` is configured"), span);
            e = if known.is_empty() {
                e.with_hint(
                    "add one to kora.toml, e.g. `[mcp.github] command = \"npx\", args = [\"-y\", \"@modelcontextprotocol/server-github\"]`",
                )
            } else {
                e.with_hint(format!(
                    "configured servers: {}",
                    known
                        .iter()
                        .map(|k| k.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            };
            return Err(e);
        };
        if config.command.is_empty() {
            return Err(RuntimeError::new(
                format!("MCP server `{name}` has no `command` in kora.toml"),
                span,
            ));
        }

        let server = kora_mcp::Server::connect(name, &config)
            .map_err(|e| RuntimeError::new(format!("could not start `{name}`: {e}"), span))?;
        servers.insert(name.to_string(), server);
        Ok(())
    }

    /// `gh.tools` — every tool the server offers, ready for `analyze`.
    fn mcp_member(
        &mut self,
        server: &str,
        member: &str,
        span: Span,
    ) -> Result<Value, RuntimeError> {
        let servers = self.mcp.lock().unwrap_or_else(|e| e.into_inner());
        let Some(connected) = servers.get(server) else {
            return Err(RuntimeError::new(
                format!("`{server}` is not connected"),
                span,
            ));
        };
        match member {
            "tools" => Ok(Value::List(Rc::new(RefCell::new(
                connected
                    .tools()
                    .iter()
                    .map(|t| Value::McpTool {
                        server: Rc::new(server.to_string()),
                        name: Rc::new(t.name.clone()),
                    })
                    .collect(),
            )))),
            // A named tool, so a program can offer a model one rather than all.
            name if connected.tool(name).is_some() => Ok(Value::McpTool {
                server: Rc::new(server.to_string()),
                name: Rc::new(name.to_string()),
            }),
            other => {
                let mut available: Vec<&str> =
                    connected.tools().iter().map(|t| t.name.as_str()).collect();
                available.sort();
                Err(
                    RuntimeError::new(format!("`{server}` has no tool `{other}`"), span).with_hint(
                        format!(
                            "`{server}` offers: {} (or use `{server}.tools` for all of them)",
                            available.join(", ")
                        ),
                    ),
                )
            }
        }
    }

    /// Describe an MCP tool to the model, using the schema the server gave us.
    fn mcp_tool_spec(
        &self,
        server: &str,
        name: &str,
        span: Span,
    ) -> Result<ToolSpec, RuntimeError> {
        let servers = self.mcp.lock().unwrap_or_else(|e| e.into_inner());
        let tool = servers
            .get(server)
            .and_then(|s| s.tool(name))
            .ok_or_else(|| RuntimeError::new(format!("`{server}` has no tool `{name}`"), span))?;

        let params = tool
            .params
            .iter()
            .filter_map(|(param, ty)| {
                // A parameter Kora cannot describe is left out rather than
                // guessed at; the model simply will not be offered it.
                let mapped = match ty {
                    kora_mcp::ParamType::Str => FieldType::Str,
                    kora_mcp::ParamType::Int => FieldType::Int,
                    kora_mcp::ParamType::Float => FieldType::Float,
                    kora_mcp::ParamType::Bool => FieldType::Bool,
                    kora_mcp::ParamType::ListOfStr => FieldType::ListOfStr,
                    kora_mcp::ParamType::Unsupported => return Option::None,
                };
                Some((param.clone(), mapped))
            })
            .collect();

        Ok(ToolSpec {
            // Namespaced, so two servers offering `search` do not collide.
            name: format!("{server}__{name}"),
            description: if tool.description.is_empty() {
                format!("The {name} tool from {server}.")
            } else {
                tool.description.clone()
            },
            params,
        })
    }

    /// Run a tool the model asked for on an MCP server.
    fn run_mcp_tool(
        &mut self,
        server: &str,
        name: &str,
        arguments_json: &str,
        span: Span,
    ) -> Result<ToolRun, RuntimeError> {
        let arguments: serde_json::Value =
            serde_json::from_str(arguments_json).unwrap_or(serde_json::json!({}));

        let mut servers = self.mcp.lock().unwrap_or_else(|e| e.into_inner());
        // Not being connected is a bug in the runtime rather than a failure
        // of the server, so it still raises.
        let connected = servers
            .get_mut(server)
            .ok_or_else(|| RuntimeError::new(format!("`{server}` is not connected"), span))?;
        match connected.call(name, arguments) {
            Ok(text) => Ok(ToolRun::Result(text)),
            Err(e) => Ok(ToolRun::Unavailable(format!(
                "`{server}.{name}` failed: {e}"
            ))),
        }
    }
}

/// A tool the model may call: declared in this program, or offered by an MCP
/// server.
#[derive(Debug, Clone)]
enum ToolHandle {
    /// A `tool` declared in this program, with the module it belongs to so it
    /// runs against its own file's names.
    Kora {
        def: Rc<FuncDef>,
        home: ModuleId,
    },
    Mcp {
        server: String,
        name: String,
    },
}

impl ToolHandle {
    /// The name the model sees. MCP tools are namespaced by server, so two
    /// servers offering `search` do not collide.
    fn model_name(&self) -> String {
        match self {
            ToolHandle::Kora { def, .. } => def.name.clone(),
            ToolHandle::Mcp { server, name } => format!("{server}__{name}"),
        }
    }
}

impl Interpreter {
    /// Bind a global by hand. Used by tests to stand in for a statement that
    /// would otherwise need external resources.
    pub fn bind_global(&mut self, name: &str, value: Value) {
        self.globals.insert(name.to_string(), value);
    }
}

/// The Python sidecar.
impl Interpreter {
    /// Call `module.function(...)` in the worker, starting it if needed.
    fn call_python(
        &mut self,
        module: &str,
        function: &str,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, RuntimeError> {
        self.require_capability(kora_pkg::Capability::Python, "Python", span)?;
        // Python is a separate process, so it is a sink: a secret released to
        // a model has not been released to Python.
        for arg in &args {
            if self.deep_label(arg).is_classified() && !arg.label().may_reach("python") {
                return Err(RuntimeError::new(
                    "classified data cannot reach Python (no declassify in scope)",
                    span,
                )
                .with_hint(
                    "Python runs in its own process, so it is its own sink: wrap it in `declassify <value> for python:` and allow that sink in kora.toml",
                ));
            }
        }

        let encoded: Vec<serde_json::Value> =
            args.iter().map(|v| value_to_json(v.unlabeled())).collect();

        // A call into Python is nondeterministic as far as the journal is
        // concerned, so a durable run replays it rather than repeating it.
        let site = format!("{}:{}#python", self.program_name, span.line);
        if let Some(recorded) = self.journal_lookup(&site, span)? {
            return Ok(python_result_value(&recorded));
        }

        let mut worker = self.python.lock().unwrap_or_else(|e| e.into_inner());
        if worker.is_none() {
            *worker = Some(
                kora_python::Worker::start(&self.config.python)
                    .map_err(|e| RuntimeError::new(e.message, span))?,
            );
        }
        let outcome = worker
            .as_mut()
            .expect("just started")
            .call(module, function, encoded)
            .map_err(|e| RuntimeError::new(e.message, span))?;
        drop(worker);

        let encoded_result = match &outcome {
            Ok(value) => serde_json::json!({ "ok": true, "result": value }).to_string(),
            Err(e) => serde_json::json!({ "ok": false, "error": e.message }).to_string(),
        };
        self.journal_record(&site, "python", &encoded_result, span)?;

        Ok(python_result_value(&encoded_result))
    }
}

/// Turn a recorded or fresh Python outcome into `Ok(value)` / `Err(reason)`.
///
/// Everything Python returns is `unverified`: it came from outside.
fn python_result_value(encoded: &str) -> Value {
    let parsed: serde_json::Value =
        serde_json::from_str(encoded).unwrap_or(serde_json::Value::Null);
    if parsed
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        let inner = parsed
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        return Value::Variant {
            tag: Rc::new("Ok".to_string()),
            payload: vec![json_to_value(&inner).with_label(Label::UNVERIFIED)],
        };
    }
    let reason = parsed
        .get("error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("the Python call failed");
    Value::Variant {
        tag: Rc::new("Err".to_string()),
        payload: vec![Value::Str(Rc::new(reason.to_string()))],
    }
}
