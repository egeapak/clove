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

    /// Publish a crate whose only version is yanked.
    fn publish_yanked(&self, name: &str, version: &str, bin: &str) {
        let body = json!({
            "crate": {"name": name, "description": "y", "repository": null, "downloads": 1},
            "versions": [{
                "id": 1, "crate": name, "num": version, "yanked": true,
                "bin_names": [bin], "published_by": {"login": "publisher"}
            }]
        });
        let mut s = self.state.lock().unwrap();
        s.crates.insert(name.to_owned(), body);
        s.dependents.push(name.to_owned());
    }

    fn unpublish_registry_root(&self) {
        self.state.lock().unwrap().registry_published = false;
    }
}

fn handle(mut stream: TcpStream, state: &Arc<Mutex<Registry>>) {
    // Read until the end of the headers rather than taking whatever one `read`
    // returned: a request split across TCP segments would otherwise parse as an
    // empty path and 404. Loopback almost always delivers one segment, so this
    // was a rare flake rather than a visible bug.
    let mut request = String::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                request.push_str(&String::from_utf8_lossy(&buf[..n]));
                if request.contains("\r\n\r\n") {
                    break;
                }
            }
            Err(_) => return,
        }
    }
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("")
        .to_owned();

    // crates.io answers 403 to a request with no User-Agent — for *every* crate,
    // which reads as "every name is taken". The client sets one; nothing proved
    // it reached the wire. Enforcing it here turns all 29 tests into evidence
    // that it does, and pins the single most surprising thing about this API.
    let has_user_agent = request
        .lines()
        .any(|line| line.to_ascii_lowercase().starts_with("user-agent:") && line.len() > 12);
    if !has_user_agent {
        let payload =
            json!({"errors":[{"detail":"missing or invalid User-Agent header"}]}).to_string();
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            )
            .as_bytes(),
        );
        return;
    }

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
                    // Read `num`/`yanked` from what was actually published.
                    // Hardcoding `1.0.0`/`false` meant discovery always reported
                    // version 1.0.0 and never reported a yanked crate as yanked,
                    // so the two read paths silently disagreed.
                    let published = s.crates.get(name).map(|c| &c["versions"][0]);
                    json!({
                        "id": i,
                        "crate": name,
                        "num": published
                            .and_then(|v| v["num"].as_str())
                            .unwrap_or("1.0.0"),
                        "yanked": published
                            .and_then(|v| v["yanked"].as_bool())
                            .unwrap_or(false),
                        "bin_names": published
                            .map(|v| v["bin_names"].clone())
                            .unwrap_or(json!([name])),
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
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len(),
        reason = if status == 200 { "OK" } else { "Not Found" }
    );
    let _ = stream.write_all(response.as_bytes());
}

// ---------------------------------------------------------------------------
// Fake cargo
// ---------------------------------------------------------------------------

/// The real `cargo`, resolved before the shim shadows it.
/// The real cargo, resolved absolutely before the shim shadows it on `PATH`.
fn which_cargo() -> std::path::PathBuf {
    // cargo always sets `$CARGO` for a test binary it launched.
    if let Some(path) = std::env::var_os("CARGO") {
        return path.into();
    }
    std::path::PathBuf::from(format!("cargo{}", std::env::consts::EXE_SUFFIX))
}

/// Build the compiled `fake-cargo` fixture once per test binary.
fn fake_cargo_bin() -> &'static Path {
    static BUILT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BUILT.get_or_init(|| {
        escargot::CargoBuild::new()
            .package("clove-plugin-echo")
            .bin("fake-cargo")
            .run()
            .expect("build the fake-cargo fixture")
            .path()
            .to_path_buf()
    })
}

