//! `clove plugin install / uninstall / update` end to end.
//!
//! Two fixtures make this hermetic and fast:
//!
//! - an **in-process mock crates.io** (`CLOVE_REGISTRY_URL`), serving whatever a
//!   test seeds — so the suite behaves *as if* `clove-plugin` and the first-party
//!   plugin crates were published, without publishing anything;
//! - a **fake `cargo`** on `PATH` that records its argv and materializes the
//!   binary it was asked to install. The real thing would take minutes and reach
//!   the network; what these tests need to assert is the orchestration — which
//!   argv is built, what happens after, and what is rolled back.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use assert_cmd::prelude::*;
use serde_json::{json, Value};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Mock crates.io
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Registry {
    /// crate name → the `GET /crates/{name}` body.
    crates: std::collections::HashMap<String, Value>,
    /// The crates listed as depending on `clove-plugin`.
    dependents: Vec<String>,
    /// When false, `reverse_dependencies` 404s — `clove-plugin` unpublished.
    registry_published: bool,
}

struct MockCratesIo {
    state: Arc<Mutex<Registry>>,
    addr: SocketAddr,
}

impl MockCratesIo {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(Registry {
            registry_published: true,
            ..Registry::default()
        }));
        let thread_state = Arc::clone(&state);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                handle(stream, &thread_state);
            }
        });
        Self { state, addr }
    }

    fn url(&self) -> String {
        format!("http://{}/api/v1", self.addr)
    }

    /// Publish a crate that builds `bin`, and register it as a `clove-plugin`
    /// dependent — i.e. a well-formed, discoverable plugin.
    fn publish_plugin(&self, name: &str, version: &str, bin: &str) {
        self.publish(name, version, &[bin]);
        self.state.lock().unwrap().dependents.push(name.to_owned());
    }

    /// Publish a crate *without* registering it as a dependent — the squatter
    /// shape: the name exists, but nothing ties it to clove.
    fn publish(&self, name: &str, version: &str, bins: &[&str]) {
        let body = json!({
            "crate": {
                "name": name,
                "description": format!("{name} plugin"),
                "repository": "https://example.com/repo",
                "downloads": 1234
            },
            "versions": [{
                "id": 1,
                "crate": name,
                "num": version,
                "yanked": false,
                "bin_names": bins,
                "published_by": { "login": "publisher" },
                "description": format!("{name} plugin"),
                "repository": "https://example.com/repo"
            }]
        });
        self.state
            .lock()
            .unwrap()
            .crates
            .insert(name.to_owned(), body);
    }

    fn unpublish_registry_root(&self) {
        self.state.lock().unwrap().registry_published = false;
    }
}

fn handle(mut stream: TcpStream, state: &Arc<Mutex<Registry>>) {
    let mut buf = [0u8; 8192];
    let n = match stream.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let request = String::from_utf8_lossy(&buf[..n]).into_owned();
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("")
        .to_owned();

    let s = state.lock().unwrap();
    let (status, body) = if path.contains("/reverse_dependencies") {
        if !s.registry_published {
            (404, json!({"errors":[{"detail":"crate does not exist"}]}))
        } else {
            let deps: Vec<Value> = s
                .dependents
                .iter()
                .enumerate()
                .map(|(i, _)| json!({"version_id": i, "crate_id": "clove-plugin", "kind": "normal", "downloads": 10}))
                .collect();
            let versions: Vec<Value> = s
                .dependents
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let bins = s
                        .crates
                        .get(name)
                        .map(|c| c["versions"][0]["bin_names"].clone());
                    json!({
                        "id": i, "crate": name, "num": "1.0.0", "yanked": false,
                        "bin_names": bins.unwrap_or(json!([name])),
                    })
                })
                .collect();
            (200, json!({"dependencies": deps, "versions": versions}))
        }
    } else {
        // `GET /crates/{name}`
        let name = path
            .trim_start_matches("/api/v1/crates/")
            .split(['?', '/'])
            .next()
            .unwrap_or("");
        match s.crates.get(name) {
            Some(body) => (200, body.clone()),
            None => (404, json!({"errors":[{"detail":"crate does not exist"}]})),
        }
    };
    drop(s);

    let payload = body.to_string();
    let response = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

