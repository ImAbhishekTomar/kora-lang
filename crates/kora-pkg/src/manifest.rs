//! `[package]` and `[dependencies]` in a `kora.toml`.
//!
//! A manifest is read from exactly one file, at a known package root. It is
//! never *discovered* by walking up the filesystem the way a program's
//! configuration is: walking up from a path dependency would find whichever
//! `kora.toml` happens to sit above it — often the consumer's — and silently
//! resolve that package's imports against the wrong `[dependencies]` table.

use crate::grants::Grants;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// The default entry file of a package, when `[package] entry` is absent.
pub const DEFAULT_ENTRY: &str = "src/lib.ko";

/// Where a dependency's source comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepSpec {
    /// `receipts = { path = "../receipts" }`, resolved against the manifest
    /// that wrote it.
    Path { path: PathBuf },
    /// `receipts = { git = "github.com/org/receipts", tag = "v0.3.1" }`.
    ///
    /// Identity is the full repository path, never a short name. There is no
    /// flat namespace to squat in, which is where dependency-confusion
    /// attacks begin.
    Git { url: String, reference: GitRef },
}

/// Which revision of a repository a dependency names.
///
/// What a human writes is a tag or a branch; what is actually used is the
/// commit the lockfile pinned. A tag can be moved and a branch always does,
/// so neither is an identity on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitRef {
    Tag(String),
    Branch(String),
    Commit(String),
    /// No reference given: the repository's default branch.
    Default,
}

impl GitRef {
    pub fn describe(&self) -> String {
        match self {
            GitRef::Tag(t) => t.clone(),
            GitRef::Branch(b) => b.clone(),
            GitRef::Commit(c) => c.clone(),
            GitRef::Default => "default branch".to_string(),
        }
    }
}

/// One dependency entry, kept with the position it was written at so an
/// unused or unresolvable entry can be reported against its own line.
#[derive(Debug, Clone)]
pub struct Dep {
    pub name: String,
    pub spec: DepSpec,
    /// `[dependencies.<name>.grants]` — the authority this program hands to
    /// that package. Absent means nothing, which is the safe default: a
    /// dependency that was never given the network cannot reach it.
    pub grants: Grants,
}

/// `[package.helper]` — a program this package carries work out to.
///
/// Some work cannot be written in Kora and does not belong in the compiler
/// either: rasterizing a PDF page needs a large C library, and linking that
/// into the interpreter would put a memory-unsafe parser in the same address
/// space as every label and capability the run holds. A helper is that work,
/// kept in its own process.
///
/// The artifact is named per target and pinned by hash. `kora install`
/// fetches only the one this machine needs, verifies it, and records it — a
/// program that never imports the package downloads nothing at all, and one
/// that does gets a binary whose contents are in `kora.sums` like any other
/// dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperSpec {
    /// The wire protocol. One today: `stdio/v1`.
    pub protocol: String,
    /// How long one call may take before the helper is killed.
    pub timeout_secs: u64,
    /// Target triple -> where that platform's helper comes from.
    pub artifacts: HashMap<String, HelperArtifact>,
    /// `needs = ["net", "fs"]` — what the helper process is allowed to reach.
    ///
    /// Empty by default, and empty is the interesting case: a helper that
    /// turns bytes into other bytes is confined to exactly that. What is
    /// declared here is still only a request — the importer's grants decide
    /// whether it is honoured, the same as every other capability.
    pub needs: BTreeSet<crate::grants::Capability>,
}

/// Where one platform's helper binary comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperArtifact {
    /// `path = "helper/kora-pdf-helper"`, relative to the package root. For a
    /// helper built alongside the package, and for developing one.
    Path { path: PathBuf },
    /// `url = "..."` with a mandatory `sha256`. An archive is unpacked and
    /// `binary` names the executable inside it.
    Fetch {
        url: String,
        sha256: String,
        binary: Option<String>,
    },
}

