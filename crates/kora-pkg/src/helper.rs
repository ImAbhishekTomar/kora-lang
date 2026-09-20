//! Fetching a package's helper: one binary, pinned by hash, only if used.
//!
//! A helper is the one thing a package carries that is not source. That makes
//! it the one thing a reader cannot check by reading, so it is pinned the
//! only way a binary can be: the manifest names its `sha256`, and a download
//! whose bytes hash to anything else is refused rather than run.
//!
//! Three properties fall out of doing it here rather than at build time:
//!
//! - **Nothing is fetched unless it is used.** `kora install` walks the same
//!   resolution the program does, so a package nobody imports downloads
//!   nothing, and a helper for another platform is never asked for.
//! - **Nothing is executed to install it.** The archive is unpacked and one
//!   named file is marked executable. There is no install script, here as
//!   everywhere else.
//! - **What arrived is recorded.** The hash goes into `kora.sums` beside
//!   every dependency's, so "what this URL has always meant" is answerable
//!   later, and a URL that starts serving different bytes under the same
//!   hash is a conflict rather than a surprise.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::manifest::{host_target, HelperArtifact, HelperSpec};

/// Beyond this the download is abandoned. A helper is a utility program, not
/// a dataset, and an unbounded read from a URL is how a fetch becomes a
/// denial of service.
const MAX_BYTES: u64 = 256 * 1024 * 1024;

/// What one helper install did.
#[derive(Debug)]
pub struct Fetched {
    /// The package the helper belongs to.
    pub package: String,
    pub url: String,
    pub sha256: String,
    pub path: PathBuf,
}

/// Why a helper could not be installed.
#[derive(Debug)]
pub struct Failed {
    pub package: String,
    pub url: String,
    pub why: String,
}

/// Install this platform's helper for `spec`, unless it is already there.
///
/// Returns `Ok(None)` when there is nothing to do: the helper is a local
/// path, this platform has no build, or the binary is already installed.
pub fn install(
    package: &str,
    spec: &HelperSpec,
    project_root: &Path,
) -> Result<Option<Fetched>, Failed> {
    let Some(HelperArtifact::Fetch {
        url,
        sha256,
        binary,
    }) = spec.artifacts.get(host_target())
    else {
        return Ok(None);
    };

    let dir = crate::lock::helper_dir(project_root, sha256);
    let path = dir.join(binary.as_deref().unwrap_or("helper"));
    if path.is_file() {
        return Ok(None);
    }

    let fail = |why: String| Failed {
        package: package.to_string(),
        url: url.clone(),
        why,
    };

    let bytes = download(url).map_err(fail)?;
    let found = sha256_hex(&bytes);
    if found != sha256.to_lowercase() {
        return Err(fail(
            [
                "the download is not what the manifest pinned".to_string(),
                format!("expected {sha256}"),
                format!("found    {found}"),
                "the artifact was replaced, or the manifest is for a different build".to_string(),
            ]
            .join("\n"),
        ));
    }

    unpack(&bytes, &dir, &path).map_err(fail)?;
    if !path.is_file() {
        return Err(fail(format!(
            "the archive has no `{}` in it",
            binary.as_deref().unwrap_or("helper")
        )));
    }
    make_executable(&path);

    Ok(Some(Fetched {
        package: package.to_string(),
        url: url.clone(),
        sha256: found,
        path,
    }))
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("could not download it: {e}"))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("could not read the download: {e}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!(
            "it is larger than the {} MB limit",
            MAX_BYTES / (1024 * 1024)
        ));
    }
    Ok(bytes)
}

/// A gzipped tar is unpacked; anything else is taken to be the binary itself.
fn unpack(bytes: &[u8], dir: &Path, binary: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;

    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        for entry in archive
            .entries()
            .map_err(|e| format!("the archive could not be read: {e}"))?
        {
            let mut entry = entry.map_err(|e| format!("the archive could not be read: {e}"))?;
            let path = entry
                .path()
                .map_err(|e| format!("the archive has an unreadable name: {e}"))?
                .into_owned();
            // An archive entry names its own path, and `../` in one is how an
            // unpack writes outside the directory it was given. Refuse rather
            // than normalize: normalizing quietly changes what was asked for.
            if path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(format!(
                    "the archive contains a path that climbs out of it: {}",
                    path.display()
                ));
            }
            entry
                .unpack_in(dir)
                .map_err(|e| format!("could not unpack {}: {e}", path.display()))?;
        }
        return Ok(());
    }

    std::fs::write(binary, bytes).map_err(|e| format!("could not write the helper: {e}"))
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        crate::scratch::dir(&format!("kora-helperfetch-{name}"))
    }

    #[test]
    fn a_bare_binary_is_written_as_it_arrived() {
        let dir = scratch("bare");
        let binary = dir.join("helper");
        unpack(b"#!/bin/sh\necho hi\n", &dir, &binary).unwrap();
        assert_eq!(std::fs::read(&binary).unwrap(), b"#!/bin/sh\necho hi\n");
    }

    #[test]
    fn an_archive_is_unpacked() {
        let dir = scratch("archive");
        let mut archive = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(5);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, "helper", &b"hello"[..])
            .unwrap();
        let tarball = archive.into_inner().unwrap();

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tarball).unwrap();
        let gzipped = gz.finish().unwrap();

        unpack(&gzipped, &dir, &dir.join("helper")).unwrap();
        assert_eq!(std::fs::read(dir.join("helper")).unwrap(), b"hello");
    }

    /// An archive naming `../` writes wherever it likes. That is the whole
    /// tar-slip class, and the answer is to refuse the archive.
    #[test]
    fn an_archive_that_climbs_out_is_refused() {
        let dir = scratch("slip");
        // The `tar` builder refuses to *write* `..` into an archive, so the
        // header is laid out by hand: this is what a hostile archive looks
        // like, and it has to be one to be refused.
        let tarball = hostile_tar("../escaped", b"bad");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tarball).unwrap();
        let gzipped = gz.finish().unwrap();

        let err = unpack(&gzipped, &dir, &dir.join("helper")).unwrap_err();
        assert!(err.contains("climbs out"), "{err}");
        assert!(!dir.parent().unwrap().join("escaped").exists());
    }

    /// One tar entry, written byte by byte, so the name can be anything.
    fn hostile_tar(name: &str, body: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..107].copy_from_slice(b"0000644");
        header[108..115].copy_from_slice(b"0000000");
        header[116..123].copy_from_slice(b"0000000");
        let size = format!("{:011o}", body.len());
        header[124..135].copy_from_slice(size.as_bytes());
        header[136..147].copy_from_slice(b"00000000000");
        header[148..156].copy_from_slice(b"        ");
        header[156] = b'0';
        let checksum: u32 = header.iter().map(|b| *b as u32).sum();
        let rendered = format!("{checksum:06o}\0 ");
        header[148..156].copy_from_slice(rendered.as_bytes());

        let mut out = header.to_vec();
        out.extend_from_slice(body);
        out.resize(out.len().div_ceil(512) * 512, 0);
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    #[test]
    fn the_hash_is_the_hash() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
