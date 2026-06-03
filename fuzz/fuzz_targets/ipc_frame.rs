//! Fuzz target (T-X02, M3): the daemon IPC decode path. Arbitrary bytes are fed
//! to the length-prefix frame reader, and each decoded payload is run through the
//! `Request`/`Response` JSON deserializers. None of this may panic, allocate
//! unboundedly (the reader rejects an oversized length prefix), or loop forever.
//!
//! Run: `cargo +nightly fuzz run ipc_frame -- -max_total_time=30`.

#![no_main]

use std::io::Cursor;

use clove_ipc::frame::read_frame;
use clove_ipc::{Request, Response};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Treat the input as a stream of frames; each `read_frame` makes progress
    // (consumes at least the 4-byte prefix) or errors, so this terminates.
    let mut cursor = Cursor::new(data);
    while let Ok(payload) = read_frame(&mut cursor) {
        // Both decoders must tolerate arbitrary payloads without panicking.
        let _ = serde_json::from_slice::<Request>(&payload);
        let _ = serde_json::from_slice::<Response>(&payload);
    }
});
