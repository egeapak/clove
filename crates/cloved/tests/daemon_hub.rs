//! The hub (DESIGN §8.1): one daemon serving many projects — attach and load,
//! isolation between projects, per-project idle eviction, detach, the legacy
//! lock, and the shared web listener. Unix-only (real sockets and signals).
#![cfg(unix)]

mod support;

use std::io::{Read, Write};
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use clove_ipc::hub::{codes, frame, recv_frame, send_frame, Hello, Welcome};
use clove_ipc::{ClientError, DaemonClient, QueryKind, QueryRequest};
use clove_types::NewSpec;
use support::{eventually, init_clove_dir, TestHub};

fn list(client: &mut DaemonClient) -> Vec<String> {
    client
        .query_list(QueryRequest {
            kind: QueryKind::List,
            filters: Default::default(),
            order: Default::default(),
            offset: 0,
            limit: None,
        })
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row.title)
        .collect()
}

fn create(client: &mut DaemonClient, title: &str) {
    client
        .create(NewSpec {
            title: title.to_owned(),
            ..Default::default()
        })
        .unwrap();
}

fn canonical(clove_dir: &Utf8Path) -> String {
    clove_dir.canonicalize_utf8().unwrap().to_string()
}

fn refusal_code(result: Result<DaemonClient, ClientError>) -> String {
    match result {
        Err(ClientError::Refused { code, .. }) => code,
        Err(other) => panic!("expected a refusal, got {other}"),
        Ok(_) => panic!("expected a refusal, got a client"),
    }
}

#[test]
fn one_hub_serves_two_projects_each_with_its_own_items() {
    let (_a_tmp, a) = init_clove_dir();
    let (_b_tmp, b) = init_clove_dir();
    let hub = TestHub::spawn(None);

    let mut alpha = hub.load(&a).unwrap();
    let mut beta = hub.load(&b).unwrap();
    create(&mut alpha, "only in alpha");
    create(&mut beta, "only in beta");

    assert_eq!(list(&mut alpha), vec!["only in alpha"]);
    assert_eq!(list(&mut beta), vec!["only in beta"]);
    assert_eq!(
        std::fs::read_dir(b.join("issues")).unwrap().count(),
        1,
        "alpha's write landed in alpha's store only"
    );

    let mut projects = hub.projects();
    projects.sort();
    let mut expected = vec![canonical(&a), canonical(&b)];
    expected.sort();
    assert_eq!(projects, expected);
    assert_eq!(hub.control().status().unwrap().pid, hub.child.id());
}

#[test]
fn a_probe_never_loads_a_project() {
    let (_a_tmp, a) = init_clove_dir();
    let (_b_tmp, b) = init_clove_dir();
    let hub = TestHub::spawn(Some(&a));

    assert!(DaemonClient::probe_at(&hub.paths, &b).is_none());
    assert_eq!(hub.projects(), vec![canonical(&a)]);
    assert!(
        std::fs::File::create(b.join("daemon.lock"))
            .unwrap()
            .try_lock()
            .is_ok(),
        "the probed project was never locked"
    );
}

#[test]
fn one_project_failing_to_load_leaves_the_others_serving() {
    let (_a_tmp, a) = init_clove_dir();
    let hub = TestHub::spawn(Some(&a));

    // Not a clove store at all: the client does not even ask.
    let bare = tempfile::tempdir().unwrap();
    let bare = Utf8PathBuf::from_path_buf(bare.path().to_path_buf()).unwrap();
    assert!(matches!(hub.load(&bare), Err(ClientError::Token(_))));

    // A store whose index cannot be opened.
    let (_c_tmp, c) = init_clove_dir();
    std::fs::create_dir(c.join("index.db")).unwrap();
    assert_eq!(refusal_code(hub.load(&c)), codes::LOAD_FAILED);

    let mut alpha = hub.client(&a);
    alpha.ping().unwrap();
    create(&mut alpha, "still here");
    assert_eq!(list(&mut alpha), vec!["still here"]);
    assert_eq!(hub.projects(), vec![canonical(&a)]);
}

