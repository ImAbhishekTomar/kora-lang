//! Which lines carry a statement.
//!
//! A debugger needs this to answer a question the editor asks constantly: the
//! user clicked the gutter on line 9, and line 9 is blank. Snapping the
//! breakpoint to the next real statement is the difference between a
//! breakpoint that works and one that silently never fires.

use std::collections::BTreeSet;

use crate::ast::*;

/// Every line the interpreter will stop on, in order.
pub fn statement_lines(program: &Program) -> BTreeSet<u32> {
    let mut lines = BTreeSet::new();
    walk(&program.items, &mut lines);
    lines
}

/// The line a breakpoint on `line` should actually be set on.
///
/// The next statement at or after the requested line, because that is what the
/// user meant by clicking there. `None` when nothing follows, which the client
/// shows as an unverified breakpoint rather than one that quietly never fires.
pub fn snap(lines: &BTreeSet<u32>, line: u32) -> Option<u32> {
    lines.range(line..).next().copied()
}

fn walk(stmts: &[Stmt], out: &mut BTreeSet<u32>) {
    for stmt in stmts {
        out.insert(stmt.span.line);
        match &stmt.kind {
            StmtKind::If {
                branches,
                else_body,
            } => {
                for (_, body) in branches {
                    walk(body, out);
                }
                if let Some(body) = else_body {
                    walk(body, out);
                }
            }
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::ParallelFor { body, .. }
            | StmtKind::WithBudget { body, .. }
            | StmtKind::WithMock { body, .. }
            | StmtKind::Declassify { body, .. }
            | StmtKind::Test { body, .. } => walk(body, out),
            StmtKind::Match { arms, .. } => {
                for arm in arms {
                    walk(&arm.body, out);
                }
            }
            StmtKind::BindOrElse { else_body, .. } => walk(else_body, out),
            StmtKind::FuncDef(f) => walk(&f.body, out),
            StmtKind::Assign { .. }
            | StmtKind::AugAssign { .. }
            | StmtKind::Expr(_)
            | StmtKind::TypeDef { .. }
            | StmtKind::Return(_)
            | StmtKind::Assert { .. }
            | StmtKind::Use { .. }
            | StmtKind::UseFile { .. }
            | StmtKind::UsePkg { .. }
            | StmtKind::UseMcp { .. }
            | StmtKind::UsePython { .. }
            | StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Pass => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    const SOURCE: &str = "\
def double(n: int) -> int:
    return n * 2

def main():
    # a comment, not a statement
    total = 0

    for n in [1, 2]:
        total = total + double(n)
    print(total)
";

    #[test]
    fn only_lines_with_statements_are_listed() {
        let lines = statement_lines(&parse(SOURCE).unwrap());
        assert_eq!(
            lines.iter().copied().collect::<Vec<u32>>(),
            vec![1, 2, 4, 6, 8, 9, 10]
        );
    }

    #[test]
    fn a_breakpoint_snaps_forward_to_the_next_statement() {
        let lines = statement_lines(&parse(SOURCE).unwrap());
        // Line 5 is a comment, line 7 is blank.
        assert_eq!(snap(&lines, 5), Some(6));
        assert_eq!(snap(&lines, 7), Some(8));
        // An exact hit stays put.
        assert_eq!(snap(&lines, 9), Some(9));
        // Past the end there is nothing to stop on.
        assert_eq!(snap(&lines, 40), None);
    }
}
