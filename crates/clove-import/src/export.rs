//! Low-level JSON / JSONL (NDJSON) export writers (T-M04).
//!
//! These helpers are deliberately *shaping-agnostic*: they take already-shaped
//! [`serde_json::Value`] item objects (built by the `clove` crate's `item_json`
//! module, which owns the full §7.4 item shape including the computed `ready` /
//! `blocked_by` fields) and serialize them either as a single standard
//! `{ v, ok, data, _meta }` envelope ([`export_json`]) or as newline-delimited
//! bare item objects ([`export_jsonl`]).
//!
//! Keeping the shaping in `clove` and only the byte-level writers here avoids
//! duplicating the item-JSON projection (and the graph/comment computation it
//! depends on) inside `clove-import`. See `docs/M2_PLAN.md` §3 Phase 1.

use std::io::{self, Write};

use serde_json::{json, Value};

/// The JSON envelope schema version (the `v` field; mirrors the CLI envelope).
const ENVELOPE_VERSION: u32 = 1;

/// Write a single standard success envelope `{ v, ok, data, _meta }` where
/// `data` is the array of all `items`, to `writer`.
///
/// The output is pretty-stable: `serde_json::to_writer` emits keys in insertion
/// order and the caller is responsible for deterministic item ordering, so two
/// calls with the same `items` produce byte-identical output. A single trailing
/// newline is written after the envelope.
pub fn export_json<W: Write>(writer: &mut W, items: &[Value], meta: Value) -> io::Result<()> {
    let envelope = json!({
        "v": ENVELOPE_VERSION,
        "ok": true,
        "data": items,
        "_meta": meta,
    });
    serde_json::to_writer(&mut *writer, &envelope)?;
    writer.write_all(b"\n")
}

/// Write `items` as NDJSON: one bare item object per line, each terminated by a
/// single `\n` (including the final line). No envelope wrapper — this is the
/// Beads-isomorphic `issues.jsonl` shape that `clove import beads` re-reads.
///
/// An empty `items` slice writes nothing (zero lines).
pub fn export_jsonl<W: Write>(writer: &mut W, items: &[Value]) -> io::Result<()> {
    for item in items {
        serde_json::to_writer(&mut *writer, item)?;
        writer.write_all(b"\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<Value> {
        vec![
            json!({ "id": "proj-AAAAAAAA", "title": "first" }),
            json!({ "id": "proj-BBBBBBBB", "title": "second" }),
        ]
    }

    #[test]
    fn json_is_single_envelope() {
        let mut buf = Vec::new();
        export_json(&mut buf, &items(), json!({ "warnings": [] })).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.ends_with('\n'));
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["v"], 1);
        assert_eq!(v["data"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn json_empty_data_array() {
        let mut buf = Vec::new();
        export_json(&mut buf, &[], json!({})).unwrap();
        let v: Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(v["data"], json!([]));
    }

    #[test]
    fn jsonl_one_object_per_line() {
        let mut buf = Vec::new();
        export_jsonl(&mut buf, &items()).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let v: Value = serde_json::from_str(line).unwrap();
            assert!(v.get("id").is_some());
            // No envelope wrapper.
            assert!(v.get("ok").is_none());
        }
        // Exactly one trailing newline after the last record.
        assert!(text.ends_with("}\n"));
        assert!(!text.ends_with("\n\n"));
    }

    #[test]
    fn jsonl_empty_writes_nothing() {
        let mut buf = Vec::new();
        export_jsonl(&mut buf, &[]).unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn deterministic() {
        let mut a = Vec::new();
        let mut b = Vec::new();
        export_jsonl(&mut a, &items()).unwrap();
        export_jsonl(&mut b, &items()).unwrap();
        assert_eq!(a, b);
    }
}
