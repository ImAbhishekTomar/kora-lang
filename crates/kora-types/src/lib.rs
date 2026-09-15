//! kora-types: conservative static checks that an editor can run on every
//! keystroke.
//!
//! The checker rejects mistakes it can prove, including incompatible declared
//! types, bad calls, missing fields, invalid control flow, and direct flows of
//! classified data into a model. Values from dynamic boundaries stay unknown
//! until runtime rather than producing speculative errors.
//!
//! The same index powers hover and go-to-definition, so the editor's answers
//! and its squiggles can never disagree.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use kora_syntax::ast::*;
use kora_syntax::token::Span;

/// How much a problem matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub span: Span,
    pub message: String,
    pub hint: Option<String>,
    pub severity: Severity,
}

impl Diagnostic {
    fn error(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            span,
            message: message.into(),
            hint: None,
            severity: Severity::Error,
        }
    }

    fn with_hint(mut self, hint: impl Into<String>) -> Diagnostic {
        self.hint = Some(hint.into());
        self
    }
}

/// What kind of thing a name refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Agent,
    Tool,
    Type,
    Field,
    Module,
    Variable,
    Test,
}

/// A definition the editor can jump to and describe.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub span: Span,
    /// One-line signature, shown on hover.
    pub detail: String,
    /// Docstring, when the definition has one.
    pub doc: Option<String>,
}

/// Everything the editor needs about one file.
#[derive(Debug, Default)]
pub struct Analysis {
    pub diagnostics: Vec<Diagnostic>,
    /// Top-level definitions, by name.
    pub symbols: HashMap<String, Symbol>,
    /// Every use of a name, so hover can resolve the token under the cursor.
    pub references: Vec<(String, Span)>,
    /// Module aliases in scope: alias -> module name.
    pub modules: HashMap<String, String>,
    /// File imports in scope: alias -> the imported file and what it defines.
    pub file_modules: HashMap<String, FileModule>,
}

/// Another Kora file, brought in with `use "./lib.ko" as lib`.
#[derive(Debug, Clone, Default)]
pub struct FileModule {
    /// Path as written in the import.
    pub written: String,
    /// Path the import resolved to, for hover and go-to-definition.
    pub path: String,
    /// Top-level definitions the file offers, by name.
    pub exports: HashMap<String, Symbol>,
}

impl Analysis {
    /// The symbol whose name appears at this position, if any.
    pub fn symbol_at(&self, line: u32, column: u32) -> Option<&Symbol> {
        self.name_at(line, column)
            .and_then(|name| self.symbols.get(&name))
    }

    /// The name token under the cursor.
    pub fn name_at(&self, line: u32, column: u32) -> Option<String> {
        self.references
            .iter()
            .find(|(name, span)| {
                span.line == line
                    && column >= span.col
                    && column < span.col + name.chars().count() as u32
            })
            .map(|(name, _)| name.clone())
    }
}

/// Drop `.` components so a path reads `lib/tax.ko`, not `./lib/./tax.ko`.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Analyse a parsed program.
///
/// Without a path, file imports are recorded but not followed: there is no
/// directory to resolve them against. Editors and `kora check` call
/// [`analyze_file`] instead, which does follow them.
pub fn analyze(program: &Program) -> Analysis {
    analyze_inner(program, None, &mut Vec::new())
}

/// Analyse a program that lives at `path`, following its file imports.
pub fn analyze_file(program: &Program, path: &Path) -> Analysis {
    let base = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut loading = vec![path.canonicalize().unwrap_or_else(|_| path.to_path_buf())];
    analyze_inner(program, Some(base), &mut loading)
}

fn analyze_inner(program: &Program, base: Option<PathBuf>, loading: &mut Vec<PathBuf>) -> Analysis {
    let mut analysis = Analysis::default();
    let mut checker = Checker {
        analysis: &mut analysis,
        scopes: vec![HashSet::new()],
        type_names: HashSet::new(),
        base,
        loading,
        parallel_depth: 0,
    };
    checker.collect_definitions(&program.items);
    checker.check_block(&program.items);
    StaticChecker::new(&mut analysis).check_program(program);
    analysis
}

struct Checker<'a> {
    analysis: &'a mut Analysis,
    /// Innermost scope last.
    scopes: Vec<HashSet<String>>,
    type_names: HashSet<String>,
    /// Directory file imports resolve against; `None` when the source has no
    /// path (an unsaved buffer, or a string in a test).
    base: Option<PathBuf>,
    /// Files whose analysis is in progress, so a cycle is reported once
    /// instead of recursing.
    loading: &'a mut Vec<PathBuf>,
    /// How many `parallel for` bodies enclose the statement being checked.
    ///
    /// `break <value>` needs somewhere to put the value, and only a fan-out
    /// has one: an ordinary loop binds nothing per iteration. A plain `for`
    /// nested inside a `parallel for` body does not count, because its
    /// `break` leaves that inner loop and never reaches the fan-out.
    parallel_depth: usize,
}

/// Names the runtime provides without a definition.
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
    "input",
    "analyze",
];

const OUTCOME_TAGS: &[&str] = &["Ok", "Err", "Uncertain", "Exhausted", "Failed"];

/// Modules the standard library provides, and what each exports.
const MODULES: &[(&str, &[&str])] = &[
    ("json", &["parse", "stringify", "get"]),
    ("csv", &["parse", "rows", "write"]),
    ("http", &["get", "post"]),
    ("sql", &["query", "execute"]),
    ("env", &["get", "has"]),
    (
        "fs",
        &[
            "read", "write", "append", "exists", "lines", "image", "bytes", "list", "glob",
        ],
    ),
    ("time", &["now", "format", "elapsed"]),
    ("re", &["matches", "find", "find_all", "replace", "split"]),
    ("notes", &["read", "write"]),
    ("pdf", &["text", "pages", "info"]),
    ("yaml", &["parse", "documents", "stringify", "get"]),
    ("xml", &["parse", "find", "find_all", "text", "get"]),
];

