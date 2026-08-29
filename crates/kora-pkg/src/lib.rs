//! Packages: manifests, and which of them a program actually uses.
//!
//! Kora derives the dependency graph from the source rather than from the
//! manifest. `[dependencies]` says *where* a package comes from and at what
//! version; the `use pkg` statements say *whether* it is needed at all. A
//! program that declares a hundred dependencies and imports four resolves
//! four.
//!
//! This is sound here in a way it is not elsewhere: a package name is always
//! a literal token in the source. Kora has no dynamic import, so a scan of
//! the syntax tree cannot miss a use, and nothing has to be guessed.

pub(crate) mod commands;
mod edit;
mod fetch;
mod grants;
pub(crate) mod hash;
mod install;
pub(crate) mod lock;
pub(crate) mod manifest;
pub(crate) mod resolve;
mod scan;
mod sumlog;

pub use commands::{
    add, checkout_of, compare, granted, remove, set_revision, unlock, vendor, Diff,
};
pub use edit::Change;
pub use fetch::{all as fetch_all, default_jobs, Fetched, Request};
pub use grants::{Capability, Grants};
pub use hash::tree as hash_tree;
pub use install::{install, Installed};
pub use lock::{deps_dir, Lock, Locked};
pub use manifest::{is_valid_name, Dep, DepSpec, GitRef, Manifest, ManifestError, DEFAULT_ENTRY};
pub use resolve::resolve as resolve_graph;
pub use resolve::{resolve, MissingDep, PackageId, Resolution, ResolvedPackage, UnusedDep, ROOT};
pub use scan::{imports, Import, Imports};
pub use sumlog::{SumLog, Verdict};
