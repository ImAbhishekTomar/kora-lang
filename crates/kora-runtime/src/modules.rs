//! File modules: `use "./lib/tax.ko" as tax`.
//!
//! A module is one Kora file. Its top-level names live in their own
//! namespace, so two files may use the same name for different things, and a
//! function always reads the file it was written in rather than the file that
//! imported it.
//!
//! Paths resolve relative to the *importing* file, never the working
//! directory, so a program is a directory that can be moved or vendored
//! whole.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Index into the interpreter's module table. Zero is the entry file.
pub type ModuleId = usize;

/// The entry file: the one named on the command line.
pub const ROOT: ModuleId = 0;

/// One module's top-level namespace, plus where it came from.
pub struct ModuleSpace {
    /// Path as displayed in errors.
    pub path: String,
    /// Canonical path, used to recognize a file already loaded.
    pub key: PathBuf,
    /// Directory imports inside this file resolve against.
    pub dir: PathBuf,
    /// Top-level bindings. Empty while this module is the active one, because
    /// the interpreter holds the live namespace in `globals` and swaps it back
    /// when it leaves.
    pub names: HashMap<String, crate::value::Value>,
}

impl ModuleSpace {
    pub fn new(path: String, key: PathBuf, dir: PathBuf) -> Self {
        ModuleSpace {
            path,
            key,
            dir,
            names: HashMap::new(),
        }
    }
}

/// Why an import could not be loaded.
pub enum ResolveError {
    NotKora(String),
    Missing { path: String, resolved: String },
}

impl ResolveError {
    pub fn message(&self) -> String {
        match self {
            ResolveError::NotKora(path) => format!("`{path}` is not a Kora file"),
            ResolveError::Missing { path, .. } => format!("cannot read `{path}`"),
        }
    }

    pub fn hint(&self) -> String {
        match self {
            ResolveError::NotKora(_) => "an imported path must end in `.ko`".to_string(),
            ResolveError::Missing { resolved, .. } => format!("looked for {resolved}"),
        }
    }
}

/// What a written import path points at.
pub struct Resolved {
    /// Path to read and to show in errors.
    pub path: PathBuf,
    /// Identity of the file, so the same module imported twice loads once.
    pub key: PathBuf,
    /// Directory that file's own imports resolve against.
    pub dir: PathBuf,
}

/// Turn a written path into a file to load, relative to `base`.
pub fn resolve(written: &str, base: &Path) -> Result<Resolved, ResolveError> {
    if !written.ends_with(".ko") {
        return Err(ResolveError::NotKora(written.to_string()));
    }
    let candidate = normalize(&base.join(written));
    if !candidate.is_file() {
        return Err(ResolveError::Missing {
            path: written.to_string(),
            resolved: candidate.display().to_string(),
        });
    }
    // Canonicalizing is what makes `./a.ko` and `../pkg/a.ko` the same module
    // rather than two copies with separate state.
    let key = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.clone());
    let dir = key
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(Resolved {
        path: candidate,
        key,
        dir,
    })
}

/// Drop `.` components, so an error reads `lib/tax.ko` rather than
/// `./lib/./tax.ko`. Purely lexical: `..` is left for the filesystem to
/// resolve, since collapsing it would follow the wrong path through a symlink.
pub fn normalize(path: &Path) -> PathBuf {
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

/// Render an import cycle the way it was entered, so the fix is visible.
pub fn cycle_message(chain: &[String], offender: &str) -> String {
    let mut parts: Vec<&str> = chain.iter().map(String::as_str).collect();
    parts.push(offender);
    format!("import cycle: {}", parts.join(" -> "))
}
