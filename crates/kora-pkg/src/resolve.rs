//! Which packages a program actually uses.
//!
//! The graph is derived from the source, not from the manifest.
//! `[dependencies]` says where a package comes from; the `use pkg` statements
//! say whether it is needed. Declaring a hundred and importing four resolves
//! four, and the same pruning applies transitively — a dependency's own
//! unused entries are never resolved either.
//!
//! Runtime and test reachability are computed by running the same walk twice
//! rather than by propagating a colour through the graph. A package reached
//! by both a test path and a runtime path is a runtime dependency, and the
//! two-pass form gets that right by construction: the runtime pass finds it
//! without ever considering the test path. Propagating "dev" downward instead
//! would mark a shared transitive dependency dev because one of its parents
//! was, and it would go missing from a shipped program.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use kora_syntax::token::Span;

use crate::grants::Grants;
use crate::manifest::{DepSpec, Manifest};
use crate::scan;

/// Index into [`Resolution::packages`]. Zero is the root program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PackageId(pub usize);

/// The root program: the file named on the command line and its own package.
pub const ROOT: PackageId = PackageId(0);

/// One package in the resolved graph.
#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub id: PackageId,
    /// The name the importer wrote. The root program has none.
    pub name: Option<String>,
    /// Canonical package directory, and the identity used for deduplication.
    pub root: PathBuf,
    /// Entry file, absolute.
    pub entry: PathBuf,
    pub manifest: Manifest,
    /// The authority this package actually holds, after being capped by
    /// every parent that granted to it.
    pub grants: Grants,
}

/// A dependency declared in a manifest that no source file imports.
#[derive(Debug, Clone)]
pub struct UnusedDep {
    /// Which package declared it.
    pub declared_by: PackageId,
    pub name: String,
}

/// One package granted two different sets of authority by two importers.
///
/// Silently taking the union would let a permissive importer widen what a
/// careful one allowed; silently taking the intersection would break the
/// permissive one's working code. Both are worse than saying so.
#[derive(Debug, Clone)]
pub struct GrantConflict {
    pub package: String,
    pub first: String,
    pub second: String,
}

/// A package that needs authority nobody granted it.
#[derive(Debug, Clone)]
pub struct GrantShortfall {
    pub package: String,
    pub missing: Vec<String>,
    pub granted: String,
}

/// A `use pkg` that names something the manifest does not declare.
#[derive(Debug, Clone)]
pub struct MissingDep {
    pub name: String,
    pub span: Span,
    /// File the import was written in.
    pub file: PathBuf,
}

#[derive(Debug, Default)]
pub struct Resolution {
    pub packages: Vec<ResolvedPackage>,
    /// Reachable without entering a `test` block.
    pub runtime: HashSet<PackageId>,
    /// Reachable only through the root program's `test` blocks.
    pub dev_only: HashSet<PackageId>,
    pub unused: Vec<UnusedDep>,
    pub missing: Vec<MissingDep>,
    /// One package granted differently by two importers.
    pub grant_conflicts: Vec<GrantConflict>,
    /// Packages whose `[package.requires]` exceeds what they were granted.
    pub shortfalls: Vec<GrantShortfall>,
    /// Files that could not be read or parsed, reported rather than fatal so
    /// one bad file does not hide the rest of the graph.
    pub unreadable: Vec<(PathBuf, String)>,
    /// Manifests that could not be parsed. A swallowed error here reads as
    /// "this package has no dependencies", which is exactly the wrong
    /// conclusion to draw from a typo.
    pub bad_manifests: Vec<(PathBuf, String)>,
}

impl Resolution {
    /// Everything that has to be present for the program to run or be tested.
    pub fn needed(&self) -> Vec<&ResolvedPackage> {
        let mut ids: Vec<&PackageId> = self.runtime.union(&self.dev_only).collect();
        ids.sort();
        ids.into_iter()
            .filter(|id| **id != ROOT)
            .map(|id| &self.packages[id.0])
            .collect()
    }

