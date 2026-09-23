//! Shared harness for the daemon integration tests: a hub per test, rooted in
//! its own temp runtime directory so parallel tests never meet on one hub (and
//! never touch the user's real one).
#![allow(dead_code)]

use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use clove_ipc::{ClientError, DaemonClient, Detached, HubClient, HubPaths};

pub const SIGTERM: i32 = 15;
pub const SIGKILL: i32 = 9;

/// The clove home every test process and hub uses: token records land here,
/// never in the user's.
pub const TEST_CLOVE_HOME: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/test-clove-home");

/// Point this test process's token records at [`TEST_CLOVE_HOME`].
pub fn isolate() {
    clove_core::daemon_token::use_records_dir(
        Utf8PathBuf::from(TEST_CLOVE_HOME).join("daemon-tokens"),
    );
}

pub fn cloved_bin() -> Utf8PathBuf {
    Utf8PathBuf::from(env!("CARGO_BIN_EXE_cloved"))
}

/// A minimal `.clove/` (config + issues) good enough for the hub to load.
pub fn init_clove_dir() -> (tempfile::TempDir, Utf8PathBuf) {
    isolate();
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap().to_owned();
    let clove_dir = root.join(".clove");
    std::fs::create_dir_all(clove_dir.join("issues")).unwrap();
    std::fs::write(
        clove_dir.join("config.toml"),
        "schema = 1\nid_prefix = \"proj\"\n",
    )
    .unwrap();
    (dir, clove_dir)
}

/// A private runtime directory for one test's hub.
pub fn runtime_dir() -> (tempfile::TempDir, HubPaths) {
    isolate();
    let dir = tempfile::tempdir().unwrap();
    let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
    (dir, HubPaths::at(path))
}

/// A running `cloved run` for one test. Killed on drop if still running.
pub struct TestHub {
    pub paths: HubPaths,
    pub child: Child,
    _run: Option<tempfile::TempDir>,
}

impl TestHub {
    /// A hub with no web listener, serving `clove_dir` if given (loaded
    /// through an ordinary call — a hub is never started for a project).
    pub fn spawn(clove_dir: Option<&Utf8Path>) -> TestHub {
        TestHub::spawn_with(clove_dir, &[("CLOVED_DISABLE_WEB", "1")])
    }

    /// A hub with the given environment, ready (its pid file written), then
    /// serving `clove_dir` if given.
    pub fn spawn_with(clove_dir: Option<&Utf8Path>, env: &[(&str, &str)]) -> TestHub {
        let (run, paths) = runtime_dir();
        let child = command(&paths, env).spawn().expect("spawn cloved");
        let hub = TestHub {
            paths,
            child,
            _run: Some(run),
        };
        hub.wait_ready(Duration::from_secs(5));
        if let Some(dir) = clove_dir {
            hub.load(dir).expect("the hub serves the project");
        }
        hub
    }

    /// Start another hub in this one's runtime directory (after this one died)
    /// and wait until it serves `clove_dir`. A leftover pid file cannot be the
    /// readiness signal here, so it waits on a real attach.
    pub fn restart(&self, clove_dir: &Utf8Path) -> TestHub {
        let child = command(&self.paths, &[("CLOVED_DISABLE_WEB", "1")])
            .spawn()
            .expect("spawn cloved");
        let hub = TestHub {
            paths: self.paths.clone(),
            child,
            _run: None,
        };
        assert!(
            eventually(Duration::from_secs(5), || hub.load(clove_dir).is_ok()),
            "restarted hub serves the project"
        );
        hub
    }

    fn wait_ready(&self, timeout: Duration) {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self.paths.pid().exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("hub did not become ready (no pid file) within {timeout:?}");
    }

    /// A client for `clove_dir`, which the hub must already serve.
    pub fn client(&self, clove_dir: &Utf8Path) -> DaemonClient {
        DaemonClient::probe_at(&self.paths, clove_dir).expect("hub serves the project")
    }

    /// Attach to `clove_dir`, loading it.
    pub fn load(&self, clove_dir: &Utf8Path) -> Result<DaemonClient, ClientError> {
        DaemonClient::attach(&self.paths, clove_dir, true)
    }

    /// The hub's status client.
    pub fn control(&self) -> HubClient {
        HubClient::connect(&self.paths).expect("hub control")
    }

    /// Stop serving `clove_dir`, which the hub must serve.
    pub fn detach(&self, clove_dir: &Utf8Path) -> Detached {
        self.client(clove_dir).detach().expect("detach")
    }

    /// The canonical `.clove/` directories the hub serves.
    pub fn projects(&self) -> Vec<String> {
        self.control()
            .status()
            .unwrap()
            .projects
            .into_iter()
            .map(|p| p.clove_dir)
            .collect()
    }

    pub fn signal(&self, sig: i32) {
        send_signal(self.child.id(), sig);
    }

    /// Wait for the hub to exit, up to `timeout`.
    pub fn wait_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if let Some(status) = self.child.try_wait().unwrap() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }
}

impl Drop for TestHub {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The `cloved run` command for a hub rooted at `paths` — no project: a hub
/// belongs to none.
pub fn command(paths: &HubPaths, env: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(cloved_bin());
    cmd.env("CLOVE_RUNTIME_DIR", paths.dir().as_str())
        .env("CLOVE_HOME", TEST_CLOVE_HOME)
        .envs(env.iter().copied())
        .arg("run");
    cmd
}

/// Poll `condition` until it holds or `timeout` passes.
pub fn eventually(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    condition()
}

pub fn send_signal(pid: u32, sig: i32) {
    // SAFETY: `kill(2)` with a pid we spawned and a constant signal number.
    unsafe {
        libc_kill(pid as i32, sig);
    }
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}
