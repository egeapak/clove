//! IPC server: the async side of the clove-ipc wire protocol (DESIGN §8.4).
//!
//! The framing matches [`clove_ipc::frame`] (4-byte LE length prefix + JSON), but
//! is driven over Tokio's `AsyncRead`/`AsyncWrite` here so the accept loop stays
//! on the runtime. The blocking client in `clove-ipc` interoperates byte-for-byte.
//!
//! **Phase 1 (this commit):** `PING` and `STATUS` (both answered from daemon
//! state alone). `QUERY`/`REINDEX` — which touch the index — land in Phase 2
//! (T-D03).

use std::io;
use std::sync::{Arc, Mutex};

use clove_index::Index;
use clove_ipc::frame::MAX_FRAME;
use clove_ipc::protocol::{ErrorResponse, Request, Response};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::state::DaemonState;

/// Shared context every connection handler needs.
#[derive(Clone)]
pub struct Dispatcher {
    // Used by the QUERY/REINDEX handlers in Phase 2 (T-D03).
    #[allow(dead_code)]
    pub index: Arc<Mutex<Index>>,
    pub state: Arc<Mutex<DaemonState>>,
}

impl Dispatcher {
    /// Map a request to a response. Phase 1 answers `PING` and `STATUS`; the
    /// index-touching commands return a structured error until Phase 2.
    pub fn dispatch(&self, req: Request) -> Response {
        if let Ok(mut state) = self.state.lock() {
            state.mark_event();
        }
        match req {
            Request::Ping => Response::Pong,
            Request::Status => match self.state.lock() {
                Ok(state) => Response::Status(state.snapshot()),
                Err(_) => Response::Error(ErrorResponse::new("internal", "state lock poisoned")),
            },
            Request::Query(_) | Request::Reindex => Response::Error(ErrorResponse::new(
                "not_implemented",
                "command not implemented until M3 Phase 2",
            )),
        }
    }
}

/// Read one length-prefixed frame asynchronously. `Ok(None)` means the peer
/// closed the connection cleanly at a frame boundary (clean EOF on the prefix).
pub async fn read_frame_async<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds MAX_FRAME",
        ));
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

/// Write one length-prefixed frame asynchronously.
pub async fn write_frame_async<W: AsyncWrite + Unpin>(w: &mut W, payload: &[u8]) -> io::Result<()> {
    let len: u32 = payload
        .len()
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame too large"))?;
    w.write_all(&len.to_le_bytes()).await?;
    w.write_all(payload).await?;
    w.flush().await?;
    Ok(())
}

/// Serve one connection: read requests, dispatch, write responses, until the peer
/// closes or a transport error occurs. A malformed frame is answered with an error
/// response and the connection is dropped; the daemon stays up.
pub async fn handle_connection<S>(mut stream: S, dispatcher: Dispatcher) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(payload) = read_frame_async(&mut stream).await? {
        let response = match serde_json::from_slice::<Request>(&payload) {
            Ok(req) => dispatcher.dispatch(req),
            Err(e) => Response::Error(ErrorResponse::new("bad_request", e.to_string())),
        };
        let out = serde_json::to_vec(&response)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        write_frame_async(&mut stream, &out).await?;
    }
    Ok(())
}