    /// The package a dependency name refers to, seen from inside `from`.
    ///
    /// Names resolve against the manifest of the package that wrote them,
    /// never a global table, so two packages may bind the same bare name to
    /// different sources.
    pub fn dep_of(&self, from: PackageId, name: &str) -> Option<&ResolvedPackage> {
        let parent = self.packages.get(from.0)?;
        let dep = parent.manifest.deps.get(name)?;
        let DepSpec::Path { path } = &dep.spec;
        let root = canonical(&parent.root.join(path));
        self.packages.iter().find(|p| p.root == root)
    }

    /// What a shipped program needs. Test-only packages are excluded.
    pub fn shipped(&self) -> Vec<&ResolvedPackage> {
        let mut ids: Vec<&PackageId> = self.runtime.iter().collect();
        ids.sort();
        ids.into_iter()
            .filter(|id| **id != ROOT)
            .map(|id| &self.packages[id.0])
            .collect()
    }
}

/// Resolve the package graph of the program whose entry file is `entry`.
pub fn resolve(entry: &Path) -> Resolution {
    let mut state = State::default();

    // The root program's manifest is found by walking up: a program file
    // sits wherever is convenient while its kora.toml sits at the project
    // root. Dependency paths are written relative to that manifest, so the
    // directory it was found in — not the program file's — is the root.
    let (root_dir, root_manifest) = Manifest::discover(entry);
    state.packages.push(ResolvedPackage {
        id: ROOT,
        name: None,
        root: canonical(&root_dir),
        entry: canonical(entry),
        manifest: root_manifest,
        // The program is bounded by its own kora.toml and nothing else.
        grants: Grants::unrestricted(),
    });
    state.by_root.insert(canonical(&root_dir), ROOT);

    // Two walks of the same graph. The runtime pass never enters a `test`
    // block anywhere; the test pass enters them in the root program only,
    // because a dependency's tests are not run by its consumer.
    let runtime = state.walk(false);
    let all = state.walk(true);

    let mut out = Resolution {
        runtime: runtime.clone(),
        dev_only: all.difference(&runtime).copied().collect(),
        packages: std::mem::take(&mut state.packages),
        unused: Vec::new(),
        missing: std::mem::take(&mut state.missing),
        grant_conflicts: std::mem::take(&mut state.grant_conflicts),
        shortfalls: Vec::new(),
        unreadable: std::mem::take(&mut state.unreadable),
        bad_manifests: std::mem::take(&mut state.bad_manifests),
    };

    // A dependency is unused when the package that declared it never names
    // it, in runtime code or in tests.
    for package in &out.packages {
        let seen = state.used.get(&package.id);
        let mut names: Vec<&String> = package.manifest.deps.keys().collect();
        names.sort();
        for name in names {
            if !seen.map(|s| s.contains(name)).unwrap_or(false) {
                out.unused.push(UnusedDep {
                    declared_by: package.id,
                    name: name.clone(),
                });
            }
        }
    }

    // A package that asks for authority nobody gave it fails here, before
    // the run, rather than at whichever call first needs it.
    for package in &out.packages {
        if package.id == ROOT {
            continue;
        }
        let missing = package.manifest.requires.missing_from(&package.grants);
        if !missing.is_empty() {
            out.shortfalls.push(GrantShortfall {
                package: package
                    .name
                    .clone()
                    .unwrap_or_else(|| package.root.display().to_string()),
                missing,
                granted: package.grants.describe(),
            });
        }
    }

    out.missing
        .sort_by(|a, b| (&a.file, a.span.line, &a.name).cmp(&(&b.file, b.span.line, &b.name)));
    out.missing
        .dedup_by(|a, b| a.file == b.file && a.span.line == b.span.line && a.name == b.name);
    out
}

