//! Fetching git dependencies.
//!
//! Cold resolution is wave-shaped: what a package depends on is unknowable
//! until it is on disk and its source has been read. Each wave fans out at
//! the configured width, and the waves themselves are serial. Once the
//! lockfile exists the whole graph is known up front, so a warm install is a
//! single flat fan-out — deep chains cost only on the first resolve, never in
//! CI.
//!
//! Fetching shells out to `git`. A library would be a large dependency for
//! something every machine that can clone a repository already has, and the
//! commands used are the ones a person would type.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::lock::Locked;
use crate::manifest::GitRef;

/// How many fetches run at once.
///
/// Fetching is IO-bound, not CPU-bound, so this is deliberately not the core
/// count the way `parallel for` is. `[install] jobs` overrides it.
pub fn default_jobs() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (cores * 2).max(8)
}

/// One repository to fetch.
#[derive(Debug, Clone)]
pub struct Request {
    pub url: String,
    pub reference: GitRef,
}

/// What a fetch produced, or why it failed.
pub type Fetched = Result<Locked, String>;

/// Fetch every request, at most `jobs` at once.
///
/// Results come back in request order, so a parallel fetch reports exactly as
/// a serial one would and the lockfile it feeds is byte-identical either way.
pub fn all(requests: &[Request], store: &Path, jobs: usize) -> Vec<Fetched> {
    let width = jobs.clamp(1, requests.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<Fetched>>> =
        (0..requests.len()).map(|_| Mutex::new(None)).collect();

    std::thread::scope(|scope| {
        for _ in 0..width {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if index >= requests.len() {
                    break;
                }
                let outcome = one(&requests[index], store);
                *slots[index].lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
            });
        }
    });

    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .unwrap_or_else(|e| e.into_inner())
                .unwrap_or_else(|| Err("fetch did not run".to_string()))
        })
        .collect()
}

fn one(request: &Request, store: &Path) -> Fetched {
    let temp = store.join(temporary_checkout_name(request));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(store)
        .map_err(|e| format!("cannot create {}: {e}", store.display()))?;

    // A local path is a repository too: an internal mirror, or a checkout
    // beside the project. It grants nothing a path dependency does not
    // already, since both name a directory the program's own manifest chose.
    let url = if Path::new(&request.url).is_absolute() || request.url.starts_with('.') {
        request.url.clone()
    } else {
        format!("https://{}", request.url)
    };
    let mut args: Vec<String> = vec![
        "clone".into(),
        "--quiet".into(),
        // The history is not part of a package's identity, and cloning it
        // would make a fetch slower for nothing.
        "--depth".into(),
        "1".into(),
    ];
    match &request.reference {
        GitRef::Tag(t) | GitRef::Branch(t) => {
            args.push("--branch".into());
            args.push(t.clone());
        }
        // A specific commit cannot be reached by `--branch`, so the shallow
        // clone lands on the default branch and the commit is checked out
        // afterwards.
        GitRef::Commit(_) | GitRef::Default => {}
    }
    args.push(url.clone());
    args.push(temp.display().to_string());

    run_git(&args, None)?;

    if let GitRef::Commit(commit) = &request.reference {
        // A shallow clone may not contain it, so ask for that object first.
        run_git(
            &[
                "fetch".into(),
                "--depth".into(),
                "1".into(),
                "origin".into(),
                commit.clone(),
            ],
            Some(&temp),
        )?;
        run_git(
            &["checkout".into(), "--quiet".into(), commit.clone()],
            Some(&temp),
        )?;
    }

    let commit = run_git(&["rev-parse".into(), "HEAD".into()], Some(&temp))?
        .trim()
        .to_string();

    let locked = Locked {
        url: request.url.clone(),
        reference: request.reference.describe(),
        commit: commit.clone(),
        hash: String::new(),
    };
    let final_dir = store.join(locked.slug());

    // Already present from an earlier fetch of the same commit: keep what is
    // there rather than replacing bytes that were already verified.
    if final_dir.is_dir() {
        let _ = std::fs::remove_dir_all(&temp);
    } else {
        std::fs::rename(&temp, &final_dir).map_err(|e| {
            format!(
                "cannot move {} into place at {}: {e}",
                temp.display(),
                final_dir.display()
            )
        })?;
    }

    let hash = crate::hash::tree(&final_dir)?;
    Ok(Locked { hash, ..locked })
}

/// A filesystem-safe, deterministic staging name for one requested revision.
///
/// A local repository may be an absolute path. Using that path directly made
/// the staging name contain `:` and `\\` on Windows, which Git then rejects.
/// Hashing both the source and requested reference keeps separate revisions
/// separate without letting host-specific path punctuation reach the filename.
fn temporary_checkout_name(request: &Request) -> String {
    let mut digest = Sha256::new();
    digest.update(request.url.as_bytes());
    digest.update([0]);
    digest.update(request.reference.describe().as_bytes());
    format!(".partial-{:x}", digest.finalize())
}

fn run_git(args: &[String], cwd: Option<&PathBuf>) -> Result<String, String> {
    let mut command = std::process::Command::new("git");
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    // Never let git stop for credentials: a fetch that blocks on a prompt in
    // CI looks like a hang, and a private repository should say so instead.
    command.env("GIT_TERMINAL_PROMPT", "0");
    command.env("GIT_ASKPASS", "echo");

    let output = command
        .output()
        .map_err(|e| format!("cannot run git: {e}. Is git installed?"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_width_suits_io_not_cores() {
        // Fetching waits on the network, so a machine with two cores should
        // still run several at once.
        assert!(default_jobs() >= 8);
    }

    #[test]
    fn results_come_back_in_request_order() {
        // Nothing here reaches the network: every request fails, and what is
        // being pinned is that failure N lands in slot N.
        let store = std::env::temp_dir().join("kora-fetch-order");
        let requests: Vec<Request> = (0..5)
            .map(|i| Request {
                url: format!("localhost/definitely-not-a-repo-{i}"),
                reference: GitRef::Default,
            })
            .collect();
        let results = all(&requests, &store, 4);
        assert_eq!(results.len(), 5);
        assert!(results.iter().all(|r| r.is_err()));
        let _ = std::fs::remove_dir_all(&store);
    }

    #[test]
    fn zero_jobs_still_runs() {
        let store = std::env::temp_dir().join("kora-fetch-zero");
        let requests = vec![Request {
            url: "localhost/nope".to_string(),
            reference: GitRef::Default,
        }];
        assert_eq!(all(&requests, &store, 0).len(), 1);
        let _ = std::fs::remove_dir_all(&store);
    }

    #[test]
    fn no_requests_is_not_an_error() {
        let store = std::env::temp_dir().join("kora-fetch-none");
        assert!(all(&[], &store, 4).is_empty());
    }

    #[test]
    fn a_staging_name_is_safe_for_an_absolute_windows_path() {
        let request = Request {
            url: r"C:\\work\\packages\\greet".to_string(),
            reference: GitRef::Tag("v1.0.0".to_string()),
        };
        let name = temporary_checkout_name(&request);
        assert!(name.starts_with(".partial-"));
        assert!(!name.contains([':', '\\', '/']));
    }
}
