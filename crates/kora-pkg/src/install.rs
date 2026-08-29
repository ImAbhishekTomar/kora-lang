//! Resolving to a fixed point, fetching what is missing along the way.
//!
//! What a package depends on cannot be known until it is on disk and its
//! source has been read, so a cold install alternates: resolve, fetch
//! whatever the resolve found missing, resolve again. Each wave fetches at
//! the configured width. It stops when a resolve asks for nothing new — or
//! when a wave fetches nothing, which means the remaining requests failed and
//! looping again would only fail identically.

use std::path::Path;

use crate::lock::{deps_dir, Lock};
use crate::manifest::{GitRef, Manifest};
use crate::resolve::Resolution;
use crate::sumlog::{SumLog, Verdict};

/// What one install did.
pub struct Installed {
    pub resolution: Resolution,
    /// Repositories fetched this run, in the order they were requested.
    pub fetched: Vec<String>,
    /// Repositories that could not be fetched, with the reason git gave.
    pub failed: Vec<(String, String)>,
    /// Whether the lockfile changed and was rewritten.
    pub lock_changed: bool,
    /// Commits recorded in the checksum log for the first time.
    pub newly_recorded: usize,
}

/// Resolve `entry`, fetching any git dependency that is not on disk.
///
/// `jobs` is how many fetches run at once; zero means the default.
pub fn install(entry: &Path, jobs: usize, write_lock: bool) -> Installed {
    let (root_dir, _) = Manifest::discover(entry);
    let store = deps_dir(&root_dir);
    let jobs = if jobs == 0 {
        crate::fetch::default_jobs()
    } else {
        jobs
    };

    let mut lock = Lock::at(&root_dir).unwrap_or_default();
    let before = lock.render();
    let mut sums = SumLog::shared(&root_dir);
    let mut fetched = Vec::new();
    let mut failed = Vec::new();

    let resolution = loop {
        let resolution = crate::resolve::resolve(entry);

        // Anything already known to have failed is not requested again: the
        // next attempt would fail the same way and the loop would not end.
        let requests: Vec<crate::fetch::Request> = resolution
            .unfetched
            .iter()
            .filter(|u| {
                !failed
                    .iter()
                    .any(|(url, _): &(String, String)| url == &u.url)
            })
            .filter_map(|u| {
                let dep = find_dep(&resolution, &u.url)?;
                let crate::manifest::DepSpec::Git { url, reference } = &dep.spec else {
                    return None;
                };
                // The lockfile is authoritative. Once a repository is locked,
                // its *commit* is what gets fetched — never the tag again.
                // Re-resolving the tag is how a force-push lands on a machine
                // with a cold cache: the lockfile would be rewritten to the
                // attacker's commit and nothing would look wrong.
                let reference = match lock.get(url) {
                    Some(locked) if !locked.commit.is_empty() => {
                        GitRef::Commit(locked.commit.clone())
                    }
                    _ => reference.clone(),
                };
                Some(crate::fetch::Request {
                    url: url.clone(),
                    reference,
                })
            })
            .collect();

        if requests.is_empty() {
            break resolution;
        }

        let mut progressed = false;
        for (request, outcome) in requests
            .iter()
            .zip(crate::fetch::all(&requests, &store, jobs))
        {
            match outcome {
                Ok(locked) => {
                    // What this commit has always meant, to anyone. The
                    // lockfile cannot help with a project's *first* fetch —
                    // there is nothing to check against yet — so a backdoor
                    // published briefly and withdrawn leaves no trace in any
                    // lockfile. The log is what remembers.
                    match sums.check(&locked.url, &locked.commit, &locked.hash) {
                        Verdict::New | Verdict::Known => {}
                        Verdict::Conflict { recorded } => {
                            failed.push((
                                locked.url.clone(),
                                [
                                    format!(
                                        "commit {} does not have the contents it had when \
                                         first seen",
                                        &locked.commit[..locked.commit.len().min(12)]
                                    ),
                                    format!("recorded {recorded}"),
                                    format!("fetched  {}", locked.hash),
                                    "the identity was reused: a rewritten repository, or a \
                                     release republished as something else"
                                        .to_string(),
                                ]
                                .join("\n"),
                            ));
                            continue;
                        }
                    }
                    sums.record(&locked.url, &locked.commit, &locked.hash);

                    // A locked repository must come back with the bytes it
                    // had when it was locked. Different bytes under the same
                    // commit means the repository was rewritten.
                    if let Some(previous) = lock.get(&locked.url) {
                        if !previous.hash.is_empty() && previous.hash != locked.hash {
                            failed.push((
                                locked.url.clone(),
                                [
                                    "content does not match the lockfile".to_string(),
                                    format!("expected {}", previous.hash),
                                    format!("found    {}", locked.hash),
                                    "the repository was rewritten, or the lockfile is for different code".to_string(),
                                ]
                                .join("\n"),
                            ));
                            continue;
                        }
                    }
                    fetched.push(locked.url.clone());
                    lock.insert(locked);
                    progressed = true;
                }
                Err(why) => failed.push((request.url.clone(), why)),
            }
        }

        // The lockfile has to be on disk before the next resolve, since that
        // is where a commit is looked up.
        if progressed {
            let _ = lock.write(&root_dir);
        } else {
            break crate::resolve::resolve(entry);
        }
    };

    let lock_changed = lock.render() != before;
    if write_lock && lock_changed {
        let _ = lock.write(&root_dir);
    }
    let newly_recorded = sums.pending();
    if write_lock {
        let _ = sums.append_shared(&root_dir);
    }

    Installed {
        resolution,
        fetched,
        failed,
        lock_changed,
        newly_recorded,
    }
}

/// The dependency entry that named a repository, from whichever manifest
/// declared it.
fn find_dep<'a>(resolution: &'a Resolution, url: &str) -> Option<&'a crate::manifest::Dep> {
    resolution.packages.iter().find_map(|package| {
        package.manifest.deps.values().find(
            |dep| matches!(&dep.spec, crate::manifest::DepSpec::Git { url: u, .. } if u == url),
        )
    })
}