#[derive(Default)]
struct State {
    packages: Vec<ResolvedPackage>,
    by_root: HashMap<PathBuf, PackageId>,
    /// Which dependency names each package was seen to import, across both
    /// passes. Drives the unused report.
    used: HashMap<PackageId, HashSet<String>>,
    missing: Vec<MissingDep>,
    unreadable: Vec<(PathBuf, String)>,
    grant_conflicts: Vec<GrantConflict>,
    bad_manifests: Vec<(PathBuf, String)>,
}

impl State {
    /// Reachable packages, optionally counting the root program's tests.
    fn walk(&mut self, include_root_tests: bool) -> HashSet<PackageId> {
        let mut seen: HashSet<PackageId> = HashSet::new();
        let mut queue = vec![ROOT];
        seen.insert(ROOT);

        while let Some(id) = queue.pop() {
            // A dependency's own `test` blocks are never roots for the
            // program that imports it: only the root program's tests run.
            let tests = include_root_tests && id == ROOT;
            for name in self.package_imports(id, tests) {
                let Some(child) = self.load_dep(id, &name) else {
                    continue;
                };
                if seen.insert(child) {
                    queue.push(child);
                }
            }
        }
        seen
    }

    /// Every `use pkg` name reachable from one package's own files.
    fn package_imports(&mut self, id: PackageId, include_tests: bool) -> Vec<String> {
        let entry = self.packages[id.0].entry.clone();
        let mut files = vec![entry];
        let mut visited: HashSet<PathBuf> = HashSet::new();
        let mut names = Vec::new();

        while let Some(file) = files.pop() {
            let file = canonical(&file);
            if !visited.insert(file.clone()) {
                continue;
            }
            let Some(program) = self.parse(&file) else {
                continue;
            };
            let found = scan::imports(&program, include_tests);
            let dir = file
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));

            for import in found.files {
                files.push(dir.join(&import.name));
            }
            for import in found.packages {
                self.used.entry(id).or_default().insert(import.name.clone());
                if !self.packages[id.0].manifest.deps.contains_key(&import.name) {
                    self.missing.push(MissingDep {
                        name: import.name.clone(),
                        span: import.span,
                        file: file.clone(),
                    });
                    continue;
                }
                names.push(import.name);
            }
        }
        names
    }

    fn parse(&mut self, file: &Path) -> Option<kora_syntax::ast::Program> {
        let source = match std::fs::read_to_string(file) {
            Ok(text) => text,
            Err(e) => {
                self.unreadable.push((file.to_path_buf(), e.to_string()));
                return None;
            }
        };
        match kora_syntax::parse(&source) {
            Ok(program) => Some(program),
            Err(e) => {
                self.unreadable.push((file.to_path_buf(), e.message));
                None
            }
        }
    }

    /// Resolve one dependency name against the manifest of the package that
    /// wrote it, registering the package if this is the first sight of it.
    fn load_dep(&mut self, from: PackageId, name: &str) -> Option<PackageId> {
        let dep = self.packages[from.0].manifest.deps.get(name)?.clone();
        let DepSpec::Path { path } = &dep.spec;
        let root = canonical(&self.packages[from.0].root.join(path));

        // A parent may only pass on what it holds, so an attacker who
        // compromises a leaf gains nothing that every link above it lacked.
        let effective = dep.grants.capped_by(&self.packages[from.0].grants);

        if let Some(id) = self.by_root.get(&root) {
            let existing = &self.packages[id.0];
            if existing.grants != effective {
                self.grant_conflicts.push(GrantConflict {
                    package: name.to_string(),
                    first: existing.grants.describe(),
                    second: effective.describe(),
                });
            }
            return Some(*id);
        }

        let manifest = match Manifest::at(&root) {
            Ok(manifest) => manifest,
            Err(why) => {
                self.bad_manifests
                    .push((root.join("kora.toml"), why.message));
                Manifest::default()
            }
        };
        let entry = canonical(&root.join(manifest.entry()));
        let id = PackageId(self.packages.len());
        self.packages.push(ResolvedPackage {
            id,
            name: Some(name.to_string()),
            root: root.clone(),
            entry,
            manifest,
            grants: effective,
        });
        self.by_root.insert(root, id);
        Some(id)
    }
}

