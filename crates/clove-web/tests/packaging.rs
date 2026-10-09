//! Guards the invariants that make a published `clove-web` embed the real UI
//! instead of the placeholder, and that keep `cargo publish` clean.
//!
//! The failures are silent: the crate builds, the server starts, and the user
//! gets a "the SPA was not built" page. None is caught by an assertion on the
//! serving code, because the serving code is fine — the assets never arrived.

use std::path::{Path, PathBuf};

// The pure half of the build script's fetch: checksum parsing and verification,
// and the path-safe, size-capped tar.gz extraction.
#[path = "../build/fetch.rs"]
#[allow(dead_code)]
mod fetch;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn manifest_include() -> String {
    let manifest = std::fs::read_to_string(crate_dir().join("Cargo.toml")).unwrap();
    manifest
        .lines()
        .find(|l| l.trim_start().starts_with("include"))
        .expect("clove-web must declare `include` so web/ and tests/ stay out of the .crate")
        .to_owned()
}

/// The built SPA must not ride in the package: `dist-gz/` is git-ignored, so
/// naming it in `include` makes Cargo call the tree dirty and every publish needs
/// `--allow-dirty`. The assets are fetched at build time instead.
#[test]
fn the_package_does_not_include_generated_assets() {
    let include = manifest_include();
    assert!(
        !include.contains("dist"),
        "`include` must not list generated SPA output; found: {include}"
    );
}

/// `build.rs` pulls in `build/fetch.rs`; leave it out of `include` and the
/// published crate fails to compile.
#[test]
fn the_package_includes_the_build_script_modules() {
    let include = manifest_include();
    assert!(
        include.contains("build.rs") && include.contains("build/**"),
        "`include` must ship build.rs and build/**; found: {include}"
    );
}

/// The packaged build must never write into the package source (Cargo's publish
/// verification rejects it) and must stay buildable on docs.rs, which has no
/// network.
#[test]
fn the_build_script_keeps_its_escape_hatches() {
    let build_rs = std::fs::read_to_string(crate_dir().join("build.rs")).unwrap();
    for needle in [
        "OUT_DIR",
        "DOCS_RS",
        "CLOVE_WEB_DIST_DIR",
        "CLOVE_WEB_DIST_BASE_URL",
    ] {
        assert!(
            build_rs.contains(needle),
            "build.rs lost its use of {needle}"
        );
    }
    let assets = std::fs::read_to_string(crate_dir().join("src/assets.rs")).unwrap();
    assert!(
        assets.contains("$OUT_DIR/dist-gz"),
        "assets.rs must embed from OUT_DIR, not from the package directory"
    );
}

/// A real build leaves ~50 files under `dist-gz/`; the placeholder leaves one.
/// Skipped when this build embedded the placeholder (no npm in the environment,
/// `CLOVE_SKIP_WEB_BUILD=1` without a prior build, or docs.rs) — the point is to
/// catch a *silent* downgrade in a tree that did build the SPA, not to force npm
/// onto every test run.
#[test]
fn a_built_tree_has_more_than_the_placeholder() {
    let dist_gz = Path::new(env!("OUT_DIR")).join("dist-gz");
    if !dist_gz.join("_app").is_dir() {
        eprintln!("skipping: this build embedded the placeholder UI");
        return;
    }
    let count = count_files(&dist_gz);
    assert!(
        count > 1,
        "dist-gz/ has a real `_app` dir but only {count} file(s); the asset mirror \
         is truncated and the crate would embed a broken UI"
    );
}

fn count_files(dir: &Path) -> usize {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => stack.push(entry.path()),
                Ok(_) => total += 1,
                Err(_) => {}
            }
        }
    }
    total
}

// ---- the fetch half: verification and extraction ----

const GOOD_DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[test]
fn asset_names_and_urls_follow_the_version() {
    let (tarball, checksum) = fetch::asset_names("1.2.3");
    assert_eq!(tarball, "clove-web-dist-v1.2.3.tar.gz");
    assert_eq!(checksum, "clove-web-dist-v1.2.3.tar.gz.sha256");
    assert_eq!(
        fetch::base_url("1.2.3", None),
        "https://github.com/egeapak/clove/releases/download/v1.2.3"
    );
    assert_eq!(
        fetch::base_url("1.2.3", Some("http://127.0.0.1:8000/x/")),
        "http://127.0.0.1:8000/x"
    );
}

