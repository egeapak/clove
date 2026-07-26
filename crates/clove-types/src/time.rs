//! Canonical timestamp spelling — one RFC 3339 form for the whole workspace
//! (READ_PATH_ROADMAP §3).
//!
//! Timestamps used to be stored *as written*. RFC 3339 has several equivalent
//! spellings of the same instant (`Z` vs `+00:00`, an equivalent non-UTC offset,
//! and any amount of sub-second precision), and clove compares those strings in
//! places where a difference in spelling reads as a difference in *content*:
//!
//! - `clove-import`'s GitHub sync compares `updated` against the fingerprint it
//!   recorded on the last sync, so a finer-grained re-spelling of the same
//!   instant looks like a local edit and pushes a no-op PATCH;
//! - `clove stats --history` orders snapshots by `captured_at` as a string.
//!
//! So there is exactly one spelling clove ever *writes* — RFC 3339, UTC, whole
//! seconds, `Z` suffix ([`canonical_rfc3339`]) — and every read accepts any
//! parseable spelling and normalizes it ([`parse_rfc3339`],
//! [`canonicalize_rfc3339`]). Normalizing on read is what makes this a
//! no-flag-day change: an existing store is not migrated, it is simply re-spelled
//! the next time each item is written.
//!
//! Whole seconds is the precision the frontmatter writer has always rendered, so
//! this is not a new lossy step for item files — it is the same truncation
//! ([`truncate_to_seconds`]) applied at every boundary rather than only at the
//! last one.

use chrono::{DateTime, SecondsFormat, Utc};

/// Truncate a timestamp to whole seconds — the canonical on-disk precision
/// (the frontmatter writer renders RFC 3339 with seconds precision).
///
/// Every place that *stamps* a timestamp destined for frontmatter
/// (`ItemStore::create`/`update`, [`crate::set_status`]) truncates through this
/// one helper, so the in-memory value a mutation returns is byte-identical to
/// what a re-read parses back from disk.
pub fn truncate_to_seconds(ts: DateTime<Utc>) -> DateTime<Utc> {
    use chrono::Timelike;
    ts.with_nanosecond(0)
        .expect("zero nanoseconds is always valid")
}

/// Render `ts` in the single spelling clove writes: RFC 3339, UTC, whole
/// seconds, `Z` suffix (`2026-06-02T10:00:00Z`).
///
/// Note this *renders* at second precision; it does not require the caller to
/// have truncated first.
pub fn canonical_rfc3339(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Parse any RFC 3339 spelling into a UTC instant, truncated to the canonical
/// precision. Returns `None` for input RFC 3339 cannot parse.
///
/// This is the read half of the contract: `2026-06-02T10:00:00Z`,
/// `2026-06-02T10:00:00+00:00`, `2026-06-02T12:00:00+02:00` and
/// `2026-06-02T10:00:00.904816670Z` all parse to the same value, so a
/// hand-edited or foreign-written timestamp compares equal to the canonical one
/// rather than looking like a change.
pub fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|dt| truncate_to_seconds(dt.with_timezone(&Utc)))
}

/// Re-spell any parseable RFC 3339 string canonically.
///
/// Unparseable input is returned unchanged: canonicalization is a normalization
/// of data clove already holds, never a reason to fail a read. (Item files get
/// their timestamps validated at parse time; the strings this is applied to —
/// stats snapshot rows, comment file names — are already-accepted data.)
pub fn canonicalize_rfc3339(s: &str) -> String {
    parse_rfc3339(s).map_or_else(|| s.to_owned(), canonical_rfc3339)
}

/// `serde` adaptor for a required timestamp field: canonical on the way out,
/// any parseable spelling (normalized) on the way in.
///
/// Applying this at the *type* boundary rather than per format means every read
/// path — YAML frontmatter, the JSON export/restore, the IPC wire, web request
/// bodies — normalizes identically, and no surface can be forgotten.
pub mod serde_ts {
    use super::{canonical_rfc3339, parse_rfc3339};
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize as the canonical spelling.
    pub fn serialize<S: Serializer>(ts: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&canonical_rfc3339(*ts))
    }

    /// Deserialize any parseable RFC 3339 spelling, normalized.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
        let raw = String::deserialize(d)?;
        parse_rfc3339(&raw).ok_or_else(|| {
            serde::de::Error::custom(format!("`{raw}` is not an RFC 3339 timestamp"))
        })
    }
}

