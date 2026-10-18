//! Test knobs: `CLOVED_*` environment overrides the integration tests use to
//! shorten timers and to stand in for a slow machine. They are read only in a
//! `test-knobs` build, so a released daemon ignores them (#61).

use std::time::Duration;

/// A millisecond knob: `None` when unset, unparseable, or compiled out.
pub fn ms(name: &str) -> Option<u64> {
    if !cfg!(feature = "test-knobs") {
        return None;
    }
    std::env::var(name).ok()?.parse().ok()
}

/// [`ms`] as a [`Duration`].
pub fn duration(name: &str) -> Option<Duration> {
    ms(name).map(Duration::from_millis)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_knob_is_honoured_only_in_a_test_knobs_build() {
        // A name no other test reads, so setting it races nothing.
        let name = "CLOVED_KNOBS_SELF_TEST_MS";
        std::env::set_var(name, "250");
        assert_eq!(super::ms(name), cfg!(feature = "test-knobs").then_some(250));
    }
}