impl Checker<'_> {
    /// Pass one: record every top-level definition, so order does not matter
    /// and a function may call one defined below it.
    fn collect_definitions(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match &stmt.kind {
                StmtKind::FuncDef(f) => {
                    let kind = match f.kind {
                        FuncKind::Def => SymbolKind::Function,
                        FuncKind::Agent => SymbolKind::Agent,
                        FuncKind::Tool => SymbolKind::Tool,
                    };
                    let keyword = match f.kind {
                        FuncKind::Def => "def",
                        FuncKind::Agent => "agent",
                        FuncKind::Tool => "tool",
                    };
                    let params: Vec<String> = f
                        .params
                        .iter()
                        .map(|p| match &p.ty {
                            Some(ty) => format!("{}: {}", p.name, ty.display()),
                            None => p.name.clone(),
                        })
                        .collect();
                    let returns = f
                        .return_ty
                        .as_ref()
                        .map(|t| format!(" -> {}", t.display()))
                        .unwrap_or_default();
                    self.define_symbol(Symbol {
                        name: f.name.clone(),
                        kind,
                        span: stmt.span,
                        detail: format!("{keyword} {}({}){returns}", f.name, params.join(", ")),
                        doc: f.doc.clone(),
                    });
                }
                StmtKind::TypeDef { name, fields } => {
                    let lines: Vec<String> = fields
                        .iter()
                        .map(|f| {
                            let marker = if f.classified { "classified " } else { "" };
                            let mut line = format!("    {marker}{}: {}", f.name, f.ty.display());
                            if let Some(description) = &f.metadata.description {
                                line.push_str(&format!(" @description({description:?})"));
                            }
                            if let Some(pattern) = &f.metadata.pattern {
                                line.push_str(&format!(" @pattern({pattern:?})"));
                            }
                            line
                        })
                        .collect();
                    self.type_names.insert(name.clone());
                    self.define_symbol(Symbol {
                        name: name.clone(),
                        kind: SymbolKind::Type,
                        span: stmt.span,
                        detail: format!("type {name}:\n{}", lines.join("\n")),
                        doc: None,
                    });
                }
                StmtKind::UseHelper { alias } => {
                    self.define_symbol(Symbol {
                        name: alias.clone(),
                        kind: SymbolKind::Module,
                        span: stmt.span,
                        detail: "use helper".to_string(),
                        doc: Some(
                            "This package's helper: a separate program it carries work out to."
                                .to_string(),
                        ),
                    });
                }
                StmtKind::UsePython { module, alias } => {
                    self.define_symbol(Symbol {
                        name: alias.clone(),
                        kind: SymbolKind::Module,
                        span: stmt.span,
                        detail: format!("use python {module}"),
                        doc: Some(
                            "A Python module, reached through the sidecar worker.".to_string(),
                        ),
                    });
                }
                StmtKind::UsePkg { package, alias } => {
                    // A package is reached by name rather than by path, but
                    // once resolved it is a file like any other — so the
                    // editor can offer its exports and jump into them.
                    let module = self.load_package_module(package, stmt.span);
                    for (name, symbol) in &module.exports {
                        if symbol.kind == SymbolKind::Type {
                            self.type_names.insert(name.clone());
                        }
                    }
                    let detail = if module.path.is_empty() {
                        format!("use pkg {package}")
                    } else {
                        format!("use pkg {package}  ({})", module.path)
                    };
                    self.analysis.file_modules.insert(alias.clone(), module);
                    self.define_symbol(Symbol {
                        name: alias.clone(),
                        kind: SymbolKind::Module,
                        span: stmt.span,
                        detail,
                        doc: Some(
                            "A package dependency, resolved against this package's `[dependencies]`."
                                .to_string(),
                        ),
                    });
                }
                StmtKind::UseMcp { server, alias } => {
                    self.define_symbol(Symbol {
                        name: alias.clone(),
                        kind: SymbolKind::Module,
                        span: stmt.span,
                        detail: format!("use mcp {server}"),
                        doc: Some(
                            "An MCP server. `.tools` offers every tool it exposes.".to_string(),
                        ),
                    });
                }
                StmtKind::UseFile { path, alias } => {
                    let module = self.load_file_module(path, stmt.span);
                    // Types are shared across files at runtime, so a type
                    // declared in an imported file may be named here.
                    for (name, symbol) in &module.exports {
                        if symbol.kind == SymbolKind::Type {
                            self.type_names.insert(name.clone());
                        }
                    }
                    let detail = if module.path.is_empty() {
                        format!("use {path:?}")
                    } else {
                        format!("use {path:?}  ({})", module.path)
                    };
                    self.analysis.file_modules.insert(alias.clone(), module);
                    self.define_symbol(Symbol {
                        name: alias.clone(),
                        kind: SymbolKind::Module,
                        span: stmt.span,
                        detail,
                        doc: None,
                    });
                }
                StmtKind::Use { module, alias } => {
                    self.analysis.modules.insert(alias.clone(), module.clone());
                    self.define_symbol(Symbol {
                        name: alias.clone(),
                        kind: SymbolKind::Module,
                        span: stmt.span,
                        detail: format!("use {module}"),
                        doc: None,
                    });
                }
                StmtKind::Test { name, .. } => {
                    self.define_symbol(Symbol {
                        name: format!("test {name}"),
                        kind: SymbolKind::Test,
                        span: stmt.span,
                        detail: format!("test \"{name}\""),
                        doc: None,
                    });
                }
                _ => {}
            }
        }
    }

    /// Read an imported file and collect what it defines.
    ///
    /// Diagnostics from inside the imported file are its own business: they
    /// belong to that file's spans, and the editor reports them when that file
    /// is open. Only the failure to load it is reported here.
    /// Resolve `use pkg <name>` to the package's entry file, then read it the
    /// way an imported file is read.
    ///
    /// Silent when the package cannot be found: whether a dependency is
    /// declared, fetched, and verified is the resolver's answer, and the
    /// editor reporting it as an unknown name would squiggle code that
    /// `kora install` is about to make correct.
    fn load_package_module(&mut self, name: &str, span: Span) -> FileModule {
        let mut module = FileModule {
            written: format!("pkg {name}"),
            ..FileModule::default()
        };
        // Resolved from the file being edited, so the editor's answer is the
        // one the runtime would reach — including which commit a git
        // dependency is pinned to.
        //
        // `loading[0]` is the root program even while a nested file is being
        // analysed. A `use pkg` written *inside* a dependency therefore
        // resolves against the root's manifest rather than its own, and finds
        // nothing when the two disagree. That fails silently, which is the
        // right direction: no diagnostic and no completion, rather than
        // confident wrong names.
        let Some(entry) = self.loading.first().cloned() else {
            return module;
        };
        let resolution = kora_pkg::resolve(&entry);
        let Some(package) = resolution.dep_of(kora_pkg::ROOT, name) else {
            return module;
        };
        let entry = package.entry.clone();
        let Ok(source) = std::fs::read_to_string(&entry) else {
            return module;
        };
        let Ok(program) = kora_syntax::parse(&source) else {
            return module;
        };
        module.path = entry.display().to_string();

        let key = entry.canonicalize().unwrap_or_else(|_| entry.clone());
        if self.loading.contains(&key) {
            return module;
        }
        let dir = key
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        self.loading.push(key);
        let inner = analyze_inner(&program, Some(dir), self.loading);
        self.loading.pop();

        module.exports = inner.symbols;
        module.exports.retain(|_, s| s.kind != SymbolKind::Test);
        for stmt in &program.items {
            let StmtKind::Assign { target, ty, .. } = &stmt.kind else {
                continue;
            };
            let ExprKind::Name(name) = &target.kind else {
                continue;
            };
            module.exports.entry(name.clone()).or_insert(Symbol {
                name: name.clone(),
                kind: SymbolKind::Variable,
                span: stmt.span,
                detail: match ty {
                    Some(t) => format!("{name}: {}", t.display()),
                    None => name.clone(),
                },
                doc: None,
            });
        }
        let _ = span;
        module
    }

    fn load_file_module(&mut self, written: &str, span: Span) -> FileModule {
        let mut module = FileModule {
            written: written.to_string(),
            ..FileModule::default()
        };

        if !written.ends_with(".ko") {
            self.analysis.diagnostics.push(
                Diagnostic::error(span, format!("`{written}` is not a Kora file"))
                    .with_hint("an imported path must end in `.ko`"),
            );
            return module;
        }

        // No path means an unsaved buffer: record the alias and stop, rather
        // than guessing a directory and reporting an import that is fine.
        let Some(base) = self.base.clone() else {
            return module;
        };

        let candidate = normalize(&base.join(written));
        let Ok(source) = std::fs::read_to_string(&candidate) else {
            self.analysis.diagnostics.push(
                Diagnostic::error(span, format!("cannot read `{written}`"))
                    .with_hint(format!("looked for {}", candidate.display())),
            );
            return module;
        };
        module.path = candidate.display().to_string();

        let key = candidate
            .canonicalize()
            .unwrap_or_else(|_| candidate.clone());
        if self.loading.contains(&key) {
            self.analysis.diagnostics.push(
                Diagnostic::error(span, format!("import cycle: `{written}` imports itself"))
                    .with_hint("move the shared code into a third file both can import"),
            );
            return module;
        }

        let Ok(program) = kora_syntax::parse(&source) else {
            self.analysis.diagnostics.push(
                Diagnostic::error(span, format!("`{written}` has a syntax error"))
                    .with_hint(format!("run `kora check {}`", candidate.display())),
            );
            return module;
        };

        let dir = key
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        self.loading.push(key);
        let inner = analyze_inner(&program, Some(dir), self.loading);
        self.loading.pop();

        module.exports = inner.symbols;
        // A `test` block is not a name another file can reach.
        module.exports.retain(|_, s| s.kind != SymbolKind::Test);
        // Top-level assignments are exported too — the runtime binds them in
        // the module's namespace — but they are not definitions the checker
        // collects, so pick them up here.
        for stmt in &program.items {
            let StmtKind::Assign { target, ty, .. } = &stmt.kind else {
                continue;
            };
            let ExprKind::Name(name) = &target.kind else {
                continue;
            };
            let detail = match ty {
                Some(t) => format!("{name}: {}", t.display()),
                None => name.clone(),
            };
            module.exports.entry(name.clone()).or_insert(Symbol {
                name: name.clone(),
                kind: SymbolKind::Variable,
                span: stmt.span,
                detail,
                doc: None,
            });
        }
        module
    }

    fn define_symbol(&mut self, symbol: Symbol) {
        self.scopes[0].insert(symbol.name.clone());
        self.analysis.symbols.insert(symbol.name.clone(), symbol);
    }

    fn declare(&mut self, name: &str) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string());
        }
    }

    fn is_known(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(name))
            || BUILTINS.contains(&name)
            || OUTCOME_TAGS.contains(&name)
            || self.type_names.contains(name)
    }

    fn check_block(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.check_stmt(stmt);
        }
    }

    /// Run a nested block.
    ///
    /// Deliberately *not* a new scope: only functions scope in Kora, exactly
    /// as in Python. A variable assigned inside an `if`, a loop, or a
    /// `declassify` block is visible afterwards, and the checker has to agree
    /// with the interpreter or it reports names that plainly work.
    fn nested(&mut self, body: &[Stmt]) {
        self.check_block(body);
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Assign {
                target,
                ty,
                value,
                on_token,
                on_tool_call,
                ..
            } => {
                self.check_expr(value);
                if let Some(ty) = ty {
                    self.check_type(ty, stmt.span);
                }
                match &target.kind {
                    ExprKind::Name(name) => self.declare(name),
                    other => self.check_expr(&Expr {
                        kind: other.clone(),
                        span: target.span,
                    }),
                }
                // The handler's variable outlives the block, like a loop
                // variable, since Kora scopes only at function boundaries.
                if let Some(handler) = on_token {
                    self.declare(&handler.var);
                    self.nested(&handler.body);
                }
                if let Some(handler) = on_tool_call {
                    self.declare(&handler.name_var);
                    self.declare(&handler.args_var);
                    self.nested(&handler.body);
                }
            }
            StmtKind::AugAssign { target, value, .. } => {
                self.check_expr(target);
                self.check_expr(value);
            }
            StmtKind::Expr(e) => self.check_expr(e),
            StmtKind::If {
                branches,
                else_body,
            } => {
                for (cond, body) in branches {
                    self.check_expr(cond);
                    self.nested(body);
                }
                if let Some(body) = else_body {
                    self.nested(body);
                }
            }
            StmtKind::While { cond, body } => {
                self.check_expr(cond);
                let outer = std::mem::take(&mut self.parallel_depth);
                self.nested(body);
                self.parallel_depth = outer;
            }
            StmtKind::For { var, iter, body } => {
                self.check_expr(iter);
                // The loop variable outlives the loop, as it does in Python.
                self.declare(var);
                // A `break` in here leaves this loop, not any fan-out around
                // it, so it has nowhere to put a value.
                let outer = std::mem::take(&mut self.parallel_depth);
                self.nested(body);
                self.parallel_depth = outer;
            }
            StmtKind::ParallelFor {
                var, iter, body, ..
            } => {
                self.check_expr(iter);
                self.declare(var);
                self.parallel_depth += 1;
                self.nested(body);
                self.parallel_depth -= 1;
                if let StmtKind::ParallelFor {
                    collect_into: Some(name),
                    ..
                } = &stmt.kind
                {
                    self.declare(name);
                }
            }
            StmtKind::FuncDef(f) => {
                self.scopes.push(HashSet::new());
                // A function defined inside a `parallel for` body is still a
                // function: its `break` never reaches the fan-out, because
                // the call returns before the loop sees anything.
                let outer_parallel = std::mem::take(&mut self.parallel_depth);
                for p in &f.params {
                    self.declare(&p.name);
                    if let Some(ty) = &p.ty {
                        self.check_type(ty, p.span);
                    }
                }
                if let Some(ty) = &f.return_ty {
                    self.check_type(ty, stmt.span);
                }
                // Definitions inside a body are visible to the rest of it.
                self.collect_local_definitions(&f.body);
                self.check_block(&f.body);
                self.parallel_depth = outer_parallel;
                self.scopes.pop();
            }
            StmtKind::TypeDef { fields, .. } => {
                for field in fields {
                    self.check_type(&field.ty, field.span);
                    self.check_field_metadata(field);
                }
            }
            StmtKind::Return(Some(e)) => self.check_expr(e),
            StmtKind::Match { subject, arms } => {
                self.check_expr(subject);
                for arm in arms {
                    self.declare_pattern_binders(&arm.pattern);
                    // Declared above, so a guard may read what the pattern
                    // bound -- that is the whole point of `case Ok(j) if j.x`.
                    if let Some(guard) = &arm.guard {
                        self.check_expr(guard);
                        self.check_guard_is_pure(guard);
                    }
                    self.nested(&arm.body);
                }
            }
            StmtKind::BindOrElse {
                name,
                ty,
                value,
                reason,
                status,
                else_body,
                ..
            } => {
                self.check_expr(value);
                if let Some(ty) = ty {
                    self.check_type(ty, stmt.span);
                }
                // The reason is only meaningful inside the else block, but
                // Kora scopes by function like Python, so it is declared the
                // same way a `case Uncertain(why)` binder is.
                if let Some(reason) = reason {
                    self.declare(reason);
                }
                if let Some(status) = status {
                    self.declare(status);
                }
                self.nested(else_body);
                if !diverges(else_body) {
                    self.analysis.diagnostics.push(
                        Diagnostic::error(
                            stmt.span,
                            format!(
                                "the `else` block of `{name} = ... else:` must not fall through"
                            ),
                        )
                        .with_hint(format!(
                            "end it with `return`, `break`, or `continue` -- otherwise `{name}` would be unbound below this statement"
                        )),
                    );
                }
                // Declared after the else block so the block cannot read the
                // name it exists to avoid binding.
                self.declare(name);
            }
            StmtKind::Declassify {
                value,
                binding,
                body,
                ..
            } => {
                self.check_expr(value);
                // The binding is scoped at runtime, but anything assigned
                // inside the block is not, so only the binding is temporary.
                let existed = self
                    .scopes
                    .last()
                    .is_some_and(|scope| scope.contains(binding));
                self.declare(binding);
                self.nested(body);
                if !existed {
                    if let Some(scope) = self.scopes.last_mut() {
                        scope.remove(binding);
                    }
                }
            }
            StmtKind::WithBudget { body, .. } | StmtKind::WithContext { body, .. } => {
                self.nested(body)
            }
            StmtKind::WithMock { result, body, .. } => {
                self.check_expr(result);
                self.nested(body);
            }
            StmtKind::Test { body, .. } => self.nested(body),
            StmtKind::Assert { condition, message } => {
                self.check_expr(condition);
                if let Some(m) = message {
                    self.check_expr(m);
                }
            }
            StmtKind::UsePython { alias, .. } => {
                // Which functions a Python module has is a runtime question,
                // so the checker records the alias and stops there.
                self.declare(alias);
            }
            StmtKind::UseHelper { alias } => {
                // What a helper answers to is the helper's business, and it
                // is a different binary on every platform. The alias is
                // recorded; whether a package declares a helper at all is
                // checked where the manifest is read.
                self.declare(alias);
            }
            StmtKind::UsePkg { alias, .. } => {
                // Whether the package resolves is the resolver's answer, not
                // the checker's: the alias is recorded so names reached
                // through it are not reported as unknown.
                self.declare(alias);
            }
            StmtKind::UseMcp { alias, .. } => {
                // Which servers exist is a runtime question: the checker
                // cannot know without launching them, so it records the alias
                // rather than reporting a name it cannot verify.
                self.declare(alias);
            }
            StmtKind::UseFile { alias, .. } => {
                // Resolution and its diagnostics happened while collecting
                // definitions, so the file is read once per analysis.
                self.declare(alias);
            }
            StmtKind::Use { module, alias } => {
                if !MODULES.iter().any(|(name, _)| name == module) {
                    let known: Vec<&str> = MODULES.iter().map(|(n, _)| *n).collect();
                    self.analysis.diagnostics.push(
                        Diagnostic::error(
                            stmt.span,
                            format!("there is no module named `{module}`"),
                        )
                        .with_hint(format!("available modules: {}", known.join(", "))),
                    );
                }
                self.declare(alias);
            }
            StmtKind::Break(Some(value)) => {
                self.check_expr(value);
                if self.parallel_depth == 0 {
                    self.analysis.diagnostics.push(
                        Diagnostic::error(
                            stmt.span,
                            "`break` can only carry a value inside a `parallel for`",
                        )
                        .with_hint(
                            "an ordinary loop has nowhere to put it; write `break` on its own, or assign the value before it",
                        ),
                    );
                }
            }
            StmtKind::Return(None)
            | StmtKind::Break(None)
            | StmtKind::Continue
            | StmtKind::Pass => {}
        }
    }

    fn declare_pattern_binders(&mut self, pattern: &Pattern) {
        match pattern {
            Pattern::Bind(name) => self.declare(name),
            Pattern::Ctor(_, binders) => {
                for binder in binders {
                    self.declare(binder);
                }
            }
            // The parser proves all alternatives bind the same names, so one
            // branch is sufficient for name resolution.
            Pattern::Or(alternatives) => {
                if let Some(first) = alternatives.first() {
                    self.declare_pattern_binders(first);
                }
            }
            Pattern::Wildcard
            | Pattern::LiteralInt(_)
            | Pattern::LiteralStr(_)
            | Pattern::LiteralBool(_) => {}
        }
    }

    /// A guard runs speculatively: arms are tried in order, so a guard may be
    /// evaluated for an arm that never runs. A guard that called a model would
    /// therefore make the token cost of a `match` depend on arm order, which
    /// is exactly the kind of invisible spending budgets exist to prevent.
    ///
    /// The general case is not decidable here -- any `def` may call `analyze`
    /// somewhere below it -- so the runtime refuses model calls while a guard
    /// is on the stack. This check exists to catch the obvious spelling early,
    /// with a better message than the runtime can give.
    fn check_guard_is_pure(&mut self, guard: &Expr) {
        fn find_analyze(e: &Expr) -> Option<Span> {
            match &e.kind {
                ExprKind::Call { callee, args, .. } => {
                    if matches!(&callee.kind, ExprKind::Name(n) if n == "analyze") {
                        return Some(e.span);
                    }
                    find_analyze(callee).or_else(|| args.iter().find_map(find_analyze))
                }
                ExprKind::Binary { left, right, .. } => {
                    find_analyze(left).or_else(|| find_analyze(right))
                }
                ExprKind::Unary { operand, .. } => find_analyze(operand),
                ExprKind::Attr { object, .. } => find_analyze(object),
                ExprKind::Index { object, index } => {
                    find_analyze(object).or_else(|| find_analyze(index))
                }
                _ => None,
            }
        }
        if let Some(span) = find_analyze(guard) {
            self.analysis.diagnostics.push(
                Diagnostic::error(span, "a `case` guard cannot call a model")
                    .with_hint(
                        "guards are tried against arms that may not run; call `analyze` before the `match` and guard on its result",
                    ),
            );
        }
    }

    /// Functions defined inside a body are hoisted within that body.
    fn collect_local_definitions(&mut self, body: &[Stmt]) {
        for stmt in body {
            if let StmtKind::FuncDef(f) = &stmt.kind {
                self.declare(&f.name);
            }
        }
    }

    fn check_type(&mut self, ty: &TypeExpr, span: Span) {
        let name = match ty {
            TypeExpr::Name(n) => n,
            TypeExpr::Generic(n, args) => {
                for arg in args {
                    self.check_type(arg, span);
                }
                n
            }
        };
        // `image` and `bytes` are values a program can hold and hand on, so
        // a signature can say so. Neither has fields to declare, which is why
        // they sit here rather than in a `type` block.
        const PRIMITIVES: &[&str] = &[
            "str", "int", "float", "bool", "list", "dict", "None", "bytes", "image",
        ];
        if !PRIMITIVES.contains(&name.as_str()) && !self.type_names.contains(name) {
            self.analysis.diagnostics.push(
                Diagnostic::error(span, format!("`{name}` is not a declared type"))
                    .with_hint("declare it with `type Name:` and typed fields below"),
            );
        }
    }

    fn check_field_metadata(&mut self, field: &FieldDef) {
        let Some(pattern) = &field.metadata.pattern else {
            return;
        };
        if field.ty.display() != "str" {
            self.analysis.diagnostics.push(
                Diagnostic::error(
                    field.span,
                    format!("field `{}` uses `pattern` but is not a `str`", field.name),
                )
                .with_hint("`pattern` is only valid on `str` fields"),
            );
            return;
        }
        if let Err(error) = regex::Regex::new(pattern) {
            self.analysis.diagnostics.push(Diagnostic::error(
                field.span,
                format!(
                    "field `{}` has an invalid pattern `{pattern}`: {error}",
                    field.name
                ),
            ));
        }
    }

    fn check_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Name(name) => {
                self.analysis.references.push((name.clone(), expr.span));
                if !self.is_known(name) {
                    let mut diagnostic =
                        Diagnostic::error(expr.span, format!("`{name}` is not defined"));
                    if let Some(close) = self.closest(name) {
                        diagnostic = diagnostic.with_hint(format!("did you mean `{close}`?"));
                    }
                    self.analysis.diagnostics.push(diagnostic);
                }
            }
            ExprKind::Attr { object, name } => {
                // `lib.helper`: check the imported file actually defines it.
                if let ExprKind::Name(alias) = &object.kind {
                    if let Some(module) = self.analysis.file_modules.get(alias).cloned() {
                        self.analysis.references.push((alias.clone(), object.span));
                        // An unreadable import already reported itself; do not
                        // report every use of it as well.
                        if !module.exports.is_empty() && !module.exports.contains_key(name) {
                            let mut available: Vec<&str> =
                                module.exports.keys().map(String::as_str).collect();
                            available.sort();
                            self.analysis.diagnostics.push(
                                Diagnostic::error(
                                    expr.span,
                                    format!("`{alias}` has no name `{name}`"),
                                )
                                .with_hint(format!("{alias} provides: {}", available.join(", "))),
                            );
                        }
                        return;
                    }
                }
                // `json.parse(...)`: check the function exists on the module.
                if let ExprKind::Name(alias) = &object.kind {
                    if let Some(module) = self.analysis.modules.get(alias).cloned() {
                        self.analysis.references.push((alias.clone(), object.span));
                        if let Some((_, functions)) = MODULES.iter().find(|(m, _)| *m == module) {
                            if !functions.contains(&name.as_str()) {
                                self.analysis.diagnostics.push(
                                    Diagnostic::error(
                                        expr.span,
                                        format!("`{module}` has no function `{name}`"),
                                    )
                                    .with_hint(format!(
                                        "{module} provides: {}",
                                        functions.join(", ")
                                    )),
                                );
                            }
                        }
                        return;
                    }
                }
                self.check_expr(object);
            }
            ExprKind::Call {
                callee,
                args,
                kwargs,
            } => {
                // Kora has no methods: `xs.append(v)` parses as a call whose
                // callee is `xs.append`, and only a module/package/mcp/python
                // alias may be called this way (`json.parse(...)`). Anything
                // else is a Python habit that would otherwise only fail at
                // runtime, one full `check`-fix-`check` cycle later.
                if let ExprKind::Attr { object, name } = &callee.kind {
                    if let ExprKind::Name(alias) = &object.kind {
                        let is_module = self
                            .analysis
                            .symbols
                            .get(alias)
                            .is_some_and(|s| s.kind == SymbolKind::Module);
                        if !is_module && self.is_known(alias) {
                            let rest = if args.is_empty() {
                                String::new()
                            } else {
                                format!(", {}", vec!["v"; args.len()].join(", "))
                            };
                            self.analysis.diagnostics.push(
                                Diagnostic::error(
                                    callee.span,
                                    format!("method calls are not supported here: `.{name}(...)`"),
                                )
                                .with_hint(format!(
                                    "Kora has no methods -- write `{name}({alias}{rest})` instead"
                                )),
                            );
                        }
                    }
                }
                if !kwargs.is_empty()
                    && !matches!(&callee.kind, ExprKind::Name(n) if n == "analyze")
                {
                    let (name, arg) = &kwargs[0];
                    self.analysis.diagnostics.push(
                        Diagnostic::error(
                            arg.span,
                            format!("`{name}` is not a keyword argument here"),
                        )
                        .with_hint("only analyze() takes keyword arguments today"),
                    );
                }
                self.check_expr(callee);
                for a in args {
                    self.check_expr(a);
                }
                for (_, v) in kwargs {
                    self.check_expr(v);
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.check_expr(left);
                self.check_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.check_expr(operand),
            ExprKind::Index { object, index } => {
                self.check_expr(object);
                self.check_expr(index);
            }
            ExprKind::Slice {
                object,
                start,
                stop,
            } => {
                self.check_expr(object);
                if let Some(e) = start {
                    self.check_expr(e);
                }
                if let Some(e) = stop {
                    self.check_expr(e);
                }
            }
            ExprKind::List(items) => {
                for item in items {
                    self.check_expr(item);
                }
            }
            ExprKind::Dict(pairs) => {
                for (k, v) in pairs {
                    self.check_expr(k);
                    self.check_expr(v);
                }
            }
            ExprKind::FString { exprs, .. } => {
                for e in exprs {
                    self.check_expr(e);
                }
            }
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::Bool(_)
            | ExprKind::None => {}
        }
    }

    /// A defined name within one edit of this one, for a "did you mean" hint.
    fn closest(&self, name: &str) -> Option<String> {
        self.scopes
            .iter()
            .flatten()
            .chain(self.type_names.iter())
            .find(|candidate| close_enough(candidate, name))
            .cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StaticTy {
    Unknown,
    None,
    Bool,
    Int,
    Float,
    Str,
    Bytes,
    Image,
    List(Box<StaticTy>),
    Dict,
    Named(String),
    Outcome(Box<StaticTy>),
}

impl StaticTy {
    fn from_annotation(ty: &TypeExpr) -> StaticTy {
        match ty {
            TypeExpr::Name(name) => match name.as_str() {
                "None" => StaticTy::None,
                "bool" => StaticTy::Bool,
                "int" => StaticTy::Int,
                "float" => StaticTy::Float,
                "str" => StaticTy::Str,
                "bytes" => StaticTy::Bytes,
                "image" => StaticTy::Image,
                "list" => StaticTy::List(Box::new(StaticTy::Unknown)),
                "dict" => StaticTy::Dict,
                other => StaticTy::Named(other.to_string()),
            },
            TypeExpr::Generic(name, args) if name == "list" => StaticTy::List(Box::new(
                args.first()
                    .map(StaticTy::from_annotation)
                    .unwrap_or(StaticTy::Unknown),
            )),
            TypeExpr::Generic(name, _) if name == "dict" => StaticTy::Dict,
            TypeExpr::Generic(name, _) => StaticTy::Named(name.clone()),
        }
    }

    fn display(&self) -> String {
        match self {
            StaticTy::Unknown => "unknown".to_string(),
            StaticTy::None => "None".to_string(),
            StaticTy::Bool => "bool".to_string(),
            StaticTy::Int => "int".to_string(),
            StaticTy::Float => "float".to_string(),
            StaticTy::Str => "str".to_string(),
            StaticTy::Bytes => "bytes".to_string(),
            StaticTy::Image => "image".to_string(),
            StaticTy::List(inner) => format!("list[{}]", inner.display()),
            StaticTy::Dict => "dict".to_string(),
            StaticTy::Named(name) => name.clone(),
            StaticTy::Outcome(inner) => format!("outcome[{}]", inner.display()),
        }
    }

    fn compatible_with(&self, expected: &StaticTy) -> bool {
        matches!(self, StaticTy::Unknown)
            || matches!(expected, StaticTy::Unknown)
            || self == expected
            || matches!((self, expected), (StaticTy::Int, StaticTy::Float))
            || matches!(
                (self, expected),
                (StaticTy::List(actual), StaticTy::List(wanted))
                    if actual.compatible_with(wanted)
            )
    }
}

#[derive(Debug, Clone)]
struct ValueFacts {
    ty: StaticTy,
    classified: bool,
    released: bool,
}

impl ValueFacts {
    fn new(ty: StaticTy) -> ValueFacts {
        ValueFacts {
            ty,
            classified: false,
            released: false,
        }
    }

    fn unknown() -> ValueFacts {
        ValueFacts::new(StaticTy::Unknown)
    }
}

#[derive(Debug, Clone)]
struct FieldFacts {
    ty: StaticTy,
    classified: bool,
}

#[derive(Debug, Clone)]
struct FunctionFacts {
    params: Vec<StaticTy>,
    /// Parameter names and spans, kept so a diagnostic about one parameter can
    /// name it and point at it.
    param_sites: Vec<(String, Span)>,
    return_ty: StaticTy,
}

/// The type pass is intentionally conservative. It follows only facts stated
/// in this file. Imported modules, sidecars, MCP servers, and values returned
/// by untyped functions remain unknown and are validated by the runtime.
struct StaticChecker<'a> {
    analysis: &'a mut Analysis,
    types: HashMap<String, Vec<(String, FieldFacts)>>,
    functions: HashMap<String, FunctionFacts>,
    scopes: Vec<HashMap<String, ValueFacts>>,
    function_depth: usize,
    expected_return: Option<StaticTy>,
    loop_depth: usize,
    parallel_depth: usize,
}