/// The target triple this build of Kora runs on.
///
/// The names match the release matrix, so what a package declares is what the
/// release artifacts are already called.
pub fn host_target() -> &'static str {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "windows") => "x86_64-pc-windows-msvc",
        ("aarch64", "windows") => "aarch64-pc-windows-msvc",
        _ => "unsupported",
    }
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
    /// `[package.requires]` — what this package needs from whoever imports
    /// it. Checked against what it was actually granted, so a shortfall is
    /// reported before the program runs rather than at the first call.
    pub requires: Grants,
    /// `[package.helper]` — a separate program this package talks to.
    pub helper: Option<HelperSpec>,
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
                // A manifest that does not parse still *is* the manifest.
                // Walking past it would resolve against whichever one sits
                // further up, which is a stranger answer than an empty one.
                return (d.clone(), Manifest::at(&d).unwrap_or_default());
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
            if let Some(requires) = section.get("requires").and_then(|v| v.as_table()) {
                manifest.requires = Grants::from_toml(requires);
            }
            if let Some(helper) = section.get("helper").and_then(|v| v.as_table()) {
                manifest.helper = Some(parse_helper(helper)?);
            }
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

/// Read `[package.helper]`: the protocol, the timeout, and one entry per
/// target triple.
fn parse_helper(table: &toml::map::Map<String, toml::Value>) -> Result<HelperSpec, ManifestError> {
    let protocol = table
        .get("protocol")
        .and_then(|v| v.as_str())
        .unwrap_or("stdio/v1")
        .to_string();
    if protocol != "stdio/v1" {
        return Err(
            ManifestError::new(format!("unknown helper protocol `{protocol}`"))
                .with_hint("this version of Kora speaks `stdio/v1`"),
        );
    }
    let timeout_secs = table
        .get("timeout_secs")
        .and_then(toml::Value::as_integer)
        .filter(|n| *n > 0)
        .unwrap_or(120) as u64;

    // Absent means empty, not unrestricted. A helper that says nothing about
    // what it needs is a helper that needs nothing, which is the only default
    // that fails safe.
    let mut needs = BTreeSet::new();
    if let Some(listed) = table.get("needs") {
        let Some(items) = listed.as_array() else {
            return Err(ManifestError::new("`needs` must be a list of capabilities")
                .with_hint("write `needs = [\"net\"]`"));
        };
        for item in items {
            let name = item.as_str().unwrap_or_default();
            let Some(capability) = crate::grants::Capability::parse(name) else {
                return Err(ManifestError::new(format!(
                    "the helper asks for an unknown capability `{name}`"
                ))
                .with_hint("a helper can ask for `net` or `fs`"));
            };
            // The rest of the capability set is about what *Kora* does on a
            // package's behalf -- a `sql` grant gates the `sql` module, not a
            // separate program. A helper is a process; `net` and `fs` are the
            // two things the operating system can be told to withhold from
            // one.
            if !matches!(
                capability,
                crate::grants::Capability::Net | crate::grants::Capability::Fs
            ) {
                return Err(
                    ManifestError::new(format!("a helper cannot ask for `{name}`"))
                        .with_hint("a helper is a separate process: it can ask for `net` or `fs`"),
                );
            }
            needs.insert(capability);
        }
    }

    let mut artifacts = HashMap::new();
    for (target, spec) in table {
        // Everything that is a table is a target; the scalars above are the
        // settings. A typo'd setting therefore reads as a target nobody
        // runs on, which `kora install` reports rather than ignoring.
        let Some(entry) = spec.as_table() else {
            continue;
        };
        if let Some(path) = entry.get("path").and_then(|v| v.as_str()) {
            artifacts.insert(
                target.clone(),
                HelperArtifact::Path {
                    path: PathBuf::from(path),
                },
            );
            continue;
        }
        let Some(url) = entry.get("url").and_then(|v| v.as_str()) else {
            return Err(ManifestError::new(format!(
                "the helper for `{target}` names neither a path nor a url"
            )));
        };
        // A downloaded binary with no hash is a binary nobody can check. The
        // format has nowhere to put an unpinned one.
        let Some(sha256) = entry.get("sha256").and_then(|v| v.as_str()) else {
            return Err(
                ManifestError::new(format!("the helper for `{target}` has no `sha256`")).with_hint(
                    "a fetched helper is pinned by hash, so what runs is what was reviewed",
                ),
            );
        };
        artifacts.insert(
            target.clone(),
            HelperArtifact::Fetch {
                url: url.to_string(),
                sha256: sha256.to_string(),
                binary: entry
                    .get("binary")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            },
        );
    }

    Ok(HelperSpec {
        protocol,
        timeout_secs,
        artifacts,
        needs,
    })
}