/// A clove 0.1.0 daemon holds the project's `daemon.lock`. The hub must not
/// serve the project behind its back — the client falls back to direct reads —
/// and must pick it up once the old daemon lets go.
#[test]
fn a_project_held_by_a_legacy_daemon_is_refused_safely() {
    let (_a_tmp, a) = init_clove_dir();
    let (_b_tmp, legacy) = init_clove_dir();
    let hub = TestHub::spawn(Some(&a));

    let held = std::fs::File::create(legacy.join("daemon.lock")).unwrap();
    held.try_lock().unwrap();
    assert_eq!(refusal_code(hub.load(&legacy)), codes::PROJECT_LOCKED);
    assert!(DaemonClient::probe_at(&hub.paths, &legacy).is_none());
    assert_eq!(hub.projects(), vec![canonical(&a)]);
    hub.client(&a).ping().unwrap();

    drop(held);
    hub.load(&legacy)
        .expect("served once the old daemon is gone");
}

#[test]
fn each_project_idles_out_on_its_own_then_the_hub_exits() {
    let (_a_tmp, a) = init_clove_dir();
    let (_b_tmp, b) = init_clove_dir();
    let mut hub = TestHub::spawn_with(
        None,
        &[
            ("CLOVED_DISABLE_WEB", "1"),
            ("CLOVED_IDLE_SHUTDOWN_MS", "400"),
            ("CLOVED_HUB_GRACE_MS", "300"),
        ],
    );
    hub.load(&a).unwrap();
    let mut busy = hub.load(&b).unwrap();

    // Keep beta active while alpha sits idle.
    let alpha_gone = eventually(Duration::from_secs(3), || {
        busy.ping().unwrap();
        hub.projects() == vec![canonical(&b)]
    });
    assert!(alpha_gone, "alpha evicted, beta kept: {:?}", hub.projects());
    assert!(
        hub.child.try_wait().unwrap().is_none(),
        "the hub outlives one evicted project"
    );

    drop(busy);
    let status = hub.wait_exit(Duration::from_secs(5));
    assert!(status.is_some_and(|s| s.success()), "hub exited once empty");
}

#[test]
fn detaching_one_project_keeps_the_others_and_the_last_stops_the_hub() {
    let (_a_tmp, a) = init_clove_dir();
    let (_b_tmp, b) = init_clove_dir();
    let mut hub = TestHub::spawn(None);
    hub.load(&a).unwrap();
    hub.load(&b).unwrap();

    let detached = hub.detach(&a);
    assert!(detached.detached && !detached.hub_exiting, "{detached:?}");
    assert!(DaemonClient::probe_at(&hub.paths, &a).is_none());
    assert!(
        std::fs::File::create(a.join("daemon.lock"))
            .unwrap()
            .try_lock()
            .is_ok(),
        "alpha's lock is released by the time detach returns"
    );
    hub.client(&b).ping().unwrap();

    let last = hub.detach(&b);
    assert!(last.detached && last.hub_exiting, "{last:?}");
    assert!(
        hub.wait_exit(Duration::from_secs(5))
            .is_some_and(|s| s.success()),
        "detaching the last project stops the hub"
    );
    assert!(!hub.paths.pid().exists());
}

#[test]
fn concurrent_loads_of_one_project_share_one_slot() {
    let (_a_tmp, a) = init_clove_dir();
    let hub = TestHub::spawn(None);
    let loads: Vec<_> = (0..8)
        .map(|_| {
            let paths = hub.paths.clone();
            let dir = a.clone();
            std::thread::spawn(move || DaemonClient::attach(&paths, &dir, true).map(|_| ()))
        })
        .collect();
    for load in loads {
        load.join()
            .unwrap()
            .expect("every concurrent load attaches");
    }
    assert_eq!(hub.projects(), vec![canonical(&a)]);
}

#[test]
fn every_spelling_of_a_project_reaches_one_slot() {
    let (tmp, a) = init_clove_dir();
    let hub = TestHub::spawn(Some(&a));
    // The repository reached through a symlink (the `.clove` in it is real).
    let link = tmp.path().join("alias");
    std::os::unix::fs::symlink(a.parent().unwrap().as_std_path(), &link).unwrap();
    let alias = Utf8PathBuf::from_path_buf(link).unwrap().join(".clove");
    hub.client(&alias).ping().unwrap();
    hub.load(&alias).unwrap();
    assert_eq!(hub.projects(), vec![canonical(&a)]);
}