/// Install the fake cargo into `dir` under the name `cargo`.
///
/// It is the same binary that will later be "installed" as the plugin, so its
/// `--clove-plugin-info` answer is chosen by `$FAKE_PLUGIN_MODE` at probe time
/// rather than baked into a script here.
fn fake_cargo(dir: &Path) {
    let dest = dir.join(format!("cargo{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(fake_cargo_bin(), &dest).expect("copy the fake cargo onto the test PATH");
    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&dest).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&dest, perms).unwrap();
    }
}

/// Put `dir` first on `PATH` for `cmd`, portably.
fn prepend_path(cmd: &mut Command, dir: &Path) {
    let mut dirs = vec![dir.to_path_buf()];
    dirs.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    cmd.env("PATH", std::env::join_paths(dirs).unwrap());
}

/// A `file://` URL git accepts on this platform.
fn file_url(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        // Windows: `C:/x` needs the third slash.
        format!("file:///{s}")
    }
}

/// Did the fake cargo run this exact subcommand?
///
/// `cargo_log(..).contains("install")` is satisfied by an `uninstall` line, so
/// every "it did not install" assertion written that way was vacuous. The shim
/// logs one `argv.join(" ")` per line, so the subcommand is the first token.
fn cargo_ran(dir: &Path, subcommand: &str) -> bool {
    cargo_log(dir)
        .lines()
        .any(|line| line.split_whitespace().next() == Some(subcommand))
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
    /// Which `--clove-plugin-info` answer the installed plugin gives.
    mode: String,
    registry: MockCratesIo,
}

/// A test environment whose "installed plugin" answers the compat probe in
/// `mode` — one of `"compatible"`, `"needs_newer"`, `"outdated"`, `"no_info"`.
fn env_with(mode: &str) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("clove-home");
    let bin = tmp.path().join("fakebin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    fake_cargo(&bin);
    Env {
        _tmp: tmp,
        home,
        bin,
        mode: mode.to_owned(),
        registry: MockCratesIo::start(),
    }
}

/// The plugin behaviour names, so call sites read as intent rather than strings.
fn compatible_plugin() -> String {
    "compatible".to_owned()
}

/// A plugin that demands a newer clove than this one — gate 3 must refuse it.
fn needs_newer_clove_plugin() -> String {
    "needs_newer".to_owned()
}

/// A copy of the `clove` binary in a directory this suite controls.
///
/// `plugin::search_dirs()` puts the running binary's own directory **first**, so
/// `assert_cmd`'s `target/debug/clove` drags in every plugin the workspace has
/// built — `clove-sync-github`, `clove-import-tk`, `clove-import-beads`,
/// `clove-echo`. That makes the installed set differ between a local
/// `-p clove-cli --test plugin_install` run and CI's full `--workspace` build,
/// and no environment variable can suppress it (`$CLOVE_PLUGIN_PATH` ranks
/// *below* the exe dir). Running a copy from a temp dir makes that first search
/// entry a directory holding nothing but `clove`.
///
/// One copy per test binary, not per test.
fn isolated_clove() -> &'static Path {
    static CLOVE: std::sync::OnceLock<(TempDir, std::path::PathBuf)> = std::sync::OnceLock::new();
    &CLOVE
        .get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let dst = dir
                .path()
                .join(format!("clove{}", std::env::consts::EXE_SUFFIX));
            std::fs::copy(assert_cmd::cargo::cargo_bin("clove"), &dst).unwrap();
            (dir, dst)
        })
        .1
}

impl Env {
    fn clove(&self) -> Command {
        let mut cmd = Command::new(isolated_clove());
        cmd.env_remove("CLOVE_FORMAT");
        // Reaches the probed plugin by plain inheritance: harness -> clove ->
        // the freshly "installed" binary.
        cmd.env("FAKE_PLUGIN_MODE", &self.mode);
        cmd.env("FAKE_CARGO_LOG", self.bin.join("argv.log"));
        cmd.env("FAKE_CARGO_REAL", which_cargo());
        // Inherited, and it outranks `<clove-home>/bin`, so a developer with
        // real plugins installed would see them in every assertion.
        cmd.env_remove("CLOVE_PLUGIN_PATH");
        cmd.env("CLOVE_HOME", &self.home);
        cmd.env("CLOVE_REGISTRY_URL", self.registry.url());
        // The fake cargo must win over the real one.
        prepend_path(&mut cmd, &self.bin);
        cmd
    }

