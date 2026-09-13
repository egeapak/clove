//! End-to-end tests for Phase 1 of the plugin registry/discovery system
//! (PLUGIN_REGISTRY.md §3/§6): the enriched `clove plugin list` and the dynamic,
//! plugin-aware `<mux> --help`. All offline — no network, no install, no registry.
//!
//! These reuse the `clove-echo` fixture (which answers `--clove-plugin-info` via
//! the `clove-plugin` harness, so it carries the auto-filled compat fields),
//! copied under the multiplexer-scoped name `clove-import-echo`, and point
//! `CLOVE_PLUGIN_PATH` at its temp dir so the search path is deterministic.

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

/// Build the `clove-echo` fixture and copy it into a fresh temp dir under `name`
/// (e.g. `clove-import-echo`), returning `(dir, path)`.
fn install_echo_as(name: &str) -> (TempDir, PathBuf) {
    let built = escargot::CargoBuild::new()
        .package("clove-plugin-echo")
        .bin("clove-echo")
        .run()
        .expect("build clove-echo fixture");
    let dir = tempfile::tempdir().unwrap();
    // The host resolver looks for `clove-<provider>{EXE_SUFFIX}`, so the renamed
    // copy must carry the platform executable suffix (`.exe` on Windows).
    let dest = dir
        .path()
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(built.path(), &dest).expect("copy echo fixture into the plugin dir");
    (dir, dest)
}

/// A `clove` invocation rooted at `dir` with a hermetic environment.
///
/// `CLOVE_HOME` is pinned into `dir` because `assert_cmd` inherits `$HOME`:
/// without it, the clove-home fallback would resolve to the developer's real
/// `~/.local/share/clove`, and a test could read (or write) their actual plugin
/// install root and registry cache.
fn clove(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("clove").unwrap();
    cmd.current_dir(dir);
    cmd.env_remove("CLOVE_FORMAT");
    cmd.env("CLOVE_AUTHOR", "tester@example.com");
    cmd.env("CLOVE_HOME", dir.join("clove-home"));
    cmd
}

/// A `.clove/` repository with a known id prefix.
fn init_repo(prefix: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", prefix])
        .assert()
        .success();
    dir
}

