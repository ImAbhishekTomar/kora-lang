//! The pattern matcher behind `fs.glob`.
//!
//! Driven by the pattern rather than by the tree: only directories the
//! pattern can still reach are opened. `dataset/*.png` reads one directory,
//! not the repository.
//!
//! `*` matches within one path component, `?` matches one character, and
//! `**` matches any run of directories. Dotfiles are skipped unless the
//! pattern component itself starts with a dot, so `*` does not sweep up
//! `.DS_Store` and `.git`.

use std::path::{Path, PathBuf};

/// Every existing path matching `pattern`, relative to the process's working
/// directory, sorted.
///
/// Sorted because directory order is whatever the filesystem feels like, and
/// an agent program fans this list out across threads: an unstable order
/// would make a durable replay visit its work in a different sequence.
pub fn expand(pattern: &str) -> Result<Vec<String>, String> {
    expand_in(Path::new("."), pattern)
}

/// `expand`, rooted at `root` instead of the working directory.
///
/// The root is a parameter so the matcher can be tested against a scratch
/// tree without a test changing the process's working directory out from
/// under every other test in the binary.
pub fn expand_in(root: &Path, pattern: &str) -> Result<Vec<String>, String> {
    let (base, shown, components) = split(root, pattern)?;
    if components.is_empty() {
        return Err("the pattern is empty".to_string());
    }
    let parts: Vec<&str> = components.iter().map(|c| c.as_str()).collect();
    let mut out = Vec::new();
    walk(&base, &shown, &parts, &mut out)?;
    out.sort();
    out.dedup();
    Ok(out)
}

/// Where to start walking, what to call it, and what is left to match.
///
/// An absolute pattern starts at its own root and keeps it in the results, so
/// `fs.glob("/srv/receipts/*.png")` returns paths that open again. A relative
/// one starts at `root` and stays relative.
fn split(root: &Path, pattern: &str) -> Result<(PathBuf, String, Vec<String>), String> {
    use std::path::Component;

    let mut base = PathBuf::new();
    let mut shown = String::new();
    let mut components = Vec::new();
    for component in Path::new(pattern).components() {
        match component {
            // A Windows drive letter or UNC share.
            Component::Prefix(prefix) => {
                base.push(prefix.as_os_str());
                shown.push_str(&prefix.as_os_str().to_string_lossy());
            }
            Component::RootDir => {
                base.push(std::path::MAIN_SEPARATOR_STR);
                shown.push('/');
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err("a pattern cannot contain `..`".to_string());
            }
            Component::Normal(name) => match name.to_str() {
                Some(name) => components.push(name.to_string()),
                None => return Err("the pattern is not valid UTF-8".to_string()),
            },
        }
    }
    if base.as_os_str().is_empty() {
        base = root.to_path_buf();
    }
    Ok((base, shown, components))
}

/// Whether `pattern` contains anything a literal path would not.
pub fn is_pattern(text: &str) -> bool {
    text.contains(['*', '?'])
}

/// Descend one pattern component at a time.
///
/// `base` is where to read from; `shown` is the path as the program will see
/// it, built with `/` on every platform so a program means the same thing
/// everywhere.
fn walk(
    base: &Path,
    shown: &str,
    components: &[&str],
    out: &mut Vec<String>,
) -> Result<(), String> {
    let Some((head, rest)) = components.split_first() else {
        if !shown.is_empty() && base.exists() {
            out.push(shown.to_string());
        }
        return Ok(());
    };

    if *head == "**" {
        // Zero directories: `a/**/b` must also match `a/b`.
        walk(base, shown, rest, out)?;
        for (name, path) in children(base)? {
            // Symlinked directories are not followed: a link pointing at an
            // ancestor turns `**` into an infinite walk.
            if path.is_dir() && !path.is_symlink() {
                walk(&path, &join(shown, &name), components, out)?;
            }
        }
        return Ok(());
    }

    if !is_pattern(head) {
        let next = base.join(head);
        if next.exists() {
            walk(&next, &join(shown, head), rest, out)?;
        }
        return Ok(());
    }

    for (name, path) in children(base)? {
        if matches_component(head, &name) {
            walk(&path, &join(shown, &name), rest, out)?;
        }
    }
    Ok(())
}

fn join(shown: &str, name: &str) -> String {
    if shown.is_empty() {
        name.to_string()
    } else if shown.ends_with('/') {
        // Already at a root; another separator would double it.
        format!("{shown}{name}")
    } else {
        format!("{shown}/{name}")
    }
}