/// [`serde_ts`] for an optional timestamp field (`closed`).
pub mod serde_ts_opt {
    use super::{canonical_rfc3339, parse_rfc3339};
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize as the canonical spelling (or `null`).
    pub fn serialize<S: Serializer>(ts: &Option<DateTime<Utc>>, s: S) -> Result<S::Ok, S::Error> {
        match ts {
            Some(ts) => s.serialize_str(&canonical_rfc3339(*ts)),
            None => s.serialize_none(),
        }
    }

    /// Deserialize any parseable RFC 3339 spelling, normalized.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<DateTime<Utc>>, D::Error> {
        let Some(raw) = Option::<String>::deserialize(d)? else {
            return Ok(None);
        };
        parse_rfc3339(&raw).map(Some).ok_or_else(|| {
            serde::de::Error::custom(format!("`{raw}` is not an RFC 3339 timestamp"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every spelling of the same instant that must survive canonicalization as
    /// one string. Written as literals on purpose: building them by *rendering*
    /// a `DateTime` would pre-normalize the input and the fixture would prove
    /// nothing.
    const EQUIVALENT: &[&str] = &[
        "2026-06-02T10:00:00Z",
        "2026-06-02T10:00:00z",
        "2026-06-02T10:00:00+00:00",
        "2026-06-02T10:00:00-00:00",
        "2026-06-02T10:00:00.0Z",
        "2026-06-02T10:00:00.000Z",
        "2026-06-02T10:00:00.000000000+00:00",
        "2026-06-02T12:00:00+02:00",
        "2026-06-02T04:30:00-05:30",
        // Sub-second precision below the canonical resolution: the on-disk
        // precision is whole seconds, so these are the same stored instant.
        "2026-06-02T10:00:00.904816670+00:00",
        "2026-06-02T10:00:00.5Z",
        "2026-06-02T12:00:00.999999999+02:00",
    ];

    #[test]
    fn every_spelling_canonicalizes_to_one_string() {
        for spelling in EQUIVALENT {
            assert_eq!(
                canonicalize_rfc3339(spelling),
                "2026-06-02T10:00:00Z",
                "`{spelling}` must canonicalize to the one spelling"
            );
        }
    }

    #[test]
    fn every_spelling_parses_to_one_instant() {
        let want = parse_rfc3339("2026-06-02T10:00:00Z").unwrap();
        for spelling in EQUIVALENT {
            assert_eq!(
                parse_rfc3339(spelling),
                Some(want),
                "`{spelling}` must parse to one instant"
            );
        }
    }

    #[test]
    fn canonical_form_is_stable_under_re_canonicalization() {
        let once = canonicalize_rfc3339("2026-06-02T10:00:00.904816670+00:00");
        assert_eq!(canonicalize_rfc3339(&once), once);
    }

    #[test]
    fn distinct_instants_stay_distinct() {
        assert_ne!(
            canonicalize_rfc3339("2026-06-02T10:00:00Z"),
            canonicalize_rfc3339("2026-06-02T10:00:01Z")
        );
        // A sub-second difference that crosses a second boundary is a real
        // difference, not a spelling one.
        assert_ne!(
            canonicalize_rfc3339("2026-06-02T10:00:00.999Z"),
            canonicalize_rfc3339("2026-06-02T10:00:01.000Z")
        );
    }

    #[test]
    fn unparseable_input_is_returned_verbatim() {
        assert_eq!(canonicalize_rfc3339("not a timestamp"), "not a timestamp");
        assert_eq!(parse_rfc3339("not a timestamp"), None);
    }

    #[test]
    fn canonical_rendering_has_no_fraction_and_a_z_suffix() {
        let ts: DateTime<Utc> = "2026-06-02T10:00:00.904816670Z".parse().unwrap();
        assert_eq!(canonical_rfc3339(ts), "2026-06-02T10:00:00Z");
    }
}