#[test]
fn plugin_list_json_is_enriched_with_version_provides_status() {
    let (plugin_dir, _echo) = install_echo_as("clove-echo");

    let assert = clove(plugin_dir.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["--format", "json", "plugin", "list"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["ok"], true, "envelope: {v}");

    let echo = v["data"]
        .as_array()
        .expect("data array")
        .iter()
        .find(|p| p["name"] == "echo")
        .expect("echo listed");

    // Additive over the old `{name,path}`: binary, version, provides, commands,
    // installed, status.
    assert_eq!(echo["binary"], "clove-echo");
    // `path` is the full filename, which carries `.exe` on Windows (unlike the
    // stripped `binary`/`name`).
    let echo_file = format!("clove-echo{}", std::env::consts::EXE_SUFFIX);
    assert!(echo["path"].as_str().unwrap().ends_with(&echo_file));
    assert!(
        !echo["version"].as_str().unwrap().is_empty(),
        "echo: {echo}"
    );
    assert_eq!(echo["provides"][0], "echo");
    assert_eq!(echo["commands"][0], "clove echo");
    assert_eq!(echo["installed"], true);
    // The echo fixture is built from this same workspace → compatible.
    assert_eq!(echo["status"], "ok");
}

#[test]
fn plugin_list_human_shows_the_enriched_columns() {
    let (plugin_dir, _echo) = install_echo_as("clove-echo");

    let assert = clove(plugin_dir.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["plugin", "list"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert!(out.contains("NAME"), "header missing: {out}");
    assert!(out.contains("RUN AS"), "header missing: {out}");
    assert!(out.contains("echo"), "echo row missing: {out}");
    assert!(out.contains("clove echo"), "run-as missing: {out}");
}

#[test]
fn import_help_lists_builtins_and_installed_providers() {
    let (plugin_dir, _echo) = install_echo_as("clove-import-echo");
    let repo = init_repo("proj");

    let assert = clove(repo.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["import", "--help"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    // Built-in native formats.
    assert!(out.contains("json"), "json missing: {out}");
    assert!(out.contains("jsonl"), "jsonl missing: {out}");
    // The static clap prose is preserved (not replaced) by the dynamic renderer:
    // the provider list is appended to it, so a static-only token like
    // `--overwrite` still appears alongside the dynamic list.
    assert!(out.contains("--overwrite"), "static prose dropped: {out}");
    // The installed provider line: provider name, the binary, and the run-as.
    assert!(
        out.contains("Installed providers:"),
        "section missing: {out}"
    );
    assert!(out.contains("clove-import-echo"), "binary missing: {out}");
    assert!(out.contains("clove import echo"), "run-as missing: {out}");
    // The globals-precede-provider note is preserved.
    assert!(
        out.contains("must come BEFORE the provider"),
        "note missing: {out}"
    );
}

#[test]
fn help_subcommand_form_is_routed_to_the_dynamic_renderer() {
    // `clove help <mux>` is intercepted and routed to the same dynamic renderer as
    // `clove <mux> --help` (the `detect` unit test pins that both argv spellings
    // resolve to the mux). End-to-end, that means `clove help import` carries the
    // dynamic "Installed providers" list clap's own static help cannot produce —
    // plus the static prose. (We assert the markers rather than byte-comparing two
    // live invocations: the resolver also scans the current-exe dir, which
    // concurrent test binaries mutate by building workspace plugins, so a
    // cross-invocation byte-compare is racy under a parallel test run.)
    let (plugin_dir, _echo) = install_echo_as("clove-import-echo");
    let repo = init_repo("proj");

    let out = {
        let assert = clove(repo.path())
            .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
            .args(["help", "import"])
            .assert()
            .success();
        String::from_utf8(assert.get_output().stdout.clone()).unwrap()
    };

    // The dynamic provider list (only the runtime renderer emits this)…
    assert!(
        out.contains("Installed providers:"),
        "dynamic list missing from `help import`: {out}"
    );
    assert!(out.contains("clove import echo"), "echo row missing: {out}");
    // …alongside clap's static prose (a static-only token proves it is preserved).
    assert!(
        out.contains("--overwrite"),
        "static prose missing from `help import`: {out}"
    );
}

#[test]
fn sync_help_reports_no_builtin_providers() {
    let repo = init_repo("proj");

    let assert = clove(repo.path())
        .env_remove("CLOVE_PLUGIN_PATH")
        .args(["sync", "--help"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    // The static prose (clap `after_help`, now shown by the dynamic renderer too)
    // states there are no built-in sync providers.
    assert!(
        out.contains("no built-in sync providers"),
        "sync builtin note missing: {out}"
    );
}

#[test]
fn provider_help_is_not_intercepted_by_the_dynamic_renderer() {
    // `clove import echo --help` has the help flag PAST the provider, so the
    // dynamic `mux_help` interception does NOT fire (its rule is: the token right
    // after the multiplexer must be the help flag). The proof is that the dynamic
    // "Installed providers:" section — which only the runtime renderer emits — is
    // absent; clap handles the argv along its normal path instead.
    let (plugin_dir, _echo) = install_echo_as("clove-import-echo");
    let repo = init_repo("proj");

    let assert = clove(repo.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["import", "echo", "--help"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert!(
        !out.contains("Installed providers:"),
        "dynamic section should be absent (not intercepted): {out}"
    );
}

// ---------------------------------------------------------------------------
// Registry-backed discovery (`plugin list --all`, `plugin search`).
//
// All hermetic: `CLOVE_REGISTRY_URL` points the client at a dead loopback port,
// so every test here exercises the *degradation* path without touching the real
// crates.io. `CLOVE_HOME` is pinned to a temp dir in `clove()` so no test can
// read or write a developer's real install root or registry cache.
// ---------------------------------------------------------------------------

/// A `clove` invocation whose registry client points at a closed port, so
/// discovery deterministically fails at the transport layer.
fn clove_offline(dir: &Path, home: &Path) -> Command {
    let mut cmd = clove(dir);
    cmd.env("CLOVE_HOME", home);
    // Port 1 on loopback: reserved, and nothing is listening.
    cmd.env("CLOVE_REGISTRY_URL", "http://127.0.0.1:1/api/v1");
    cmd
}

#[test]
fn plain_plugin_list_never_consults_the_registry() {
    let (plugin_dir, _echo) = install_echo_as("clove-echo");
    let home = tempfile::tempdir().unwrap();

    // Even with the registry pointed at a dead port, a plain `list` succeeds and
    // reports nothing about the registry — it is a pure filesystem walk.
    let assert = clove_offline(plugin_dir.path(), home.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["--format", "json", "plugin", "list"])
        .assert()
        .success();

    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert!(
        v["_meta"]["warnings"]
            .as_array()
            .is_some_and(|w| w.is_empty()),
        "plain list must not consult the registry: {v}"
    );
    assert!(v["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["name"] == "echo"));
}

#[test]
fn list_all_degrades_to_the_installed_set_when_discovery_fails() {
    let (plugin_dir, _echo) = install_echo_as("clove-echo");
    let home = tempfile::tempdir().unwrap();

    let assert = clove_offline(plugin_dir.path(), home.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["--format", "json", "plugin", "list", "--all"])
        .assert()
        // Degradation, not failure: a discovery outage must never fail the
        // command or hide the installed plugins.
        .success();

    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["ok"], true, "envelope: {v}");
    assert!(
        !v["_meta"]["warnings"]
            .as_array()
            .expect("warnings array")
            .is_empty(),
        "the failure cause must be reported: {v}"
    );
    assert!(
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "echo"),
        "the installed set must still print: {v}"
    );
}

#[test]
fn jsonl_reports_the_discovery_warning_without_breaking_its_line_shape() {
    // jsonl is "one envelope per line, `data` is a single item" (DESIGN §7.3).
    // A discovery failure must still be reported, but not by appending a
    // `_meta`-only line to stdout: that line has no `data`, so `jq -r .data.name`
    // emits a spurious `null` for it, and this was the only jsonl surface in the
    // repo that did it. The warning goes to stderr, where human mode prints it.
    let (plugin_dir, _echo) = install_echo_as("clove-echo");
    let home = tempfile::tempdir().unwrap();

    let assert = clove_offline(plugin_dir.path(), home.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["--format", "jsonl", "plugin", "list", "--all"])
        .assert()
        .success();

    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let lines: Vec<Value> = out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each line is valid JSON"))
        .collect();

    assert!(
        lines.iter().any(|l| l["data"]["name"] == "echo"),
        "installed plugin missing: {out}"
    );
    for line in &lines {
        assert!(
            line.get("data").is_some(),
            "every jsonl line is an item envelope: {out}"
        );
    }

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("warning:"),
        "the discovery failure must still reach the user: stderr={stderr:?} stdout={out}"
    );
}

#[test]
fn human_list_all_labels_its_sections_even_with_no_plugins_installed() {
    // `render_human` used to return early on an empty installed set, so `--all`
    // on a clean machine printed nothing at all — indistinguishable from a
    // broken command.
    let empty = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();

    let assert = clove_offline(empty.path(), home.path())
        .env("CLOVE_PLUGIN_PATH", empty.path())
        .args(["plugin", "list", "--all"])
        .assert()
        .success();

    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(out.contains("Installed"), "no Installed section: {out}");
    assert!(err.contains("warning:"), "no warning on stderr: {err}");
}

#[test]
fn search_reports_no_matches_without_failing_when_discovery_is_down() {
    let empty = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();

    clove_offline(empty.path(), home.path())
        .env("CLOVE_PLUGIN_PATH", empty.path())
        .args(["--format", "json", "plugin", "search", "gitlab"])
        .assert()
        .success();
}

#[test]
fn a_traversal_or_flag_shaped_query_is_never_put_into_a_request() {
    // The query is interpolated into a URL on the name-probe path, so it has to
    // be filtered before that. Two independent layers do it:
    //
    //   1. clap rejects a bare `-rf` as an unknown flag (exit 1) — it never
    //      reaches the command at all;
    //   2. behind `--`, the value does reach the command, and the crate-name
    //      validator refuses to probe it. That is *not* an error: a search query
    //      may legitimately be a description phrase like "two-way sync". It
    //      simply cannot match a crate name, so there are no name hits.
    let empty = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();

    // Layer 1: clap.
    clove_offline(empty.path(), home.path())
        .env("CLOVE_PLUGIN_PATH", empty.path())
        .args(["plugin", "search", "-rf"])
        .assert()
        .failure();

    // Layer 2: the validator, for values that get past argument parsing.
    for query in ["../../summary", "-rf", "--upload-pack=/bin/sh", "a b"] {
        clove_offline(empty.path(), home.path())
            .env("CLOVE_PLUGIN_PATH", empty.path())
            .args(["--format", "json", "plugin", "search", "--", query])
            .assert()
            .success();
    }
}

#[test]
fn the_registry_cache_lands_in_clove_home_not_a_dot_clove_dir() {
    // The install root must never be `~/.clove`: repository discovery treats any
    // ancestor containing a `.clove/` *directory* as a repo root, so that name
    // would turn the user's home into a clove repository.
    let empty = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();

    clove_offline(empty.path(), home.path())
        .env("CLOVE_PLUGIN_PATH", empty.path())
        .args(["plugin", "list", "--all"])
        .assert()
        .success();

    assert!(
        !home.path().join(".clove").exists(),
        "the clove home must not contain a `.clove/` directory"
    );
}

#[test]
fn a_plugin_in_the_clove_home_bin_is_resolvable_and_pinnable() {
    // Two things at once:
    //
    //   1. `<clove-home>/bin` really is on the search path — a plugin installed
    //      there resolves with no `$CLOVE_PLUGIN_PATH` and no `$PATH` edit, which
    //      is the whole point of the clove-managed install root;
    //   2. `CLOVE_HOME` really does pin it. Every plugin test relies on that: the
    //      root falls back to `~/.local/share/clove/bin`, and `assert_cmd`
    //      inherits `$HOME`, so an unpinned test would resolve a developer's real
    //      installed plugins and silently break "no plugin installed" assertions.
    let home = tempfile::tempdir().unwrap();
    let bin = home.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();

    let built = escargot::CargoBuild::new()
        .package("clove-plugin-echo")
        .bin("clove-echo")
        .run()
        .expect("build clove-echo fixture");
    let dest = bin.join(format!("clove-homed{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(built.path(), &dest).unwrap();

    let workdir = tempfile::tempdir().unwrap();

    // Pinned at the home that holds it → resolvable.
    let assert = clove(workdir.path())
        .env("CLOVE_HOME", home.path())
        .env("CLOVE_PLUGIN_PATH", workdir.path())
        .args(["--format", "json", "plugin", "list"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert!(
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "homed"),
        "a plugin in <clove-home>/bin must resolve: {v}"
    );

    // Pinned elsewhere → invisible. This is the assertion that fails if the
    // pinning ever regresses to reading the real home.
    let other = tempfile::tempdir().unwrap();
    let assert = clove(workdir.path())
        .env("CLOVE_HOME", other.path())
        .env("CLOVE_PLUGIN_PATH", workdir.path())
        .args(["--format", "json", "plugin", "list"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert!(
        !v["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "homed"),
        "CLOVE_HOME must pin the search path: {v}"
    );
}

#[test]
fn clove_plugin_path_outranks_the_clove_home_install_root() {
    // Precedence is security-relevant: `$CLOVE_PLUGIN_PATH` is the user's explicit
    // opt-in directory, so a binary clove fetched from the internet and installed
    // into `<clove-home>/bin` must never shadow it.
    let home = tempfile::tempdir().unwrap();
    let home_bin = home.path().join("bin");
    std::fs::create_dir_all(&home_bin).unwrap();
    let explicit = tempfile::tempdir().unwrap();

    let built = escargot::CargoBuild::new()
        .package("clove-plugin-echo")
        .bin("clove-echo")
        .run()
        .expect("build clove-echo fixture");
    let name = format!("clove-contended{}", std::env::consts::EXE_SUFFIX);
    std::fs::copy(built.path(), home_bin.join(&name)).unwrap();
    std::fs::copy(built.path(), explicit.path().join(&name)).unwrap();

    let workdir = tempfile::tempdir().unwrap();
    let assert = clove(workdir.path())
        .env("CLOVE_HOME", home.path())
        .env("CLOVE_PLUGIN_PATH", explicit.path())
        .args(["--format", "json", "plugin", "list"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();

    let resolved = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "contended")
        .expect("the contended plugin is listed")["path"]
        .as_str()
        .unwrap()
        .to_owned();

    assert!(
        resolved.starts_with(explicit.path().to_str().unwrap()),
        "$CLOVE_PLUGIN_PATH must win over <clove-home>/bin, resolved to {resolved}"
    );
}