impl<'a> StaticChecker<'a> {
    fn new(analysis: &'a mut Analysis) -> StaticChecker<'a> {
        StaticChecker {
            analysis,
            types: HashMap::new(),
            functions: HashMap::new(),
            scopes: vec![HashMap::new()],
            function_depth: 0,
            expected_return: None,
            loop_depth: 0,
            parallel_depth: 0,
        }
    }

    fn check_program(&mut self, program: &Program) {
        self.collect_facts(&program.items);
        self.check_stmts(&program.items);
    }

    /// Parameter types a model can actually be handed.
    ///
    /// A tool's signature becomes a JSON schema in the request, and the
    /// runtime refuses anything outside this set when it builds one. Checking
    /// it here means a program whose tool cannot be described to a provider is
    /// rejected before the run starts, rather than partway through — after
    /// earlier calls in the same program have already been paid for.
    ///
    /// Conservative on purpose: an unannotated parameter is `Unknown` and is
    /// left to the runtime, and a tool reached through anything other than a
    /// plain name (an MCP server's `.tools`, a computed list) is not resolved
    /// here at all.
    fn check_model_tool_params(&mut self, tools: &Expr) {
        fn model_representable(ty: &StaticTy) -> bool {
            match ty {
                StaticTy::Str | StaticTy::Int | StaticTy::Float | StaticTy::Bool => true,
                StaticTy::List(inner) => matches!(**inner, StaticTy::Str),
                // Not stated in this file, so not this pass's to refuse.
                StaticTy::Unknown => true,
                _ => false,
            }
        }

        let ExprKind::List(items) = &tools.kind else {
            return;
        };
        for item in items {
            let ExprKind::Name(tool_name) = &item.kind else {
                continue;
            };
            let Some(facts) = self.functions.get(tool_name).cloned() else {
                continue;
            };
            for (ty, (param_name, param_span)) in facts.params.iter().zip(facts.param_sites.iter())
            {
                if model_representable(ty) {
                    continue;
                }
                self.analysis.diagnostics.push(
                    Diagnostic::error(
                        *param_span,
                        format!(
                            "`{tool_name}` takes `{param_name}: {}`, which a model cannot be given",
                            ty.display()
                        ),
                    )
                    .with_hint(
                        "a tool parameter must be `str`, `int`, `float`, `bool`, or `list[str]`; pass structured data as a JSON string and decode it inside the tool",
                    ),
                );
            }
        }
    }

    fn collect_facts(&mut self, stmts: &[Stmt]) {
        let mut definitions: HashMap<String, Span> = HashMap::new();
        for stmt in stmts {
            match &stmt.kind {
                StmtKind::FuncDef(function) => {
                    self.report_duplicate(&mut definitions, &function.name, stmt.span);
                    self.functions.insert(
                        function.name.clone(),
                        FunctionFacts {
                            params: function
                                .params
                                .iter()
                                .map(|p| {
                                    p.ty.as_ref()
                                        .map(StaticTy::from_annotation)
                                        .unwrap_or(StaticTy::Unknown)
                                })
                                .collect(),
                            param_sites: function
                                .params
                                .iter()
                                .map(|p| (p.name.clone(), p.span))
                                .collect(),
                            return_ty: function
                                .return_ty
                                .as_ref()
                                .map(StaticTy::from_annotation)
                                .unwrap_or(StaticTy::Unknown),
                        },
                    );
                }
                StmtKind::TypeDef { name, fields } => {
                    self.report_duplicate(&mut definitions, name, stmt.span);
                    let mut seen = HashSet::new();
                    let mut facts = Vec::new();
                    for field in fields {
                        if !seen.insert(field.name.clone()) {
                            self.analysis.diagnostics.push(
                                Diagnostic::error(
                                    field.span,
                                    format!("field `{}` is declared more than once", field.name),
                                )
                                .with_hint("keep one declaration for each field"),
                            );
                        }
                        facts.push((
                            field.name.clone(),
                            FieldFacts {
                                ty: StaticTy::from_annotation(&field.ty),
                                classified: field.classified,
                            },
                        ));
                    }
                    self.types.insert(name.clone(), facts);
                }
                _ => {}
            }
        }
    }

    fn report_duplicate(
        &mut self,
        definitions: &mut HashMap<String, Span>,
        name: &str,
        span: Span,
    ) {
        if definitions.insert(name.to_string(), span).is_some() {
            self.analysis.diagnostics.push(
                Diagnostic::error(span, format!("`{name}` is defined more than once"))
                    .with_hint("rename or remove one of the definitions"),
            );
        }
    }

    fn check_stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.check_static_stmt(stmt);
        }
    }

    fn check_static_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Assign {
                target,
                ty,
                value,
                classified,
                on_token,
                on_tool_call,
                ..
            } => {
                let mut actual = self.infer(value);
                let declared = ty.as_ref().map(StaticTy::from_annotation);
                let is_analyze = matches!(
                    &value.kind,
                    ExprKind::Call { callee, .. }
                        if matches!(&callee.kind, ExprKind::Name(name) if name == "analyze")
                );
                if is_analyze && declared.is_none() {
                    self.analysis.diagnostics.push(
                        Diagnostic::error(
                            stmt.span,
                            "an `analyze` result needs a declared payload type",
                        )
                        .with_hint("write `result: YourType = analyze(data, prompt)`"),
                    );
                }
                if let Some(expected) = &declared {
                    if !is_analyze {
                        self.require_type(&actual.ty, expected, value.span, "assignment");
                        actual.ty = expected.clone();
                    } else {
                        actual.ty = StaticTy::Outcome(Box::new(expected.clone()));
                    }
                }
                actual.classified |= *classified;
                if let ExprKind::Name(name) = &target.kind {
                    self.bind(name, actual);
                } else {
                    self.infer(target);
                }
                if let Some(handler) = on_token {
                    self.bind(&handler.var, ValueFacts::new(StaticTy::Str));
                    self.check_stmts(&handler.body);
                }
                if let Some(handler) = on_tool_call {
                    self.bind(&handler.name_var, ValueFacts::new(StaticTy::Str));
                    self.bind(&handler.args_var, ValueFacts::new(StaticTy::Dict));
                    self.check_stmts(&handler.body);
                }
            }
            StmtKind::AugAssign { target, value, .. } => {
                let target_ty = self.infer(target);
                let value_ty = self.infer(value);
                self.require_type(
                    &value_ty.ty,
                    &target_ty.ty,
                    value.span,
                    "augmented assignment",
                );
            }
            StmtKind::Expr(expr) => {
                self.infer(expr);
            }
            StmtKind::If {
                branches,
                else_body,
            } => {
                for (condition, body) in branches {
                    self.infer(condition);
                    self.check_stmts(body);
                }
                if let Some(body) = else_body {
                    self.check_stmts(body);
                }
            }
            StmtKind::While { cond, body } => {
                self.infer(cond);
                self.loop_depth += 1;
                self.check_stmts(body);
                self.loop_depth -= 1;
            }
            StmtKind::For { var, iter, body } => {
                let iter = self.infer(iter);
                let item = self.iter_item(iter);
                self.bind(var, item);
                self.loop_depth += 1;
                self.check_stmts(body);
                self.loop_depth -= 1;
            }
            StmtKind::ParallelFor {
                var,
                iter,
                body,
                collect_into,
                first,
            } => {
                let iter = self.infer(iter);
                let item = self.iter_item(iter);
                self.bind(var, item);
                self.loop_depth += 1;
                self.parallel_depth += 1;
                self.check_stmts(body);
                self.parallel_depth -= 1;
                self.loop_depth -= 1;
                if let Some(name) = collect_into {
                    let ty = if *first {
                        // A race yields one branch's value (or None), never
                        // the collection produced by an ordinary fan-out.
                        // Branch return inference is deliberately conservative
                        // for now, so bind it as unknown rather than lying
                        // that it is a list.
                        StaticTy::Unknown
                    } else {
                        StaticTy::List(Box::new(StaticTy::Unknown))
                    };
                    self.bind(name, ValueFacts::new(ty));
                }
            }
            StmtKind::FuncDef(function) => {
                let old_return = self.expected_return.take();
                let old_loops = std::mem::take(&mut self.loop_depth);
                let old_parallel = std::mem::take(&mut self.parallel_depth);
                self.function_depth += 1;
                self.expected_return = function.return_ty.as_ref().map(StaticTy::from_annotation);
                self.scopes.push(HashMap::new());
                for param in &function.params {
                    self.bind(
                        &param.name,
                        ValueFacts::new(
                            param
                                .ty
                                .as_ref()
                                .map(StaticTy::from_annotation)
                                .unwrap_or(StaticTy::Unknown),
                        ),
                    );
                }
                self.check_stmts(&function.body);
                self.scopes.pop();
                self.function_depth -= 1;
                self.expected_return = old_return;
                self.loop_depth = old_loops;
                self.parallel_depth = old_parallel;
            }
            StmtKind::TypeDef { .. }
            | StmtKind::Use { .. }
            | StmtKind::UseFile { .. }
            | StmtKind::UsePython { .. }
            | StmtKind::UseHelper { .. }
            | StmtKind::UsePkg { .. }
            | StmtKind::UseMcp { .. }
            | StmtKind::Pass => {}
            StmtKind::Return(value) => {
                if self.function_depth == 0 && self.parallel_depth == 0 {
                    self.analysis.diagnostics.push(
                        Diagnostic::error(stmt.span, "`return` is only valid inside a function")
                            .with_hint("move this statement into `def`, `agent`, or `tool`"),
                    );
                }
                let actual = value
                    .as_ref()
                    .map(|expr| self.infer(expr).ty)
                    .unwrap_or(StaticTy::None);
                if self.parallel_depth == 0 {
                    if let Some(expected) = self.expected_return.clone() {
                        self.require_type(&actual, &expected, stmt.span, "return value");
                    }
                }
            }
            StmtKind::Match { subject, arms } => {
                let subject = self.infer(subject);
                for arm in arms {
                    self.bind_pattern(&arm.pattern, &subject);
                    if let Some(guard) = &arm.guard {
                        self.infer(guard);
                    }
                    self.check_stmts(&arm.body);
                }
            }
            StmtKind::BindOrElse {
                name,
                ty,
                value,
                classified,
                reason,
                status,
                else_body,
                ..
            } => {
                let outcome = self.infer(value);
                let is_analyze = matches!(
                    &value.kind,
                    ExprKind::Call { callee, .. }
                        if matches!(&callee.kind, ExprKind::Name(name) if name == "analyze")
                );
                if is_analyze && ty.is_none() {
                    self.analysis.diagnostics.push(
                        Diagnostic::error(
                            stmt.span,
                            "an `analyze` result needs a declared payload type",
                        )
                        .with_hint("write `result: YourType = analyze(data, prompt) else:`"),
                    );
                }
                let payload = match outcome.ty {
                    StaticTy::Outcome(inner) => *inner,
                    _ => StaticTy::Unknown,
                };
                let declared = ty.as_ref().map(StaticTy::from_annotation);
                if let Some(expected) = &declared {
                    self.require_type(&payload, expected, value.span, "outcome payload");
                }
                if let Some(reason) = reason {
                    self.bind(reason, ValueFacts::new(StaticTy::Str));
                }
                if let Some(status) = status {
                    self.bind(status, ValueFacts::new(StaticTy::Str));
                }
                self.check_stmts(else_body);
                self.bind(
                    name,
                    ValueFacts {
                        ty: declared.unwrap_or(payload),
                        classified: outcome.classified || *classified,
                        released: outcome.released,
                    },
                );
            }
            StmtKind::Declassify {
                value,
                binding,
                body,
                ..
            } => {
                let mut facts = self.infer(value);
                facts.released = true;
                let old = self.current_scope_mut().insert(binding.clone(), facts);
                self.check_stmts(body);
                if let Some(old) = old {
                    self.current_scope_mut().insert(binding.clone(), old);
                } else {
                    self.current_scope_mut().remove(binding);
                }
            }
            StmtKind::WithBudget { body, .. } | StmtKind::WithContext { body, .. } => {
                self.check_stmts(body);
            }
            StmtKind::WithMock { result, body, .. } => {
                self.infer(result);
                self.check_stmts(body);
            }
            StmtKind::Test { body, .. } => self.check_stmts(body),
            StmtKind::Assert { condition, message } => {
                self.infer(condition);
                if let Some(message) = message {
                    self.infer(message);
                }
            }
            StmtKind::Break(value) => {
                if let Some(value) = value {
                    self.infer(value);
                }
                if self.loop_depth == 0 {
                    self.analysis.diagnostics.push(
                        Diagnostic::error(stmt.span, "`break` is only valid inside a loop")
                            .with_hint("move it into `for`, `while`, or `parallel for`"),
                    );
                }
            }
            StmtKind::Continue => {
                if self.loop_depth == 0 {
                    self.analysis.diagnostics.push(
                        Diagnostic::error(stmt.span, "`continue` is only valid inside a loop")
                            .with_hint("move it into `for`, `while`, or `parallel for`"),
                    );
                }
            }
        }
    }

    fn infer(&mut self, expr: &Expr) -> ValueFacts {
        match &expr.kind {
            ExprKind::Int(_) => ValueFacts::new(StaticTy::Int),
            ExprKind::Float(_) => ValueFacts::new(StaticTy::Float),
            ExprKind::Str(_) => ValueFacts::new(StaticTy::Str),
            ExprKind::Bool(_) => ValueFacts::new(StaticTy::Bool),
            ExprKind::None => ValueFacts::new(StaticTy::None),
            ExprKind::Name(name) => self.lookup(name).unwrap_or_else(ValueFacts::unknown),
            ExprKind::List(items) => {
                let facts: Vec<ValueFacts> = items.iter().map(|item| self.infer(item)).collect();
                let first = facts.first().cloned().unwrap_or_else(ValueFacts::unknown);
                let mut classified = self.is_sensitive(&first);
                let mut released = first.released;
                for item in facts.iter().skip(1) {
                    classified |= self.is_sensitive(item);
                    released &= item.released;
                }
                ValueFacts {
                    ty: StaticTy::List(Box::new(first.ty)),
                    classified,
                    released,
                }
            }
            ExprKind::Dict(pairs) => {
                let classified = pairs.iter().any(|(key, value)| {
                    let key = self.infer(key);
                    let value = self.infer(value);
                    self.is_sensitive(&key) || self.is_sensitive(&value)
                });
                ValueFacts {
                    ty: StaticTy::Dict,
                    classified,
                    released: false,
                }
            }
            ExprKind::FString { exprs, .. } => {
                let classified = exprs.iter().any(|part| {
                    let facts = self.infer(part);
                    self.is_sensitive(&facts)
                });
                ValueFacts {
                    ty: StaticTy::Str,
                    classified,
                    released: false,
                }
            }
            ExprKind::Binary { op, left, right } => {
                let left = self.infer(left);
                let right = self.infer(right);
                let ty = match op {
                    BinOp::Eq
                    | BinOp::NotEq
                    | BinOp::Lt
                    | BinOp::Gt
                    | BinOp::LtEq
                    | BinOp::GtEq
                    | BinOp::In
                    | BinOp::NotIn
                    | BinOp::And
                    | BinOp::Or => StaticTy::Bool,
                    BinOp::Div => StaticTy::Float,
                    _ if left.ty == right.ty => left.ty.clone(),
                    _ if matches!(left.ty, StaticTy::Float)
                        && matches!(right.ty, StaticTy::Int) =>
                    {
                        StaticTy::Float
                    }
                    _ if matches!(left.ty, StaticTy::Int)
                        && matches!(right.ty, StaticTy::Float) =>
                    {
                        StaticTy::Float
                    }
                    _ => StaticTy::Unknown,
                };
                ValueFacts {
                    ty,
                    classified: self.is_sensitive(&left) || self.is_sensitive(&right),
                    released: left.released && right.released,
                }
            }
            ExprKind::Unary { op, operand } => {
                let operand = self.infer(operand);
                ValueFacts {
                    ty: if matches!(op, UnaryOp::Not) {
                        StaticTy::Bool
                    } else {
                        operand.ty.clone()
                    },
                    classified: self.is_sensitive(&operand),
                    released: operand.released,
                }
            }
            ExprKind::Call {
                callee,
                args,
                kwargs,
            } => self.infer_call(expr.span, callee, args, kwargs),
            ExprKind::Attr { object, name } => {
                let object = self.infer(object);
                if let StaticTy::Named(type_name) = &object.ty {
                    if let Some(fields) = self.types.get(type_name) {
                        if let Some((_, field)) = fields.iter().find(|(field, _)| field == name) {
                            return ValueFacts {
                                ty: field.ty.clone(),
                                classified: object.classified || field.classified,
                                released: object.released,
                            };
                        }
                        let available = fields
                            .iter()
                            .map(|(field, _)| field.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.analysis.diagnostics.push(
                            Diagnostic::error(
                                expr.span,
                                format!("`{type_name}` has no field `{name}`"),
                            )
                            .with_hint(format!("available fields: {available}")),
                        );
                    }
                }
                ValueFacts::unknown()
            }
            ExprKind::Index { object, index } => {
                let object = self.infer(object);
                self.infer(index);
                let ty = match &object.ty {
                    StaticTy::List(inner) => (**inner).clone(),
                    StaticTy::Str => StaticTy::Str,
                    _ => StaticTy::Unknown,
                };
                ValueFacts {
                    ty,
                    classified: self.is_sensitive(&object),
                    released: object.released,
                }
            }
            ExprKind::Slice {
                object,
                start,
                stop,
            } => {
                let object = self.infer(object);
                if let Some(start) = start {
                    self.infer(start);
                }
                if let Some(stop) = stop {
                    self.infer(stop);
                }
                object
            }
        }
    }

    fn infer_call(
        &mut self,
        span: Span,
        callee: &Expr,
        args: &[Expr],
        kwargs: &[(String, Expr)],
    ) -> ValueFacts {
        let arg_facts: Vec<ValueFacts> = args.iter().map(|arg| self.infer(arg)).collect();
        let kwarg_facts: Vec<ValueFacts> =
            kwargs.iter().map(|(_, value)| self.infer(value)).collect();
        let any_classified = arg_facts
            .iter()
            .chain(kwarg_facts.iter())
            .any(|facts| self.is_sensitive(facts));

        let ExprKind::Name(name) = &callee.kind else {
            self.infer(callee);
            // An external or computed callable may sanitize, preserve, or
            // replace its input. Do not invent a data-flow fact here; the
            // runtime checks the value that actually reaches a sink.
            return ValueFacts::unknown();
        };

        if name == "analyze" {
            if args.len() != 2 {
                self.analysis.diagnostics.push(
                    Diagnostic::error(
                        span,
                        format!(
                            "`analyze` expects 2 arguments (data and prompt), but {} provided",
                            args.len()
                        ),
                    )
                    .with_hint("write `analyze(data, \"what to do\")`"),
                );
            }
            if let Some(prompt) = arg_facts.get(1) {
                self.require_type(&prompt.ty, &StaticTy::Str, args[1].span, "model prompt");
            }
            for (keyword, value) in kwargs {
                if keyword != "model" && keyword != "tools" {
                    self.analysis.diagnostics.push(
                        Diagnostic::error(
                            value.span,
                            format!("`analyze` has no keyword argument `{keyword}`"),
                        )
                        .with_hint("the keyword arguments are `model` and `tools`"),
                    );
                }
                if keyword == "tools" {
                    self.check_model_tool_params(value);
                }
            }
            for (arg, facts) in args.iter().zip(arg_facts.iter()).chain(
                kwargs
                    .iter()
                    .map(|(_, value)| value)
                    .zip(kwarg_facts.iter()),
            ) {
                if self.is_sensitive(facts) {
                    self.analysis.diagnostics.push(
                        Diagnostic::error(
                            arg.span,
                            "classified data cannot be sent to `analyze` directly",
                        )
                        .with_hint(
                            "bind it inside `declassify <value> as <name> for <sink>:` and pass that binding; the runtime will also enforce the named sink policy",
                        ),
                    );
                }
            }
            return ValueFacts::new(StaticTy::Outcome(Box::new(StaticTy::Unknown)));
        }

        if let Some(function) = self.functions.get(name).cloned() {
            if args.len() != function.params.len() {
                self.analysis.diagnostics.push(
                    Diagnostic::error(
                        span,
                        format!(
                            "`{name}` expects {} argument{}, but {} provided",
                            function.params.len(),
                            if function.params.len() == 1 { "" } else { "s" },
                            args.len()
                        ),
                    )
                    .with_hint("pass one value for each declared parameter"),
                );
            }
            for ((arg, actual), expected) in args
                .iter()
                .zip(arg_facts.iter())
                .zip(function.params.iter())
            {
                self.require_type(&actual.ty, expected, arg.span, "function argument");
            }
            return ValueFacts {
                ty: function.return_ty,
                // This pass is not interprocedural. The function may return a
                // constant or a transformed input, so only its declared type
                // is known here. Runtime labels remain authoritative.
                classified: false,
                released: false,
            };
        }

        if let Some(fields) = self.types.get(name).cloned() {
            if args.len() != fields.len() {
                self.analysis.diagnostics.push(
                    Diagnostic::error(
                        span,
                        format!(
                            "`{name}` expects {} field value{}, but {} provided",
                            fields.len(),
                            if fields.len() == 1 { "" } else { "s" },
                            args.len()
                        ),
                    )
                    .with_hint(format!(
                        "provide fields in this order: {}",
                        fields
                            .iter()
                            .map(|(field, _)| field.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                );
            }
            for ((arg, actual), (_, field)) in args.iter().zip(arg_facts.iter()).zip(fields.iter())
            {
                self.require_type(&actual.ty, &field.ty, arg.span, "field value");
            }
            return ValueFacts {
                ty: StaticTy::Named(name.clone()),
                classified: any_classified,
                released: false,
            };
        }

        match name.as_str() {
            "Ok" => ValueFacts {
                ty: StaticTy::Outcome(Box::new(
                    arg_facts
                        .first()
                        .map(|facts| facts.ty.clone())
                        .unwrap_or(StaticTy::Unknown),
                )),
                classified: any_classified,
                released: false,
            },
            "Err" | "Uncertain" | "Exhausted" | "Failed" => {
                ValueFacts::new(StaticTy::Outcome(Box::new(StaticTy::Unknown)))
            }
            "str" => ValueFacts::new(StaticTy::Str),
            "int" => ValueFacts::new(StaticTy::Int),
            "float" => ValueFacts::new(StaticTy::Float),
            "bool" => ValueFacts::new(StaticTy::Bool),
            "len" | "tokens_spent" | "tokens_remaining" | "calls_spent" => {
                ValueFacts::new(StaticTy::Int)
            }
            "input" => ValueFacts::new(StaticTy::Str),
            "redact" => ValueFacts::new(StaticTy::Str),
            _ => ValueFacts {
                ty: StaticTy::Unknown,
                classified: any_classified,
                released: false,
            },
        }
    }

    fn require_type(&mut self, actual: &StaticTy, expected: &StaticTy, span: Span, use_: &str) {
        if !actual.compatible_with(expected) {
            self.analysis.diagnostics.push(
                Diagnostic::error(
                    span,
                    format!(
                        "{use_} has type `{}`, but `{}` is required",
                        actual.display(),
                        expected.display()
                    ),
                )
                .with_hint(format!("produce a `{}` value here", expected.display())),
            );
        }
    }

    fn bind_pattern(&mut self, pattern: &Pattern, subject: &ValueFacts) {
        match pattern {
            Pattern::Bind(name) => self.bind(name, subject.clone()),
            Pattern::Ctor(tag, binders) => {
                let payload = if tag == "Ok" {
                    match &subject.ty {
                        StaticTy::Outcome(inner) => (**inner).clone(),
                        _ => StaticTy::Unknown,
                    }
                } else {
                    StaticTy::Str
                };
                for binder in binders {
                    self.bind(
                        binder,
                        ValueFacts {
                            ty: payload.clone(),
                            classified: subject.classified,
                            released: subject.released,
                        },
                    );
                }
            }
            Pattern::Or(alternatives) => {
                if let Some(first) = alternatives.first() {
                    self.bind_pattern(first, subject);
                }
            }
            Pattern::Wildcard
            | Pattern::LiteralInt(_)
            | Pattern::LiteralStr(_)
            | Pattern::LiteralBool(_) => {}
        }
    }

    fn iter_item(&self, iter: ValueFacts) -> ValueFacts {
        let ty = match iter.ty {
            StaticTy::List(inner) => *inner,
            StaticTy::Str => StaticTy::Str,
            _ => StaticTy::Unknown,
        };
        ValueFacts {
            ty,
            classified: iter.classified,
            released: iter.released,
        }
    }

    fn bind(&mut self, name: &str, facts: ValueFacts) {
        self.current_scope_mut().insert(name.to_string(), facts);
    }

    fn current_scope_mut(&mut self) -> &mut HashMap<String, ValueFacts> {
        self.scopes
            .last_mut()
            .expect("the root scope always exists")
    }

    fn lookup(&self, name: &str) -> Option<ValueFacts> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
    }

    fn is_sensitive(&self, facts: &ValueFacts) -> bool {
        !facts.released
            && (facts.classified || self.type_contains_classified(&facts.ty, &mut HashSet::new()))
    }

    fn type_contains_classified(&self, ty: &StaticTy, visiting: &mut HashSet<String>) -> bool {
        match ty {
            StaticTy::Named(name) if visiting.insert(name.clone()) => {
                self.types.get(name).is_some_and(|fields| {
                    fields.iter().any(|(_, field)| {
                        field.classified || self.type_contains_classified(&field.ty, visiting)
                    })
                })
            }
            StaticTy::List(inner) | StaticTy::Outcome(inner) => {
                self.type_contains_classified(inner, visiting)
            }
            _ => false,
        }
    }
}

fn close_enough(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    if a.len().abs_diff(b.len()) > 1 || a.len() < 3 {
        return false;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (mut i, mut j, mut edits) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
        } else {
            edits += 1;
            if edits > 1 {
                return false;
            }
            match a.len().cmp(&b.len()) {
                std::cmp::Ordering::Greater => i += 1,
                std::cmp::Ordering::Less => j += 1,
                std::cmp::Ordering::Equal => {
                    i += 1;
                    j += 1;
                }
            }
        }
    }
    edits + (a.len() - i) + (b.len() - j) <= 1
}

/// The stdlib modules and their exports, for completion.
pub fn module_functions(module: &str) -> Option<&'static [&'static str]> {
    MODULES
        .iter()
        .find(|(name, _)| *name == module)
        .map(|(_, functions)| *functions)
}

