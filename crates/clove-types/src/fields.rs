//! Shared field-parsing helpers for the write surfaces.
//!
//! Item creation (`clove_core::ops::create`) and item editing ([`crate::request`])
//! both have to turn loose external inputs (priority `u8`, type/status words, raw
//! label and id lists) into the validated model types. Centralizing those
//! conversions here means every surface — the CLI, the web server, the MCP tools,
//! the daemon, and the TUI — parses a field exactly once, with one error message,
//! instead of each re-implementing the same coercions (the divergence that this
//! module exists to prevent).

use crate::{normalize_label, CloveError, CloveId, ItemType, Priority};

/// Validate a raw priority value (0–4) into a [`Priority`].
pub fn parse_priority(value: u8) -> Result<Priority, CloveError> {
    Priority::new(value)
}

/// Parse a type word (`bug|feature|chore|docs|epic`) into an [`ItemType`].
pub fn parse_type(raw: &str) -> Result<ItemType, CloveError> {
    ItemType::parse(raw)
}

/// Canonicalize a list of raw labels: normalize each, then sort + dedup so the
/// stored set is always canonical (DESIGN §2.2).
pub fn parse_labels(raw: &[String]) -> Result<Vec<String>, CloveError> {
    let mut labels = Vec::with_capacity(raw.len());
    for label in raw {
        labels.push(normalize_label(label)?);
    }
    labels.sort();
    labels.dedup();
    Ok(labels)
}

/// Parse a list of raw id strings into validated [`CloveId`]s.
pub fn parse_ids(raw: &[String]) -> Result<Vec<CloveId>, CloveError> {
    raw.iter().map(|id| CloveId::new(id)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_range_is_validated() {
        assert_eq!(parse_priority(0).unwrap(), Priority(0));
        assert_eq!(parse_priority(4).unwrap(), Priority(4));
        assert!(parse_priority(5).is_err());
    }

    #[test]
    fn type_words_parse() {
        assert_eq!(parse_type("Bug").unwrap(), ItemType::Bug);
        assert!(parse_type("saga").is_err());
    }

    #[test]
    fn labels_are_canonicalized_sorted_deduped() {
        let got = parse_labels(&[
            "Area:Core".to_owned(),
            "urgent".to_owned(),
            "area:core".to_owned(),
        ])
        .unwrap();
        assert_eq!(got, vec!["area:core".to_owned(), "urgent".to_owned()]);
        // An empty label is rejected.
        assert!(parse_labels(&["   ".to_owned()]).is_err());
    }

    #[test]
    fn ids_are_validated() {
        assert!(parse_ids(&["proj-7AF3K2MN".to_owned()]).is_ok());
        assert!(parse_ids(&["not a real id".to_owned()]).is_err());
    }
}