    /// Simulate cargo's bookkeeping for an already-installed package.
    /// Simulate cargo's bookkeeping for an already-installed package.
    ///
    /// Real cargo records the file name, which carries `.exe` on Windows — so
    /// seeding the bare name would exercise `bare_subcommand`'s tolerant branch
    /// instead of the real one.
    fn seed_installed(&self, pkgid: &str, bins: &[&str]) {
        let bins: Vec<String> = bins
            .iter()
            .map(|b| format!("{b}{}", std::env::consts::EXE_SUFFIX))
            .collect();
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
fn the_suite_sees_no_plugin_it_did_not_install() {
    // The guard for `isolated_clove()`. Without it the suite's view of "what is
    // installed" depended on which workspace plugins happened to be built —
    // differing between a local `-p clove-cli --test plugin_install` and CI's
    // `--workspace` — so every count assertion was environment-dependent, and a
    // test whose plugin name collided with a workspace one would resolve the
    // wrong binary, passing locally and failing in CI.
    let env = env_with(&compatible_plugin());
    let v = json_of(
        &env.clove()
            .args(["--format", "json", "plugin", "list"])
            .assert()
            .success(),
    );
    assert_eq!(
        v["data"].as_array().map(Vec::len),
        Some(0),
        "the plugin search path must be hermetic: {v}"
    );
}

#[test]
fn a_failed_rollback_says_the_binary_is_still_on_the_search_path() {
    // The `Err` branch of `roll_back`, unreachable until the compiled shim could
    // be told to fail. It is the branch that matters: the rejected binary is
    // still in `<clove-home>/bin`, which outranks `$PATH`.
    let env = env_with(&needs_newer_clove_plugin());
    env.registry
        .publish_plugin("clove-sync-gitlab", "0.2.0", "clove-sync-gitlab");

    let assert = env
        .clove()
        .env("FAKE_CARGO_FAIL_ON", "uninstall")
        .args(["plugin", "install", "gitlab", "--yes"])
        .assert()
        .failure();
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();

    assert!(err.contains("FAILED"), "{err}");
    assert!(
        err.contains("Remove it manually"),
        "must say what to do: {err}"
    );
    assert!(
        !err.contains("has been rolled back"),
        "must not assert an action that did not happen: {err}"
    );
    // …and the file really is still there, which is why it is worded that way.
    assert!(
        env.home
            .join("bin")
            .join(format!("clove-sync-gitlab{}", std::env::consts::EXE_SUFFIX))
            .exists(),
        "the message is only honest if the binary is in fact still present"
    );
}

#[test]
fn a_failed_cargo_install_leaves_nothing_behind_and_says_so() {
    let env = env_with(&compatible_plugin());
    env.registry
        .publish_plugin("clove-sync-gitlab", "0.2.0", "clove-sync-gitlab");

    let assert = env
        .clove()
        .env("FAKE_CARGO_FAIL_ON", "install")
        .args(["plugin", "install", "gitlab", "--yes"])
        .assert()
        .failure();
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(err.contains("`cargo install` failed"), "{err}");
    assert!(
        !env.home.join("bin").exists()
            || std::fs::read_dir(env.home.join("bin"))
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
        "a failed install must not leave a binary on the search path"
    );
}

#[test]
fn a_missing_cargo_names_the_toolchain_not_the_os_error() {
    // Installing builds from source, so "cargo is not installed" is a likely
    // first-run failure and deserves the actionable message.
    let env = env_with(&compatible_plugin());
    env.registry
        .publish_plugin("clove-sync-gitlab", "0.2.0", "clove-sync-gitlab");

    let empty = env._tmp.path().join("emptybin");
    std::fs::create_dir_all(&empty).unwrap();

    let assert = env
        .clove()
        .env("PATH", &empty)
        .args(["plugin", "install", "gitlab", "--yes"])
        .assert()
        .failure();
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(err.contains("rustup.rs"), "must name the remedy: {err}");
}

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

    let assert = env
        .clove()
        .args(["plugin", "install", "evil", "--yes"])
        .assert()
        .failure();
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        err.contains("does not depend on `clove-plugin`"),
        "must fail on gate 1 specifically, not resolution or ambiguity: {err}"
    );
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
    // `--yes` goes *before* `--`, or clap rejects it as a second positional and
    // the command dies in argument parsing — passing the test without ever
    // reaching `validate_crate_name`.
    for name in ["../../summary", "--config=x"] {
        let assert = env
            .clove()
            .args(["plugin", "install", "--yes", "--", name])
            .assert()
            .failure();
        let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
        assert!(
            err.contains("crate name"),
            "must fail on name validation, not argument parsing: {err}"
        );
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

/// Run a helper subprocess, isolated from the developer's git configuration.
///
/// Without this a global `commit.gpgsign`, `core.hooksPath`, `init.templateDir`,
/// a `url.<base>.insteadOf` rewrite, or `safe.directory` ownership rules can each
/// fail or silently redirect these repositories on a real workstation.
fn run(cmd: &mut Command) {
    cmd.env("GIT_CONFIG_GLOBAL", "/nonexistent/clove-test-gitconfig")
        .env("GIT_CONFIG_SYSTEM", "/nonexistent/clove-test-gitconfig")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
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
        .args(["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"])
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
            &file_url(&repo),
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
        .args(["plugin", "install", "--git", &file_url(&repo), "--yes"])
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
            &file_url(&repo),
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
        .args(["plugin", "install", "--git", &file_url(&repo), "--yes"])
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
            &file_url(&repo),
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

    // cargo is given the resolved commit, not the tag: a tag is a mutable
    // server-side name that cargo would re-resolve in its own clone, so only the
    // sha ties what clove inspected to what cargo builds.
    let log = cargo_log(&env.bin);
    assert!(
        log.contains("--rev "),
        "the commit pin must reach cargo: {log}"
    );
    assert!(
        !log.contains("--tag"),
        "the mutable tag must not be what cargo resolves: {log}"
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

#[test]
fn a_crate_cannot_install_a_binary_belonging_to_another_plugin() {
    // The shadowing hole: `clove-sync-gitlab` is a genuine clove-plugin
    // dependent, so gate 1 passes — but it declares someone else's binary first.
    // Installing it must not place a `clove-sync-github` into <clove-home>/bin,
    // which outranks $PATH and would receive GITHUB_TOKEN on the next dispatch.
    let env = env_with(&compatible_plugin());
    env.registry.publish(
        "clove-sync-gitlab",
        "1.0.0",
        &["clove-sync-github", "clove-sync-gitlab"],
    );
    env.registry
        .state
        .lock()
        .unwrap()
        .dependents
        .push("clove-sync-gitlab".to_owned());

    env.clove()
        .args(["plugin", "install", "gitlab", "--yes"])
        .assert()
        .success();

    // The crate's *own* binary is installed; the one it listed first, belonging
    // to another plugin, is ignored entirely.
    let log = cargo_log(&env.bin);
    assert!(log.contains("--bin clove-sync-gitlab"), "{log}");
    assert!(
        !log.contains("clove-sync-github"),
        "another plugin's binary must never be installed: {log}"
    );
}

#[test]
fn a_crate_that_builds_no_binary_of_its_own_name_is_refused() {
    // If the only clove-* binary belongs to someone else, there is nothing safe
    // to install and clove must not pick it.
    let env = env_with(&compatible_plugin());
    env.registry
        .publish("clove-sync-gitlab", "1.0.0", &["clove-sync-github"]);
    env.registry
        .state
        .lock()
        .unwrap()
        .dependents
        .push("clove-sync-gitlab".to_owned());

    let assert = env
        .clove()
        .args(["plugin", "install", "gitlab", "--yes"])
        .assert()
        .failure();
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        err.contains("does not build a binary of its own name"),
        "{err}"
    );
    assert!(!cargo_ran(&env.bin, "install"));
}

#[test]
fn a_glob_shaped_binary_name_is_refused() {
    // `cargo install --bin` takes a GLOB. A crate naming a binary
    // `clove-[a-z]*` would otherwise install every binary matching it —
    // verified against cargo 1.94 — defeating the single-binary restriction.
    let env = env_with(&compatible_plugin());
    env.registry
        .publish_plugin("clove-sync-gitlab", "1.0.0", "clove-[a-z]*");

    env.clove()
        .args(["plugin", "install", "gitlab", "--yes"])
        .assert()
        .failure();
    assert!(
        !cargo_log(&env.bin).contains("install"),
        "a glob-shaped bin name must never reach cargo"
    );
}

#[test]
fn update_re_runs_the_gates_that_install_applies() {
    // A crate that has since stopped depending on clove-plugin — the shape of an
    // account takeover publishing a non-plugin follow-up — is refused by
    // `install`, and must be by `update`.
    let env = env_with(&compatible_plugin());
    env.seed_installed(
        "clove-sync-gitlab 0.1.0 (registry+https://github.com/rust-lang/crates.io-index)",
        &["clove-sync-gitlab"],
    );
    env.registry
        .publish("clove-sync-gitlab", "2.0.0", &["clove-sync-gitlab"]);
    env.registry
        .publish_plugin("clove-sync-other", "1.0.0", "clove-sync-other");

    let assert = env
        .clove()
        .args(["--format", "json", "plugin", "update", "--yes"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(
        v["data"]["updated"].as_array().unwrap().len(),
        0,
        "a crate that is no longer a plugin must not be updated into place: {v}"
    );
    assert!(!cargo_log(&env.bin).contains("install"));
}

#[test]
fn update_does_not_move_to_a_yanked_version() {
    // A yank is the standard response to a compromised release.
    let env = env_with(&compatible_plugin());
    env.seed_installed(
        "clove-sync-gitlab 0.1.0 (registry+https://github.com/rust-lang/crates.io-index)",
        &["clove-sync-gitlab"],
    );
    env.registry
        .publish_yanked("clove-sync-gitlab", "0.9.9", "clove-sync-gitlab");

    let assert = env
        .clove()
        .args(["--format", "json", "plugin", "update", "--yes"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["data"]["updated"].as_array().unwrap().len(), 0, "{v}");
    assert!(!cargo_log(&env.bin).contains("install"));
}

#[test]
fn update_fails_loudly_when_the_registry_is_unreachable() {
    // It used to print "everything is up to date" while offline — the worst
    // possible answer for a scheduled job whose purpose is picking up fixes.
    let env = env_with(&compatible_plugin());
    env.seed_installed(
        "clove-sync-gitlab 0.1.0 (registry+https://github.com/rust-lang/crates.io-index)",
        &["clove-sync-gitlab"],
    );

    let assert = env
        .clove()
        .env("CLOVE_REGISTRY_URL", "http://127.0.0.1:1/api/v1")
        .args(["plugin", "update", "--yes"])
        .assert()
        .failure();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(!out.contains("up to date"), "{out}");
}

#[test]
fn a_git_install_is_pinned_to_the_commit_that_was_inspected() {
    // cargo does its own clone, so a tag or branch is a mutable name the host
    // re-resolves. Only the resolved sha ties what was shown to what is built.
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
            &file_url(&repo),
            "--tag",
            "v1.0.0",
            "--yes",
        ])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    let commit = v["data"]["commit"].as_str().unwrap_or_default();
    assert_eq!(commit.len(), 40, "a full sha must be recorded: {v}");

    let log = cargo_log(&env.bin);
    assert!(
        log.contains(&format!("--rev {commit}")),
        "cargo must build the inspected commit, not re-resolve the tag: {log}"
    );
}

// ---------------------------------------------------------------------------
// Published JSON schemas
// ---------------------------------------------------------------------------
//
// `clove plugin` is the newest machine-readable surface in the CLI, and it was
// the only one shipping without a schema — so a client generated from
// `docs/json-schema/v1/` simply could not see it. These tests hold the two
// schemas to the same contract every other command's schema has: every key the
// command actually emits is described, and `additionalProperties: false` means
// a new key cannot ship invisibly.

/// Compile a schema from `docs/json-schema/v1/<name>`.
fn plugin_schema(name: &str) -> jsonschema::Validator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/json-schema/v1")
        .join(name);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    jsonschema::validator_for(&serde_json::from_str(&text).unwrap()).expect("valid schema")
}

fn assert_matches(validator: &jsonschema::Validator, instance: &Value) {
    if let Err(error) = validator.validate(instance) {
        panic!("schema violation: {error} in {instance:#}");
    }
}

/// The row for plugin `name`, which must be present.
fn row<'a>(envelope: &'a Value, name: &str) -> &'a Value {
    envelope["data"]
        .as_array()
        .expect("data is an array")
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("no `{name}` row in {envelope:#}"))
}

