//! Finding every `use pkg` and `use "./file.ko"` in a syntax tree.
//!
//! `use` is an ordinary statement, so an import may sit inside a function, a
//! loop, or a `match` arm. The scan walks every body rather than the top
//! level alone: missing one would silently drop a dependency the program
//! needs. Being over-inclusive is safe here and being under-inclusive is not,
//! so a `use pkg` inside a function nobody calls still counts as used.

use kora_syntax::ast::{Program, Stmt, StmtKind};
use kora_syntax::token::Span;

/// One import found in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub name: String,
    pub span: Span,
}

/// What one file imports.
#[derive(Debug, Clone, Default)]
pub struct Imports {
    /// `use pkg <name>` — dependencies of the enclosing package.
    pub packages: Vec<Import>,
    /// `use "./x.ko"` — other files of the same package.
    pub files: Vec<Import>,
}

/// Collect the imports of one parsed file.
///
/// When `include_tests` is false, `test` blocks are skipped entirely. Running
/// the scan both ways is what separates a runtime dependency from one only
/// the tests reach, without anyone having to declare which is which.
pub fn imports(program: &Program, include_tests: bool) -> Imports {
    let mut out = Imports::default();
    walk(&program.items, include_tests, &mut out);
    out
}

fn walk(stmts: &[Stmt], include_tests: bool, out: &mut Imports) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::UsePkg { package, .. } => out.packages.push(Import {
                name: package.clone(),
                span: stmt.span,
            }),
            StmtKind::UseFile { path, .. } => out.files.push(Import {
                name: path.clone(),
                span: stmt.span,
            }),
            StmtKind::Test { body, .. } => {
                if include_tests {
                    walk(body, include_tests, out);
                }
            }

            StmtKind::If {
                branches,
                else_body,
            } => {
                for (_, body) in branches {
                    walk(body, include_tests, out);
                }
                if let Some(body) = else_body {
                    walk(body, include_tests, out);
                }
            }
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::ParallelFor { body, .. }
            | StmtKind::WithMock { body, .. }
            | StmtKind::Declassify { body, .. }
            | StmtKind::WithBudget { body, .. } => walk(body, include_tests, out),
            StmtKind::FuncDef(def) => walk(&def.body, include_tests, out),
            StmtKind::Match { arms, .. } => {
                for arm in arms {
                    walk(&arm.body, include_tests, out);
                }
            }
            StmtKind::BindOrElse { else_body, .. } => walk(else_body, include_tests, out),
            // `on token(t):` has a body like any other block, so it can hide
            // an import the same way a loop or a match arm can.
            StmtKind::Assign {
                on_token: Some(handler),
                ..
            } => walk(&handler.body, include_tests, out),
            StmtKind::Assign {
                on_tool_call: Some(handler),
                ..
            } => walk(&handler.body, include_tests, out),

            // Every other statement has no body and cannot hide an import.
            StmtKind::Assign {
                on_token: None,
                on_tool_call: None,
                ..
            }
            | StmtKind::AugAssign { .. }
            | StmtKind::Expr(_)
            | StmtKind::TypeDef { .. }
            | StmtKind::Use { .. }
            | StmtKind::UsePython { .. }
            | StmtKind::UseMcp { .. }
            | StmtKind::Assert { .. }
            | StmtKind::Return(_)
            | StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Pass => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Program {
        kora_syntax::parse(src).expect("test source must parse")
    }

    fn names(imports: &[Import]) -> Vec<&str> {
        imports.iter().map(|i| i.name.as_str()).collect()
    }

    #[test]
    fn finds_a_top_level_package_import() {
        let p = parse("use pkg receipts as r\n");
        assert_eq!(names(&imports(&p, false).packages), ["receipts"]);
    }

    #[test]
    fn finds_an_import_nested_in_a_function() {
        let p = parse("def f():\n    use pkg receipts as r\n    return 1\n");
        assert_eq!(names(&imports(&p, false).packages), ["receipts"]);
    }

    #[test]
    fn finds_an_import_inside_a_match_arm() {
        let src = "def f(x: int) -> int:\n    match x:\n        case 1:\n            use pkg receipts as r\n            return 1\n        case _:\n            return 0\n";
        let p = parse(src);
        assert_eq!(names(&imports(&p, false).packages), ["receipts"]);
    }

    #[test]
    fn a_test_block_import_is_skipped_unless_asked_for() {
        let p = parse("test \"t\":\n    use pkg fixtures as f\n    assert True, \"x\"\n");
        assert!(imports(&p, false).packages.is_empty());
        assert_eq!(names(&imports(&p, true).packages), ["fixtures"]);
    }

    #[test]
    fn file_imports_are_collected_separately() {
        let p = parse("use \"./lib/tax.ko\" as tax\nuse pkg receipts as r\n");
        let found = imports(&p, false);
        assert_eq!(names(&found.files), ["./lib/tax.ko"]);
        assert_eq!(names(&found.packages), ["receipts"]);
    }
}
