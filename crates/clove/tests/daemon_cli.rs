//! Phase 4 (T-D05/T-D07): `clove daemon start|stop|status` against the per-user
//! hub, idempotent start, no-op stop, `stop --all`, and the `clove doctor`
//! daemon-health check. Unix-only. Builds the sibling `cloved` on demand, so a
//! daemon test can never pass by skipping. Every test's hub lives in its own
//! temp runtime directory, never the user's.
#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use assert_cmd::Command;

/// Token records for this test's processes go here, never the user's clove home.
const TEST_CLOVE_HOME: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/test-clove-home");

/// The `cloved` binary, built on demand rather than hoped for in `target/`.
fn cloved() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        escargot::CargoBuild::new()
            .package("cloved")
            .bin("cloved")
            .run()
            .expect("build cloved for the daemon CLI tests")
            .path()
            .to_path_buf()
    })
}

/// `clove` in `dir`, talking to the hub rooted at `run`.
fn clove(dir: &Path, run: &Path) -> Command {
    let mut c = Command::cargo_bin("clove").unwrap();
    c.current_dir(dir)
        .env("CLOVE_HOME", TEST_CLOVE_HOME)
        .env("CLOVE_RUNTIME_DIR", run)
        .env("CLOVED_PATH", cloved())
        .env("CLOVED_DISABLE_WEB", "1");
    c
}

fn init(dir: &Path, run: &Path) {
    clove(dir, run).arg("init").assert().success();
}

fn json(out: &[u8]) -> serde_json::Value {
    serde_json::from_slice(out).unwrap()
}

fn daemon_status(dir: &Path, run: &Path) -> serde_json::Value {
    json(
        &clove(dir, run)
            .args(["daemon", "status", "-f", "json"])
            .output()
            .unwrap()
            .stdout,
    )
}

