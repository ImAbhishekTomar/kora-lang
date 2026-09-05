//! Stable identities for the operations a program performs.
//!
//! An effect's identity is what the journal checks on resume and what a
//! cassette is keyed on, and it used to be `file:line`. A line number is a
//! property of the text, not of the program: adding a comment above a call
//! moves it, which invalidates that call's committed cassette entry and makes
//! an in-flight durable run refuse to resume. Nothing about the program
//! changed, so nothing should have.
//!
//! An operation id is structural instead: the enclosing function, and the
//! position of this call among the calls that function makes. Comments,
//! blank lines, and reformatting do not move it. Adding, removing, or
//! reordering a *call* does — which is a real change to what the program
//! does, and exactly when a recorded answer should stop matching.
//!
//! ## Why calls, and not every node
//!
//! Every effect this runtime journals happens at a call: `analyze(...)`,
//! `ask_human(...)`, `print(...)`, `fs.append(...)`, `http.get(...)`. Numbering
//! calls alone keeps the id stable across edits that are not calls —
//! `x = 1 + 2` becoming `x = 1 + 2 + 3` leaves every id in the function
//! untouched — where numbering all expressions would shift everything after
//! it.
//!
//! ## What this is not
//!
//! It is not the typed effect-aware IR (see TODO.md). There is no lowering
//! and no second representation of the program: this is a side table over the
//! AST, computed once, answering one question. The IR would subsume it.

use std::collections::HashMap;

use crate::ast::{Expr, ExprKind, FuncDef, Program, Stmt, StmtKind};
use crate::token::Span;

/// Where a call sits in the program, independent of how the file is laid out.
#[derive(Debug, Default, Clone)]
pub struct OperationIds {
    /// Keyed by `span.start`, which is unique per node: two calls cannot begin
    /// at the same byte.
    ids: HashMap<usize, String>,
}

impl OperationIds {
    /// The id for the call beginning at `span`, if this span is a call.
    pub fn get(&self, span: Span) -> Option<&str> {
        self.ids.get(&span.start).map(String::as_str)
    }

    /// How many calls were numbered. For tests and diagnostics.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// Number every call in `program`.
pub fn assign(program: &Program) -> OperationIds {
    let mut out = OperationIds::default();
    // Definitions first, each its own scope, so nothing they contain can
    // borrow the same table as the top-level walk below.
    for item in &program.items {
        match &item.kind {
            StmtKind::FuncDef(def) => number_function(def, &mut out),
            StmtKind::Test { name, body } => {
                let mut scope = Scope::new(&format!("test:{name}"), &mut out);
                scope.statements(body);
            }
            _ => {}
        }
    }
    // Statements written outside any function run at the top level, and are
    // their own scope: they execute before `main` and must not share its
    // numbering.
    let mut top = Scope::new("<top>", &mut out);
    for item in &program.items {
        if !matches!(item.kind, StmtKind::FuncDef(_) | StmtKind::Test { .. }) {
            top.statement(item);
        }
    }
    out
}

fn number_function(def: &FuncDef, out: &mut OperationIds) {
    let mut scope = Scope::new(&def.name, out);
    scope.statements(&def.body);
}

/// One function's worth of numbering.
struct Scope<'a> {
    name: String,
    next: usize,
    out: &'a mut OperationIds,
}

