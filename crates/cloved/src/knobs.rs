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

/// Whether `dir` holds the file named by the environment variable `name`: a
/// test holds a timer or a step by creating the file, does its setup, then
/// removes it, instead of racing a short window against its own setup. Always
/// `false` outside a `test-knobs` build.
pub fn held(name: &str, dir: &std::path::Path) -> bool {
    if !cfg!(feature = "test-knobs") {
        return false;
    }
    std::env::var_os(name).is_some_and(|file| dir.join(file).exists())
}

/// [`held`] for the idle timers (`CLOVED_IDLE_HOLD_FILE`): a project's idle
/// window, or — in the hub's runtime directory — the hub's empty grace period.
pub fn idle_held(dir: &std::path::Path) -> bool {
    held("CLOVED_IDLE_HOLD_FILE", dir)
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
