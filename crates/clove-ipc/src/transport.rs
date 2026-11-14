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

/// The largest RPC frame either end sends or accepts. `tokio_util`'s default is
/// 8 MiB, which one `query` answer for a large store can exceed; the frame was
/// then refused and the connection closed. Both ends are the same user's
/// processes on a private socket, so the limit only bounds a broken peer.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Apply [`MAX_FRAME_BYTES`] to a codec. The hub handshake keeps the codec
/// default; the limit is raised when the stream is handed to tarpc.
fn raise_frame_limit(codec: &mut LengthDelimitedCodec) {
    codec.set_max_frame_length(MAX_FRAME_BYTES);
}

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
    mut framed: Framed<Stream, LengthDelimitedCodec>,
) -> Transport<Stream, Item, SinkItem, Json<Item, SinkItem>>
where
    Item: DeserializeOwned,
    SinkItem: Serialize,
{
    raise_frame_limit(framed.codec_mut());
    tarpc::serde_transport::new(framed, Json::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tarpc::tokio_util::bytes::{Bytes, BytesMut};
    use tarpc::tokio_util::codec::{Decoder, Encoder};

    /// A frame above `tokio_util`'s 8 MiB default passes the RPC codec in both
    /// directions, and a frame above [`MAX_FRAME_BYTES`] is still refused.
    #[test]
    fn a_frame_above_the_codec_default_round_trips() {
        let mut codec = LengthDelimitedCodec::new();
        raise_frame_limit(&mut codec);
        let payload = Bytes::from(vec![b'x'; 9 * 1024 * 1024]);
        let mut wire = BytesMut::new();
        codec
            .encode(payload.clone(), &mut wire)
            .expect("a 9 MiB frame is sent");
        let frame = codec
            .decode(&mut wire)
            .expect("a 9 MiB frame is accepted")
            .expect("the frame is complete");
        assert_eq!(frame.len(), payload.len());

        let too_big = Bytes::from(vec![b'x'; MAX_FRAME_BYTES + 1]);
        assert!(codec.encode(too_big, &mut BytesMut::new()).is_err());
    }
}
