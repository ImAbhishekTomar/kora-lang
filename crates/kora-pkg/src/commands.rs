//! `add`, `remove`, and `update`.
//!
//! `update` is the one command that deliberately moves past the lockfile, so
//! it is where a new version's *authority* has to be looked at. A dependency
//! that quietly starts asking for the network, or starts declassifying, is
//! how a package people already trust turns into a problem — and a bump that
//! reports only a version number gives nobody the chance to notice.

use std::path::{Path, PathBuf};

use crate::grants::Grants;
use crate::lock::Lock;
use crate::manifest::{DepSpec, GitRef, Manifest};

/// What changed about a dependency between the locked version and a new one.
#[derive(Debug, Default)]
pub struct Diff {
    pub name: String,
    pub from: String,
    pub to: String,
    /// Authority the new version asks for that the old one did not.
    pub new_requirements: Vec<String>,
    /// `declassify` sites, before and after.
    pub declassify_before: usize,
    pub declassify_after: usize,
}

impl Diff {
    /// Whether this bump changes what the package is allowed to do.
    ///
    /// Not "is it newer" — that is expected. This is the question a person
    /// has to answer before accepting the bump.
    pub fn needs_a_look(&self) -> bool {
        !self.new_requirements.is_empty() || self.declassify_after > self.declassify_before
    }
}

/// Add a dependency to the manifest, without fetching it.
pub fn add(root: &Path, name: &str, spec: &DepSpec) -> Result<crate::edit::Change, String> {
    if !crate::manifest::is_valid_name(name) {
        return Err(format!(
            "`{name}` is not a usable package name: lowercase letters, digits, and underscores"
        ));
    }
    let mut doc = crate::edit::open(root)?;
    let change = crate::edit::add(&mut doc, name, spec);
    crate::edit::save(root, &doc)?;
    Ok(change)
}

/// Remove a dependency from the manifest.
///
/// The lockfile and the fetched copy are left alone: the next resolve prunes
/// what nothing imports, and deleting bytes on the strength of one edit is
/// the kind of helpfulness that loses work.
pub fn remove(root: &Path, name: &str) -> Result<crate::edit::Change, String> {
    let mut doc = crate::edit::open(root)?;
    let change = crate::edit::remove(&mut doc, name);
    crate::edit::save(root, &doc)?;
    Ok(change)
}

/// Compare a package's old checkout against a freshly fetched one.
pub fn compare(name: &str, before: &Path, after: &Path, from: &str, to: &str) -> Diff {
    let old_requires = Manifest::at(before).map(|m| m.requires).unwrap_or_default();
    let new_requires = Manifest::at(after).map(|m| m.requires).unwrap_or_default();

    Diff {
        name: name.to_string(),
        from: from.to_string(),
        to: to.to_string(),
        new_requirements: new_requires.missing_from(&old_requires),
        declassify_before: declassify_sites(before),
        declassify_after: declassify_sites(after),
    }
}

/// How many `declassify` blocks a package contains, across every `.ko` file.
///
/// Counted rather than located, because the question here is only "did this
/// bump introduce more" — `kora audit` is what says where they are.
fn declassify_sites(root: &Path) -> usize {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "ko") {
                if let Ok(source) = std::fs::read_to_string(&path) {
                    if let Ok(program) = kora_syntax::parse(&source) {
                        total += count_declassify(&program.items);
                    }
                }
            }
        }
    }
    total
}

fn count_declassify(stmts: &[kora_syntax::ast::Stmt]) -> usize {
    use kora_syntax::ast::StmtKind;
    let mut total = 0;
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Declassify { body, .. } => {
                total += 1 + count_declassify(body);
            }
            StmtKind::If {
                branches,
                else_body,
            } => {
                for (_, body) in branches {
                    total += count_declassify(body);
                }
                if let Some(body) = else_body {
                    total += count_declassify(body);
                }
            }
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::ParallelFor { body, .. }
            | StmtKind::WithMock { body, .. }
            | StmtKind::WithBudget { body, .. }
            | StmtKind::Test { body, .. } => total += count_declassify(body),
            StmtKind::FuncDef(def) => total += count_declassify(&def.body),
            StmtKind::Match { arms, .. } => {
                for arm in arms {
                    total += count_declassify(&arm.body);
                }
            }
            _ => {}
        }
    }
    total
}