fn parse_dep(name: &str, spec: &toml::Value) -> Result<Dep, ManifestError> {
    let table = spec.as_table().ok_or_else(|| {
        ManifestError::new(format!("dependency `{name}` must be a table"))
            .with_hint(format!("write `{name} = {{ path = \"../{name}\" }}`"))
    })?;

    let grants = table
        .get("grants")
        .and_then(|v| v.as_table())
        .map(Grants::from_toml)
        .unwrap_or_default();

    if let Some(url) = table.get("git").and_then(|v| v.as_str()) {
        let reference = if let Some(tag) = table.get("tag").and_then(|v| v.as_str()) {
            GitRef::Tag(tag.to_string())
        } else if let Some(branch) = table.get("branch").and_then(|v| v.as_str()) {
            GitRef::Branch(branch.to_string())
        } else if let Some(commit) = table.get("rev").and_then(|v| v.as_str()) {
            GitRef::Commit(commit.to_string())
        } else {
            GitRef::Default
        };
        return Ok(Dep {
            name: name.to_string(),
            spec: DepSpec::Git {
                url: normalize_git_url(url),
                reference,
            },
            grants,
        });
    }

    let Some(path) = table.get("path").and_then(|v| v.as_str()) else {
        return Err(
            ManifestError::new(format!("dependency `{name}` has no source")).with_hint(format!(
                "write `{name} = {{ path = \"../{name}\" }}` or \
                 `{name} = {{ git = \"github.com/org/{name}\", tag = \"v1.0.0\" }}`"
            )),
        );
    };

    Ok(Dep {
        name: name.to_string(),
        spec: DepSpec::Path {
            path: PathBuf::from(path),
        },
        grants,
    })
}

/// Whether a dependency's source names a directory rather than a remote.
///
/// One definition, because the manifest and the fetcher have to agree: if the
/// manifest normalizes a path the fetcher then treats as local, a bare
/// repository at `C:\\mirrors\\receipts.git` loses its suffix and points at a
/// directory that does not exist.
pub(crate) fn is_local_path(url: &str) -> bool {
    // Deliberately not `Path::is_absolute()` at all: that answers for the
    // *host*. `C:\\mirrors\\receipts.git` is absolute only on Windows, and
    // `/mirrors/receipts.git` is absolute only on Unix -- so the same manifest
    // named different things on different machines, and the one that lost was
    // whichever developer was not on the author's operating system. A manifest
    // is committed and shared, so what it means must not change with the
    // machine reading it: the same reason listings are sorted and lockfiles
    // are content-addressed.
    //
    // Every form is therefore recognised everywhere, by shape rather than by
    // asking the host.
    url.starts_with('.') || url.starts_with('/') || url.starts_with('\\') || has_drive_letter(url)
}

/// `C:\\...` or `C:/...`, the one absolute form Unix does not recognize.
fn has_drive_letter(url: &str) -> bool {
    let mut chars = url.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.next() == Some(':')
        && matches!(chars.next(), Some('\\') | Some('/'))
}