/// Canonicalize when the path exists, so the same package reached by two
/// routes is one entry. A path that does not exist is normalized instead,
/// and the missing file is reported when something tries to read it.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| normalize(path))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a package tree under a fresh temporary directory.
    ///
    /// Each entry is (relative path, contents). Directories are created as
    /// needed, so a test reads as the layout it is describing.
    pub(super) struct Tree {
        root: PathBuf,
    }

    impl Tree {
        pub(super) fn new(label: &str, files: &[(&str, &str)]) -> Tree {
            let root = std::env::temp_dir().join(format!("kora-pkg-{label}"));
            let _ = std::fs::remove_dir_all(&root);
            for (path, contents) in files {
                let full = root.join(path);
                std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                std::fs::write(&full, contents).unwrap();
            }
            Tree { root }
        }

        pub(super) fn resolve(&self, entry: &str) -> Resolution {
            super::resolve(&self.root.join(entry))
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn names(packages: &[&ResolvedPackage]) -> Vec<String> {
        let mut out: Vec<String> = packages.iter().filter_map(|p| p.name.clone()).collect();
        out.sort();
        out
    }

    #[test]
    fn resolves_a_used_dependency_and_prunes_an_unused_one() {
        let tree = Tree::new(
            "prune",
            &[
                (
                    "kora.toml",
                    "[dependencies]\nreceipts = { path = \"./receipts\" }\nunused = { path = \"./unused\" }\n",
                ),
                ("main.ko", "use pkg receipts as r\n\ndef main():\n    print(1)\n"),
                ("receipts/kora.toml", "[package]\nname = \"receipts\"\n"),
                ("receipts/src/lib.ko", "def read() -> int:\n    return 1\n"),
                ("unused/kora.toml", "[package]\nname = \"unused\"\n"),
                ("unused/src/lib.ko", "def never() -> int:\n    return 0\n"),
            ],
        );

        let r = tree.resolve("main.ko");
        assert_eq!(names(&r.needed()), ["receipts"]);
        assert_eq!(r.unused.len(), 1, "{:?}", r.unused);
        assert_eq!(r.unused[0].name, "unused");
        assert!(r.missing.is_empty(), "{:?}", r.missing);
    }

    #[test]
    fn a_dependency_reached_by_both_a_test_and_runtime_path_is_runtime() {
        // The bug that two passes exist to avoid: propagating "dev" downward
        // from `fixtures` would mark `shared` dev-only, and it would go
        // missing from a shipped program even though `receipts` needs it.
        let tree = Tree::new(
            "union",
            &[
                (
                    "kora.toml",
                    "[dependencies]\nreceipts = { path = \"./receipts\" }\nfixtures = { path = \"./fixtures\" }\n",
                ),
                (
                    "main.ko",
                    "use pkg receipts as r\n\ndef main():\n    print(1)\n\ntest \"t\":\n    use pkg fixtures as f\n    assert True, \"x\"\n",
                ),
                (
                    "receipts/kora.toml",
                    "[package]\nname = \"receipts\"\n\n[dependencies]\nshared = { path = \"../shared\" }\n",
                ),
                ("receipts/src/lib.ko", "use pkg shared as s\n"),
                (
                    "fixtures/kora.toml",
                    "[package]\nname = \"fixtures\"\n\n[dependencies]\nshared = { path = \"../shared\" }\n",
                ),
                ("fixtures/src/lib.ko", "use pkg shared as s\n"),
                ("shared/kora.toml", "[package]\nname = \"shared\"\n"),
                ("shared/src/lib.ko", "def helper() -> int:\n    return 1\n"),
            ],
        );

        let r = tree.resolve("main.ko");
        assert_eq!(names(&r.needed()), ["fixtures", "receipts", "shared"]);
        // Shipped drops fixtures, keeps shared.
        assert_eq!(names(&r.shipped()), ["receipts", "shared"]);
    }

    #[test]
    fn a_package_reached_only_through_tests_is_dev_only() {
        let tree = Tree::new(
            "devonly",
            &[
                ("kora.toml", "[dependencies]\nfixtures = { path = \"./fixtures\" }\n"),
                (
                    "main.ko",
                    "def main():\n    print(1)\n\ntest \"t\":\n    use pkg fixtures as f\n    assert True, \"x\"\n",
                ),
                ("fixtures/kora.toml", "[package]\nname = \"fixtures\"\n"),
                ("fixtures/src/lib.ko", "def fake() -> int:\n    return 1\n"),
            ],
        );

        let r = tree.resolve("main.ko");
        assert_eq!(names(&r.needed()), ["fixtures"]);
        assert!(r.shipped().is_empty());
        assert_eq!(r.dev_only.len(), 1);
    }

    #[test]
    fn a_dependencys_own_tests_are_not_roots_for_the_consumer() {
        // `receipts` uses `heavy` in its own tests only. A consumer never
        // runs those tests, so `heavy` is not part of the consumer's graph.
        let tree = Tree::new(
            "deptests",
            &[
                ("kora.toml", "[dependencies]\nreceipts = { path = \"./receipts\" }\n"),
                ("main.ko", "use pkg receipts as r\n"),
                (
                    "receipts/kora.toml",
                    "[package]\nname = \"receipts\"\n\n[dependencies]\nheavy = { path = \"../heavy\" }\n",
                ),
                (
                    "receipts/src/lib.ko",
                    "def read() -> int:\n    return 1\n\ntest \"t\":\n    use pkg heavy as h\n    assert True, \"x\"\n",
                ),
                ("heavy/kora.toml", "[package]\nname = \"heavy\"\n"),
                ("heavy/src/lib.ko", "def big() -> int:\n    return 1\n"),
            ],
        );

        let r = tree.resolve("main.ko");
        assert_eq!(names(&r.needed()), ["receipts"]);
        // `heavy` is declared by receipts and never imported outside its
        // tests, so it is reported unused rather than silently fetched.
        assert!(r.unused.iter().any(|u| u.name == "heavy"), "{:?}", r.unused);
    }

    #[test]
    fn each_package_resolves_names_against_its_own_manifest() {
        // Both packages import the bare name `helper`, pointing at different
        // directories. Neither may see the other's table.
        let tree = Tree::new(
            "ownmanifest",
            &[
                (
                    "kora.toml",
                    "[dependencies]\nleft = { path = \"./left\" }\nright = { path = \"./right\" }\n",
                ),
                ("main.ko", "use pkg left as l\nuse pkg right as r\n"),
                (
                    "left/kora.toml",
                    "[package]\nname = \"left\"\n\n[dependencies]\nhelper = { path = \"../helper_a\" }\n",
                ),
                ("left/src/lib.ko", "use pkg helper as h\n"),
                (
                    "right/kora.toml",
                    "[package]\nname = \"right\"\n\n[dependencies]\nhelper = { path = \"../helper_b\" }\n",
                ),
                ("right/src/lib.ko", "use pkg helper as h\n"),
                ("helper_a/kora.toml", "[package]\nname = \"helper_a\"\n"),
                ("helper_a/src/lib.ko", "def a() -> int:\n    return 1\n"),
                ("helper_b/kora.toml", "[package]\nname = \"helper_b\"\n"),
                ("helper_b/src/lib.ko", "def b() -> int:\n    return 2\n"),
            ],
        );

        let r = tree.resolve("main.ko");
        // Four dependencies: left, right, and one distinct helper for each.
        assert_eq!(r.needed().len(), 4, "{:?}", names(&r.needed()));
        let roots: HashSet<&PathBuf> = r.needed().iter().map(|p| &p.root).collect();
        assert_eq!(roots.len(), 4);
        assert!(r.missing.is_empty(), "{:?}", r.missing);
    }

    #[test]
    fn the_root_manifest_is_found_by_walking_up_from_the_program() {
        // A program sits wherever is convenient; its kora.toml sits at the
        // project root, and dependency paths are relative to that manifest.
        let tree = Tree::new(
            "discover",
            &[
                (
                    "kora.toml",
                    "[dependencies]\ngreet = { path = \"./lib/greet\" }\n",
                ),
                ("examples/demo.ko", "use pkg greet as g\n"),
                ("lib/greet/kora.toml", "[package]\nname = \"greet\"\n"),
                ("lib/greet/src/lib.ko", "def hi() -> int:\n    return 1\n"),
            ],
        );
        let r = tree.resolve("examples/demo.ko");
        assert!(r.missing.is_empty(), "{:?}", r.missing);
        assert_eq!(names(&r.needed()), ["greet"]);
    }

    #[test]
    fn a_use_of_an_undeclared_package_is_reported() {
        let tree = Tree::new(
            "missing",
            &[
                ("kora.toml", "[dependencies]\n"),
                ("main.ko", "use pkg ghost as g\n"),
            ],
        );
        let r = tree.resolve("main.ko");
        assert_eq!(r.missing.len(), 1, "{:?}", r.missing);
        assert_eq!(r.missing[0].name, "ghost");
    }

    #[test]
    fn imports_are_followed_through_a_packages_own_files() {
        let tree = Tree::new(
            "files",
            &[
                (
                    "kora.toml",
                    "[dependencies]\nreceipts = { path = \"./receipts\" }\n",
                ),
                ("main.ko", "use \"./lib/inner.ko\" as inner\n"),
                ("lib/inner.ko", "use pkg receipts as r\n"),
                ("receipts/kora.toml", "[package]\nname = \"receipts\"\n"),
                ("receipts/src/lib.ko", "def read() -> int:\n    return 1\n"),
            ],
        );
        let r = tree.resolve("main.ko");
        assert_eq!(names(&r.needed()), ["receipts"]);
    }
}

#[cfg(test)]
mod grant_tests {
    use super::tests::Tree;
    use super::*;
    use crate::grants::Capability;

    #[test]
    fn a_dependency_holds_only_what_the_program_granted() {
        let tree = Tree::new(
            "granted",
            &[
                (
                    "kora.toml",
                    "[dependencies.receipts]\npath = \"./receipts\"\ngrants = { net = true }\n",
                ),
                ("main.ko", "use pkg receipts as r\n"),
                ("receipts/kora.toml", "[package]\nname = \"receipts\"\n"),
                ("receipts/src/lib.ko", "def read() -> int:\n    return 1\n"),
            ],
        );
        let r = tree.resolve("main.ko");
        let receipts = r.needed()[0];
        assert!(receipts.grants.allows(Capability::Net));
        assert!(!receipts.grants.allows(Capability::Fs));
        assert!(!receipts.grants.allows_declassify());
    }

    #[test]
    fn a_dependency_cannot_pass_on_what_it_lacks() {
        // `receipts` was granted only net, so the fs it hands `helper` is
        // not its to give.
        let tree = Tree::new(
            "capped",
            &[
                (
                    "kora.toml",
                    "[dependencies.receipts]\npath = \"./receipts\"\ngrants = { net = true }\n",
                ),
                ("main.ko", "use pkg receipts as r\n"),
                (
                    "receipts/kora.toml",
                    "[package]\nname = \"receipts\"\n\n[dependencies.helper]\npath = \"../helper\"\ngrants = { net = true, fs = true }\n",
                ),
                ("receipts/src/lib.ko", "use pkg helper as h\n"),
                ("helper/kora.toml", "[package]\nname = \"helper\"\n"),
                ("helper/src/lib.ko", "def x() -> int:\n    return 1\n"),
            ],
        );
        let r = tree.resolve("main.ko");
        let helper = r
            .needed()
            .into_iter()
            .find(|p| p.name.as_deref() == Some("helper"))
            .expect("helper resolved");
        assert!(helper.grants.allows(Capability::Net));
        assert!(
            !helper.grants.allows(Capability::Fs),
            "fs was never receipts' to grant"
        );
    }

    #[test]
    fn a_package_asking_for_more_than_it_was_given_is_reported() {
        let tree = Tree::new(
            "shortfall",
            &[
                (
                    "kora.toml",
                    "[dependencies]\nreceipts = { path = \"./receipts\" }\n",
                ),
                ("main.ko", "use pkg receipts as r\n"),
                (
                    "receipts/kora.toml",
                    "[package]\nname = \"receipts\"\n\n[package.requires]\nnet = true\nsinks = [\"stripe\"]\n",
                ),
                ("receipts/src/lib.ko", "def read() -> int:\n    return 1\n"),
            ],
        );
        let r = tree.resolve("main.ko");
        assert_eq!(r.shortfalls.len(), 1, "{:?}", r.shortfalls);
        assert!(r.shortfalls[0].missing.contains(&"net".to_string()));
        assert!(r.shortfalls[0]
            .missing
            .contains(&"sink `stripe`".to_string()));
    }

    #[test]
    fn one_package_granted_two_different_ways_is_a_conflict() {
        // Union would let `right` widen what `left` carefully withheld;
        // intersection would break `right`. Saying so beats both.
        let tree = Tree::new(
            "conflict",
            &[
                (
                    "kora.toml",
                    "[dependencies.left]\npath = \"./left\"\ngrants = { net = true, fs = true }\n\n[dependencies.right]\npath = \"./right\"\ngrants = { net = true, fs = true }\n",
                ),
                ("main.ko", "use pkg left as l\nuse pkg right as r\n"),
                (
                    "left/kora.toml",
                    "[package]\nname = \"left\"\n\n[dependencies.shared]\npath = \"../shared\"\ngrants = { net = true }\n",
                ),
                ("left/src/lib.ko", "use pkg shared as s\n"),
                (
                    "right/kora.toml",
                    "[package]\nname = \"right\"\n\n[dependencies.shared]\npath = \"../shared\"\ngrants = { fs = true }\n",
                ),
                ("right/src/lib.ko", "use pkg shared as s\n"),
                ("shared/kora.toml", "[package]\nname = \"shared\"\n"),
                ("shared/src/lib.ko", "def x() -> int:\n    return 1\n"),
            ],
        );
        let r = tree.resolve("main.ko");
        assert!(!r.grant_conflicts.is_empty(), "expected a conflict");
        assert_eq!(r.grant_conflicts[0].package, "shared");
    }

    #[test]
    fn a_manifest_that_does_not_parse_is_reported_not_swallowed() {
        // Treating a broken manifest as an empty one reads as "this package
        // has no dependencies", which is the wrong conclusion to draw from a
        // typo — and, with grants, the wrong conclusion to draw about what a
        // package was allowed to do.
        let tree = Tree::new(
            "badmanifest",
            &[
                (
                    "kora.toml",
                    "[dependencies]\nbroken = { path = \"./broken\" }\n",
                ),
                ("main.ko", "use pkg broken as b\n"),
                ("broken/kora.toml", "[package\nname = oops\n"),
                ("broken/src/lib.ko", "def x() -> int:\n    return 1\n"),
            ],
        );
        let r = tree.resolve("main.ko");
        assert_eq!(r.bad_manifests.len(), 1, "{:?}", r.bad_manifests);
    }

    #[test]
    fn the_root_program_is_unrestricted() {
        let tree = Tree::new(
            "rootfree",
            &[
                ("kora.toml", "[dependencies]\n"),
                ("main.ko", "def main():\n    print(1)\n"),
            ],
        );
        let r = tree.resolve("main.ko");
        assert!(r.packages[ROOT.0].grants.is_unrestricted());
    }
}
