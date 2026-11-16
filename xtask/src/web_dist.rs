//! `cargo xtask pack-web-dist <built dist dir> <version> <output dir>`: pack
//! the built web UI into the release assets that a published `clove-web`
//! downloads at build time (`crates/clove-web/build.rs`):
//!
//! - `clove-web-dist-v<version>.tar.gz`: the contents of `dist/`, at the
//!   archive root.
//! - `clove-web-dist-v<version>.tar.gz.sha256`: `<hex digest>  <file name>`.
//!
//! `.github/workflows/release.yml` runs it and uploads both files
//! (`docs/RELEASE.md` §6).
//!
//! The archive is reproducible: entries are sorted, and their times, owners
//! and modes are fixed (`tar::HeaderMode::Deterministic`). It holds only plain
//! files and directories, because the extractor (`build/fetch.rs`) refuses
//! anything else, so a symlink in `dist/` is an error here and not at a user's
//! build.

use std::fs::File;
use std::io::Write;

use anyhow::{bail, ensure, Context};
use camino::{Utf8Path, Utf8PathBuf};
use flate2::write::GzEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};

/// The two files that [`pack`] writes.
#[derive(Debug)]
pub struct Packed {
    pub tarball: Utf8PathBuf,
    pub checksum: Utf8PathBuf,
    /// The checksum line: `<hex digest>  <file name>`.
    pub line: String,
}

/// Every file and directory under `dir`, relative to it, sorted so that a
/// directory comes before its contents.
fn walk(dir: &Utf8Path) -> anyhow::Result<Vec<Utf8PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![Utf8PathBuf::new()];
    while let Some(rel) = pending.pop() {
        let abs = dir.join(&rel);
        let mut children = Vec::new();
        for entry in abs.read_dir_utf8().with_context(|| format!("read {abs}"))? {
            let entry = entry?;
            let child = rel.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                bail!(
                    "{} is a symlink; the web dist may hold only files and directories",
                    dir.join(&child)
                );
            }
            if kind.is_dir() {
                pending.push(child.clone());
            } else if !kind.is_file() {
                bail!("{} is not a file or directory", dir.join(&child));
            }
            children.push(child);
        }
        found.extend(children);
    }
    found.sort();
    Ok(found)
}

/// Pack `dist` for `version` into `out`.
pub fn pack(dist: &Utf8Path, version: &str, out: &Utf8Path) -> anyhow::Result<Packed> {
    ensure!(
        dist.join("index.html").is_file(),
        "{dist} has no index.html (build the SPA first)"
    );
    ensure!(
        dist.join("_app").is_dir(),
        "{dist} has no _app/ (that is the placeholder, not the built SPA)"
    );
    let version = version.strip_prefix('v').unwrap_or(version);
    std::fs::create_dir_all(out).with_context(|| format!("create {out}"))?;
    let name = format!("clove-web-dist-v{version}.tar.gz");
    let tarball = out.join(&name);

    let file = File::create(&tarball).with_context(|| format!("create {tarball}"))?;
    let mut builder = tar::Builder::new(GzEncoder::new(file, Compression::best()));
    builder.mode(tar::HeaderMode::Deterministic);
    builder.follow_symlinks(false);
    for rel in walk(dist)? {
        builder
            .append_path_with_name(dist.join(&rel), &rel)
            .with_context(|| format!("add {rel} to the archive"))?;
    }
    builder.into_inner()?.finish()?.sync_all()?;

    let digest = Sha256::digest(std::fs::read(&tarball)?);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let line = format!("{hex}  {name}");
    let checksum = out.join(format!("{name}.sha256"));
    let mut sum = File::create(&checksum).with_context(|| format!("create {checksum}"))?;
    writeln!(sum, "{line}")?;
    Ok(Packed {
        tarball,
        checksum,
        line,
    })
}

