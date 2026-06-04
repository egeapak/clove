//! The IPC frame codec (DESIGN §8.4): a 4-byte little-endian length prefix
//! followed by a UTF-8 JSON payload of exactly that many bytes.
//!
//! The codec is generic over [`std::io::Read`]/[`std::io::Write`] so it serves the
//! synchronous client ([`crate::client`]) directly and can be driven over any
//! transport in tests. A hard [`MAX_FRAME`] bound makes the reader safe against a
//! hostile or corrupt peer: an oversized length prefix is rejected up front rather
//! than triggering a huge allocation (exercised by the `ipc_frame` fuzz target).

use std::io::{Read, Write};

use serde::de::DeserializeOwned;
use serde::Serialize;
use thiserror::Error;

/// Maximum accepted frame payload size (16 MiB). Far larger than any real
/// request/response, but bounded so a bogus length prefix cannot force an
/// unbounded allocation.
pub const MAX_FRAME: u32 = 16 * 1024 * 1024;

/// A framing or (de)serialization failure.
#[derive(Debug, Error)]
pub enum FrameError {
    /// Underlying transport I/O failure.
    #[error("frame i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// The length prefix exceeds [`MAX_FRAME`]; the peer is corrupt or hostile.
    #[error("frame too large: {0} bytes exceeds the {MAX_FRAME}-byte limit")]
    TooLarge(u32),

    /// The payload was not valid UTF-8 / JSON for the expected type.
    #[error("frame decode error: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Write one length-prefixed frame: a 4-byte LE length, then `payload`.
pub fn write_frame<W: Write>(w: &mut W, payload: &[u8]) -> Result<(), FrameError> {
    let len: u32 = payload
        .len()
        .try_into()
        .map_err(|_| FrameError::TooLarge(u32::MAX))?;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    w.write_all(&len.to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()?;
    Ok(())
}

/// Read one length-prefixed frame, returning the raw payload bytes.
///
/// Rejects a length prefix over [`MAX_FRAME`] before allocating. A clean EOF on
/// the length prefix surfaces as [`std::io::ErrorKind::UnexpectedEof`] via
/// [`FrameError::Io`], letting the caller distinguish "peer closed" from a decode
/// error.
pub fn read_frame<R: Read>(r: &mut R) -> Result<Vec<u8>, FrameError> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
    Ok(payload)
}

/// Serialize `msg` to JSON and write it as one frame.
pub fn write_message<W: Write, T: Serialize>(w: &mut W, msg: &T) -> Result<(), FrameError> {
    let payload = serde_json::to_vec(msg)?;
    write_frame(w, &payload)
}

/// Read one frame and deserialize it from JSON into `T`.
pub fn read_message<R: Read, T: DeserializeOwned>(r: &mut R) -> Result<T, FrameError> {
    let payload = read_frame(r)?;
    Ok(serde_json::from_slice(&payload)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Request, Response};

    #[test]
    fn frame_round_trips_over_a_buffer() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"hello world").unwrap();
        // 4-byte LE length prefix for 11 bytes.
        assert_eq!(&buf[..4], &11u32.to_le_bytes());
        let mut cursor = std::io::Cursor::new(buf);
        let got = read_frame(&mut cursor).unwrap();
        assert_eq!(got, b"hello world");
    }

    #[test]
    fn message_round_trips() {
        let mut buf = Vec::new();
        write_message(&mut buf, &Request::Ping).unwrap();
        write_message(&mut buf, &Response::Pong).unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        let req: Request = read_message(&mut cursor).unwrap();
        let resp: Response = read_message(&mut cursor).unwrap();
        assert_eq!(req, Request::Ping);
        assert_eq!(resp, Response::Pong);
    }

    #[test]
    fn empty_payload_is_valid() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"").unwrap();
        assert_eq!(buf, 0u32.to_le_bytes());
        let mut cursor = std::io::Cursor::new(buf);
        assert_eq!(read_frame(&mut cursor).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn oversize_length_prefix_is_rejected_without_allocating() {
        // A length prefix just over the cap, with no payload following: the reader
        // must reject on the prefix alone, never trying to allocate/read the body.
        let mut bytes = (MAX_FRAME + 1).to_le_bytes().to_vec();
        let mut cursor = std::io::Cursor::new(std::mem::take(&mut bytes));
        match read_frame(&mut cursor) {
            Err(FrameError::TooLarge(n)) => assert_eq!(n, MAX_FRAME + 1),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn truncated_frame_errors_not_hangs() {
        // Claims 10 bytes but supplies 3: read_exact hits EOF → Io error.
        let mut bytes = 10u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"abc");
        let mut cursor = std::io::Cursor::new(bytes);
        match read_frame(&mut cursor) {
            Err(FrameError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof),
            other => panic!("expected Io(UnexpectedEof), got {other:?}"),
        }
    }

    #[test]
    fn junk_payload_fails_to_decode_as_message() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"not json at all").unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        let got: Result<Request, _> = read_message(&mut cursor);
        assert!(matches!(got, Err(FrameError::Decode(_))));
    }
}