#[test]
fn a_client_of_another_protocol_is_refused_with_proof_of_life() {
    let (_a_tmp, a) = init_clove_dir();
    let hub = TestHub::spawn(Some(&a));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // 6: before the hub. 7: the hub before calls carried a project token — its
    // calls would reach this hub without one.
    for protocol in [6, 7] {
        let welcome = rt.block_on(async {
            use interprocess::local_socket::tokio::prelude::*;
            let stream = interprocess::local_socket::tokio::Stream::connect(
                hub.paths.socket_name().unwrap(),
            )
            .await
            .unwrap();
            let mut framed = frame(stream);
            send_frame(&mut framed, &Hello::Clove { protocol })
                .await
                .unwrap();
            recv_frame::<Welcome>(&mut framed).await.unwrap()
        });
        match welcome {
            Some(Welcome::Err { code, .. }) => {
                assert_eq!(code, codes::PROTOCOL_MISMATCH, "protocol {protocol}")
            }
            other => panic!("protocol {protocol}: expected a refusal, got {other:?}"),
        }
    }
}

/// Minimal HTTP GET over std TCP: `(status, location header, body)`.
fn http_get(addr: &str, path: &str) -> (u16, Option<String>, String) {
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let location = head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case("location")
            .then(|| value.trim().to_owned())
    });
    (status, location, body.to_owned())
}

#[test]
fn every_project_is_reachable_on_the_one_web_port() {
    let (_a_tmp, a) = init_clove_dir();
    let (_b_tmp, b) = init_clove_dir();
    let hub = TestHub::spawn_with(None, &[("CLOVED_WEB_PORT", "0")]);
    let mut alpha = hub.load(&a).unwrap();
    let mut beta = hub.load(&b).unwrap();
    create(&mut alpha, "alpha on the web");
    create(&mut beta, "beta on the web");

    let alpha_status = alpha.status().unwrap();
    let beta_status = beta.status().unwrap();
    let addr = alpha_status.web_addr.clone().expect("web served");
    assert_eq!(
        beta_status.web_addr.as_deref(),
        Some(addr.as_str()),
        "one port"
    );
    let alpha_url = alpha_status.web_url.expect("alpha has a URL");
    let beta_url = beta_status.web_url.expect("beta has a URL");
    assert_ne!(alpha_url, beta_url);

    for (url, title, other) in [
        (&alpha_url, "alpha on the web", "beta on the web"),
        (&beta_url, "beta on the web", "alpha on the web"),
    ] {
        let path = url.strip_prefix(&format!("http://{addr}")).unwrap();
        let (status, _, body) = http_get(&addr, &format!("{path}api/v1/items"));
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(title) && !body.contains(other), "{body}");
    }

    let (status, _, picker) = http_get(&addr, "/");
    assert_eq!(status, 200);
    assert!(picker.contains(&alpha_url[format!("http://{addr}").len()..]));

    // Down to one project: the root leads straight into it.
    hub.detach(&b);
    let (status, location, _) = http_get(&addr, "/");
    assert_eq!(status, 307);
    assert_eq!(
        location.as_deref(),
        Some(&alpha_url[format!("http://{addr}").len()..])
    );
}

/// A cloned repository can plant `.clove/daemon.lock` as a symlink to a file
/// the user cares about. Loading the project must never truncate the target.
#[test]
fn a_symlinked_project_lock_never_clobbers_its_target() {
    let (_tmp, clove_dir) = init_clove_dir();
    let victim_dir = tempfile::tempdir().unwrap();
    let victim = victim_dir.path().join("precious.txt");
    std::fs::write(&victim, "precious").unwrap();
    std::os::unix::fs::symlink(&victim, clove_dir.join("daemon.lock")).unwrap();

    let hub = TestHub::spawn(None);
    let _ = hub.load(&clove_dir);
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
}

/// A hub nobody can reach any more — its runtime directory deleted from under
/// it — exits instead of lingering for hours.
#[test]
fn a_hub_whose_runtime_directory_vanishes_exits() {
    let (_tmp, clove_dir) = init_clove_dir();
    let mut hub = TestHub::spawn(None);
    hub.load(&clove_dir).unwrap();
    std::fs::remove_dir_all(hub.paths.dir()).unwrap();
    assert!(
        hub.wait_exit(Duration::from_secs(10)).is_some(),
        "an unreachable hub must exit"
    );
}

