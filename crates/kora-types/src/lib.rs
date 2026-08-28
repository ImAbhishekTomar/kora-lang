//! kora-types: name resolution and the checks an editor can run on every
//! keystroke.
//!
//! This is deliberately not a full type checker. It answers the questions that
//! make an editor useful — what is defined, where, and is this name real —
//! fast enough to run on every change, and without executing anything.
//!
//! The same index powers hover and go-to-definition, so the editor's answers
//! and its squiggles can never disagree.

use std::collections::{HashMap, HashSet};

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

/// Analyse a parsed program.
pub fn analyze(program: &Program) -> Analysis {
    let mut analysis = Analysis::default();
    let mut checker = Checker {
        analysis: &mut analysis,
        scopes: vec![HashSet::new()],
        type_names: HashSet::new(),
    };
    checker.collect_definitions(&program.items);
    checker.check_block(&program.items);
    analysis
}

struct Checker<'a> {
    analysis: &'a mut Analysis,
    /// Innermost scope last.
    scopes: Vec<HashSet<String>>,
    type_names: HashSet<String>,
}

/// Names the runtime provides without a definition.
const BUILTINS: &[&str] = &[
    "print",
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
    "analyze",
];

const OUTCOME_TAGS: &[&str] = &["Ok", "Err", "Uncertain", "Exhausted"];

/// Modules the standard library provides, and what each exports.
const MODULES: &[(&str, &[&str])] = &[
    ("json", &["parse", "stringify", "get"]),
    ("csv", &["parse", "rows", "write"]),
    ("http", &["get", "post"]),
    ("sql", &["query", "execute"]),
    ("env", &["get", "has"]),
    ("fs", &["read", "write", "append", "exists", "lines"]),
    ("time", &["now", "format", "elapsed"]),
    ("re", &["matches", "find", "find_all", "replace", "split"]),
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
                            format!("    {marker}{}: {}", f.name, f.ty.display())
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
                target, ty, value, ..
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
                self.nested(body);
            }
            StmtKind::For { var, iter, body }
            | StmtKind::ParallelFor {
                var, iter, body, ..
            } => {
                self.check_expr(iter);
                // The loop variable outlives the loop, as it does in Python.
                self.declare(var);
                self.nested(body);
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
                self.scopes.pop();
            }
            StmtKind::TypeDef { fields, .. } => {
                for field in fields {
                    self.check_type(&field.ty, field.span);
                }
            }
            StmtKind::Return(Some(e)) => self.check_expr(e),
            StmtKind::Match { subject, arms } => {
                self.check_expr(subject);
                for arm in arms {
                    match &arm.pattern {
                        Pattern::Bind(name) => self.declare(name),
                        Pattern::Ctor(_, binders) => {
                            for b in binders {
                                self.declare(b);
                            }
                        }
                        _ => {}
                    }
                    self.nested(&arm.body);
                }
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
                self.declare(binding);
                self.nested(body);
            }
            StmtKind::WithBudget { body, .. } => self.nested(body),
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
            StmtKind::UseMcp { alias, .. } => {
                // Which servers exist is a runtime question: the checker
                // cannot know without launching them, so it records the alias
                // rather than reporting a name it cannot verify.
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
            StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
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
        const PRIMITIVES: &[&str] = &["str", "int", "float", "bool", "list", "dict", "None"];
        if !PRIMITIVES.contains(&name.as_str()) && !self.type_names.contains(name) {
            self.analysis.diagnostics.push(
                Diagnostic::error(span, format!("`{name}` is not a declared type"))
                    .with_hint("declare it with `type Name:` and typed fields below"),
            );
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
    fn parallel_results_are_bound_after_the_loop() {
        let src = r#"def main():
    results = parallel for x in [1, 2]:
        return x
    print(results)
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
}