fn doctor_codes(dir: &Path, run: &Path) -> Vec<String> {
    let out = clove(dir, run)
        .args(["doctor", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    json(&out)["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap().to_owned())
        .collect()
}

/// A test's hub runtime directory; stops any hub still running in it when the
/// test ends, however it ends.
struct Run {
    _tmp: tempfile::TempDir,
    path: PathBuf,
}

impl Run {
    fn new() -> Run {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("run");
        Run { _tmp: tmp, path }
    }

    fn pid_file(&self) -> PathBuf {
        self.path.join("hub.pid")
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if let Some(pid) = std::fs::read_to_string(self.pid_file())
            .ok()
            .and_then(|p| p.trim().parse::<i32>().ok())
        {
            // SAFETY: kill(2) with a pid our own test hub wrote.
            unsafe {
                libc_kill(pid, 15);
            }
        }
    }
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[test]
fn doctor_flags_and_fixes_stale_daemon_footprints() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run = Run::new();
    init(dir, &run.path);
    let clove_dir = dir.join(".clove");

    // A crashed clove 0.1.0 daemon: corpse socket + pid in `.clove/`.
    std::fs::write(clove_dir.join("daemon.sock"), b"").unwrap();
    std::fs::write(clove_dir.join("daemon.pid"), b"999999").unwrap();
    // A crashed hub: corpse socket + pid in the runtime directory.
    clove_ipc::ensure_private_dir(camino::Utf8Path::from_path(&run.path).unwrap()).unwrap();
    std::fs::write(run.path.join("hub.sock"), b"").unwrap();
    std::fs::write(run.pid_file(), b"999999").unwrap();

    // doctor (no fix) reports both as warnings, exit 0.
    let codes = doctor_codes(dir, &run.path);
    assert_eq!(
        codes.iter().filter(|c| *c == "DAEMON_STALE_SOCKET").count(),
        2,
        "one finding per corpse footprint, got {codes:?}"
    );

    // --strict with only warnings still exits 0 (warnings are not errors).
    clove(dir, &run.path)
        .args(["doctor", "--strict"])
        .assert()
        .success();

    // --fix removes the corpse files.
    clove(dir, &run.path)
        .args(["doctor", "--fix"])
        .assert()
        .success();
    assert!(!clove_dir.join("daemon.sock").exists());
    assert!(!clove_dir.join("daemon.pid").exists());
    assert!(!run.path.join("hub.sock").exists());
    assert!(!run.pid_file().exists());

    assert!(
        !doctor_codes(dir, &run.path).contains(&"DAEMON_STALE_SOCKET".to_owned()),
        "clean store reports no daemon finding"
    );
}

#[test]
fn stop_with_no_daemon_is_a_clean_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run = Run::new();
    init(dir, &run.path);
    for args in [
        &["daemon", "stop", "-f", "json"][..],
        &["daemon", "stop", "--all", "-f", "json"][..],
    ] {
        let out = clove(dir, &run.path)
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        assert_eq!(
            json(&out)["data"]["running"],
            serde_json::json!(false),
            "{args:?}"
        );
    }
}

#[test]
fn start_status_stop_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run = Run::new();
    init(dir, &run.path);
    clove(dir, &run.path)
        .args(["new", "alpha"])
        .assert()
        .success();

    // start
    let out = clove(dir, &run.path)
        .args(["daemon", "start", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["data"]["started"], serde_json::json!(true));
    // The project's daemon token was created on first need, owner-only.
    let token = dir.join(".clove/daemon.token");
    assert!(token.is_file(), "no daemon token was created");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&token).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // status → running with 1 item, and the hub lists this one project
    let v = daemon_status(dir, &run.path);
    assert_eq!(v["data"]["running"], serde_json::json!(true));
    assert_eq!(v["data"]["items_indexed"], serde_json::json!(1));
    assert_eq!(v["data"]["hub"]["projects"].as_array().unwrap().len(), 1);

    // second start is idempotent (already running, exit 0)
    let out = clove(dir, &run.path)
        .args(["daemon", "start", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["data"]["running"], serde_json::json!(true));

    // stop → this was the only project, so the hub is torn down too
    clove(dir, &run.path)
        .args(["daemon", "stop"])
        .assert()
        .success();
    assert!(!run.pid_file().exists());
    assert!(!run.path.join("hub.sock").exists());

    let v = daemon_status(dir, &run.path);
    assert_eq!(v["data"]["running"], serde_json::json!(false));
    assert!(v["data"]["hub"].is_null(), "no hub left: {v}");
}

/// #54: `.clove/daemon.sock` in a deeply nested repository overflowed the
/// 104-byte `sun_path`, so the daemon could never bind. The hub's socket lives
/// in the per-user runtime directory, whatever the repository's depth.
#[test]
fn daemon_runs_for_a_repo_nested_past_the_socket_path_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let run = Run::new();
    let dir = tmp.path().join("nested-directory/".repeat(8));
    std::fs::create_dir_all(&dir).unwrap();
    assert!(
        dir.join(".clove/daemon.sock").as_os_str().len() > 108,
        "the repo must be deep enough to overflow the old in-repo socket path"
    );
    init(&dir, &run.path);

    clove(&dir, &run.path)
        .args(["daemon", "start"])
        .assert()
        .success();
    assert_eq!(
        daemon_status(&dir, &run.path)["data"]["running"],
        serde_json::json!(true)
    );
    clove(&dir, &run.path)
        .args(["daemon", "stop"])
        .assert()
        .success();
    assert_eq!(
        daemon_status(&dir, &run.path)["data"]["running"],
        serde_json::json!(false)
    );
}

/// One daemon serves every project of a user: two repos started from the same
/// runtime directory share one hub process, each is served its own data, and
/// stopping one does not touch the other.
#[test]
fn one_daemon_serves_every_project_independently() {
    let t1 = tempfile::tempdir().unwrap();
    let t2 = tempfile::tempdir().unwrap();
    let (d1, d2) = (t1.path(), t2.path());
    let run = Run::new();
    init(d1, &run.path);
    init(d2, &run.path);
    clove(d1, &run.path)
        .args(["new", "only-one"])
        .assert()
        .success();
    clove(d2, &run.path).args(["new", "a"]).assert().success();
    clove(d2, &run.path).args(["new", "b"]).assert().success();

    clove(d1, &run.path)
        .args(["daemon", "start"])
        .assert()
        .success();
    let hub_pid = std::fs::read_to_string(run.pid_file()).unwrap();
    clove(d2, &run.path)
        .args(["daemon", "start"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(run.pid_file()).unwrap(),
        hub_pid,
        "the second project joined the running daemon"
    );

    // Each is served its OWN data (distinct item counts), by one process.
    let s1 = daemon_status(d1, &run.path);
    let s2 = daemon_status(d2, &run.path);
    assert_eq!(s1["data"]["running"], serde_json::json!(true));
    assert_eq!(s1["data"]["items_indexed"], serde_json::json!(1));
    assert_eq!(s2["data"]["running"], serde_json::json!(true));
    assert_eq!(s2["data"]["items_indexed"], serde_json::json!(2));
    assert_eq!(s1["data"]["hub"]["projects"].as_array().unwrap().len(), 2);
    assert_eq!(s1["data"]["hub"]["pid"], s2["data"]["hub"]["pid"]);

    // Stopping one leaves the other served, and the daemon running.
    clove(d1, &run.path)
        .args(["daemon", "stop"])
        .assert()
        .success();
    assert_eq!(
        daemon_status(d1, &run.path)["data"]["running"],
        serde_json::json!(false)
    );
    assert_eq!(
        daemon_status(d2, &run.path)["data"]["running"],
        serde_json::json!(true)
    );
    assert!(run.pid_file().exists());

    clove(d2, &run.path)
        .args(["daemon", "stop"])
        .assert()
        .success();
    assert_eq!(
        daemon_status(d2, &run.path)["data"]["running"],
        serde_json::json!(false)
    );
    assert!(!run.pid_file().exists(), "the last stop ends the daemon");
}

#[test]
fn stop_all_stops_the_daemon_for_every_project() {
    let t1 = tempfile::tempdir().unwrap();
    let t2 = tempfile::tempdir().unwrap();
    let (d1, d2) = (t1.path(), t2.path());
    let run = Run::new();
    init(d1, &run.path);
    init(d2, &run.path);
    clove(d1, &run.path)
        .args(["daemon", "start"])
        .assert()
        .success();
    clove(d2, &run.path)
        .args(["daemon", "start"])
        .assert()
        .success();

    let out = clove(d1, &run.path)
        .args(["daemon", "stop", "--all", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["data"]["stopped"], serde_json::json!(true));
    assert!(!run.pid_file().exists());
    for dir in [d1, d2] {
        assert_eq!(
            daemon_status(dir, &run.path)["data"]["running"],
            serde_json::json!(false)
        );
    }
}

/// A clove 0.1.0 daemon holds this project's `daemon.lock`: `start` explains why
/// it cannot serve the project, and reads keep working from the files.
#[test]
fn a_project_locked_by_an_older_daemon_falls_back_safely() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run = Run::new();
    init(dir, &run.path);
    clove(dir, &run.path).args(["new", "x"]).assert().success();

    let held = std::fs::File::create(dir.join(".clove/daemon.lock")).unwrap();
    held.try_lock().unwrap();

    let out = clove(dir, &run.path)
        .args(["daemon", "start"])
        .assert()
        .code(7)
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8_lossy(&out);
    assert!(stderr.contains("daemon.lock"), "{stderr}");

    let v = json(
        &clove(dir, &run.path)
            .args(["ls", "-f", "json"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["data"].as_array().unwrap().len(), 1);
    assert_ne!(v["_meta"]["source"], serde_json::json!("daemon"));
}

/// The daemon is keyed to the repo's resolved `.clove/` (not the cwd): a project
/// started at the repo root is served — reads included — from any nested
/// subdirectory. This is the same path-resolution that makes git worktrees which
/// share one `.clove/` share one slot.
#[test]
fn daemon_is_reachable_from_any_subdirectory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let run = Run::new();
    init(root, &run.path);
    clove(root, &run.path).args(["new", "x"]).assert().success();
    clove(root, &run.path)
        .args(["daemon", "start"])
        .assert()
        .success();

    let sub = root.join("a").join("b").join("c");
    std::fs::create_dir_all(&sub).unwrap();

    let s = daemon_status(&sub, &run.path);
    assert_eq!(s["data"]["running"], serde_json::json!(true));
    assert_eq!(s["data"]["items_indexed"], serde_json::json!(1));

    let v = json(
        &clove(&sub, &run.path)
            .args(["ls", "-f", "json"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["_meta"]["source"], serde_json::json!("daemon"));

    clove(root, &run.path)
        .args(["daemon", "stop"])
        .assert()
        .success();
}

fn titles(v: &serde_json::Value) -> Vec<String> {
    v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap().to_owned())
        .collect()
}

/// A daemon token that git already tracks (committed before `.gitignore`
/// listed it) would be published by the next `git commit -a`: doctor says so
/// and how to untrack it.
#[test]
fn doctor_flags_a_daemon_token_git_tracks() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run = Run::new();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q"]);
    init(dir, &run.path);
    std::fs::write(
        dir.join(".clove/daemon.token"),
        "0123456789abcdef0123456789abcdef\n",
    )
    .unwrap();
    assert!(!doctor_codes(dir, &run.path).contains(&"DAEMON_TOKEN_TRACKED".to_owned()));
    git(&["add", "-f", ".clove/daemon.token"]);
    git(&["commit", "-q", "-m", "oops"]);
    let out = clove(dir, &run.path)
        .args(["doctor", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let issues = json(&out)["data"]["issues"].clone();
    let tracked = issues
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["code"] == "DAEMON_TOKEN_TRACKED")
        .unwrap_or_else(|| panic!("no DAEMON_TOKEN_TRACKED in {issues}"));
    assert!(
        tracked["message"]
            .as_str()
            .unwrap()
            .contains("git rm --cached .clove/daemon.token"),
        "{tracked}"
    );
}

/// `clove daemon stop` while the project is still loading stops it: the stop
/// reaches the hub even though a probe would find nothing loaded yet, and
/// the load, when it completes, does not leave the project served (L-new-1).
#[test]
fn a_stop_while_the_start_is_still_loading_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run = Run::new();
    init(dir, &run.path);
    let mut start = std::process::Command::new(assert_cmd::cargo::cargo_bin("clove"))
        .current_dir(dir)
        .env("CLOVE_HOME", TEST_CLOVE_HOME)
        .env("CLOVE_RUNTIME_DIR", &run.path)
        .env("CLOVED_PATH", cloved())
        .env("CLOVED_DISABLE_WEB", "1")
        .env("CLOVED_LOAD_DELAY_MS", "6000")
        .args(["daemon", "start"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // The hub has the load once it opens the index (inside the load, before
    // the delay); the token alone is written before the request is even sent.
    let index = dir.join(".clove/index.db");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !(run.pid_file().exists() && index.exists()) {
        assert!(
            std::time::Instant::now() < deadline,
            "the start never got going"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let stop = clove(dir, &run.path)
        .args(["daemon", "stop", "-f", "json"])
        .output()
        .unwrap();
    let _ = start.wait();
    let stopped = json(&stop.stdout);
    assert_eq!(
        stopped["data"]["stopped"],
        serde_json::json!(true),
        "{stopped}"
    );
    for _ in 0..2 {
        assert_eq!(
            daemon_status(dir, &run.path)["data"]["running"],
            serde_json::json!(false),
            "the project is served after its stop"
        );
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}

/// Reads never write: with a hub running, `clove ls` creates no token and
/// touches no `.gitignore` — and a committed `.clove` symlink (`.clove -> ~`,
/// say) gets no token work at all, so nothing lands in the link's target.
#[test]
fn a_read_never_writes_a_token_even_through_a_symlinked_store() {
    use std::os::unix::fs::PermissionsExt;
    let run = Run::new();
    let (other_tmp, plain_tmp, target_tmp, repo_tmp) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    init(other_tmp.path(), &run.path);
    clove(other_tmp.path(), &run.path)
        .args(["daemon", "start"])
        .assert()
        .success();

    init(plain_tmp.path(), &run.path);
    clove(plain_tmp.path(), &run.path)
        .args(["ls"])
        .assert()
        .success();
    let plain_token = plain_tmp.path().join(".clove/daemon.token");

    init(target_tmp.path(), &run.path);
    let target = target_tmp.path().join(".clove");
    let gitignore = target.join(".gitignore");
    std::fs::write(&gitignore, "index.db\n").unwrap();
    std::fs::set_permissions(&gitignore, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::os::unix::fs::symlink(&target, repo_tmp.path().join(".clove")).unwrap();
    clove(repo_tmp.path(), &run.path)
        .args(["ls"])
        .assert()
        .success();
    clove(other_tmp.path(), &run.path)
        .args(["daemon", "stop"])
        .assert()
        .success();

    assert!(!plain_token.exists(), "a read created a token");
    assert!(
        !target.join("daemon.token").exists(),
        "a token landed in the link's target"
    );
    assert_eq!(std::fs::read_to_string(&gitignore).unwrap(), "index.db\n");
    let mode = std::fs::metadata(&gitignore).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o644);
}

/// A token the client can neither read nor replace (here: mode 000 in a
/// read-only `.clove/`) is what `status` and `stop` report — naming the file —
/// not a project the daemon "isn't serving" or a remedy that doesn't fit.
#[test]
fn an_unreadable_token_is_named_by_status_and_stop() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run = Run::new();
    init(dir, &run.path);
    clove(dir, &run.path)
        .args(["daemon", "start"])
        .assert()
        .success();
    let clove_dir = dir.join(".clove");
    let token = clove_dir.join("daemon.token");
    let set_mode = |path: &Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
    };
    set_mode(&token, 0o000);
    set_mode(&clove_dir, 0o500);
    let status = clove(dir, &run.path)
        .args(["daemon", "status"])
        .output()
        .unwrap();
    let stop = clove(dir, &run.path)
        .args(["daemon", "stop"])
        .output()
        .unwrap();
    set_mode(&clove_dir, 0o755);
    set_mode(&token, 0o600);

    for (what, out) in [("status", &status), ("stop", &stop)] {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.status.success(), "{what}: {text}");
        assert!(text.contains("daemon.token"), "{what}: {text}");
        assert!(!text.contains("not serving"), "{what}: {text}");
        assert!(!text.contains("--all"), "{what}: {text}");
    }
    clove(dir, &run.path)
        .args(["daemon", "stop"])
        .assert()
        .success();
}

/// A relative `--clove-dir` names the caller's own project — never whichever
/// project the daemon's working directory happens to hold.
#[test]
fn a_relative_clove_dir_always_means_the_callers_own_project() {
    let (ta, tb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, b) = (ta.path(), tb.path());
    let run = Run::new();
    init(a, &run.path);
    init(b, &run.path);
    clove(a, &run.path)
        .args(["new", "only-in-a"])
        .assert()
        .success();
    clove(b, &run.path)
        .args(["new", "only-in-b"])
        .assert()
        .success();
    // Started from A, so a daemon that kept its spawner's cwd would sit in A.
    clove(a, &run.path)
        .args(["daemon", "start"])
        .assert()
        .success();

    clove(b, &run.path)
        .args(["--clove-dir", ".clove", "daemon", "start"])
        .assert()
        .success();
    let v = json(
        &clove(b, &run.path)
            .args(["--clove-dir", ".clove", "ls", "-f", "json"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(titles(&v), vec!["only-in-b"], "{v}");
    // Answered by the hub — the file fallback would list B's items too.
    assert_eq!(v["_meta"]["source"], "daemon", "{v}");

    clove(b, &run.path)
        .args(["--clove-dir", ".clove", "daemon", "stop"])
        .assert()
        .success();
    assert_eq!(
        daemon_status(a, &run.path)["data"]["running"],
        serde_json::json!(true),
        "stopping B must leave A served"
    );
}

/// The environment of a process, as `KEY=VALUE` text.
fn process_env(pid: u32) -> String {
    if cfg!(target_os = "linux") {
        std::fs::read(format!("/proc/{pid}/environ"))
            .map(|raw| String::from_utf8_lossy(&raw).replace('\0', "\n"))
            .unwrap_or_default()
    } else {
        let out = std::process::Command::new("ps")
            .args(["eww", "-o", "command=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// The working directory of a process.
fn process_cwd(pid: u32) -> String {
    if cfg!(target_os = "linux") {
        std::fs::read_link(format!("/proc/{pid}/cwd"))
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    } else {
        let out = std::process::Command::new("lsof")
            .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix('n').map(str::to_owned))
            .unwrap_or_default()
    }
}

fn process_command(pid: u32) -> String {
    let out = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// The spawned daemon belongs to no project and inherits nothing it does not
/// need: no project argument, no spawner's cwd, no stray secrets.
#[test]
fn the_daemon_is_spawned_bare_with_a_minimal_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run = Run::new();
    init(dir, &run.path);
    clove(dir, &run.path)
        .env("GITHUB_TOKEN", "ghp_marker_not_for_the_daemon")
        .env("CLOVE_TEST_UNRELATED_VAR", "unrelated_marker")
        .args(["daemon", "start"])
        .assert()
        .success();
    let pid: u32 = std::fs::read_to_string(run.pid_file())
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    let command = process_command(pid);
    assert!(command.ends_with("cloved run"), "spawn args: {command}");
    let env = process_env(pid);
    assert!(!env.contains("ghp_marker_not_for_the_daemon"), "{env}");
    assert!(!env.contains("unrelated_marker"), "{env}");
    assert!(env.contains("CLOVE_RUNTIME_DIR="), "{env}");
    // It works from its own private runtime directory: not `/`, and not the
    // directory of the client that started it.
    assert_eq!(
        std::path::PathBuf::from(process_cwd(pid)),
        run.path.canonicalize().unwrap()
    );
}

/// A process that holds a socket open but never answers: a live, busy daemon.
fn silent_listener(path: &Path) -> UnixListener {
    UnixListener::bind(path).unwrap()
}

/// A clove 0.1.0 daemon that is alive but slow to answer must keep its files:
/// only a daemon proven gone may be cleaned up.
#[test]
fn a_live_but_slow_old_daemon_keeps_its_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run = Run::new();
    init(dir, &run.path);
    let clove_dir = dir.join(".clove");
    let _busy = silent_listener(&clove_dir.join("daemon.sock"));
    std::fs::write(clove_dir.join("daemon.pid"), b"999999").unwrap();

    let _ = clove(dir, &run.path)
        .args(["daemon", "stop"])
        .output()
        .unwrap();
    assert!(
        clove_dir.join("daemon.sock").exists(),
        "stop kept the socket"
    );
    assert!(clove_dir.join("daemon.pid").exists(), "stop kept the pid");

    let codes = doctor_codes(dir, &run.path);
    assert!(codes.contains(&"DAEMON_LEGACY".to_owned()), "{codes:?}");
    clove(dir, &run.path)
        .args(["doctor", "--fix"])
        .assert()
        .success();
    assert!(
        clove_dir.join("daemon.sock").exists(),
        "doctor --fix kept the socket"
    );
    assert!(
        clove_dir.join("daemon.pid").exists(),
        "doctor --fix kept the pid"
    );
}

/// A stand-in hub in `run` that answers every hello with `reply` (a JSON
/// `Welcome`), framed the way tarpc's length-delimited codec frames it.
fn fake_hub(run: &Path, reply: &'static str) -> std::thread::JoinHandle<()> {
    clove_ipc::ensure_private_dir(camino::Utf8Path::from_path(run).unwrap()).unwrap();
    let listener = UnixListener::bind(run.join("hub.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut held = Vec::new();
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    let mut len = [0u8; 4];
                    if stream.read_exact(&mut len).is_ok() {
                        let mut hello = vec![0u8; u32::from_be_bytes(len) as usize];
                        let _ = stream.read_exact(&mut hello);
                        let _ = stream.write_all(&(reply.len() as u32).to_be_bytes());
                        let _ = stream.write_all(reply.as_bytes());
                    }
                    held.push(stream);
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    })
}

/// A per-project `stop` against a daemon this client cannot talk to must say
/// so — and how to stop it — rather than claim no daemon is running.
#[test]
fn stopping_a_project_on_an_incompatible_daemon_says_how() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run = Run::new();
    init(dir, &run.path);
    let _hub = fake_hub(
        &run.path,
        r#"{"welcome":"err","protocol":99,"code":"PROTOCOL_MISMATCH","message":"client protocol 7 != daemon protocol 99"}"#,
    );

    let out = clove(dir, &run.path)
        .args(["daemon", "stop"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(7), "{stderr}");
    assert!(stderr.contains("stop --all"), "{stderr}");
    assert!(run.path.join("hub.sock").exists());
}

/// `clove serve` racing a daemon that is still starting (it holds the lock but
/// has not bound its socket yet) waits for it and hands off, instead of starting
/// a second, standalone server.
#[test]
fn serve_waits_for_a_daemon_that_is_still_starting() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run = Run::new();
    init(dir, &run.path);
    // A hub that takes its lock and then takes its time to bind.
    let mut hub = std::process::Command::new(cloved())
        .env("CLOVE_HOME", TEST_CLOVE_HOME)
        .env("CLOVE_RUNTIME_DIR", &run.path)
        .env("CLOVED_WEB_PORT", "0")
        .env("CLOVED_BIND_DELAY_MS", "2000")
        .arg("run")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let hub_paths = clove_ipc::HubPaths::at(camino::Utf8Path::from_path(&run.path).unwrap());
    let deadline = Instant::now() + Duration::from_secs(30);
    while !hub_paths.running() {
        assert!(Instant::now() < deadline, "the hub never took its lock");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !hub_paths.footprint_present(),
        "the hub bound before serve ran"
    );

    let mut serve = std::process::Command::new(assert_cmd::cargo::cargo_bin("clove"))
        .current_dir(dir)
        .env("CLOVE_HOME", TEST_CLOVE_HOME)
        .env("CLOVE_RUNTIME_DIR", &run.path)
        .env("CLOVED_PATH", cloved())
        .env("CLOVED_WEB_PORT", "0")
        .arg("serve")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = serve.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() > deadline {
            let _ = serve.kill();
            let _ = serve.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    serve
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let _ = hub.kill();
    let _ = hub.wait();
    assert!(
        status.is_some_and(|s| s.success()) && stderr.contains("served by the running daemon"),
        "serve must hand off to the starting daemon: {status:?} {stderr}"
    );
}