/// `cargo xtask pack-web-dist <built dist dir> <version> <output dir>`.
pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let [dist, version, out] = args.as_slice() else {
        bail!("usage: cargo xtask pack-web-dist <built dist dir> <version> <output dir>");
    };
    let packed = pack(Utf8Path::new(dist), version, Utf8Path::new(out))?;
    let size = std::fs::metadata(&packed.tarball)?.len();
    println!("packed {} ({size} bytes)", packed.tarball);
    println!("wrote {}: {}", packed.checksum, packed.line);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch;

    fn fake_dist(root: &Utf8Path) -> Utf8PathBuf {
        let dist = root.join("dist");
        std::fs::create_dir_all(dist.join("_app/immutable")).unwrap();
        std::fs::write(dist.join("index.html"), "<!doctype html>").unwrap();
        std::fs::write(dist.join("_app/immutable/app.js"), "console.log(1)").unwrap();
        std::fs::write(dist.join("favicon.svg"), "<svg/>").unwrap();
        dist
    }

    fn utf8(dir: &tempfile::TempDir) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap()
    }

    /// What `pack` writes is what a published `clove-web` build accepts: the
    /// checksum file verifies the tarball, and the extractor unpacks it with
    /// `index.html` at the root.
    #[test]
    fn the_packed_dist_is_what_the_clove_web_build_accepts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = utf8(&tmp);
        let dist = fake_dist(&root);
        let packed = pack(&dist, "1.2.3", &root.join("out")).unwrap();
        assert_eq!(
            packed.tarball.file_name(),
            Some("clove-web-dist-v1.2.3.tar.gz")
        );
        assert_eq!(
            packed.checksum.file_name(),
            Some("clove-web-dist-v1.2.3.tar.gz.sha256")
        );
        let (tarball, checksum) = fetch::asset_names("1.2.3");
        assert_eq!(packed.tarball.file_name(), Some(tarball.as_str()));
        assert_eq!(packed.checksum.file_name(), Some(checksum.as_str()));

        let bytes = std::fs::read(&packed.tarball).unwrap();
        let sum = std::fs::read_to_string(&packed.checksum).unwrap();
        let digest = fetch::parse_sha256(&sum).unwrap();
        fetch::verify(&bytes, &digest).unwrap();

        let dest = tempfile::tempdir().unwrap();
        let files = fetch::extract(&bytes, dest.path()).unwrap();
        assert_eq!(files, 3);
        assert_eq!(
            std::fs::read_to_string(dest.path().join("_app/immutable/app.js")).unwrap(),
            "console.log(1)"
        );
    }

    #[test]
    fn packing_twice_gives_the_same_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = utf8(&tmp);
        let dist = fake_dist(&root);
        let a = pack(&dist, "1.2.3", &root.join("a")).unwrap();
        // A newer time on a file must not change the archive.
        std::fs::write(dist.join("favicon.svg"), "<svg/>").unwrap();
        let b = pack(&dist, "1.2.3", &root.join("b")).unwrap();
        assert_eq!(
            std::fs::read(&a.tarball).unwrap(),
            std::fs::read(&b.tarball).unwrap()
        );
        assert_eq!(a.line, b.line);
    }

    #[test]
    fn the_placeholder_and_a_missing_dist_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = utf8(&tmp);
        let dist = root.join("dist");
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(dist.join("index.html"), "placeholder").unwrap();
        let err = pack(&dist, "1.2.3", &root.join("out")).unwrap_err();
        assert!(err.to_string().contains("no _app/"), "{err}");
        let err = pack(&root.join("missing"), "1.2.3", &root.join("out")).unwrap_err();
        assert!(err.to_string().contains("no index.html"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_in_the_dist_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = utf8(&tmp);
        let dist = fake_dist(&root);
        std::os::unix::fs::symlink("/etc/passwd", dist.join("_app/leak")).unwrap();
        let err = pack(&dist, "1.2.3", &root.join("out")).unwrap_err();
        assert!(err.to_string().contains("is a symlink"), "{err}");
    }
}
