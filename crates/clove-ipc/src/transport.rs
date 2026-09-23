//! tarpc transport over a local socket (Unix domain socket / Windows named pipe
//! via `interprocess` + tokio). The raw stream is length-delimited and given a
//! JSON codec, so the wire is debuggable and language-agnostic.
//!
//! We use `tarpc`'s own re-exports of `tokio_util`/`tokio_serde` so the `Framed`
//! and `Json` types match exactly what `tarpc::serde_transport::new` expects
//! (avoiding a version skew with a separately-pinned tokio-util).
//!
//! [`build_transport`] is generic over the in/out message types so both ends —
//! which see the request/response types in opposite positions — reuse it; the
//! concrete `Item`/`SinkItem` are inferred from the `tarpc` client/server that
//! consumes the returned transport.

use interprocess::local_socket::tokio::Stream;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tarpc::serde_transport::Transport;
use tarpc::tokio_serde::formats::Json;
use tarpc::tokio_util::codec::{Framed, LengthDelimitedCodec};

/// Wrap a connected local-socket [`Stream`] in a length-delimited JSON tarpc
/// transport carrying `Item` (received) and `SinkItem` (sent).
pub fn build_transport<Item, SinkItem>(
    stream: Stream,
) -> Transport<Stream, Item, SinkItem, Json<Item, SinkItem>>
where
    Item: DeserializeOwned,
    SinkItem: Serialize,
{
    transport_from_framed(LengthDelimitedCodec::builder().new_framed(stream))
}

/// Continue an already-framed connection as a tarpc transport — the hand-over
/// after the hub handshake ([`crate::hub`]). Reusing the `Framed` rather than
/// its inner stream keeps any bytes it already buffered.
pub fn transport_from_framed<Item, SinkItem>(
    framed: Framed<Stream, LengthDelimitedCodec>,
) -> Transport<Stream, Item, SinkItem, Json<Item, SinkItem>>
where
    Item: DeserializeOwned,
    SinkItem: Serialize,
{
    tarpc::serde_transport::new(framed, Json::default())
}
