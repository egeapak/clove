//! Test knobs: `CLOVE_*` environment overrides the integration tests use to
//! shorten or lengthen timeouts and to observe internal waits. They are read
//! only in a `test-knobs` build, so a released `clove` ignores them (#61).

use std::time::Duration;

/// A knob's raw value: `None` when unset or compiled out.
pub fn var(name: &str) -> Option<String> {
    if !cfg!(feature = "test-knobs") {
        return None;
    }
    std::env::var(name).ok()
}

/// A millisecond knob as a [`Duration`]: `None` when unset, unparseable, or
/// compiled out.
pub fn duration(name: &str) -> Option<Duration> {
    var(name)?.parse().ok().map(Duration::from_millis)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_knob_is_honoured_only_in_a_test_knobs_build() {
        // A name no other test reads, so setting it races nothing.
        let name = "CLOVE_KNOBS_SELF_TEST_MS";
        std::env::set_var(name, "250");
        assert_eq!(
            super::duration(name),
            cfg!(feature = "test-knobs").then_some(std::time::Duration::from_millis(250))
        );
    }
}