/// Where a package's checkout sits, for comparing before against after.
pub fn checkout_of(root: &Path, url: &str) -> Option<PathBuf> {
    let lock = Lock::at(root).ok()?;
    let locked = lock.get(url)?;
    let path = crate::lock::deps_dir(root).join(locked.slug());
    path.is_dir().then_some(path)
}

/// Point a manifest entry at a new revision.
pub fn set_revision(root: &Path, name: &str, reference: &GitRef) -> Result<bool, String> {
    let mut doc = crate::edit::open(root)?;
    let changed = crate::edit::set_revision(&mut doc, name, reference);
    if changed {
        crate::edit::save(root, &doc)?;
    }
    Ok(changed)
}

/// Drop a repository's lock entry, so the next install resolves its
/// reference afresh. This is the only sanctioned way past the lockfile.
pub fn unlock(root: &Path, url: &str) -> Result<Option<String>, String> {
    let lock = Lock::at(root)?;
    let previous = lock.get(url).map(|l| l.commit.clone());
    let mut rebuilt = Lock::default();
    for entry in lock.entries() {
        if entry.url != url {
            rebuilt.insert(entry.clone());
        }
    }
    rebuilt.write(root)?;
    Ok(previous)
}

/// Copy the packages a shipped program needs into `vendor/`.
///
/// Distinct from `.kora/deps`, which is a cache: `vendor/` is deliberate and
/// committed, so a program is a directory that moves whole and builds with no
/// network at all. Test-only packages are excluded, because they are not part
/// of what ships.
///
/// Returns the packages copied, by name.
pub fn vendor(entry: &Path, include_tests: bool) -> Result<Vec<String>, String> {
    let root = Manifest::discover(entry).0;
    let resolution = crate::resolve::resolve(entry);
    let wanted = if include_tests {
        resolution.needed()
    } else {
        resolution.shipped()
    };

    let dir = root.join("vendor");
    // Rebuilt from scratch, so a package removed from the graph does not
    // linger in a directory that is supposed to be the shipped set.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    let mut copied = Vec::new();
    for package in wanted {
        let name = package.name.clone().unwrap_or_else(|| "?".to_string());
        copy_tree(&package.root, &dir.join(&name))
            .map_err(|e| format!("cannot vendor `{name}`: {e}"))?;
        copied.push(name);
    }
    copied.sort();
    Ok(copied)
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        // `.git` is a history, and `.kora` is a cache. Neither is part of
        // what a package *is*, and both would bloat what gets committed.
        if matches!(name.to_string_lossy().as_ref(), ".git" | ".kora") {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Grants a package holds, for reporting alongside a diff.
pub fn granted(root: &Path, entry: &Path, name: &str) -> Option<Grants> {
    let resolution = crate::resolve::resolve(entry);
    let _ = root;
    resolution
        .packages
        .iter()
        .find(|p| p.name.as_deref() == Some(name))
        .map(|p| p.grants.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree(PathBuf);

    impl Tree {
        fn new(label: &str, files: &[(&str, &str)]) -> Tree {
            let root = std::env::temp_dir().join(format!("kora-cmd-{label}"));
            let _ = std::fs::remove_dir_all(&root);
            for (path, contents) in files {
                let full = root.join(path);
                std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                std::fs::write(&full, contents).unwrap();
            }
            Tree(root)
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn declassify_sites_are_counted_across_a_package() {
        let tree = Tree::new(
            "count",
            &[
                (
                    "src/lib.ko",
                    "def a(s: str) -> str:\n    declassify s as x for m:\n        return x\n    return \"\"\n",
                ),
                (
                    "src/more.ko",
                    "def b(s: str) -> str:\n    if True:\n        declassify s as x for m:\n            return x\n    return \"\"\n",
                ),
            ],
        );
        assert_eq!(declassify_sites(&tree.0), 2);
    }

    #[test]
    fn a_bump_that_adds_a_declassify_is_flagged() {
        let before = Tree::new(
            "before",
            &[("src/lib.ko", "def a() -> int:\n    return 1\n")],
        );
        let after = Tree::new(
            "after",
            &[(
                "src/lib.ko",
                "def a(s: str) -> str:\n    declassify s as x for m:\n        return x\n    return \"\"\n",
            )],
        );
        let diff = compare("thing", &before.0, &after.0, "v1", "v2");
        assert_eq!(diff.declassify_before, 0);
        assert_eq!(diff.declassify_after, 1);
        assert!(diff.needs_a_look());
    }

    #[test]
    fn a_bump_that_asks_for_more_authority_is_flagged() {
        let before = Tree::new(
            "req-before",
            &[(
                "kora.toml",
                "[package]\nname = \"t\"\n\n[package.requires]\nfs = true\n",
            )],
        );
        let after = Tree::new(
            "req-after",
            &[(
                "kora.toml",
                "[package]\nname = \"t\"\n\n[package.requires]\nfs = true\nnet = true\nsinks = [\"stripe\"]\n",
            )],
        );
        let diff = compare("t", &before.0, &after.0, "v1", "v2");
        assert!(diff.new_requirements.contains(&"net".to_string()));
        assert!(diff.new_requirements.contains(&"sink `stripe`".to_string()));
        assert!(diff.needs_a_look());
    }

    #[test]
    fn an_ordinary_bump_needs_no_second_look() {
        let before = Tree::new(
            "plain-before",
            &[("src/lib.ko", "def a() -> int:\n    return 1\n")],
        );
        let after = Tree::new(
            "plain-after",
            &[("src/lib.ko", "def a() -> int:\n    return 2\n")],
        );
        let diff = compare("t", &before.0, &after.0, "v1", "v2");
        assert!(!diff.needs_a_look());
    }

    #[test]
    fn diff_formats_version_strings() {
        let before = Tree::new(
            "fmt-before",
            &[("src/lib.ko", "def a() -> int:\n    return 1\n")],
        );
        let after = Tree::new(
            "fmt-after",
            &[("src/lib.ko", "def a() -> int:\n    return 2\n")],
        );
        let diff = compare("my_pkg", &before.0, &after.0, "1.0.0", "1.0.1");
        assert_eq!(diff.name, "my_pkg");
        assert_eq!(diff.from, "1.0.0");
        assert_eq!(diff.to, "1.0.1");
    }

    #[test]
    fn diff_detects_multiple_new_requirements() {
        let before = Tree::new(
            "multi-before",
            &[(
                "kora.toml",
                "[package]\nname = \"t\"\n\n[package.requires]\nfs = true\n",
            )],
        );
        let after = Tree::new(
            "multi-after",
            &[(
                "kora.toml",
                "[package]\nname = \"t\"\n\n[package.requires]\nfs = true\nnet = true\nanalyze = true\nsinks = [\"stripe\", \"payment\"]\n",
            )],
        );
        let diff = compare("t", &before.0, &after.0, "v1", "v2");
        assert!(diff.new_requirements.len() >= 3);
        assert!(diff.needs_a_look());
    }

    #[test]
    fn diff_handles_missing_manifest_gracefully() {
        let before = Tree::new(
            "missing-before",
            &[("src/lib.ko", "def a() -> int:\n    return 1\n")],
        );
        let after = Tree::new(
            "missing-after",
            &[("src/lib.ko", "def a() -> int:\n    return 2\n")],
        );
        let diff = compare("t", &before.0, &after.0, "v1", "v2");
        // Should handle gracefully with default Manifest
        assert_eq!(diff.new_requirements.len(), 0);
    }

    #[test]
    fn declassify_sites_handles_no_files() {
        let tree = Tree::new("empty", &[("dummy.txt", "not kora")]);
        assert_eq!(declassify_sites(&tree.0), 0);
    }

    #[test]
    fn declassify_sites_handles_parse_errors() {
        let tree = Tree::new(
            "invalid",
            &[("src/lib.ko", "this is definitely not valid kora code !! @@")],
        );
        // Should handle parse errors and return 0
        assert_eq!(declassify_sites(&tree.0), 0);
    }

    #[test]
    fn declassify_sites_in_nested_structures() {
        let tree = Tree::new(
            "nested",
            &[(
                "src/lib.ko",
                "def outer():\n    def inner():\n        declassify x as y for sink:\n            pass\n    return 1\n",
            )],
        );
        assert_eq!(declassify_sites(&tree.0), 1);
    }

    #[test]
    fn diff_empty_before_has_new_requirements() {
        let before = Tree::new(
            "empty-before",
            &[("kora.toml", "[package]\nname = \"t\"\n")],
        );
        let after = Tree::new(
            "empty-after",
            &[(
                "kora.toml",
                "[package]\nname = \"t\"\n\n[package.requires]\nnet = true\n",
            )],
        );
        let diff = compare("t", &before.0, &after.0, "v1", "v2");
        assert!(diff.needs_a_look());
    }

    #[test]
    fn diff_both_have_same_requirements() {
        let before = Tree::new(
            "same-before",
            &[(
                "kora.toml",
                "[package]\nname = \"t\"\n\n[package.requires]\nfs = true\nnet = true\n",
            )],
        );
        let after = Tree::new(
            "same-after",
            &[(
                "kora.toml",
                "[package]\nname = \"t\"\n\n[package.requires]\nfs = true\nnet = true\n",
            )],
        );
        let diff = compare("t", &before.0, &after.0, "v1", "v2");
        assert!(!diff.needs_a_look());
    }

    #[test]
    fn diff_detects_removed_declassify() {
        let before = Tree::new(
            "decl-before",
            &[(
                "src/lib.ko",
                "def a(s: str) -> str:\n    declassify s as x for m:\n        return x\n    return \"\"\n",
            )],
        );
        let after = Tree::new(
            "decl-after",
            &[("src/lib.ko", "def a(s: str) -> str:\n    return \"\"\n")],
        );
        let diff = compare("t", &before.0, &after.0, "v1", "v2");
        assert_eq!(diff.declassify_before, 1);
        assert_eq!(diff.declassify_after, 0);
        assert!(!diff.needs_a_look()); // Removed declassify doesn't need a look
    }

    #[test]
    fn copy_tree_creates_directory_structure() {
        let src = std::env::temp_dir().join("copy-src");
        let dst = std::env::temp_dir().join("copy-dst");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);

        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("file.txt"), "content").unwrap();
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub").join("nested.txt"), "nested").unwrap();

        copy_tree(&src, &dst).unwrap();

        assert!(dst.exists());
        assert!(dst.join("file.txt").exists());
        assert!(dst.join("sub").join("nested.txt").exists());

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn copy_tree_skips_git_directory() {
        let src = std::env::temp_dir().join("copy-git-src");
        let dst = std::env::temp_dir().join("copy-git-dst");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);

        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::write(src.join(".git").join("config"), "git").unwrap();
        std::fs::write(src.join("file.txt"), "keep").unwrap();

        copy_tree(&src, &dst).unwrap();

        assert!(dst.join("file.txt").exists());
        assert!(!dst.join(".git").exists());

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn copy_tree_skips_kora_cache() {
        let src = std::env::temp_dir().join("copy-kora-src");
        let dst = std::env::temp_dir().join("copy-kora-dst");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);

        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(src.join(".kora")).unwrap();
        std::fs::write(src.join(".kora").join("cache"), "cache").unwrap();
        std::fs::write(src.join("file.txt"), "keep").unwrap();

        copy_tree(&src, &dst).unwrap();

        assert!(dst.join("file.txt").exists());
        assert!(!dst.join(".kora").exists());

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn diff_version_combination_tracking() {
        let before = Tree::new(
            "ver-before",
            &[(
                "kora.toml",
                "[package]\nname = \"lib\"\n\n[package.requires]\n",
            )],
        );
        let after = Tree::new(
            "ver-after",
            &[(
                "kora.toml",
                "[package]\nname = \"lib\"\n\n[package.requires]\nfs = true\n",
            )],
        );
        let diff = compare("lib", &before.0, &after.0, "0.1.0", "0.2.0");
        assert_eq!(diff.from, "0.1.0");
        assert_eq!(diff.to, "0.2.0");
    }
}
