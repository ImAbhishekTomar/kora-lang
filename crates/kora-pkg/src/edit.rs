//! Editing a `kora.toml` without destroying it.
//!
//! A manifest is written by a person: it has comments explaining why a
//! dependency is pinned where it is, and an order that means something to
//! whoever arranged it. A tool that reparses and reserializes throws all of
//! that away on the first `kora add`. These edits are format-preserving, so
//! adding a dependency changes exactly the lines it needs to.

use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, Value};

use crate::manifest::{DepSpec, GitRef};

/// What an edit changed, for reporting.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    Added,
    /// Already present, and pointing at the same source.
    Unchanged,
    /// Already present, pointing somewhere else.
    Replaced {
        previous: String,
    },
    Removed,
    /// Not there to begin with.
    Absent,
}

/// Read a manifest for editing, or start an empty one.
pub fn open(root: &Path) -> Result<DocumentMut, String> {
    let path = root.join("kora.toml");
    if !path.is_file() {
        return Ok(DocumentMut::new());
    }
    std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?
        .parse::<DocumentMut>()
        .map_err(|e| format!("kora.toml is not valid TOML: {e}"))
}

pub fn save(root: &Path, doc: &DocumentMut) -> Result<(), String> {
    let path = root.join("kora.toml");
    std::fs::write(&path, doc.to_string())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Add or update one dependency.
pub fn add(doc: &mut DocumentMut, name: &str, spec: &DepSpec) -> Change {
    let deps = dependencies(doc);
    let previous = deps.get(name).map(describe);

    let mut table = Table::new();
    // Written as a table rather than an inline one, because grants have to
    // go somewhere and TOML forbids extending an inline table.
    table.set_implicit(false);
    match spec {
        DepSpec::Path { path } => {
            table["path"] = value_of(&path.display().to_string());
        }
        DepSpec::Git { url, reference } => {
            table["git"] = value_of(url);
            match reference {
                GitRef::Tag(t) => table["tag"] = value_of(t),
                GitRef::Branch(b) => table["branch"] = value_of(b),
                GitRef::Commit(c) => table["rev"] = value_of(c),
                GitRef::Default => {}
            }
        }
    }

    // Grants already written by hand are kept: `kora add` on an existing
    // dependency must not quietly hand it more authority than it had.
    if let Some(existing) = deps.get(name).and_then(|item| item.get("grants")) {
        table["grants"] = existing.clone();
    }

    let new = describe(&Item::Table(table.clone()));
    deps[name] = Item::Table(table);

    match previous {
        None => Change::Added,
        Some(before) if before == new => Change::Unchanged,
        Some(before) => Change::Replaced { previous: before },
    }
}

/// Remove one dependency.
pub fn remove(doc: &mut DocumentMut, name: &str) -> Change {
    let deps = dependencies(doc);
    let Some(table) = deps.as_table_like_mut() else {
        return Change::Absent;
    };
    if table.remove(name).is_none() {
        return Change::Absent;
    }
    Change::Removed
}

/// Point an existing git dependency at a different revision.
pub fn set_revision(doc: &mut DocumentMut, name: &str, reference: &GitRef) -> bool {
    let deps = dependencies(doc);
    let Some(entry) = deps.get_mut(name) else {
        return false;
    };
    for key in ["tag", "branch", "rev"] {
        if let Some(table) = entry.as_table_like_mut() {
            table.remove(key);
        }
    }
    match reference {
        GitRef::Tag(t) => entry["tag"] = value_of(t),
        GitRef::Branch(b) => entry["branch"] = value_of(b),
        GitRef::Commit(c) => entry["rev"] = value_of(c),
        GitRef::Default => {}
    }
    true
}

fn dependencies(doc: &mut DocumentMut) -> &mut Item {
    if doc.get("dependencies").is_none() {
        let mut table = Table::new();
        table.set_implicit(true);
        doc["dependencies"] = Item::Table(table);
    }
    &mut doc["dependencies"]
}

fn value_of(text: &str) -> Item {
    Item::Value(Value::from(text))
}

/// A one-line summary of where a dependency points, for reporting a change.
fn describe(item: &Item) -> String {
    let get = |key: &str| item.get(key).and_then(|v| v.as_str()).map(str::to_string);
    if let Some(path) = get("path") {
        return format!("path {path}");
    }
    let Some(url) = get("git") else {
        return "?".to_string();
    };
    match get("tag").or_else(|| get("branch")).or_else(|| get("rev")) {
        Some(reference) => format!("{url} at {reference}"),
        None => url,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn doc(text: &str) -> DocumentMut {
        text.parse().unwrap()
    }

    #[test]
    fn adding_keeps_comments_and_the_rest_of_the_file() {
        let mut d = doc(
            "# our models\n[models]\ndefault = \"local:x\"\n\n# pinned deliberately\n[dependencies.old]\npath = \"./old\"\n",
        );
        assert_eq!(
            add(
                &mut d,
                "receipts",
                &DepSpec::Git {
                    url: "github.com/org/receipts".to_string(),
                    reference: GitRef::Tag("v1.0.0".to_string()),
                }
            ),
            Change::Added
        );
        let out = d.to_string();
        assert!(out.contains("# our models"), "{out}");
        assert!(out.contains("# pinned deliberately"), "{out}");
        assert!(out.contains("default = \"local:x\""), "{out}");
        assert!(out.contains("github.com/org/receipts"), "{out}");
        assert!(out.contains("tag = \"v1.0.0\""), "{out}");
    }

    #[test]
    fn adding_the_same_dependency_twice_changes_nothing() {
        let mut d = doc("[dependencies.a]\npath = \"./a\"\n");
        let spec = DepSpec::Path {
            path: PathBuf::from("./a"),
        };
        assert_eq!(add(&mut d, "a", &spec), Change::Unchanged);
    }

    #[test]
    fn repointing_a_dependency_reports_what_it_was() {
        let mut d = doc("[dependencies.a]\ngit = \"github.com/org/a\"\ntag = \"v1.0.0\"\n");
        let change = add(
            &mut d,
            "a",
            &DepSpec::Git {
                url: "github.com/org/a".to_string(),
                reference: GitRef::Tag("v2.0.0".to_string()),
            },
        );
        assert_eq!(
            change,
            Change::Replaced {
                previous: "github.com/org/a at v1.0.0".to_string()
            }
        );
    }

    #[test]
    fn grants_written_by_hand_survive_a_re_add() {
        // Otherwise `kora add` on an existing dependency would quietly hand
        // it more authority than the program had given it.
        let mut d = doc(
            "[dependencies.a]\ngit = \"github.com/org/a\"\ntag = \"v1.0.0\"\ngrants = { net = true }\n",
        );
        add(
            &mut d,
            "a",
            &DepSpec::Git {
                url: "github.com/org/a".to_string(),
                reference: GitRef::Tag("v2.0.0".to_string()),
            },
        );
        let out = d.to_string();
        assert!(out.contains("net = true"), "{out}");
    }

    #[test]
    fn removing_reports_whether_it_was_there() {
        let mut d = doc("[dependencies.a]\npath = \"./a\"\n");
        assert_eq!(remove(&mut d, "a"), Change::Removed);
        assert_eq!(remove(&mut d, "a"), Change::Absent);
        assert!(!d.to_string().contains("./a"));
    }

    #[test]
    fn setting_a_revision_replaces_the_old_one() {
        let mut d = doc("[dependencies.a]\ngit = \"github.com/org/a\"\ntag = \"v1.0.0\"\n");
        assert!(set_revision(
            &mut d,
            "a",
            &GitRef::Tag("v2.0.0".to_string())
        ));
        let out = d.to_string();
        assert!(out.contains("v2.0.0"), "{out}");
        assert!(!out.contains("v1.0.0"), "{out}");
    }

    #[test]
    fn adding_to_a_manifest_with_no_dependencies_section_works() {
        let mut d = doc("[models]\ndefault = \"local:x\"\n");
        add(
            &mut d,
            "a",
            &DepSpec::Path {
                path: PathBuf::from("./a"),
            },
        );
        assert!(d.to_string().contains("[dependencies.a]"), "{d}");
    }
}
