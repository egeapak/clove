//! `clove serve` — run the web UI server for this repository.
//!
//! Standalone mode: builds a small tokio runtime, constructs the shared
//! [`clove_web::AppState`] from the discovered repo, starts a file-watcher (unless
//! `--no-watch`) for real-time push, and serves the embedded SPA + JSON/WebSocket
//! API until interrupted.

use std::net::{IpAddr, SocketAddr};

use clove_ipc::{DaemonClient, HubPaths};
use clove_types::CloveError;
use clove_web::AppState;

use crate::cli::ServeArgs;
use crate::context::Ctx;

pub fn run(
    ctx: &Ctx,
    args: ServeArgs,
    quiet: bool,
    no_index: bool,
    deep: bool,
) -> Result<(), CloveError> {
    // Hand off to a running daemon: it serves every project's web UI on one
    // port, so we have it serve this one too and point the user there instead of
    // binding a second server (and blocking this process). With no daemon
    // running, none is started — `serve` runs standalone as it always has. An
    // explicit `--port` the daemon isn't on is honored with a standalone server.
    if let Some(clove_dir) = ctx.issues_dir.parent() {
        let client = match HubPaths::resolve() {
            Ok(hub) if hub.footprint_present() || wait_for_starting_hub(&hub) => {
                DaemonClient::attach(&hub, clove_dir, true).ok()
            }
            _ => None,
        };
        if let Some(mut client) = client {
            if let Ok(status) = client.status() {
                match status.web_addr.zip(status.web_url) {
                    Some((addr, url))
                        if args.port.is_none_or(|port| port_of(&addr) == Some(port)) =>
                    {
                        if !quiet {
                            eprintln!("clove web UI served by the running daemon: {url}");
                        }
                        if args.open {
                            open_browser(&url);
                        }
                        return Ok(());
                    }
                    Some(_) => {}
                    None if !quiet => eprintln!(
                        "note: the running daemon is not serving the web UI; \
                         starting a standalone server"
                    ),
                    None => {}
                }
            }
        }
    }

    let ip: IpAddr = args.host.parse().map_err(|_| CloveError::InvalidField {
        field: "host".to_owned(),
        reason: format!("not a valid IP address: {}", args.host),
    })?;

    if !ip.is_loopback() && !args.allow_non_loopback {
        return Err(CloveError::InvalidField {
            field: "host".to_owned(),
            reason: "binding a non-loopback address requires --allow-non-loopback".to_owned(),
        });
    }
    if !ip.is_loopback() && !quiet {
        eprintln!(
            "warning: serving on a non-loopback address ({ip}) exposes write access \
             with no authentication; use only on a trusted network"
        );
    }

    let requested = SocketAddr::new(ip, args.port.unwrap_or(ctx.config.web.port));

    let state = AppState::new(
        ctx.store.clone(),
        ctx.issues_dir.clone(),
        ctx.config.id_prefix.clone(),
        "standalone",
        false,
        ctx.config.default_type,
    )
    // `--no-index`/`--deep` are global flags; `serve` used to drop them. Before
    // the engine the web always read files, so "force a file scan" was kept by
    // accident — afterwards it was simply false.
    .with_read_tiers(!no_index, !no_index, deep);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|source| CloveError::Io {
            path: ctx.root.clone(),
            source,
        })?;

    let result = runtime.block_on(async move {
        let listener = if args.port.is_some() {
            tokio::net::TcpListener::bind(requested).await?
        } else {
            clove_web::bind_or_free_port(requested).await?
        };
        let url = format!("http://{}", listener.local_addr()?);
        if !quiet {
            if args.port.is_none() && listener.local_addr()?.port() != requested.port() {
                eprintln!(
                    "note: port {} is in use; using a free port",
                    requested.port()
                );
            }
            eprintln!("clove web UI: {url}");
            if args.no_watch {
                eprintln!("  (file-watcher disabled — no live updates)");
            }
            eprintln!("  press Ctrl-C to stop");
        }
        if args.open {
            open_browser(&url);
        }
        if args.no_watch {
            clove_web::serve_on(state, listener).await
        } else {
            clove_web::serve_with_watch_on(state, listener).await
        }
    });

    result.map_err(|source| CloveError::Io {
        path: ctx.root.clone(),
        source,
    })
}

/// The port of a `host:port` address as the daemon advertises it.
fn port_of(addr: &str) -> Option<u16> {
    addr.parse::<SocketAddr>().ok().map(|addr| addr.port())
}

/// A daemon another client is starting holds its lock before it binds its
/// socket. Give it a few seconds to come up rather than race it with a second,
/// standalone server; `false` when no daemon is starting, or it never appears.
fn wait_for_starting_hub(hub: &HubPaths) -> bool {
    if !hub.running() {
        return false;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if hub.footprint_present() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    false
}

/// Best-effort browser launch (ignores failure).
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let cmd = "xdg-open";
    let _ = std::process::Command::new(cmd).arg(url).spawn();
}
