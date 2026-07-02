//! `clove serve` — run the web UI server for this repository.
//!
//! Standalone mode: builds a small tokio runtime, constructs the shared
//! [`clove_web::AppState`] from the discovered repo, starts a file-watcher (unless
//! `--no-watch`) for real-time push, and serves the embedded SPA + JSON/WebSocket
//! API until interrupted.

use std::net::{IpAddr, SocketAddr};

use clove_ipc::DaemonClient;
use clove_types::CloveError;
use clove_web::AppState;

use crate::cli::ServeArgs;
use crate::context::Ctx;

pub fn run(ctx: &Ctx, args: ServeArgs, quiet: bool) -> Result<(), CloveError> {
    // Hand off to a running daemon if it is already serving the web UI: the
    // daemon serves by default, so we point the user at it instead of binding a
    // second server (and blocking this process).
    if let Some(clove_dir) = ctx.issues_dir.parent() {
        if let Some(mut client) = DaemonClient::probe(clove_dir) {
            if let Ok(status) = client.status() {
                if let Some(addr) = status.web_addr {
                    let url = format!("http://{addr}");
                    if !quiet {
                        eprintln!("clove web UI served by the running daemon: {url}");
                    }
                    if args.open {
                        open_browser(&url);
                    }
                    return Ok(());
                } else if !quiet {
                    eprintln!(
                        "note: a daemon is running but web serving is disabled \
                         ([web] enabled = false); starting a standalone server"
                    );
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

    let addr = SocketAddr::new(ip, args.port);
    let url = format!("http://{addr}");

    let state = AppState::new(
        ctx.store.clone(),
        ctx.issues_dir.clone(),
        ctx.config.id_prefix.clone(),
        "standalone",
        false,
        ctx.config.default_type,
    );

    if !quiet {
        eprintln!("clove web UI: {url}");
        if args.no_watch {
            eprintln!("  (file-watcher disabled — no live updates)");
        }
        eprintln!("  press Ctrl-C to stop");
    }

    if args.open {
        open_browser(&url);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|source| CloveError::Io {
            path: ctx.root.clone(),
            source,
        })?;

    let result = runtime.block_on(async move {
        if args.no_watch {
            clove_web::serve(state, addr).await
        } else {
            clove_web::serve_with_watch(state, addr).await
        }
    });

    result.map_err(|source| CloveError::Io {
        path: ctx.root.clone(),
        source,
    })
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
