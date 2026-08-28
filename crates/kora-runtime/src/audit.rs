//! Static inventory of declassification sites.
//!
//! Because every release of classified data is a `declassify` block, the
//! compiler can enumerate them all. That completeness is the point: a grep
//! is best-effort, this is exhaustive, and it is what makes the feature
//! answerable to a reviewer or an auditor.

use kora_syntax::ast::*;

use crate::label::DeclassifySite;

/// Walk a parsed program and collect every declassification site.
pub fn audit(program: &Program, file: &str) -> Vec<DeclassifySite> {
    let mut sites = Vec::new();
    walk_stmts(&program.items, file, &mut sites);
    sites.sort_by_key(|s| s.line);
    sites
}

fn walk_stmts(stmts: &[Stmt], file: &str, out: &mut Vec<DeclassifySite>) {
    for stmt in stmts {
        walk_stmt(stmt, file, out);
    }
}

fn walk_stmt(stmt: &Stmt, file: &str, out: &mut Vec<DeclassifySite>) {
    match &stmt.kind {
        StmtKind::Declassify {
            binding,
            sink,
            body,
            ..
        } => {
            out.push(DeclassifySite {
                file: file.to_string(),
                line: stmt.span.line,
                expression: binding.clone(),
                sink: sink.clone(),
            });
            // Nested declassifications count too.
            walk_stmts(body, file, out);
        }
        StmtKind::If {
            branches,
            else_body,
        } => {
            for (_, body) in branches {
                walk_stmts(body, file, out);
            }
            if let Some(body) = else_body {
                walk_stmts(body, file, out);
            }
        }
        StmtKind::While { body, .. }
        | StmtKind::For { body, .. }
        | StmtKind::ParallelFor { body, .. }
        | StmtKind::WithBudget { body, .. }
        | StmtKind::WithMock { body, .. }
        | StmtKind::Test { body, .. } => walk_stmts(body, file, out),
        StmtKind::Match { arms, .. } => {
            for arm in arms {
                walk_stmts(&arm.body, file, out);
            }
        }
        StmtKind::FuncDef(f) => walk_stmts(&f.body, file, out),
        StmtKind::Assign { .. }
        | StmtKind::AugAssign { .. }
        | StmtKind::Expr(_)
        | StmtKind::TypeDef { .. }
        | StmtKind::Return(_)
        | StmtKind::Break
        | StmtKind::Continue
        | StmtKind::Use { .. }
        | StmtKind::UseMcp { .. }
        | StmtKind::Assert { .. }
        | StmtKind::Pass => {}
    }
}

/// Render the audit as a report.
pub fn render(sites: &[DeclassifySite]) -> String {
    if sites.is_empty() {
        return "no declassification sites: no classified data reaches any sink\n".to_string();
    }
    let mut out = String::new();
    for site in sites {
        out.push_str(&format!(
            "  {}:{}  declassify {} for {}\n",
            site.file, site.line, site.expression, site.sink
        ));
    }
    out.push_str(&format!(
        "\n{} declassification site{}\n",
        sites.len(),
        if sites.len() == 1 { "" } else { "s" }
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kora_syntax::parse;

    fn sites_of(src: &str) -> Vec<DeclassifySite> {
        audit(&parse(src).expect("should parse"), "test.ko")
    }

    #[test]
    fn finds_a_top_level_site() {
        let src = "classified ssn = \"x\"\ndeclassify ssn for local_model:\n    print(ssn)\n";
        let sites = sites_of(src);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].line, 2);
        assert_eq!(sites[0].expression, "ssn");
        assert_eq!(sites[0].sink, "local_model");
    }

    #[test]
    fn finds_sites_inside_functions_and_branches() {
        let src = r#"def handle(x: str) -> int:
    if x == "a":
        declassify x for local_model:
            print(x)
    return 1

agent worker() -> int:
    classified s = "secret"
    declassify s for onprem:
        print(s)
    return 0
"#;
        let sites = sites_of(src);
        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0].sink, "local_model");
        assert_eq!(sites[1].sink, "onprem");
    }

    #[test]
    fn finds_sites_inside_loops_and_parallel() {
        let src = r#"def main():
    for x in [1]:
        declassify x for a:
            print(x)
    parallel for y in [2]:
        declassify y for b:
            print(y)
    with budget(max_tokens = 10):
        declassify z as w for c:
            print(w)
"#;
        let sinks: Vec<String> = sites_of(src).into_iter().map(|s| s.sink).collect();
        assert_eq!(sinks, vec!["a", "b", "c"]);
    }

    #[test]
    fn finds_nested_sites() {
        let src = r#"declassify a for one:
    declassify b for two:
        print(b)
"#;
        assert_eq!(sites_of(src).len(), 2);
    }

    #[test]
    fn finds_sites_inside_match_arms() {
        let src = r#"match x:
    case Ok(v):
        declassify v for local_model:
            print(v)
    case _:
        pass
"#;
        assert_eq!(sites_of(src).len(), 1);
    }

    #[test]
    fn clean_program_reports_nothing() {
        let sites = sites_of("def main():\n    print(1)\n");
        assert!(sites.is_empty());
        assert!(render(&sites).contains("no declassification sites"));
    }

    #[test]
    fn report_lists_every_site() {
        let src = "declassify a for x:\n    print(a)\ndeclassify b for y:\n    print(b)\n";
        let report = render(&sites_of(src));
        assert!(report.contains("test.ko:1  declassify a for x"));
        assert!(report.contains("test.ko:3  declassify b for y"));
        assert!(report.contains("2 declassification sites"));
    }
}