/// Strip a scheme and any trailing `.git`, so one repository is one identity.
///
/// `https://github.com/org/x`, `github.com/org/x`, and
/// `https://github.com/org/x.git` name the same thing. Treating them as three
/// would put three copies in the graph and three rows in the lockfile.
fn normalize_git_url(url: &str) -> String {
    // A local path is left exactly as written: stripping a scheme it never
    // had, a trailing separator that is part of it, or the `.git` suffix a
    // bare repository is conventionally named with, would all name a
    // different directory.
    if is_local_path(url) {
        return url.to_string();
    }
    let url = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("ssh://"))
        .unwrap_or(url);
    url.strip_suffix(".git")
        .unwrap_or(url)
        .trim_end_matches('/')
        .to_string()
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
    fn a_local_bare_repository_keeps_its_git_suffix() {
        // A bare repository is conventionally named `name.git`, and that
        // suffix is part of the directory. Stripping it — as a remote URL's
        // would be — points at a directory that does not exist.
        let m = Manifest::parse(
            "[dependencies.mirror]\ngit = \"/mirrors/receipts.git\"\ntag = \"v1\"\n",
        )
        .unwrap();
        let DepSpec::Git { url, .. } = &m.deps["mirror"].spec else {
            panic!("expected a git dependency");
        };
        assert_eq!(url, "/mirrors/receipts.git");
    }

    #[test]
    fn a_windows_local_path_is_recognised_as_local() {
        // The manifest and the fetcher must agree on what a local path is.
        // While they disagreed, `C:\mirrors\receipts.git` was normalised as
        // if it were a remote and lost its suffix.
        assert!(is_local_path(r"C:\mirrors\receipts.git"));
        assert!(is_local_path("C:/mirrors/receipts.git"));
        // A scheme-like prefix is not a drive letter.
        assert!(!is_local_path("git:github.com/org/x"));
        assert!(is_local_path("/mirrors/receipts.git"));
        assert!(is_local_path("./receipts"));
        assert!(!is_local_path("github.com/org/receipts"));
        assert!(!is_local_path("https://github.com/org/receipts.git"));
    }

    #[test]
    fn a_local_path_means_the_same_on_every_host() {
        // The regression this function exists to prevent, and which it had
        // itself: `Path::is_absolute()` answers for the host, so a Unix path
        // read on Windows -- or a Windows path read on Unix -- stopped being
        // local, and the manifest named a different thing depending on who
        // opened it. These hold on every platform or the assertion is wrong
        // on the one running it.
        for local in [
            "/mirrors/receipts.git",
            r"C:\mirrors\receipts.git",
            "C:/mirrors/receipts.git",
            r"\\build-server\mirrors\receipts.git",
            "./receipts",
            "../receipts",
        ] {
            assert!(is_local_path(local), "{local} should be local everywhere");
        }
        for remote in [
            "github.com/org/receipts",
            "https://github.com/org/receipts.git",
            "git@github.com:org/receipts.git",
            "git:github.com/org/x",
        ] {
            assert!(!is_local_path(remote), "{remote} is a remote everywhere");
        }
    }

    #[test]
    fn a_remote_url_is_one_identity_however_it_is_written() {
        for written in [
            "https://github.com/org/x",
            "github.com/org/x",
            "https://github.com/org/x.git",
            "github.com/org/x/",
        ] {
            assert_eq!(normalize_git_url(written), "github.com/org/x", "{written}");
        }
    }

    fn helper_of(toml_text: &str) -> Result<HelperSpec, ManifestError> {
        let value: toml::Value = toml_text.parse().expect("valid toml");
        parse_helper(value.as_table().expect("a table"))
    }

    /// A helper that says nothing about what it needs needs nothing. Any other
    /// default would mean an author who forgot the line ships an unconfined
    /// process, which is the failure mode this whole field exists to close.
    #[test]
    fn a_helper_that_asks_for_nothing_gets_nothing() {
        let spec = helper_of("protocol = \"stdio/v1\"\n").expect("parses");
        assert!(spec.needs.is_empty());
    }

    #[test]
    fn a_helper_may_ask_for_net_and_fs() {
        let spec = helper_of("needs = [\"net\", \"fs\"]\n").expect("parses");
        assert!(spec.needs.contains(&crate::grants::Capability::Net));
        assert!(spec.needs.contains(&crate::grants::Capability::Fs));
    }

    /// The rest of the capability set gates what *Kora* does for a package.
    /// A helper is a separate process, and `net` and `fs` are what an
    /// operating system can be told to withhold from one -- so asking for
    /// `sql` is a mistake to report, not a request to quietly drop.
    #[test]
    fn a_helper_cannot_ask_for_a_capability_the_os_cannot_withhold() {
        let err = helper_of("needs = [\"sql\"]\n").expect_err("refused");
        assert!(
            err.message.contains("cannot ask for `sql`"),
            "{}",
            err.message
        );
    }

    #[test]
    fn an_unknown_capability_is_named_rather_than_ignored() {
        let err = helper_of("needs = [\"gpu\"]\n").expect_err("refused");
        assert!(err.message.contains("`gpu`"), "{}", err.message);
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