// ---------------------------------------------------------------------------
// Fake cargo
// ---------------------------------------------------------------------------

/// The real `cargo`, resolved before the shim shadows it.
fn which_cargo() -> std::path::PathBuf {
    if let Ok(explicit) = std::env::var("CARGO") {
        return std::path::PathBuf::from(explicit);
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = dir.join("cargo");
        if candidate.is_file() {
            return candidate;
        }
    }
    std::path::PathBuf::from("cargo")
}

/// A directory holding a `cargo` shim, to be prepended to `PATH`.
///
/// The shim appends its argv to `<dir>/argv.log` and, for `install`, creates the
/// binary named by `--bin` under `--root`/bin. `plugin_behavior` lets a test make
/// that binary answer `--clove-plugin-info` in a particular way — which is how
/// the post-install gate and its rollback are exercised.
fn fake_cargo(dir: &Path, plugin_behavior: &str) {
    let log = dir.join("argv.log");
    // Only `install`/`uninstall` are faked. Everything else — notably
    // `cargo metadata`, which the git path uses to work out which package in a
    // repository is a plugin — is passed through to the real cargo, or the shim
    // would be answering questions it knows nothing about.
    let real_cargo = which_cargo();
    let script = format!(
        r#"#!/bin/sh
case "$1" in
  install|uninstall) ;;
  *) exec "{real_cargo}" "$@" ;;
esac
printf '%s\n' "$*" >> "{log}"
if [ "$1" = "install" ]; then
  root=""; bin=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --root) root="$2"; shift 2 ;;
      --bin) bin="$2"; shift 2 ;;
      *) shift ;;
    esac
  done
  mkdir -p "$root/bin"
  cat > "$root/bin/$bin" <<'PLUGIN'
{plugin_behavior}
PLUGIN
  chmod +x "$root/bin/$bin"
fi
exit 0
"#,
        log = log.display(),
        real_cargo = real_cargo.display(),
    );
    let path = dir.join("cargo");
    std::fs::write(&path, script).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).unwrap();
}

/// A plugin that answers the compat probe as built against this clove.
fn compatible_plugin() -> String {
    let api = clove_plugin::CLOVE_PLUGIN_API;
    format!(
        r#"#!/bin/sh
if [ "$1" = "--clove-plugin-info" ]; then
  echo '{{"name":"p","version":"1.0.0","about":"ok","provides":["sync:gitlab"],"clove_plugin_api":{api},"min_clove_plugin_api":{api},"max_clove_plugin_api":{api},"max_schema":1}}'
fi
exit 0"#
    )
}

/// A plugin that demands a newer clove than this one — gate 3 must refuse it.
fn needs_newer_clove_plugin() -> String {
    let api = clove_plugin::CLOVE_PLUGIN_API;
    format!(
        r#"#!/bin/sh
if [ "$1" = "--clove-plugin-info" ]; then
  echo '{{"name":"p","version":"9.0.0","about":"future","provides":["sync:gitlab"],"clove_plugin_api":{next},"min_clove_plugin_api":{next},"max_clove_plugin_api":{next},"max_schema":1}}'
fi
exit 0"#,
        next = api + 1
    )
}

fn cargo_log(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("argv.log")).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Env {
    _tmp: TempDir,
    home: std::path::PathBuf,
    bin: std::path::PathBuf,
    registry: MockCratesIo,
}

fn env_with(plugin_behavior: &str) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("clove-home");
    let bin = tmp.path().join("fakebin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    fake_cargo(&bin, plugin_behavior);
    Env {
        _tmp: tmp,
        home,
        bin,
        registry: MockCratesIo::start(),
    }
}

impl Env {
    fn clove(&self) -> Command {
        let mut cmd = Command::cargo_bin("clove").unwrap();
        cmd.env_remove("CLOVE_FORMAT");
        cmd.env("CLOVE_HOME", &self.home);
        cmd.env("CLOVE_REGISTRY_URL", self.registry.url());
        // The fake cargo must win over the real one.
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        cmd.env("PATH", path);
        cmd
    }

