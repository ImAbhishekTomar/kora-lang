//! `[package]` and `[dependencies]` in a `kora.toml`.
//!
//! A manifest is read from exactly one file, at a known package root. It is
//! never *discovered* by walking up the filesystem the way a program's
//! configuration is: walking up from a path dependency would find whichever
//! `kora.toml` happens to sit above it — often the consumer's — and silently
//! resolve that package's imports against the wrong `[dependencies]` table.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The default entry file of a package, when `[package] entry` is absent.
pub const DEFAULT_ENTRY: &str = "src/lib.ko";

/// Where a dependency's source comes from.
///
/// Only local paths today. Fetched sources arrive with the lockfile, and the
/// resolver is written so that adding a variant here is the whole change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepSpec {
    /// `receipts = { path = "../receipts" }`, resolved against the manifest
    /// that wrote it.
    Path { path: PathBuf },
}

/// One dependency entry, kept with the position it was written at so an
/// unused or unresolvable entry can be reported against its own line.
#[derive(Debug, Clone)]
pub struct Dep {
    pub name: String,
    pub spec: DepSpec,
}

/// A parsed `kora.toml`, from the package's point of view.
///
/// A root program usually has no `[package]` section at all; it still has a
/// manifest, because it still has dependencies.
#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub name: Option<String>,
    pub version: Option<String>,
    /// Relative to the package root.
    pub entry: Option<PathBuf>,
    pub deps: HashMap<String, Dep>,
}

/// Why a manifest could not be read.
#[derive(Debug)]
pub struct ManifestError {
    pub message: String,
    pub hint: Option<String>,
}

impl ManifestError {
    fn new(message: impl Into<String>) -> ManifestError {
        ManifestError {
            message: message.into(),
            hint: None,
        }
    }

    fn with_hint(mut self, hint: impl Into<String>) -> ManifestError {
        self.hint = Some(hint.into());
        self
    }
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl Manifest {
    /// Read the manifest at `root/kora.toml`.
    ///
    /// A package with no `kora.toml` is an empty manifest rather than an
    /// error: a single-file package that depends on nothing needs no file to
    /// say so.
    pub fn at(root: &Path) -> Result<Manifest, ManifestError> {
        let path = root.join("kora.toml");
        if !path.is_file() {
            return Ok(Manifest::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| ManifestError::new(format!("cannot read `{}`: {e}", path.display())))?;
        Manifest::parse(&text)
    }

    /// Find the manifest governing a program file, walking up from it.
    ///
    /// A program is written wherever it is convenient — `examples/x.ko`,
    /// `src/main.ko` — while its `kora.toml` sits at the project root. This
    /// is the same search the rest of the configuration already uses.
    ///
    /// Only the *root program* is discovered this way. A dependency's
    /// manifest is read at its own root with [`Manifest::at`], because
    /// walking up from a dependency would find whichever manifest happens to
    /// sit above it — often the consumer's.
    ///
    /// Returns the directory the manifest was found in, since dependency
    /// paths are written relative to it rather than to the program file.
    pub fn discover(start: &Path) -> (PathBuf, Manifest) {
        let mut dir = if start.is_dir() {
            Some(start.to_path_buf())
        } else {
            start.parent().map(PathBuf::from)
        };
        while let Some(d) = dir {
            if d.join("kora.toml").is_file() {
                if let Ok(manifest) = Manifest::at(&d) {
                    return (d, manifest);
                }
            }
            dir = d.parent().map(PathBuf::from);
        }
        let fallback = start
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        (fallback, Manifest::default())
    }

    pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
        let root: toml::Value = text
            .parse()
            .map_err(|e| ManifestError::new(format!("kora.toml is not valid TOML: {e}")))?;

        let mut manifest = Manifest::default();

        if let Some(section) = root.get("package").and_then(|v| v.as_table()) {
            manifest.name = section
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            manifest.version = section
                .get("version")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            manifest.entry = section
                .get("entry")
                .and_then(|v| v.as_str())
                .map(PathBuf::from);
        }

        if let Some(section) = root.get("dependencies").and_then(|v| v.as_table()) {
            for (name, spec) in section {
                let dep = parse_dep(name, spec)?;
                manifest.deps.insert(name.clone(), dep);
            }
        }

        Ok(manifest)
    }

    /// The entry file, relative to the package root.
    pub fn entry(&self) -> PathBuf {
        self.entry
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_ENTRY))
    }
}

fn parse_dep(name: &str, spec: &toml::Value) -> Result<Dep, ManifestError> {
    let table = spec.as_table().ok_or_else(|| {
        ManifestError::new(format!("dependency `{name}` must be a table"))
            .with_hint(format!("write `{name} = {{ path = \"../{name}\" }}`"))
    })?;

    let Some(path) = table.get("path").and_then(|v| v.as_str()) else {
        return Err(
            ManifestError::new(format!("dependency `{name}` has no source"))
                .with_hint(format!("write `{name} = {{ path = \"../{name}\" }}`")),
        );
    };

    Ok(Dep {
        name: name.to_string(),
        spec: DepSpec::Path {
            path: PathBuf::from(path),
        },
    })
}

/// Package names are Kora identifiers, so `use pkg <name>` can bind the name
/// directly and `as` stays optional. A dash would force every import to
/// invent an alias, which is the friction that makes people write one.
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_package_and_dependencies() {
        let m = Manifest::parse(
            r#"
[package]
name = "receipts"
version = "0.3.1"
entry = "src/lib.ko"

[dependencies]
retry = { path = "../retry" }
"#,
        )
        .unwrap();
        assert_eq!(m.name.as_deref(), Some("receipts"));
        assert_eq!(m.version.as_deref(), Some("0.3.1"));
        assert_eq!(m.entry(), PathBuf::from("src/lib.ko"));
        assert_eq!(
            m.deps["retry"].spec,
            DepSpec::Path {
                path: PathBuf::from("../retry")
            }
        );
    }

    #[test]
    fn entry_defaults_when_absent() {
        let m = Manifest::parse("[package]\nname = \"x\"\n").unwrap();
        assert_eq!(m.entry(), PathBuf::from(DEFAULT_ENTRY));
    }

    #[test]
    fn a_program_manifest_needs_no_package_section() {
        let m = Manifest::parse("[dependencies]\nreceipts = { path = \"./receipts\" }\n").unwrap();
        assert!(m.name.is_none());
        assert_eq!(m.deps.len(), 1);
    }

    #[test]
    fn a_dependency_without_a_source_names_the_fix() {
        let err = Manifest::parse("[dependencies]\nreceipts = { version = \"1\" }\n").unwrap_err();
        assert!(err.message.contains("no source"), "{}", err.message);
        assert!(err.hint.unwrap().contains("path"));
    }

    #[test]
    fn unknown_sections_are_ignored() {
        // A manifest is read by older binaries than the one that wrote it.
        let m = Manifest::parse("[models]\ndefault = \"local:x\"\n").unwrap();
        assert!(m.deps.is_empty());
    }

    #[test]
    fn names_are_kora_identifiers() {
        assert!(is_valid_name("receipts"));
        assert!(is_valid_name("http_retry"));
        assert!(!is_valid_name("http-retry"));
        assert!(!is_valid_name("Receipts"));
        assert!(!is_valid_name("2fast"));
        assert!(!is_valid_name(""));
    }
}
