//! #55: per-project daemons (and `clove serve`) share one configured web port.
//! Whoever loses the bind now serves on a free port and advertises it, so every
//! project's UI stays reachable and `clove serve` hands off to a working URL.
//! Unix-only, like the other daemon tests.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use assert_cmd::cargo::cargo_bin;
use assert_cmd::prelude::*;

fn cloved_built() -> bool {
    cargo_bin("clove").with_file_name("cloved").exists()
}

fn clove(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("clove").unwrap();
    cmd.current_dir(dir);
    cmd.env_remove("CLOVE_FORMAT");
    cmd.env_remove("CLOVED_DISABLE_WEB");
    cmd.env("CLOVE_AUTHOR", "tester@example.com");
    cmd
}

/// An initialized repo whose configured web port is `port`, holding one item
/// titled [`title`] so a response can be traced to this repo and no other (a
/// developer's own daemon may be serving 7373 while these tests run).
fn repo_with_web_port(port: u16) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path()).arg("init").assert().success();
    clove(dir.path())
        .args(["new", &title(dir.path())])
        .assert()
        .success();
    let config = dir.path().join(".clove/config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(&format!("\n[web]\nport = {port}\n"));
    std::fs::write(config, text).unwrap();
    dir
}

fn title(dir: &Path) -> String {
    format!("item-{}", dir.file_name().unwrap().to_string_lossy())
}

/// Whether the web UI on loopback `port` answers `GET /api/v1/items` with 200
/// and lists `repo`'s item.
fn serves_repo(port: u16, repo: &Path) -> bool {
    let response = items_response(port);
    response.starts_with("HTTP/1.1 200") && response.contains(&title(repo))
}

/// The raw response to `GET /api/v1/items` on loopback `port`.
fn items_response(port: u16) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(b"GET /api/v1/items HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

/// The port in the first `http://127.0.0.1:<port>` URL of `text`.
fn url_port(text: &str) -> Option<u16> {
    let rest = &text[text.find("http://127.0.0.1:")? + "http://127.0.0.1:".len()..];
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// Spawn a standalone `clove serve` and return it with the port it announced,
/// or panic (after reaping it) if it exits instead of serving.
fn spawn_serve(dir: &Path, args: &[&str]) -> (Child, u16) {
    let mut child = clove(dir)
        .arg("serve")
        .args(args)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    let announced = loop {
        line.clear();
        if stderr.read_line(&mut line).unwrap_or(0) == 0 {
            break None;
        }
        if let Some(port) = url_port(&line) {
            break Some(port);
        }
    };
    std::thread::sleep(std::time::Duration::from_millis(200));
    match (announced, child.try_wait().unwrap()) {
        (Some(port), None) => (child, port),
        (_, status) => {
            let _ = child.kill();
            let status = status.or_else(|| child.wait().ok());
            panic!("serve did not keep serving (last line {line:?}, status {status:?})");
        }
    }
}

#[test]
fn daemon_that_loses_the_web_port_serves_on_a_free_one() {
    if !cloved_built() {
        return;
    }
    let held = TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = held.local_addr().unwrap().port();
    let repo = repo_with_web_port(taken);

    clove(repo.path())
        .args(["daemon", "start"])
        .assert()
        .success();
    let out = clove(repo.path()).arg("serve").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let port = url_port(&stderr);
    let serves = port.is_some_and(|port| serves_repo(port, repo.path()));
    clove(repo.path())
        .args(["daemon", "stop"])
        .assert()
        .success();

    assert!(out.status.success(), "serve failed: {stderr}");
    assert!(stderr.contains("served by the running daemon"), "{stderr}");
    let port = port.expect("serve printed the daemon's URL");
    assert_ne!(port, taken, "advertised the port another process holds");
    assert!(serves, "port {port} does not serve this repo");
}

#[test]
fn daemon_web_ui_on_the_fallback_port_answers() {
    if !cloved_built() {
        return;
    }
    let held = TcpListener::bind("127.0.0.1:0").unwrap();
    let repo = repo_with_web_port(held.local_addr().unwrap().port());

    clove(repo.path())
        .args(["daemon", "start"])
        .assert()
        .success();
    let out = clove(repo.path()).arg("serve").output().unwrap();
    let port = url_port(&String::from_utf8_lossy(&out.stderr)).expect("daemon URL");
    let serves = serves_repo(port, repo.path());
    clove(repo.path())
        .args(["daemon", "stop"])
        .assert()
        .success();
    assert!(serves, "port {port} does not serve this repo");
}

#[test]
fn standalone_serve_falls_back_when_the_configured_port_is_taken() {
    let held = TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = held.local_addr().unwrap().port();
    let repo = repo_with_web_port(taken);

    let (mut child, port) = spawn_serve(repo.path(), &[]);
    let serves = serves_repo(port, repo.path());
    child.kill().unwrap();
    child.wait().unwrap();
    assert_ne!(port, taken);
    assert!(serves, "port {port} does not serve this repo");
}

#[test]
fn explicit_port_is_honored_not_swapped() {
    let held = TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = held.local_addr().unwrap().port();
    let repo = repo_with_web_port(taken);

    // A port the user asked for by name either binds or fails — never a surprise.
    let out = clove(repo.path())
        .args(["serve", "--port", &taken.to_string()])
        .output()
        .unwrap();
    assert!(!out.status.success(), "bound a port another process holds");

    let free = TcpListener::bind("127.0.0.1:0").unwrap();
    let wanted = free.local_addr().unwrap().port();
    drop(free);
    let (mut child, port) = spawn_serve(repo.path(), &["--port", &wanted.to_string()]);
    let serves = serves_repo(port, repo.path());
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(port, wanted);
    assert!(serves, "port {port} does not serve this repo");
}

#[test]
fn explicit_port_skips_the_daemon_hand_off() {
    if !cloved_built() {
        return;
    }
    let held = TcpListener::bind("127.0.0.1:0").unwrap();
    let repo = repo_with_web_port(held.local_addr().unwrap().port());
    clove(repo.path())
        .args(["daemon", "start"])
        .assert()
        .success();

    let free = TcpListener::bind("127.0.0.1:0").unwrap();
    let wanted = free.local_addr().unwrap().port();
    drop(free);
    let (mut child, port) = spawn_serve(repo.path(), &["--port", &wanted.to_string()]);
    let serves = serves_repo(port, repo.path());
    child.kill().unwrap();
    child.wait().unwrap();
    clove(repo.path())
        .args(["daemon", "stop"])
        .assert()
        .success();
    assert_eq!(
        port, wanted,
        "an explicit --port must not be swapped for the daemon's"
    );
    assert!(serves, "port {port} does not serve this repo");
}
