//! Unique scratch directories for tests.
//!
//! Two `cargo test` processes can run at the same time, and within one
//! process tests run on many threads. A fixture directory named only for
//! what it holds is shared by all of them, so one test's cleanup deletes
//! another's fixtures. Mixing in the process and thread id gives every
//! test its own directory.

use std::path::PathBuf;

/// A directory under the system temp dir that no other test can collide
/// with. The directory is removed first if a previous run left it behind;
/// creating it is left to the caller, since some callers want to write a
/// tree into it and others want the path to start out absent.
pub(crate) fn path(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "{label}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Like [`path`], but the directory exists on return.
pub(crate) fn dir(label: &str) -> PathBuf {
    let dir = path(label);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
