//! The pure half of fetching the prebuilt SPA for a packaged `clove-web`:
//! asset names, checksum parsing and verification, and a size-capped,
//! path-safe tar.gz extraction. No network here, so `tests/packaging.rs` can
//! include this file and test it directly; the download itself is in `build.rs`.

use std::io::Read;
use std::path::Path;

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

/// Largest tarball accepted from the network (compressed bytes).
pub const MAX_DOWNLOAD: u64 = 64 << 20;
/// Largest total size accepted when extracting (uncompressed bytes).
pub const MAX_EXTRACTED: u64 = 256 << 20;

/// The release asset holding the built SPA, and the one holding its checksum.
pub fn asset_names(version: &str) -> (String, String) {
    let tarball = format!("clove-web-dist-v{version}.tar.gz");
    let checksum = format!("{tarball}.sha256");
    (tarball, checksum)
}

/// Where a version's assets live: its GitHub Release, unless `override_base` is set.
pub fn base_url(version: &str, override_base: Option<&str>) -> String {
    match override_base {
        Some(base) => base.trim_end_matches('/').to_owned(),
        None => format!("https://github.com/egeapak/clove/releases/download/v{version}"),
    }
}

/// Only HTTPS, or plain HTTP to loopback (a local mirror or a test server).
pub fn check_url(url: &str) -> Result<(), String> {
    if url.starts_with("https://") {
        return Ok(());
    }
    for loopback in ["http://127.0.0.1", "http://localhost", "http://[::1]"] {
        if let Some(rest) = url.strip_prefix(loopback) {
            if rest.is_empty() || rest.starts_with(':') || rest.starts_with('/') {
                return Ok(());
            }
        }
    }
    Err(format!("refusing to download over a non-HTTPS URL: {url}"))
}

/// The hex digest from a `sha256sum`-style line (`<64 hex>  <name>`), or a bare digest.
pub fn parse_sha256(text: &str) -> Result<String, String> {
    let token = text
        .split_whitespace()
        .next()
        .ok_or_else(|| "the checksum file is empty".to_owned())?;
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("not a SHA-256 digest: {token:?}"));
    }
    Ok(token.to_ascii_lowercase())
}

/// Check `bytes` against `expected_hex`.
pub fn verify(bytes: &[u8], expected_hex: &str) -> Result<(), String> {
    let actual = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if actual == expected_hex {
        Ok(())
    } else {
        Err(format!(
            "checksum mismatch: the download hashes to {actual}, the release lists {expected_hex}"
        ))
    }
}

/// Extract a gzipped tar into `dest`, refusing anything but plain files and
/// directories, any path that would leave `dest`, and more than
/// [`MAX_EXTRACTED`] bytes. Returns the number of files written.
pub fn extract(tar_gz: &[u8], dest: &Path) -> Result<usize, String> {
    let limited = GzDecoder::new(tar_gz).take(MAX_EXTRACTED + 1);
    let mut archive = tar::Archive::new(limited);
    let entries = archive
        .entries()
        .map_err(|e| format!("not a tar.gz: {e}"))?;
    let (mut files, mut total) = (0usize, 0u64);
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("corrupt archive: {e}"))?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(format!("unsupported entry type {kind:?} in the archive"));
        }
        total += entry.header().size().unwrap_or(0);
        if total > MAX_EXTRACTED {
            return Err(format!("the archive expands past {MAX_EXTRACTED} bytes"));
        }
        let path = entry.path().map_err(|e| format!("bad entry path: {e}"))?;
        let shown = path.display().to_string();
        let unpacked = entry
            .unpack_in(dest)
            .map_err(|e| format!("could not extract {shown}: {e}"))?;
        if !unpacked {
            return Err(format!("entry {shown} would leave the destination"));
        }
        files += usize::from(kind.is_file());
    }
    if !dest.join("index.html").is_file() {
        return Err("the archive has no index.html at its root".to_owned());
    }
    Ok(files)
}