fn json_of(assert: &assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(assert.get_output().stdout.as_slice()).expect("valid JSON")
}

#[test]
fn every_install_outcome_matches_the_published_schema() {
    let schema = plugin_schema("plugin-install.json");
    let env = env_with(&compatible_plugin());
    env.registry
        .publish_plugin("clove-sync-gitlab", "0.2.0", "clove-sync-gitlab");

    // 1. A fresh install.
    let installed = json_of(
        &env.clove()
            .args(["--format", "json", "plugin", "install", "gitlab", "--yes"])
            .assert()
            .success(),
    );
    assert_matches(&schema, &installed);
    assert_eq!(installed["data"]["installed"], true, "{installed}");

    // The fake cargo does not keep cargo's books, so record the install the way
    // cargo would. Everything after this reads `.crates2.json`.
    env.seed_installed(
        "clove-sync-gitlab 0.2.0 (registry+https://github.com/rust-lang/crates.io-index)",
        &["clove-sync-gitlab"],
    );

    // 2. The no-op: already installed, no --force. `ok` is true and nothing
    //    changed — the payload is the only thing that says so.
    let already = json_of(
        &env.clove()
            .args(["--format", "json", "plugin", "install", "gitlab", "--yes"])
            .assert()
            .success(),
    );
    assert_matches(&schema, &already);
    assert_eq!(already["data"]["already_installed"], true, "{already}");

    // 3. Nothing newer to move to.
    let current = json_of(
        &env.clove()
            .args(["--format", "json", "plugin", "update", "--yes"])
            .assert()
            .success(),
    );
    assert_matches(&schema, &current);
    assert_eq!(current["data"]["scope"], "all", "{current}");

    // 4. An actual upgrade.
    env.registry
        .publish_plugin("clove-sync-gitlab", "0.3.0", "clove-sync-gitlab");
    let updated = json_of(
        &env.clove()
            .args(["--format", "json", "plugin", "update", "--yes"])
            .assert()
            .success(),
    );
    assert_matches(&schema, &updated);
    assert_eq!(updated["data"]["updated"][0]["to"], "0.3.0", "{updated}");

    // 5. Removal — by the same bare name that installed it.
    let removed = json_of(
        &env.clove()
            .args(["--format", "json", "plugin", "uninstall", "gitlab"])
            .assert()
            .success(),
    );
    assert_matches(&schema, &removed);
    assert_eq!(removed["data"]["uninstalled"], true, "{removed}");
}

