//! Shared harness for the daemon integration tests: a hub per test, rooted in
//! its own temp runtime directory so parallel tests never meet on one hub (and
//! never touch the user's real one).
#![allow(dead_code)]

use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use clove_ipc::{ClientError, DaemonClient, HubClient, HubPaths};

pub const SIGTERM: i32 = 15;
pub const SIGKILL: i32 = 9;

pub fn cloved_bin() -> Utf8PathBuf {
    Utf8PathBuf::from(env!("CARGO_BIN_EXE_cloved"))
}

/// A minimal `.clove/` (config + issues) good enough for the hub to load.
pub fn init_clove_dir() -> (tempfile::TempDir, Utf8PathBuf) {
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
    /// A hub with no web listener, optionally preloading `clove_dir`.
    pub fn spawn(clove_dir: Option<&Utf8Path>) -> TestHub {
        TestHub::spawn_with(clove_dir, &[("CLOVED_DISABLE_WEB", "1")])
    }

    /// A hub with the given environment, ready (its pid file written).
    pub fn spawn_with(clove_dir: Option<&Utf8Path>, env: &[(&str, &str)]) -> TestHub {
        let (run, paths) = runtime_dir();
        let child = command(&paths, clove_dir, env)
            .spawn()
            .expect("spawn cloved");
        let hub = TestHub {
            paths,
            child,
            _run: Some(run),
        };
        hub.wait_ready(Duration::from_secs(5));
        hub
    }

    /// Start another hub in this one's runtime directory (after this one died)
    /// and wait until it serves `clove_dir`. A leftover pid file cannot be the
    /// readiness signal here, so it waits on a real attach.
    pub fn restart(&self, clove_dir: &Utf8Path) -> TestHub {
        let child = command(&self.paths, Some(clove_dir), &[("CLOVED_DISABLE_WEB", "1")])
            .spawn()
            .expect("spawn cloved");
        let hub = TestHub {
            paths: self.paths.clone(),
            child,
            _run: None,
        };
        assert!(
            eventually(Duration::from_secs(5), || {
                DaemonClient::probe_at(&hub.paths, clove_dir).is_some()
            }),
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

    /// The hub's control service.
    pub fn control(&self) -> HubClient {
        HubClient::connect(&self.paths).expect("hub control")
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

/// The `cloved run` command for a hub rooted at `paths`.
pub fn command(paths: &HubPaths, clove_dir: Option<&Utf8Path>, env: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(cloved_bin());
    cmd.env("CLOVE_RUNTIME_DIR", paths.dir().as_str())
        .envs(env.iter().copied())
        .arg("run");
    if let Some(dir) = clove_dir {
        cmd.arg("--clove-dir").arg(dir.as_str());
    }
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