    /// Simulate cargo's bookkeeping for an already-installed package.
    fn seed_installed(&self, pkgid: &str, bins: &[&str]) {
        let body = json!({ "installs": { pkgid: { "bins": bins } } });
        std::fs::write(
            self.home.join(".crates2.json"),
            serde_json::to_string(&body).unwrap(),
        )
        .unwrap();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn install_pins_the_version_and_the_single_binary() {
    let env = env_with(&compatible_plugin());
    env.registry
        .publish_plugin("clove-sync-gitlab", "0.2.0", "clove-sync-gitlab");

    let assert = env
        .clove()
        .args(["--format", "json", "plugin", "install", "gitlab", "--yes"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["data"]["installed"], true, "{v}");
    assert_eq!(v["data"]["package"], "clove-sync-gitlab");

    let log = cargo_log(&env.bin);
    assert!(log.contains("--bin clove-sync-gitlab"), "argv: {log}");
    assert!(
        log.contains("--version =0.2.0"),
        "the approved version must be pinned, or the prompt was advisory: {log}"
    );
    assert!(log.contains("--locked"), "argv: {log}");
}

#[test]
fn a_crate_the_registry_does_not_vouch_for_is_refused() {
    // The squatter shape: `clove-sync-evil` exists on crates.io but nothing ties
    // it to clove. The registry answered, so this is a definite "no".
    let env = env_with(&compatible_plugin());
    env.registry
        .publish("clove-sync-evil", "1.0.0", &["clove-sync-evil"]);
    // Something else is a real dependent, so reverse-deps is non-empty.
    env.registry
        .publish_plugin("clove-sync-gitlab", "0.2.0", "clove-sync-gitlab");

    env.clove()
        .args(["plugin", "install", "evil", "--yes"])
        .assert()
        .failure();
    assert!(
        !cargo_log(&env.bin).contains("install"),
        "cargo must never be invoked for a crate the registry disowns"
    );
}

#[test]
fn a_non_interactive_run_refuses_without_yes() {
    // The rule the superseded design had backwards. Nothing may be built.
    let env = env_with(&compatible_plugin());
    env.registry
        .publish_plugin("clove-sync-gitlab", "0.2.0", "clove-sync-gitlab");

    let assert = env
        .clove()
        .args(["plugin", "install", "gitlab"])
        .assert()
        .failure();
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(err.contains("--yes"), "must say how to proceed: {err}");
    assert!(
        !cargo_log(&env.bin).contains("install"),
        "no third-party code may be built without consent"
    );
}

#[test]
fn an_incompatible_plugin_is_rolled_back_not_left_on_the_search_path() {
    // Gate 3 runs after cargo has already placed the binary in <clove-home>/bin,
    // which is on the plugin search path. Refusing without removing it would
    // leave the rejected plugin resolvable by the next dispatch.
    let env = env_with(&needs_newer_clove_plugin());
    env.registry
        .publish_plugin("clove-sync-gitlab", "0.2.0", "clove-sync-gitlab");

    let assert = env
        .clove()
        .args(["plugin", "install", "gitlab", "--yes"])
        .assert()
        .failure();
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(err.contains("newer clove"), "{err}");
    assert!(err.contains("rolled back"), "{err}");

    let log = cargo_log(&env.bin);
    assert!(log.contains("install"), "it did install first: {log}");
    assert!(
        log.contains("uninstall"),
        "the refused plugin must be uninstalled: {log}"
    );
}

#[test]
fn an_unpublished_registry_does_not_block_an_install_but_is_reported() {
    // `clove-plugin` not published → gate 1 is unevaluable. That must not block
    // every install (it would today), but the prompt has to say so. With --yes
    // the install proceeds; with --strict it refuses.
    let env = env_with(&compatible_plugin());
    env.registry
        .publish("clove-sync-gitlab", "0.2.0", &["clove-sync-gitlab"]);
    env.registry.unpublish_registry_root();

    env.clove()
        .args(["plugin", "install", "gitlab", "--yes"])
        .assert()
        .success();
    assert!(cargo_log(&env.bin).contains("install"));

    // …and --strict makes the same situation fatal.
    let env2 = env_with(&compatible_plugin());
    env2.registry
        .publish("clove-sync-gitlab", "0.2.0", &["clove-sync-gitlab"]);
    env2.registry.unpublish_registry_root();
    env2.clove()
        .args(["plugin", "install", "gitlab", "--yes", "--strict"])
        .assert()
        .failure();
    assert!(!cargo_log(&env2.bin).contains("install"));
}

#[test]
fn an_ambiguous_bare_name_asks_instead_of_guessing_which_mux_wins() {
    // Both `clove-sync-x` and `clove-import-x` exist. A first-match ladder would
    // have to pick one, and would then disagree with dispatch about which binary
    // is authoritative — so the command refuses and names both.
    let env = env_with(&compatible_plugin());
    env.registry
        .publish_plugin("clove-sync-x", "1.0.0", "clove-sync-x");
    env.registry
        .publish_plugin("clove-import-x", "1.0.0", "clove-import-x");

    let assert = env
        .clove()
        .args(["plugin", "install", "x", "--yes"])
        .assert()
        .failure();
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(err.contains("ambiguous"), "{err}");
    assert!(
        err.contains("clove-sync-x") && err.contains("clove-import-x"),
        "{err}"
    );
    assert!(!cargo_log(&env.bin).contains("install"));
}

#[test]
fn uninstall_uses_the_package_name_and_needs_no_network() {
    // The package differs from the binary, and the user asks by the binary. The
    // mapping comes from cargo's bookkeeping, so this works offline — the mock
    // registry is deliberately not consulted.
    let env = env_with(&compatible_plugin());
    env.seed_installed(
        "clove-plugin-echo 0.1.0 (registry+https://github.com/rust-lang/crates.io-index)",
        &["clove-echo"],
    );

    let assert = env
        .clove()
        .args(["--format", "json", "plugin", "uninstall", "echo"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["data"]["package"], "clove-plugin-echo");

    let log = cargo_log(&env.bin);
    assert!(
        log.contains("uninstall") && log.contains("clove-plugin-echo"),
        "cargo uninstall takes the package, not the bin: {log}"
    );
    assert!(
        !log.contains("clove-echo "),
        "the bin name must not be passed: {log}"
    );
}

#[test]
fn uninstalling_something_clove_did_not_install_says_so() {
    let env = env_with(&compatible_plugin());
    let assert = env
        .clove()
        .args(["plugin", "uninstall", "never-installed"])
        .assert()
        .failure();
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(err.contains("never-installed"), "{err}");
    assert!(!cargo_log(&env.bin).contains("uninstall"));
}

#[test]
fn update_reports_a_version_change_before_making_it() {
    let env = env_with(&compatible_plugin());
    env.seed_installed(
        "clove-sync-gitlab 0.1.0 (registry+https://github.com/rust-lang/crates.io-index)",
        &["clove-sync-gitlab"],
    );
    env.registry
        .publish_plugin("clove-sync-gitlab", "0.2.0", "clove-sync-gitlab");

    let assert = env
        .clove()
        .args(["--format", "json", "plugin", "update", "--yes"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["data"]["updated"][0]["from"], "0.1.0");
    assert_eq!(v["data"]["updated"][0]["to"], "0.2.0");

    let log = cargo_log(&env.bin);
    assert!(log.contains("--version =0.2.0"), "{log}");
    assert!(
        log.contains("--force"),
        "an update replaces in place: {log}"
    );
}

#[test]
fn update_does_not_re_resolve_a_git_install_through_crates_io() {
    // Silently converting a git install into a registry one would swap the code
    // the user chose for a same-named crate from a different source.
    let env = env_with(&compatible_plugin());
    env.seed_installed(
        "clove-sync-gitlab 0.1.0 (git+https://example.com/x?tag=v1#abc)",
        &["clove-sync-gitlab"],
    );
    env.registry
        .publish_plugin("clove-sync-gitlab", "9.9.9", "clove-sync-gitlab");

    let assert = env
        .clove()
        .args(["--format", "json", "plugin", "update", "--yes"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(
        v["data"]["updated"].as_array().unwrap().len(),
        0,
        "a git install must not be updated from crates.io: {v}"
    );
    assert!(!cargo_log(&env.bin).contains("install"));
}

#[test]
fn installing_something_already_installed_is_a_no_op_without_force() {
    let env = env_with(&compatible_plugin());
    env.seed_installed(
        "clove-sync-gitlab 0.2.0 (registry+https://github.com/rust-lang/crates.io-index)",
        &["clove-sync-gitlab"],
    );
    env.registry
        .publish_plugin("clove-sync-gitlab", "0.2.0", "clove-sync-gitlab");

    let assert = env
        .clove()
        .args(["--format", "json", "plugin", "install", "gitlab", "--yes"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["data"]["already_installed"], true, "{v}");
    assert!(!cargo_log(&env.bin).contains("install"));
}

#[test]
fn a_traversal_or_flag_shaped_name_never_reaches_cargo_or_a_url() {
    let env = env_with(&compatible_plugin());
    for name in ["../../summary", "--config=x"] {
        env.clove()
            .args(["plugin", "install", "--", name, "--yes"])
            .assert()
            .failure();
    }
    assert!(
        cargo_log(&env.bin).is_empty(),
        "cargo must never be invoked for an invalid name"
    );
}

// ---------------------------------------------------------------------------
// `install --git`
//
// These build real local git repositories and let clove clone them over
// `file://`, so the clone, the manifest inspection and the package selection are
// all exercised for real — only the final `cargo install` is faked.
// ---------------------------------------------------------------------------

fn run(cmd: &mut Command) {
    let out = cmd.output().expect("spawn");
    assert!(
        out.status.success(),
        "{:?} failed: {}",
        cmd,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A git repository containing `members`, each `(package, bin, extra_manifest)`.
fn make_repo(dir: &Path, members: &[(&str, &str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    let names: Vec<String> = members
        .iter()
        .map(|(p, _, _)| format!("\"crates/{p}\""))
        .collect();
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[workspace]\nresolver = \"2\"\nmembers = [{}]\n",
            names.join(", ")
        ),
    )
    .unwrap();

    for (package, bin, extra) in members {
        let crate_dir = dir.join("crates").join(package);
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        std::fs::write(
            crate_dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{package}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
                 {extra}\n[[bin]]\nname = \"{bin}\"\npath = \"src/main.rs\"\n\
                 [dependencies]\nclove-plugin = \"1\"\n"
            ),
        )
        .unwrap();
        std::fs::write(crate_dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    }

    run(Command::new("git").arg("init").arg("-q").current_dir(dir));
    run(Command::new("git")
        .args(["config", "user.email", "t@example.com"])
        .current_dir(dir));
    run(Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(dir));
    run(Command::new("git").args(["add", "-A"]).current_dir(dir));
    run(Command::new("git")
        .args(["commit", "-q", "-m", "init"])
        .current_dir(dir));
}

#[test]
fn install_from_a_git_repo_with_one_plugin() {
    let env = env_with(&compatible_plugin());
    let repo = env._tmp.path().join("srcrepo");
    make_repo(&repo, &[("clove-sync-gitlab", "clove-sync-gitlab", "")]);

    let assert = env
        .clove()
        .args([
            "--format",
            "json",
            "plugin",
            "install",
            "--git",
            &format!("file://{}", repo.display()),
            "--yes",
        ])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["data"]["package"], "clove-sync-gitlab", "{v}");

    let log = cargo_log(&env.bin);
    assert!(log.contains("--git"), "{log}");
    assert!(log.contains("--bin clove-sync-gitlab"), "{log}");
    // An unpinned install must warn that the branch moves.
    assert!(
        v["_meta"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("--tag")),
        "{v}"
    );
}

#[test]
fn a_workspace_shaped_like_clove_offers_only_the_real_plugins() {
    // The over-match this filter exists for: three real plugins, plus the host
    // (a `clove-plugin` dependent that builds `clove`) and a `publish = false`
    // fixture. Selecting without --package must name exactly the three.
    let env = env_with(&compatible_plugin());
    let repo = env._tmp.path().join("srcrepo");
    make_repo(
        &repo,
        &[
            ("clove-sync-github", "clove-sync-github", ""),
            ("clove-import-tk", "clove-import-tk", ""),
            ("clove-import-beads", "clove-import-beads", ""),
            ("clove-cli", "clove", ""),
            ("clove-plugin-echo", "clove-echo", "publish = false"),
        ],
    );

    let assert = env
        .clove()
        .args([
            "plugin",
            "install",
            "--git",
            &format!("file://{}", repo.display()),
            "--yes",
        ])
        .assert()
        .failure();
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();

    assert!(err.contains("several"), "{err}");
    for expected in ["clove-sync-github", "clove-import-tk", "clove-import-beads"] {
        assert!(err.contains(expected), "{expected} missing from: {err}");
    }
    assert!(
        !err.contains("clove-cli"),
        "the host must not be offered as a plugin: {err}"
    );
    assert!(
        !err.contains("clove-plugin-echo"),
        "a publish=false fixture must not be offered: {err}"
    );
    assert!(!cargo_log(&env.bin).contains("install"));
}

#[test]
fn package_selects_one_plugin_from_a_multi_plugin_repo() {
    let env = env_with(&compatible_plugin());
    let repo = env._tmp.path().join("srcrepo");
    make_repo(
        &repo,
        &[
            ("clove-sync-github", "clove-sync-github", ""),
            ("clove-import-tk", "clove-import-tk", ""),
        ],
    );

    env.clove()
        .args([
            "plugin",
            "install",
            "--git",
            &format!("file://{}", repo.display()),
            "--package",
            "clove-import-tk",
            "--yes",
        ])
        .assert()
        .success();

    let log = cargo_log(&env.bin);
    assert!(log.contains("--bin clove-import-tk"), "{log}");
    assert!(!log.contains("clove-sync-github"), "{log}");
}

#[test]
fn a_repo_with_no_clove_plugin_is_refused() {
    let env = env_with(&compatible_plugin());
    let repo = env._tmp.path().join("srcrepo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"unrelated\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
    run(Command::new("git").arg("init").arg("-q").current_dir(&repo));
    run(Command::new("git")
        .args(["config", "user.email", "t@example.com"])
        .current_dir(&repo));
    run(Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(&repo));
    run(Command::new("git").args(["add", "-A"]).current_dir(&repo));
    run(Command::new("git")
        .args(["commit", "-q", "-m", "init"])
        .current_dir(&repo));

    env.clove()
        .args([
            "plugin",
            "install",
            "--git",
            &format!("file://{}", repo.display()),
            "--yes",
        ])
        .assert()
        .failure();
    assert!(!cargo_log(&env.bin).contains("install"));
}

#[test]
fn a_tag_pins_the_install_and_suppresses_the_moving_branch_warning() {
    let env = env_with(&compatible_plugin());
    let repo = env._tmp.path().join("srcrepo");
    make_repo(&repo, &[("clove-sync-gitlab", "clove-sync-gitlab", "")]);
    run(Command::new("git")
        .args(["tag", "v1.0.0"])
        .current_dir(&repo));

    let assert = env
        .clove()
        .args([
            "--format",
            "json",
            "plugin",
            "install",
            "--git",
            &format!("file://{}", repo.display()),
            "--tag",
            "v1.0.0",
            "--yes",
        ])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert!(
        v["_meta"]["warnings"].as_array().unwrap().is_empty(),
        "a pinned install has nothing to warn about: {v}"
    );

    let log = cargo_log(&env.bin);
    assert!(
        log.contains("--tag v1.0.0"),
        "the pin must reach cargo: {log}"
    );
}

#[test]
fn a_flag_shaped_git_url_never_reaches_git() {
    let env = env_with(&compatible_plugin());
    for url in [
        "--upload-pack=/bin/sh",
        "--template=/tmp/evil",
        "ext::sh -c id",
    ] {
        env.clove()
            .args(["plugin", "install", "--git", url, "--yes"])
            .assert()
            .failure();
    }
    assert!(cargo_log(&env.bin).is_empty());
}