#[test]
fn install_from_git_matches_the_published_schema() {
    // The `--git` payload carries `source`/`commit` instead of `version`, so it
    // is a distinct branch of the schema's `oneOf`.
    let env = env_with(&compatible_plugin());
    let repo = env.home.parent().unwrap().join("gitrepo");
    make_repo(&repo, &[("clove-sync-gitlab", "clove-sync-gitlab", "")]);

    let out = json_of(
        &env.clove()
            .args([
                "--format",
                "json",
                "plugin",
                "install",
                "--git",
                &file_url(&repo),
                "--yes",
            ])
            .assert()
            .success(),
    );
    assert_matches(&plugin_schema("plugin-install.json"), &out);
    assert!(out["data"]["commit"].as_str().is_some(), "{out}");
}

#[test]
fn plugin_list_and_search_match_the_published_schema() {
    let schema = plugin_schema("plugin-list.json");
    let env = env_with(&compatible_plugin());
    env.registry
        .publish_plugin("clove-sync-gitlab", "0.2.0", "clove-sync-gitlab");
    env.registry
        .publish_plugin("clove-import-jira", "1.1.0", "clove-import-jira");

    // Discovery only: both rows are `installed: false`, so the schema sees the
    // registry-derived fields (`crate`, `downloads`, `latest_version`, `yanked`).
    let discovered = json_of(
        &env.clove()
            .args(["--format", "json", "plugin", "list", "--all"])
            .assert()
            .success(),
    );
    assert_matches(&schema, &discovered);
    // The ambient `$PATH` carries this repo's own built plugins, so the counts
    // are not fixed — assert on the rows this test actually published.
    assert_eq!(
        row(&discovered, "sync-gitlab")["installed"],
        false,
        "{discovered}"
    );
    assert!(
        row(&discovered, "import-jira")["latest_version"]
            .as_str()
            .is_some(),
        "a discovered row carries the published version: {discovered}"
    );

    // Install one, so the same schema now has to cover a row that is both
    // discovered and installed — the merge that carries a real `path`,
    // a probed `version`, and a compat `status` rather than `available`.
    env.clove()
        .args(["--format", "json", "plugin", "install", "gitlab", "--yes"])
        .assert()
        .success();

    let mixed = json_of(
        &env.clove()
            .args(["--format", "json", "plugin", "list", "--all"])
            .assert()
            .success(),
    );
    assert_matches(&schema, &mixed);
    let gitlab = row(&mixed, "sync-gitlab");
    assert_eq!(gitlab["installed"], true, "{mixed}");
    assert!(
        gitlab["path"]
            .as_str()
            .is_some_and(|p| p.contains("clove-home")),
        "an installed row must carry the real path from clove's install root: {mixed}"
    );
    assert_eq!(
        gitlab["status"], "ok",
        "an installed row reports the compat verdict, not `available`: {mixed}"
    );

    let searched = json_of(
        &env.clove()
            .args(["--format", "json", "plugin", "search", "gitlab"])
            .assert()
            .success(),
    );
    assert_matches(&schema, &searched);
    assert_eq!(
        row(&searched, "sync-gitlab")["installed"],
        true,
        "{searched}"
    );
}

#[test]
fn a_degraded_discovery_still_matches_the_list_schema() {
    // Discovery is additive: with crates.io unreachable the command still
    // succeeds, and the failure has to travel in `_meta.warnings` — which is
    // only a real contract if the schema describes it.
    let env = env_with(&compatible_plugin());
    let mut cmd = env.clove();
    cmd.env("CLOVE_REGISTRY_URL", "http://127.0.0.1:1/api/v1");
    let out = json_of(
        &cmd.args(["--format", "json", "plugin", "list", "--all"])
            .assert()
            .success(),
    );
    assert_matches(&plugin_schema("plugin-list.json"), &out);
    assert!(
        !out["_meta"]["warnings"].as_array().unwrap().is_empty(),
        "an unreachable registry must be reported, not silently empty: {out}"
    );
}