/// A raw, handshaken connection to `hub`: the wire as any client sees it.
fn raw_client(hub: &TestHub, rt: &tokio::runtime::Runtime) -> clove_ipc::CloveRpcClient {
    rt.block_on(async {
        use interprocess::local_socket::tokio::prelude::*;
        let stream =
            interprocess::local_socket::tokio::Stream::connect(hub.paths.socket_name().unwrap())
                .await
                .unwrap();
        let mut framed = frame(stream);
        send_frame(&mut framed, &Hello::current()).await.unwrap();
        let welcome = recv_frame::<Welcome>(&mut framed).await.unwrap();
        assert!(matches!(welcome, Some(Welcome::Ok { .. })), "{welcome:?}");
        clove_ipc::CloveRpcClient::new(
            tarpc::client::Config::default(),
            clove_ipc::transport_from_framed(framed),
        )
        .spawn()
    })
}

/// A loading call for `dir`, as its own client makes it: with its token.
fn project(dir: &Utf8Path) -> clove_ipc::Project {
    clove_ipc::project(dir, true).unwrap()
}

/// Nothing binds a connection to a project: each call names its own, so one
/// connection can carry calls for several projects, and each is answered from
/// the project it names.
#[test]
fn one_connection_carries_calls_for_two_projects() {
    let (_ta, a) = init_clove_dir();
    let (_tb, b) = init_clove_dir();
    let hub = TestHub::spawn(None);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let client = raw_client(&hub, &rt);
    let new = |dir: &Utf8Path, title: &str| {
        rt.block_on(client.create(
            tarpc::context::current(),
            project(dir),
            NewSpec {
                title: title.to_owned(),
                ..Default::default()
            },
        ))
        .unwrap()
        .unwrap();
    };
    new(&a, "for a");
    new(&b, "for b");
    let titles = |dir: &Utf8Path| -> Vec<String> {
        rt.block_on(client.query(
            tarpc::context::current(),
            project(dir),
            QueryRequest {
                kind: QueryKind::List,
                filters: Default::default(),
                order: Default::default(),
                offset: 0,
                limit: None,
            },
        ))
        .unwrap()
        .unwrap()
        .rows
        .into_iter()
        .map(|r| r.title)
        .collect()
    };
    assert_eq!(titles(&a), vec!["for a"]);
    assert_eq!(titles(&b), vec!["for b"]);
}

/// A call naming its project by a relative path is refused: relative to what?
/// The hub's working directory is its own runtime directory, and belongs to no
/// client.
#[test]
fn a_call_naming_a_relative_project_is_refused() {
    let (_ta, a) = init_clove_dir();
    let hub = TestHub::spawn(Some(&a));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let client = raw_client(&hub, &rt);
    for load in [false, true] {
        let refused = rt
            .block_on(client.status(
                tarpc::context::current(),
                clove_ipc::Project {
                    clove_dir: ".clove".to_owned(),
                    load,
                    token: project(&a).token,
                },
            ))
            .unwrap()
            .unwrap_err();
        assert_eq!(refused.code, codes::BAD_PROJECT, "{refused:?}");
    }
    assert_eq!(hub.projects(), vec![canonical(&a)]);
}

/// Over the wire, as any client would try it: a call carrying a wrong token,
/// or naming project B with project A's token, is refused with `BAD_TOKEN` —
/// nothing is read from, written to, or loaded for B.
#[test]
fn a_call_is_refused_without_its_projects_token() {
    let (_ta, a) = init_clove_dir();
    let (_tb, b) = init_clove_dir();
    let hub = TestHub::spawn(Some(&a));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let client = raw_client(&hub, &rt);
    let create = |call: clove_ipc::Project| {
        rt.block_on(client.create(
            tarpc::context::current(),
            call,
            NewSpec {
                title: "smuggled".to_owned(),
                ..Default::default()
            },
        ))
        .unwrap()
    };
    let wrong = clove_ipc::Project {
        token: "0".repeat(64),
        ..project(&a)
    };
    assert_eq!(create(wrong).unwrap_err().code, codes::BAD_TOKEN);
    let b_with_a_token = clove_ipc::Project {
        token: project(&a).token,
        ..project(&b)
    };
    assert_eq!(create(b_with_a_token).unwrap_err().code, codes::BAD_TOKEN);

    assert_eq!(hub.projects(), vec![canonical(&a)], "B was loaded");
    assert_eq!(
        std::fs::read_dir(b.join("issues")).unwrap().count(),
        0,
        "B was written to"
    );
    assert!(create(project(&a)).is_ok(), "A's own token works");
}