#[test]
fn downloads_must_be_https_or_loopback() {
    assert!(fetch::check_url("https://example.com/a").is_ok());
    assert!(fetch::check_url("http://127.0.0.1:8080/a").is_ok());
    assert!(fetch::check_url("http://localhost/a").is_ok());
    assert!(fetch::check_url("http://[::1]:9000/a").is_ok());
    assert!(fetch::check_url("http://127.0.0.1").is_ok());
    for bad in [
        "http://example.com/a",
        "http://127.0.0.1.evil.example/a",
        "http://localhost.evil.example/a",
        "http://127.0.0.1:80@evil.example/a",
        "http://localhost@evil.example/a",
        "http://user:pw@127.0.0.1/a",
        "http://[::1]@evil.example/a",
        "ftp://example.com/a",
        "file:///etc/passwd",
    ] {
        assert!(fetch::check_url(bad).is_err(), "{bad} must be refused");
    }
}

#[test]
fn checksum_lines_parse_and_reject_junk() {
    let line = format!("{GOOD_DIGEST}  clove-web-dist-v1.tar.gz\n");
    assert_eq!(fetch::parse_sha256(&line).unwrap(), GOOD_DIGEST);
    assert_eq!(fetch::parse_sha256(GOOD_DIGEST).unwrap(), GOOD_DIGEST);
    assert_eq!(
        fetch::parse_sha256(&GOOD_DIGEST.to_ascii_uppercase()).unwrap(),
        GOOD_DIGEST
    );
    for bad in ["", "   \n", "deadbeef", &"z".repeat(64), &"a".repeat(63)] {
        assert!(fetch::parse_sha256(bad).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn verify_accepts_the_right_digest_and_names_a_mismatch() {
    // SHA-256 of the empty input.
    assert!(fetch::verify(b"", GOOD_DIGEST).is_ok());
    let err = fetch::verify(b"tampered", GOOD_DIGEST).unwrap_err();
    assert!(err.contains("checksum mismatch"), "{err}");
    assert!(err.contains(GOOD_DIGEST), "{err}");
}

/// A gzipped tar of `(path, bytes)` files, built by hand so a test can include
/// entries a well-behaved packer never would.
fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        // `set_path` rejects `..`, which is the case being tested: write the
        // name into the raw header instead.
        let name = path.as_bytes();
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_cksum();
        builder.append(&header, *data).unwrap();
    }
    let tar_bytes = builder.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &tar_bytes).unwrap();
    gz.finish().unwrap()
}

#[test]
fn extract_unpacks_a_well_formed_archive() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tar_gz(&[
        ("index.html", b"<html></html>"),
        ("_app/immutable/a.js", b"let a;"),
    ]);
    let files = fetch::extract(&archive, tmp.path()).unwrap();
    assert_eq!(files, 2);
    assert!(tmp.path().join("_app/immutable/a.js").is_file());
}

/// What `tar -czf … -C dist .` (pack-web-dist.sh) actually produces: a `./`
/// directory entry and `./`-prefixed paths.
#[test]
fn extract_unpacks_the_archive_shape_the_pack_script_makes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut builder = tar::Builder::new(Vec::new());
    let mut dir = tar::Header::new_gnu();
    dir.set_entry_type(tar::EntryType::Directory);
    dir.set_size(0);
    dir.set_mode(0o755);
    dir.as_old_mut().name[..2].copy_from_slice(b"./");
    dir.set_cksum();
    builder.append(&dir, &b""[..]).unwrap();
    let mut file = tar::Header::new_gnu();
    file.set_entry_type(tar::EntryType::Regular);
    file.set_size(5);
    file.set_mode(0o644);
    file.as_old_mut().name[..12].copy_from_slice(b"./index.html");
    file.set_cksum();
    builder.append(&file, &b"<html"[..]).unwrap();
    let tar_bytes = builder.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &tar_bytes).unwrap();
    let archive = gz.finish().unwrap();

    assert_eq!(fetch::extract(&archive, tmp.path()).unwrap(), 1);
    assert_eq!(
        std::fs::read(tmp.path().join("index.html")).unwrap(),
        b"<html"
    );
}

#[test]
fn extract_refuses_paths_that_leave_the_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let inner = tmp.path().join("dest");
    std::fs::create_dir(&inner).unwrap();
    let archive = tar_gz(&[("index.html", b"x"), ("../escaped.txt", b"owned")]);
    assert!(fetch::extract(&archive, &inner).is_err());
    assert!(!tmp.path().join("escaped.txt").exists());
}

#[test]
fn extract_refuses_links_and_a_missing_index() {
    let tmp = tempfile::tempdir().unwrap();
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    builder
        .append_link(&mut header, "index.html", "/etc/passwd")
        .unwrap();
    let tar_bytes = builder.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &tar_bytes).unwrap();
    let linked = gz.finish().unwrap();
    assert!(fetch::extract(&linked, tmp.path()).is_err());

    let no_index = tar_gz(&[("other.html", b"x")]);
    let err = fetch::extract(&no_index, tempfile::tempdir().unwrap().path()).unwrap_err();
    assert!(err.contains("index.html"), "{err}");
}

#[test]
fn extract_rejects_garbage() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(fetch::extract(b"not a gzip stream", tmp.path()).is_err());
}