pub fn module_names() -> Vec<&'static str> {
    MODULES.iter().map(|(name, _)| *name).collect()
}

pub fn builtin_names() -> &'static [&'static str] {
    BUILTINS
}

/// Whether a block always leaves the enclosing block, so control never
/// reaches the statement after it.
///
/// Conservative on purpose: it says yes only when it can prove it. A block it
/// cannot prove diverging is reported, and the fix is to write the `return`
/// explicitly, which is clearer than the analysis being clever.
fn diverges(body: &[Stmt]) -> bool {
    let Some(last) = body.last() else {
        return false;
    };
    match &last.kind {
        StmtKind::Return(_) | StmtKind::Break(_) | StmtKind::Continue => true,
        StmtKind::If {
            branches,
            else_body,
        } => {
            // Without an `else` there is a path that falls through.
            else_body.as_ref().is_some_and(|e| diverges(e))
                && branches.iter().all(|(_, b)| diverges(b))
        }
        // Every arm must leave, and an unmatched value is itself an error, so
        // a `match` whose arms all diverge cannot fall through.
        StmtKind::Match { arms, .. } => {
            !arms.is_empty() && arms.iter().all(|arm| diverges(&arm.body))
        }
        StmtKind::WithBudget { body, .. }
        | StmtKind::WithContext { body, .. }
        | StmtKind::WithMock { body, .. }
        | StmtKind::Declassify { body, .. } => diverges(body),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kora_syntax::parse;

    fn check(src: &str) -> Analysis {
        analyze(&parse(src).expect("should parse"))
    }

    fn messages(src: &str) -> Vec<String> {
        check(src)
            .diagnostics
            .into_iter()
            .map(|d| d.message)
            .collect()
    }

    #[test]
    fn clean_program_has_no_diagnostics() {
        let src = r#"type Point:
    x: int
    y: int

def add(a: int, b: int) -> int:
    return a + b

def main():
    p = Point(1, 2)
    total = add(p.x, p.y)
    print(total)
"#;
        assert!(messages(src).is_empty(), "{:?}", messages(src));
    }

    #[test]
    fn undefined_names_are_reported_with_a_suggestion() {
        let src = "def main():\n    count = 1\n    print(cont)\n";
        let analysis = check(src);
        assert_eq!(analysis.diagnostics.len(), 1);
        assert!(analysis.diagnostics[0]
            .message
            .contains("`cont` is not defined"));
        assert!(analysis.diagnostics[0]
            .hint
            .as_deref()
            .unwrap_or("")
            .contains("count"));
    }

    #[test]
    fn forward_references_between_functions_are_fine() {
        // Order should not matter at the top level.
        let src = "def a() -> int:\n    return b()\n\ndef b() -> int:\n    return 1\n";
        assert!(messages(src).is_empty(), "{:?}", messages(src));
    }

    #[test]
    fn undeclared_types_are_reported() {
        let src = "def main():\n    x: Ghost = 1\n";
        assert!(messages(src)[0].contains("`Ghost` is not a declared type"));
    }

    #[test]
    fn unknown_modules_are_reported() {
        assert!(messages("use jsonn\n")[0].contains("no module named"));
    }

    #[test]
    fn unknown_module_functions_are_reported() {
        let src = "use json\ndef main():\n    json.parze(\"{}\")\n";
        let analysis = check(src);
        assert!(analysis.diagnostics[0]
            .message
            .contains("no function `parze`"));
        assert!(analysis.diagnostics[0]
            .hint
            .as_deref()
            .unwrap_or("")
            .contains("parse"));
    }

    #[test]
    fn a_python_style_method_call_is_reported_at_check_time() {
        let src = "def main():\n    xs = [1, 2, 3]\n    xs.append(4)\n";
        let analysis = check(src);
        assert!(analysis.diagnostics[0]
            .message
            .contains("method calls are not supported"));
        assert!(analysis.diagnostics[0]
            .hint
            .as_deref()
            .unwrap_or("")
            .contains("append(xs, v)"));
    }

    #[test]
    fn a_module_call_is_not_mistaken_for_a_method_call() {
        let src = "use json\ndef main():\n    json.parse(\"{}\")\n";
        let analysis = check(src);
        assert_eq!(analysis.diagnostics.len(), 0);
    }

    #[test]
    fn keyword_arguments_on_a_user_function_are_reported_at_check_time() {
        let src = "def greet(name: str) -> str:\n    return name\n\ndef main():\n    print(greet(name=\"ada\"))\n";
        let analysis = check(src);
        assert!(analysis.diagnostics[0]
            .message
            .contains("`name` is not a keyword argument here"));
        assert!(analysis.diagnostics[0]
            .hint
            .as_deref()
            .unwrap_or("")
            .contains("only analyze()"));
    }

    #[test]
    fn loop_and_match_bindings_are_in_scope() {
        let src = r#"def main():
    for item in [1, 2]:
        print(item)
    match Ok(1):
        case Ok(value):
            print(value)
        case _:
            pass
"#;
        assert!(messages(src).is_empty(), "{:?}", messages(src));
    }

    #[test]
    fn declassify_binding_is_in_scope_inside_the_block() {
        let src = r#"def main():
    classified s = "x"
    declassify s as plain for local_model:
        print(plain)
"#;
        assert!(messages(src).is_empty(), "{:?}", messages(src));
    }

    #[test]
    fn declassify_binding_does_not_escape_the_block() {
        let src = r#"def main():
    classified s = "x"
    declassify s as plain for local_model:
        print(plain)
    print(plain)
"#;
        assert!(messages(src)
            .iter()
            .any(|message| message.contains("`plain` is not defined")));
    }

    #[test]
    fn parallel_results_are_bound_after_the_loop() {
        let src = r#"def main():
    results = parallel for x in [1, 2]:
        return x
    print(results)
"#;
        assert!(messages(src).is_empty(), "{:?}", messages(src));
    }

    #[test]
    fn parallel_first_result_is_not_typed_as_a_list() {
        let src = r#"agent race() -> str:
    answer = parallel for x in ["first", "second"] first:
        return x
    return answer
"#;
        assert!(messages(src).is_empty(), "{:?}", messages(src));
    }

    #[test]
    fn symbols_carry_signatures_for_hover() {
        let src = "agent triage(raw: str) -> str:\n    \"Classify a ticket.\"\n    return raw\n";
        let analysis = check(src);
        let symbol = analysis.symbols.get("triage").expect("agent is a symbol");
        assert_eq!(symbol.kind, SymbolKind::Agent);
        assert_eq!(symbol.detail, "agent triage(raw: str) -> str");
        assert_eq!(symbol.doc.as_deref(), Some("Classify a ticket."));
    }

    #[test]
    fn type_symbols_show_their_fields() {
        let src = "type E:\n    name: str\n    classified ssn: str\n";
        let analysis = check(src);
        let symbol = analysis.symbols.get("E").unwrap();
        assert!(symbol.detail.contains("name: str"));
        assert!(
            symbol.detail.contains("classified ssn: str"),
            "the classified marker should be visible on hover"
        );
    }

    #[test]
    fn references_resolve_the_token_under_the_cursor() {
        let src = "def helper() -> int:\n    return 1\n\ndef main():\n    helper()\n";
        let analysis = check(src);
        // `helper` on line 5, starting at column 5.
        assert_eq!(analysis.name_at(5, 5).as_deref(), Some("helper"));
        assert_eq!(analysis.name_at(5, 8).as_deref(), Some("helper"));
        assert_eq!(analysis.name_at(5, 99), None);
    }

    #[test]
    fn tests_appear_in_the_outline() {
        let analysis = check("test \"it works\":\n    assert True\n");
        assert!(analysis.symbols.contains_key("test it works"));
    }

    /// A scratch directory for the file-import tests.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir =
                std::env::temp_dir().join(format!("kora-types-{name}-{}", std::process::id()));
            std::fs::remove_dir_all(&dir).ok();
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn write(&self, name: &str, source: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, source).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn check_at(path: &Path) -> Analysis {
        let source = std::fs::read_to_string(path).expect("readable");
        analyze_file(&parse(&source).expect("should parse"), path)
    }

    #[test]
    fn names_from_an_imported_file_resolve() {
        let scratch = Scratch::new("resolve");
        scratch.write(
            "lib.ko",
            "RATE = 2

type Money:
    amount: int

def go(n: int) -> int:
    return n
",
        );
        let main = scratch.write(
            "main.ko",
            "use \"./lib.ko\" as lib\n\ndef main():\n    m = lib.Money(1)\n    print(lib.go(lib.RATE))\n",
        );
        let analysis = check_at(&main);
        let messages: Vec<&str> = analysis
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        assert!(messages.is_empty(), "{messages:?}");
        assert!(analysis.file_modules["lib"].exports.contains_key("go"));
    }

    #[test]
    fn a_name_the_imported_file_lacks_is_reported() {
        let scratch = Scratch::new("missing-name");
        scratch.write("lib.ko", "def go() -> int:\n    return 1\n");
        let main = scratch.write(
            "main.ko",
            "use \"./lib.ko\" as lib\n\ndef main():\n    print(lib.nope())\n",
        );
        let analysis = check_at(&main);
        assert_eq!(analysis.diagnostics.len(), 1);
        assert!(analysis.diagnostics[0]
            .message
            .contains("`lib` has no name `nope`"));
    }

    #[test]
    fn an_import_that_cannot_be_read_is_reported_once() {
        let scratch = Scratch::new("unreadable");
        let main = scratch.write(
            "main.ko",
            "use \"./nope.ko\" as lib\n\ndef main():\n    print(lib.go())\n",
        );
        let analysis = check_at(&main);
        // One diagnostic for the import, not one per use of the alias.
        assert_eq!(analysis.diagnostics.len(), 1);
        assert!(analysis.diagnostics[0].message.contains("cannot read"));
    }

    #[test]
    fn a_file_import_without_a_path_is_recorded_but_not_followed() {
        // An unsaved buffer has no directory, so the alias must still resolve.
        let analysis = check("use \"./lib.ko\" as lib\n\ndef main():\n    print(lib.go())\n");
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
    }

    #[test]
    fn pattern_metadata_is_checked_and_exposed_in_hover() {
        let valid = "type E:\n    code: str @description(\"Identifier\") @pattern(\"^[A-Z]+$\")\n";
        let analysis = check(valid);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        assert!(analysis.symbols["E"]
            .detail
            .contains("@description(\"Identifier\")"));

        let invalid = "type E:\n    count: int @pattern(\"[0-9]+\")\n";
        assert!(messages(invalid)[0].contains("is not a `str`"));
    }
    // --- `case` guards and `else` bindings ---

    #[test]
    fn a_guard_may_read_the_patterns_binders() {
        let src = r#"def main():
    match Ok(3):
        case Ok(v) if v > 1:
            print(v)
        case _:
            print("no")
"#;
        assert_eq!(messages(src), Vec::<String>::new());
    }

    #[test]
    fn a_guard_may_not_call_a_model() {
        let src = r#"agent main():
    match Ok(3):
        case Ok(v) if analyze(v, "well?"):
            print(v)
        case _:
            print("no")
"#;
        let msgs = messages(src);
        assert!(
            msgs.iter().any(|m| m.contains("guard cannot call a model")),
            "{msgs:?}"
        );
    }

    #[test]
    fn an_else_block_must_not_fall_through() {
        let src = r#"def main():
    v = Ok(1) else:
        print("oops")
    print(v)
"#;
        let msgs = messages(src);
        assert!(
            msgs.iter().any(|m| m.contains("must not fall through")),
            "{msgs:?}"
        );
    }

    #[test]
    fn an_else_block_ending_in_return_is_accepted() {
        let src = r#"def main():
    v = Ok(1) else:
        return
    print(v)
"#;
        assert_eq!(messages(src), Vec::<String>::new());
    }

    #[test]
    fn an_else_block_diverging_through_a_branch_is_accepted() {
        let src = r#"def main():
    v = Ok(1) else (why):
        if why == "x":
            return
        else:
            return
    print(v)
"#;
        assert_eq!(messages(src), Vec::<String>::new());
    }

    #[test]
    fn an_if_without_an_else_does_not_count_as_diverging() {
        let src = r#"def main():
    v = Ok(1) else:
        if True:
            return
    print(v)
"#;
        let msgs = messages(src);
        assert!(
            msgs.iter().any(|m| m.contains("must not fall through")),
            "{msgs:?}"
        );
    }

    #[test]
    fn the_bound_name_is_visible_after_the_statement() {
        let src = r#"def main():
    parsed = Ok(1) else:
        return
    print(parsed)
"#;
        assert_eq!(messages(src), Vec::<String>::new());
    }

    #[test]
    fn the_reason_is_only_named_when_asked_for() {
        let src = r#"def main():
    v = Ok(1) else:
        print(why)
        return
    print(v)
"#;
        let msgs = messages(src);
        assert!(
            msgs.iter().any(|m| m.contains("why")),
            "an unbound reason should still be reported: {msgs:?}"
        );
    }
}
