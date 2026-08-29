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

mod grants;
mod manifest;
mod resolve;
mod scan;

pub use grants::{Capability, Grants};
pub use manifest::{is_valid_name, Dep, DepSpec, Manifest, ManifestError, DEFAULT_ENTRY};
pub use resolve::{resolve, MissingDep, PackageId, Resolution, ResolvedPackage, UnusedDep, ROOT};
pub use scan::{imports, Import, Imports};