impl<'a> Scope<'a> {
    fn new(name: &str, out: &'a mut OperationIds) -> Scope<'a> {
        Scope {
            name: name.to_string(),
            next: 0,
            out,
        }
    }

    fn claim(&mut self, span: Span) {
        let id = format!("{}/{}", self.name, self.next);
        self.next += 1;
        self.out.ids.insert(span.start, id);
    }

    fn statements(&mut self, body: &[Stmt]) {
        for stmt in body {
            self.statement(stmt);
        }
    }

    /// Deliberately exhaustive: a new statement kind should fail to compile
    /// here rather than quietly go unnumbered, which would send its effects
    /// back to line-based identity without anyone noticing.
    fn statement(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Assign {
                target,
                value,
                on_token,
                on_tool_call,
                ..
            } => {
                self.expr(target);
                self.expr(value);
                if let Some(handler) = on_token {
                    self.statements(&handler.body);
                }
                if let Some(handler) = on_tool_call {
                    self.statements(&handler.body);
                }
            }
            StmtKind::AugAssign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            StmtKind::Expr(e) => self.expr(e),
            StmtKind::If {
                branches,
                else_body,
            } => {
                for (cond, body) in branches {
                    self.expr(cond);
                    self.statements(body);
                }
                if let Some(body) = else_body {
                    self.statements(body);
                }
            }
            StmtKind::While { cond, body } => {
                self.expr(cond);
                self.statements(body);
            }
            StmtKind::For { iter, body, .. } => {
                self.expr(iter);
                self.statements(body);
            }
            StmtKind::ParallelFor { iter, body, .. } => {
                self.expr(iter);
                self.statements(body);
            }
            // A function defined inside another is still its own scope: its
            // calls belong to it, not to the body that declared it.
            StmtKind::FuncDef(def) => number_function(def, self.out),
            StmtKind::Test { name, body } => {
                let mut scope = Scope::new(&format!("test:{name}"), self.out);
                scope.statements(body);
            }
            StmtKind::Return(value) => {
                if let Some(e) = value {
                    self.expr(e);
                }
            }
            StmtKind::Assert { condition, message } => {
                self.expr(condition);
                if let Some(e) = message {
                    self.expr(e);
                }
            }
            StmtKind::WithMock { result, body, .. } => {
                self.expr(result);
                self.statements(body);
            }
            StmtKind::Declassify { value, body, .. } => {
                self.expr(value);
                self.statements(body);
            }
            StmtKind::WithBudget { body, .. } => self.statements(body),
            StmtKind::WithContext { body, .. } => self.statements(body),
            StmtKind::Match { subject, arms } => {
                self.expr(subject);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.statements(&arm.body);
                }
            }
            StmtKind::BindOrElse {
                value, else_body, ..
            } => {
                self.expr(value);
                self.statements(else_body);
            }
            // Nothing to number: declarations and jumps make no calls.
            StmtKind::TypeDef { .. }
            | StmtKind::Use { .. }
            | StmtKind::UseFile { .. }
            | StmtKind::UsePython { .. }
            | StmtKind::UsePkg { .. }
            | StmtKind::UseMcp { .. }
            | StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Pass => {}
        }
    }

    /// Pre-order, and the call itself is claimed *before* its arguments, so
    /// `f(g())` numbers `f` then `g` — the order they are written, which is
    /// the order a reader would count them in.
    fn expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Call {
                callee,
                args,
                kwargs,
            } => {
                self.claim(expr.span);
                self.expr(callee);
                for arg in args {
                    self.expr(arg);
                }
                for (_, value) in kwargs {
                    self.expr(value);
                }
            }
            ExprKind::FString { exprs, .. } => {
                for e in exprs {
                    self.expr(e);
                }
            }
            ExprKind::List(items) => {
                for e in items {
                    self.expr(e);
                }
            }
            ExprKind::Dict(pairs) => {
                for (k, v) in pairs {
                    self.expr(k);
                    self.expr(v);
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            ExprKind::Unary { operand, .. } => self.expr(operand),
            ExprKind::Attr { object, .. } => self.expr(object),
            ExprKind::Index { object, index } => {
                self.expr(object);
                self.expr(index);
            }
            ExprKind::Slice {
                object,
                start,
                stop,
            } => {
                self.expr(object);
                if let Some(e) = start {
                    self.expr(e);
                }
                if let Some(e) = stop {
                    self.expr(e);
                }
            }
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::Bool(_)
            | ExprKind::None
            | ExprKind::Name(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    /// Every id in a program, in source order, so a test can read the shape
    /// the numbering produced.
    fn ids(src: &str) -> Vec<String> {
        let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
        let assigned = assign(&program);
        let mut pairs: Vec<(usize, String)> =
            assigned.ids.iter().map(|(k, v)| (*k, v.clone())).collect();
        pairs.sort();
        pairs.into_iter().map(|(_, v)| v).collect()
    }

    #[test]
    fn calls_are_numbered_within_their_function() {
        assert_eq!(
            ids("def main():\n    print(\"a\")\n    print(\"b\")\n"),
            vec!["main/0", "main/1"]
        );
    }

    #[test]
    fn each_function_counts_from_zero() {
        // The point of scoping by function: editing one does not renumber
        // the others.
        assert_eq!(
            ids("def helper():\n    print(\"a\")\n\ndef main():\n    print(\"b\")\n"),
            vec!["helper/0", "main/0"]
        );
    }

    #[test]
    fn a_comment_does_not_move_an_id() {
        // The defect this exists to fix. Both programs make the same call in
        // the same place; only the text above it differs.
        let plain = "def main():\n    print(\"a\")\n";
        let commented =
            "# a note\n# and another\ndef main():\n    # explaining\n    print(\"a\")\n";
        assert_eq!(ids(plain), ids(commented));
    }

    #[test]
    fn blank_lines_and_reformatting_do_not_move_an_id() {
        let tight = "def main():\n    print(\"a\")\n    print(\"b\")\n";
        let airy = "def main():\n\n    print(\"a\")\n\n\n    print(\"b\")\n\n";
        assert_eq!(ids(tight), ids(airy));
    }

    #[test]
    fn an_edit_that_is_not_a_call_does_not_move_an_id() {
        let before = "def main():\n    x = 1 + 2\n    print(x)\n";
        let after = "def main():\n    x = 1 + 2 + 3\n    print(x)\n";
        assert_eq!(ids(before), ids(after));
    }

    #[test]
    fn adding_a_call_renumbers_what_follows_it() {
        // And it should: a program that calls something new before this one
        // is a different program, and a recorded answer for the old position
        // is not an answer for the new one.
        let before = ids("def main():\n    print(\"b\")\n");
        let after = ids("def main():\n    print(\"a\")\n    print(\"b\")\n");
        assert_eq!(before, vec!["main/0"]);
        assert_eq!(after, vec!["main/0", "main/1"]);
    }

    #[test]
    fn a_nested_call_is_numbered_in_reading_order() {
        assert_eq!(
            ids("def main():\n    print(str(1))\n"),
            vec!["main/0", "main/1"]
        );
    }

    #[test]
    fn calls_inside_control_flow_are_numbered() {
        let out = ids(
            "def main():\n    for i in range(3):\n        print(i)\n    if True:\n        print(\"y\")\n",
        );
        assert_eq!(out, vec!["main/0", "main/1", "main/2"]);
    }

    #[test]
    fn top_level_statements_are_their_own_scope() {
        // They run before `main`, so sharing its numbering would make the
        // two interleave.
        let out = ids("print(\"top\")\n\ndef main():\n    print(\"in main\")\n");
        assert_eq!(out, vec!["<top>/0", "main/0"]);
    }

    #[test]
    fn a_test_block_is_its_own_scope() {
        let out = ids("def main():\n    print(\"a\")\n\ntest \"it works\":\n    print(\"b\")\n");
        assert_eq!(out, vec!["main/0", "test:it works/0"]);
    }

    #[test]
    fn a_streaming_handler_body_belongs_to_the_function_around_it() {
        // The handler is part of what the enclosing function does, and its
        // `write` is an output effect that needs an id like any other.
        let out = ids(
            "def main():\n    answer: str = analyze(\"q\", \"d\") on token(t):\n        write(t)\n",
        );
        assert_eq!(out, vec!["main/0", "main/1"]);
    }

    #[test]
    fn a_match_arm_body_is_numbered() {
        let out = ids(
            "def main():\n    match compute():\n        case Ok(v):\n            print(v)\n        case Err(w):\n            print(w)\n",
        );
        assert_eq!(out, vec!["main/0", "main/1", "main/2"]);
    }

    #[test]
    fn declarations_number_nothing() {
        let out = ids("use fs\n\ntype User:\n    name: str\n\ndef main():\n    pass\n");
        assert!(out.is_empty(), "got {out:?}");
    }
}
