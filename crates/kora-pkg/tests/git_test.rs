//! Fetching from a real repository, and the attack the lockfile exists for.
//!
//! These build actual git repositories on disk. What they pin down is that a
//! version number cannot come to mean two different things: once a repository
//! is locked, moving the tag it was locked from changes nothing about what
//! runs, on a warm cache or a cold one.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("kora-git-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git must be installed");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository with one package in it, tagged `v1.0.0`.
fn upstream(scratch: &Scratch, body: &str) -> PathBuf {
    let repo = scratch.0.join("upstream");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("kora.toml"), "[package]\nname = \"greet\"\n").unwrap();
    std::fs::write(repo.join("src/lib.ko"), body).unwrap();
    git(&repo, &["init", "-q", "."]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "one"]);
    git(&repo, &["tag", "v1.0.0"]);
    repo
}

/// An app depending on that repository at `v1.0.0`.
fn app(scratch: &Scratch, repo: &Path) -> PathBuf {
    scratch.write(
        "app/kora.toml",
        &format!(
            "[dependencies.greet]\ngit = {:?}\ntag = \"v1.0.0\"\n",
            repo.display().to_string()
        ),
    );
    scratch.write("app/main.ko", "use pkg greet as g\n")
}

#[test]
fn a_git_dependency_is_fetched_locked_and_resolved() {
    let scratch = Scratch::new("fetch");
    let repo = upstream(&scratch, "def hello() -> str:\n    return \"honest\"\n");
    let main = app(&scratch, &repo);

    let outcome = kora_pkg::install(&main, 4, true);
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert_eq!(outcome.fetched.len(), 1);
    assert!(outcome.lock_changed);

    let resolution = &outcome.resolution;
    assert!(
        resolution.unfetched.is_empty(),
        "{:?}",
        resolution.unfetched
    );
    assert!(resolution.tampered.is_empty(), "{:?}", resolution.tampered);
    assert_eq!(resolution.needed().len(), 1);

    let lock = kora_pkg::Lock::at(&scratch.0.join("app")).unwrap();
    let locked = lock.entries().next().expect("one entry");
    assert_eq!(locked.reference, "v1.0.0");
    assert_eq!(locked.commit.len(), 40, "the commit, not the tag");
    assert!(locked.hash.starts_with("sha256:"));
}

#[test]
fn a_moved_tag_does_not_change_what_runs_on_a_cold_cache() {
    // The attack the lockfile exists for. A maintainer account is taken over,
    // the tag is force-pushed to a backdoored commit, and CI — which has no
    // cache — fetches fresh. Re-resolving the *tag* is what would land it.
    let scratch = Scratch::new("forcepush");
    let repo = upstream(&scratch, "def hello() -> str:\n    return \"honest\"\n");
    let main = app(&scratch, &repo);

    let first = kora_pkg::install(&main, 4, true);
    assert!(first.failed.is_empty(), "{:?}", first.failed);
    let honest = kora_pkg::Lock::at(&scratch.0.join("app"))
        .unwrap()
        .entries()
        .next()
        .unwrap()
        .clone();

    std::fs::write(
        repo.join("src/lib.ko"),
        "def hello() -> str:\n    return \"backdoor\"\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "backdoor"]);
    git(&repo, &["tag", "-f", "v1.0.0"]);

    std::fs::remove_dir_all(scratch.0.join("app/.kora")).unwrap();

    let second = kora_pkg::install(&main, 4, true);
    assert!(second.failed.is_empty(), "{:?}", second.failed);

    let after = kora_pkg::Lock::at(&scratch.0.join("app"))
        .unwrap()
        .entries()
        .next()
        .unwrap()
        .clone();
    assert_eq!(
        after.commit, honest.commit,
        "the locked commit must survive a moved tag"
    );
    assert_eq!(after.hash, honest.hash, "and so must its contents");

    let checkout = kora_pkg::deps_dir(&scratch.0.join("app")).join(after.slug());
    let source = std::fs::read_to_string(checkout.join("src/lib.ko")).unwrap();
    assert!(source.contains("honest"), "got: {source}");
    assert!(!source.contains("backdoor"));
}

#[test]
fn contents_that_disagree_with_the_lock_are_refused() {
    let scratch = Scratch::new("rewritten");
    let repo = upstream(&scratch, "def hello() -> str:\n    return \"honest\"\n");
    let main = app(&scratch, &repo);

    kora_pkg::install(&main, 4, true);

    // Stand in for a rewritten repository: the lock claims a hash the commit
    // does not produce.
    let lock_path = scratch.0.join("app/kora.lock");
    let text = std::fs::read_to_string(&lock_path).unwrap();
    let doctored = text
        .lines()
        .map(|line| {
            if line.starts_with("hash = ") {
                "hash = \"sha256:0000000000000000000000000000000000000000000000000000000000000000\""
                    .to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&lock_path, doctored).unwrap();
    std::fs::remove_dir_all(scratch.0.join("app/.kora")).unwrap();

    let outcome = kora_pkg::install(&main, 4, true);
    assert_eq!(outcome.failed.len(), 1, "{:?}", outcome.failed);
    assert!(
        outcome.failed[0].1.contains("does not match the lockfile"),
        "{}",
        outcome.failed[0].1
    );
}

#[test]
fn an_unfetched_dependency_is_reported_rather_than_guessed() {
    let scratch = Scratch::new("unfetched");
    let repo = upstream(&scratch, "def hello() -> str:\n    return \"x\"\n");
    let main = app(&scratch, &repo);

    let resolution = kora_pkg::resolve(&main);
    assert_eq!(resolution.unfetched.len(), 1, "{:?}", resolution.unfetched);
    assert!(resolution.needed().is_empty());
}
