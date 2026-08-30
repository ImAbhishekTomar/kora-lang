//! Static inventory of declassification sites.
//!
//! Because every release of classified data is a `declassify` block, the
//! compiler can enumerate them all. That completeness is the point: a grep
//! is best-effort, this is exhaustive, and it is what makes the feature
//! answerable to a reviewer or an auditor.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use kora_syntax::ast::*;

use crate::label::DeclassifySite;

/// Walk a parsed program and collect every declassification site.
pub fn audit(program: &Program, file: &str) -> Vec<DeclassifySite> {
    let mut sites = Vec::new();
    walk_stmts(&program.items, file, &mut sites);
    sites.sort_by_key(|s| s.line);
    sites
}

/// Audit a program, every file it imports, and every package it uses.
///
/// Imports are part of the program, so an inventory that stopped at the entry
/// file would not be the complete list this command promises. A dependency is
/// part of it too: a `declassify` inside a package a program pulled in
/// releases that program's data, and an audit that could not see it would
/// make adding a dependency the way to hide one. Files that cannot be read or
/// parsed are skipped rather than failing the audit: the sites that *are*
/// visible are still worth reporting.
pub fn audit_program(
    program: &Program,
    file: &str,
    packages: &kora_pkg::Resolution,
) -> Vec<DeclassifySite> {
    let mut sites = Vec::new();
    let mut seen = HashSet::new();
    let base = Path::new(file)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    seen.insert(
        Path::new(file)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(file)),
    );
    walk_stmts(&program.items, file, &mut sites);
    walk_imports(
        &program.items,
        &base,
        kora_pkg::ROOT,
        packages,
        &mut seen,
        &mut sites,
    );
    sites.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    sites
}

/// Follow `use "..."` and `use pkg` statements, depth first, once per file.
///
/// `package` is the package the statements were written in: a `use pkg` name
/// resolves against that package's own `[dependencies]`, exactly as it does
/// at run time.
fn walk_imports(
    stmts: &[Stmt],
    base: &Path,
    package: kora_pkg::PackageId,
    packages: &kora_pkg::Resolution,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<DeclassifySite>,
) {
    for stmt in stmts {
        let (candidate, next_package) = match &stmt.kind {
            StmtKind::UseFile { path, .. } => {
                (crate::modules::normalize(&base.join(path)), package)
            }
            StmtKind::UsePkg { package: name, .. } => match packages.dep_of(package, name) {
                Some(target) => (target.entry.clone(), target.id),
                None => continue,
            },
            _ => continue,
        };

        let key = candidate
            .canonicalize()
            .unwrap_or_else(|_| candidate.clone());
        if !seen.insert(key.clone()) {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&candidate) else {
            continue;
        };
        let Ok(program) = kora_syntax::parse(&source) else {
            continue;
        };
        let display = candidate.display().to_string();
        walk_stmts(&program.items, &display, out);
        let dir = key
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        walk_imports(&program.items, &dir, next_package, packages, seen, out);
    }
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
        StmtKind::BindOrElse { else_body, .. } => walk_stmts(else_body, file, out),
        StmtKind::FuncDef(f) => walk_stmts(&f.body, file, out),
        // `on token(t):` / `on tool_call(name, args):` each carry a body of
        // their own: a `declassify` inside either releases data the same as
        // one anywhere else, and the exhaustiveness this module promises
        // means it cannot hide there just because the block hangs off an
        // assignment.
        StmtKind::Assign {
            on_token: Some(handler),
            ..
        } => walk_stmts(&handler.body, file, out),
        StmtKind::Assign {
            on_tool_call: Some(handler),
            ..
        } => walk_stmts(&handler.body, file, out),
        StmtKind::Assign {
            on_token: None,
            on_tool_call: None,
            ..
        }
        | StmtKind::AugAssign { .. }
        | StmtKind::Expr(_)
        | StmtKind::TypeDef { .. }
        | StmtKind::Return(_)
        | StmtKind::Break
        | StmtKind::Continue
        | StmtKind::Use { .. }
        | StmtKind::UseFile { .. }
        | StmtKind::UsePkg { .. }
        | StmtKind::UseMcp { .. }
        | StmtKind::UsePython { .. }
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
