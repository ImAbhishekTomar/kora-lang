//! Content hashing for a package tree.
//!
//! What is hashed is the *contents*, not the archive: a walk in sorted order,
//! each file's path and bytes fed to one digest. Two checkouts of the same
//! commit therefore hash the same on any machine, whatever order the
//! filesystem hands back and whatever the tarball framing was.
//!
//! This is what makes a version number mean one thing forever. A tag can be
//! moved and a repository can be rewritten; a content hash recorded in the
//! lockfile cannot follow it.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Directories never part of a package's identity.
///
/// `.git` holds a whole history whose bytes differ between a shallow clone
/// and a full one, so including it would make the same commit hash
/// differently depending on how it was fetched. `.kora` is the dependency
/// cache: a package's hash must not depend on what it has downloaded.
const SKIP: &[&str] = &[".git", ".kora", "target"];

/// Hash every file under `root`, in sorted order.
pub fn tree(root: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    // Sorted, because a filesystem's own order differs between machines and
    // an unstable order would make the hash meaningless.
    files.sort();

    let mut digest = Sha256::new();
    for relative in &files {
        let bytes = std::fs::read(root.join(relative))
            .map_err(|e| format!("cannot read {}: {e}", relative.display()))?;
        // The path is hashed with a length prefix, so renaming a file across
        // a boundary cannot produce the same digest as moving its contents.
        let path = relative.to_string_lossy().replace('\\', "/");
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(&bytes);
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if SKIP.contains(&name.as_ref()) {
            continue;
        }
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            collect(root, &path, out)?;
        } else if kind.is_file() {
            // Symlinks are skipped: their target is outside the tree being
            // hashed, so what they point at could change without the hash.
            let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
            out.push(relative.to_path_buf());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree(PathBuf);

    impl Tree {
        fn new(label: &str, files: &[(&str, &str)]) -> Tree {
            let root = std::env::temp_dir().join(format!("kora-hash-{label}"));
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
    fn the_same_contents_hash_the_same() {
        let a = Tree::new("same-a", &[("src/lib.ko", "def x():\n    return 1\n")]);
        let b = Tree::new("same-b", &[("src/lib.ko", "def x():\n    return 1\n")]);
        assert_eq!(tree(&a.0).unwrap(), tree(&b.0).unwrap());
    }

    #[test]
    fn changed_contents_change_the_hash() {
        let a = Tree::new("diff-a", &[("src/lib.ko", "def x():\n    return 1\n")]);
        let b = Tree::new("diff-b", &[("src/lib.ko", "def x():\n    return 2\n")]);
        assert_ne!(tree(&a.0).unwrap(), tree(&b.0).unwrap());
    }

    #[test]
    fn moving_contents_between_files_changes_the_hash() {
        // Without hashing the path, "ab" in one file and "a" + "b" split
        // across two would digest identically.
        let a = Tree::new("move-a", &[("one.ko", "ab")]);
        let b = Tree::new("move-b", &[("one.ko", "a"), ("two.ko", "b")]);
        assert_ne!(tree(&a.0).unwrap(), tree(&b.0).unwrap());
    }

    #[test]
    fn renaming_a_file_changes_the_hash() {
        let a = Tree::new("rename-a", &[("one.ko", "same")]);
        let b = Tree::new("rename-b", &[("two.ko", "same")]);
        assert_ne!(tree(&a.0).unwrap(), tree(&b.0).unwrap());
    }

    #[test]
    fn the_git_directory_is_not_part_of_a_packages_identity() {
        // A shallow clone and a full one of the same commit hold different
        // bytes under .git. Including it would make the hash depend on how
        // the package was fetched.
        let a = Tree::new("git-a", &[("src/lib.ko", "x"), (".git/HEAD", "shallow")]);
        let b = Tree::new(
            "git-b",
            &[("src/lib.ko", "x"), (".git/HEAD", "full history")],
        );
        assert_eq!(tree(&a.0).unwrap(), tree(&b.0).unwrap());
    }

    #[test]
    fn a_hash_names_its_algorithm() {
        let t = Tree::new("named", &[("a.ko", "x")]);
        assert!(tree(&t.0).unwrap().starts_with("sha256:"));
    }
}