/// Entries of `base`, skipping names that are not valid UTF-8.
///
/// A path Kora cannot represent as a `str` is skipped rather than mangled
/// with a replacement character: a lossy name would not open again.
fn children(base: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let entries = match std::fs::read_dir(base) {
        Ok(entries) => entries,
        // A directory that cannot be read is not a match; only the top-level
        // caller reports "no such directory".
        Err(_) => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            out.push((name.to_string(), entry.path()));
        }
    }
    Ok(out)
}

/// Match one path component against one pattern component.
fn matches_component(pattern: &str, name: &str) -> bool {
    // A dotfile is only reachable when the pattern asks for one, so `*` does
    // not quietly pick up `.git` or `.DS_Store`.
    if name.starts_with('.') && !pattern.starts_with('.') {
        return false;
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    matches_from(&pattern, &name)
}

/// Greedy-with-backtracking wildcard match, iterative so a long name cannot
/// blow the stack.
fn matches_from(pattern: &[char], name: &[char]) -> bool {
    let (mut p, mut n) = (0usize, 0usize);
    // Where to resume if the current `*` turns out to have eaten too little.
    let mut star: Option<(usize, usize)> = None;

    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, n));
                p += 1;
            }
            Some('?') => {
                p += 1;
                n += 1;
            }
            Some(c) if *c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some((sp, sn)) => {
                    p = sp + 1;
                    n = sn + 1;
                    star = Some((sp, sn + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_stays_inside_one_component() {
        assert!(matches_component("*.png", "0.png"));
        assert!(matches_component("*", "anything"));
        assert!(!matches_component("*.png", "0.jpg"));
        assert!(matches_component("receipt-*.png", "receipt-01.png"));
        assert!(!matches_component("receipt-*.png", "invoice-01.png"));
    }

    #[test]
    fn question_mark_is_exactly_one_character() {
        assert!(matches_component("?.png", "0.png"));
        assert!(!matches_component("?.png", "10.png"));
        assert!(matches_component("??.png", "10.png"));
    }

    #[test]
    fn dotfiles_need_to_be_asked_for() {
        assert!(!matches_component("*", ".git"));
        assert!(!matches_component("*.json", ".eslintrc.json"));
        assert!(matches_component(".*", ".git"));
    }

    /// The backtracking case: the first `*` must give characters back.
    #[test]
    fn multiple_stars_backtrack() {
        assert!(matches_component("*a*b", "xxaybzzab"));
        assert!(!matches_component("*a*b", "xxaybzz"));
        assert!(matches_component("*", ""));
        assert!(matches_component("**", "anything"));
    }

    #[test]
    fn is_pattern_only_for_wildcards() {
        assert!(is_pattern("*.png"));
        assert!(is_pattern("0?.png"));
        assert!(!is_pattern("dataset/0.png"));
    }

    /// Everything above is pure matching; this is the part that touches disk.
    #[test]
    fn expand_walks_a_real_tree() {
        let dir = std::env::temp_dir().join(format!("kora-glob-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("nested/deep")).unwrap();
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        std::fs::write(dir.join("b.png"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.join(".hidden.png"), b"x").unwrap();
        std::fs::write(dir.join("nested/deep/c.png"), b"x").unwrap();

        let flat = expand_in(&dir, "*.png").unwrap();
        assert_eq!(flat, vec!["a.png", "b.png"]);

        let recursive = expand_in(&dir, "**/*.png").unwrap();
        assert_eq!(recursive, vec!["a.png", "b.png", "nested/deep/c.png"]);

        // A literal component still has to exist.
        assert_eq!(
            expand_in(&dir, "nested/deep/c.png").unwrap(),
            vec!["nested/deep/c.png"]
        );
        assert!(expand_in(&dir, "*.pdf").unwrap().is_empty());
        assert!(expand_in(&dir, "missing/*.png").unwrap().is_empty());

        // An absolute pattern keeps its root, so results open again.
        let absolute = format!("{}/*.png", dir.to_string_lossy());
        let found = expand(&absolute).unwrap();
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(std::path::Path::new(&found[0]).is_absolute(), "{found:?}");
        assert!(std::fs::read(&found[0]).is_ok(), "{found:?}");

        std::fs::remove_dir_all(&dir).ok();
    }
}
