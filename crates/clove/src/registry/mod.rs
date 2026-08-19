//! The crates.io plugin registry (Stage 1: discovery).
//!
//! clove uses **crates.io itself** as its plugin registry rather than a curated
//! manifest: a plugin becomes discoverable by publishing, not by being added to a
//! list clove ships. This module owns the three discovery primitives:
//!
//! - **resolution** — an exact-name probe of `clove-<mux>-<source>`
//!   ([`crates_io::crate_exists`]). crates.io has no prefix search (`?q=` is
//!   fuzzy full-text: `?q=clove-sync` returns *zero* results), so constructing
//!   the name is both necessary and strictly more precise than searching;
//! - **discovery** — the reverse dependencies of `clove-plugin`
//!   ([`crates_io::reverse_dependents`]). A crate appears there only if it
//!   genuinely depends on `clove-plugin`, which is what makes it a plugin;
//! - **caching** — [`cache`], so `plugin list --all` stays fast and works
//!   offline after a first fetch.
//!
//! # Containment
//!
//! Two rules keep this module from leaking:
//!
//! 1. **`ureq` is named in [`http`] and nowhere else.** Everything else depends
//!    on the [`Fetch`] trait, so every test in this module is offline by
//!    construction.
//! 2. **This module is CLI-only.** `ureq` is blocking/sync — correct for a CLI,
//!    but it must never be called from `cloved` or `clove-web`, which are
//!    tokio/axum. Discovery is reached only from `clove plugin …`.
//!
//! Dispatch (`clove sync github` resolving and exec'ing a plugin binary) never
//! touches any of this: it stays the pure `stat` walk in [`crate::plugin`].

pub mod cache;
pub mod crates_io;
pub mod git_source;
pub mod http;
pub mod install;
pub mod provenance;

use std::time::Duration;

/// A plugin discovered from the registry — not necessarily installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryPlugin {
    /// The crate name, e.g. `clove-sync-gitlab`.
    pub crate_name: String,
    /// The highest **non-yanked** published version, if any.
    pub latest: Option<semver::Version>,
    /// The highest version overall when every version is yanked — so a crate
    /// whose releases were all pulled is still representable (and reportable)
    /// rather than collapsing to a blank version.
    pub latest_yanked: Option<semver::Version>,
    /// The crate's one-line description.
    pub description: Option<String>,
    /// The crate's source repository.
    pub repository: Option<String>,
    /// The binaries the crate builds — the dispatch-resolvable names.
    pub bin_names: Vec<String>,
    /// The crates.io account that published it.
    pub published_by: Option<String>,
    /// Download count, as a (weak) popularity signal.
    pub downloads: u64,
}

impl RegistryPlugin {
    /// True when the crate has published versions but every one is yanked.
    pub fn fully_yanked(&self) -> bool {
        self.latest.is_none() && self.latest_yanked.is_some()
    }

    /// The version to display, preferring the installable one and falling back
    /// to a yanked release so a fully-yanked crate still shows something real.
    pub fn display_version(&self) -> Option<String> {
        self.latest
            .as_ref()
            .or(self.latest_yanked.as_ref())
            .map(|v| v.to_string())
    }
}

/// A transport failure from [`Fetch::get`].
///
/// Modeled as an enum rather than an opaque string for three reasons: tests need
/// `Debug`, `CloveError` needs `Display`, and — the substantive one — crates.io
/// **rate-limits with `429` + `Retry-After`**. The name probe issues several
/// sequential requests and discovery paginates, so a 429 is a live path that
/// must be distinguishable (and backed off) rather than reported as "the network
/// is down".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// A non-success HTTP status other than 404 (which is `Ok(None)`).
    Status {
        code: u16,
        retry_after: Option<Duration>,
        body: Option<String>,
    },
    /// A connection/TLS/DNS failure.
    Transport(String),
    /// A malformed response body.
    Decode(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Status { code, .. } if *code == 429 => {
                write!(f, "crates.io rate limit reached (HTTP 429)")
            }
            FetchError::Status { code, .. } if *code == 403 => write!(
                f,
                "crates.io refused the request (HTTP 403) — a missing User-Agent \
                 is the usual cause"
            ),
            FetchError::Status { code, .. } => write!(f, "crates.io returned HTTP {code}"),
            FetchError::Transport(message) => write!(f, "could not reach crates.io: {message}"),
            FetchError::Decode(message) => {
                write!(f, "could not read the crates.io response: {message}")
            }
        }
    }
}

impl std::error::Error for FetchError {}

impl FetchError {
    /// How long to wait before retrying, when the server asked us to wait.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            FetchError::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// Is this a *server-side* failure worth retrying — 429 or 5xx?
    ///
    /// Deliberately excludes [`FetchError::Transport`]. A transport failure has
    /// usually already consumed the full request timeout, and retrying spends
    /// that budget again; the timeout is short specifically because several
    /// requests run in sequence. A 403 is not transient either (it is the
    /// missing-User-Agent case), and a decode failure will fail identically
    /// every time.
    pub fn is_retryable_status(&self) -> bool {
        match self {
            FetchError::Status { code, .. } => *code == 429 || (500..600).contains(code),
            FetchError::Transport(_) | FetchError::Decode(_) => false,
        }
    }
}

impl From<FetchError> for clove_types::CloveError {
    fn from(error: FetchError) -> Self {
        clove_types::CloveError::Registry {
            message: error.to_string(),
        }
    }
}

/// An HTTP GET against the registry.
///
/// The `Option` in the success type is load-bearing: `Ok(None)` is an
/// **authoritative 404** ("this crate does not exist") while `Err` is a
/// transport failure ("we could not find out"). Collapsing the two would make a
/// flaky network read as "not found", which is the wrong answer in the
/// security-relevant direction — so the distinction is encoded in the type
/// rather than left to each caller's discipline.
pub trait Fetch {
    fn get(&self, url: &str) -> Result<Option<String>, FetchError>;
}

/// The maximum length of a plugin binary name.
const MAX_BIN_NAME: usize = 64;

/// Validate a binary name that will become a `cargo install --bin` argument.
///
/// **`--bin` is a glob pattern, not a literal.** Verified against cargo 1.94:
/// a crate declaring `[[bin]] name = "clove-[a-z]*"` alongside
/// `clove-import-thing` and `clove-sync-github`, installed with
/// `--bin 'clove-[a-z]*'`, installs **both** real binaries. So the
/// "only the binary the user approved" restriction is worth exactly as much as
/// this validation: without it, a crate picks its own glob and lands any number
/// of binaries in `<clove-home>/bin` — which outranks `$PATH` and whose contents
/// receive the full inherited environment, `GITHUB_TOKEN` included, on the next
/// dispatch.
///
/// The name arrives from the registry response or a cloned `Cargo.toml`, neither
/// of which clove controls and neither of which cargo validates for this purpose,
/// so it is checked here before it becomes argv. Rejecting everything outside
/// `[A-Za-z0-9_-]` closes the glob metacharacters (`*?[]{}`), path separators and
/// `..` in one rule.
pub fn validate_bin_name(name: &str) -> Result<(), clove_types::CloveError> {
    let invalid = |reason: String| clove_types::CloveError::InvalidField {
        field: "bin".to_owned(),
        reason,
    };

    let Some(rest) = name.strip_prefix("clove-") else {
        return Err(invalid(format!(
            "`{name}` is not a dispatchable plugin binary (must be `clove-<name>`)"
        )));
    };
    if rest.is_empty() {
        return Err(invalid(
            "a plugin binary needs a name after `clove-`".to_owned(),
        ));
    }
    if name.len() > MAX_BIN_NAME {
        return Err(invalid(format!(
            "binary name is longer than {MAX_BIN_NAME} characters"
        )));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        return Err(invalid(format!(
            "binary name {name:?} contains {bad:?}; only letters, digits, `-` and `_`              are allowed (cargo treats `--bin` as a glob, so a name containing              `*`, `?` or `[` would install binaries the user never approved)"
        )));
    }
    Ok(())
}

/// Render an untrusted string for display on a single terminal line.
///
/// Every field shown in the install confirmation — crate name, binary, owner,
/// repository, git URL — comes from a registry response or a cloned manifest, and
/// the confirmation *is* the security decision. Left raw, a `repository` value
/// containing a newline forges extra prompt lines ("checks: audited by clove"),
/// and CR or CSI sequences overwrite the line stating that third-party code is
/// about to run, before the user answers.
///
/// So: control characters (C0, DEL and C1) are dropped rather than escaped —
/// nothing legitimate needs them — and the result is clamped, because a very long
/// value scrolls the real facts off screen just as effectively.
pub fn display_safe(value: &str) -> String {
    const MAX: usize = 200;
    let mut out: String = value
        .chars()
        .filter(|c| !c.is_control() && !('\u{80}'..='\u{9f}').contains(c))
        .collect();
    if out.chars().count() > MAX {
        out = out.chars().take(MAX).collect::<String>() + "…";
    }
    out
}

/// The maximum length crates.io allows for a crate name.
const MAX_CRATE_NAME: usize = 64;

/// Validate a crate/plugin name against crates.io's own rule
/// (`^[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}$`).
///
/// Every accepted name is interpolated into a request URL and (in Stage 2) into
/// a subprocess argv, so this is validated centrally — mirroring how
/// [`crate::plugin`] guards *dispatch* for every caller in one place rather than
/// per call site. Two failure modes this closes:
///
/// - a name containing `/` or `..` path-traverses after URL normalization onto
///   an unrelated crates.io endpoint, which a name probe would read as
///   "resolved";
/// - a name beginning with `-` is parsed as a **flag** by any subprocess it
///   reaches (`git clone --upload-pack=…` executes a command; `--template=<dir>`
///   runs hooks from a directory; `cargo --config source.crates-io.replace-with`
///   redirects the download to another registry).
pub fn validate_crate_name(name: &str) -> Result<(), clove_types::CloveError> {
    let invalid = |reason: String| clove_types::CloveError::InvalidField {
        field: "name".to_owned(),
        reason,
    };

    if name.is_empty() {
        return Err(invalid("a plugin name cannot be empty".to_owned()));
    }
    if name.len() > MAX_CRATE_NAME {
        return Err(invalid(format!(
            "a crate name may be at most {MAX_CRATE_NAME} characters, got {}",
            name.len()
        )));
    }
    let first = name.as_bytes()[0];
    if !first.is_ascii_alphanumeric() {
        return Err(invalid(format!(
            "a crate name must start with a letter or digit, got {name:?}"
        )));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        return Err(invalid(format!(
            "a crate name may only contain letters, digits, `-` and `_`, found {bad:?} in {name:?}"
        )));
    }
    Ok(())
}

/// The crate names a bare plugin name could refer to, in probe order.
///
/// `clove plugin search gitlab` and `clove plugin install gitlab` ask the same
/// question — "what could `gitlab` be?" — so they must build the same list. They
/// used to build it from two copy-pasted literals, which is how they drifted:
/// `search` filtered its hits for dispatchability and `install` did not, so a
/// lib-only `clove-gitlab` published alongside a real `clove-sync-gitlab` made
/// `search` show one result while `install` refused the same query as ambiguous.
///
/// A name already carrying the `clove-` prefix is exact — it names a crate, not a
/// provider, and must not be expanded into `clove-sync-clove-foo`.
///
/// The caller is responsible for validating `name` first; this only assembles
/// strings.
pub fn candidate_crate_names(name: &str) -> Vec<String> {
    if name.starts_with("clove-") {
        return vec![name.to_owned()];
    }
    // Multiplexer providers first (`clove sync gitlab` is the common shape), then
    // the plain top-level subcommand.
    ["sync", "import", "export"]
        .iter()
        .map(|mux| format!("clove-{mux}-{name}"))
        .chain(std::iter::once(format!("clove-{name}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_crate_names_are_accepted() {
        for name in [
            "clove-sync-gitlab",
            "clove_plugin",
            "a",
            "gitlab",
            "clove-sync-gitlab2",
        ] {
            assert!(validate_crate_name(name).is_ok(), "{name} should be valid");
        }
    }

    #[test]
    fn traversal_and_separator_names_are_rejected() {
        // These would path-traverse onto an unrelated endpoint after URL
        // normalization, and a probe would read the resulting 200 as "resolved".
        for name in [
            "..",
            "../../summary",
            "clove/../../summary",
            "clove-sync-gitlab/versions",
            "a\\b",
        ] {
            assert!(
                validate_crate_name(name).is_err(),
                "{name} must be rejected"
            );
        }
    }

    #[test]
    fn flag_shaped_names_are_rejected() {
        // The sharpest case: anything reaching a subprocess argv must not be
        // parsable as a flag.
        for name in [
            "-rf",
            "--upload-pack=/bin/sh",
            "--template=/tmp/evil",
            "--config=source.crates-io.replace-with=\"evil\"",
        ] {
            assert!(
                validate_crate_name(name).is_err(),
                "{name} must be rejected — it would be read as a flag"
            );
        }
    }

    #[test]
    fn empty_and_overlong_names_are_rejected() {
        assert!(validate_crate_name("").is_err());
        assert!(validate_crate_name(&"a".repeat(MAX_CRATE_NAME)).is_ok());
        assert!(validate_crate_name(&"a".repeat(MAX_CRATE_NAME + 1)).is_err());
    }

    #[test]
    fn query_and_fragment_characters_are_rejected() {
        for name in ["clove?x=1", "clove#frag", "clove%2e%2e", "clove name"] {
            assert!(
                validate_crate_name(name).is_err(),
                "{name} must be rejected"
            );
        }
    }

    #[test]
    fn fetch_error_display_names_the_actionable_cause() {
        let rate_limited = FetchError::Status {
            code: 429,
            retry_after: Some(Duration::from_secs(30)),
            body: None,
        };
        assert!(rate_limited.to_string().contains("rate limit"));
        assert!(rate_limited.is_retryable_status());
        assert_eq!(rate_limited.retry_after(), Some(Duration::from_secs(30)));

        // A 403 is the missing-User-Agent case, and is *not* worth retrying.
        let forbidden = FetchError::Status {
            code: 403,
            retry_after: None,
            body: None,
        };
        assert!(forbidden.to_string().contains("User-Agent"));
        assert!(!forbidden.is_retryable_status());

        // A transport failure has already spent the request timeout; retrying it
        // spends that budget again, which is what the short timeout exists to
        // avoid. Only a server that answered quickly and asked us to come back
        // is retried.
        assert!(!FetchError::Transport("dns".to_owned()).is_retryable_status());
        assert!(!FetchError::Decode("bad json".to_owned()).is_retryable_status());
        assert!(FetchError::Status {
            code: 503,
            retry_after: None,
            body: None
        }
        .is_retryable_status());
    }

    #[test]
    fn fully_yanked_crate_is_representable() {
        // `version: String` + `yanked: bool` could not express this: if `version`
        // is non-yanked by construction the flag is dead, and a crate whose every
        // release was pulled has no value to put there.
        let plugin = RegistryPlugin {
            crate_name: "clove-sync-gone".to_owned(),
            latest: None,
            latest_yanked: Some(semver::Version::new(0, 3, 0)),
            description: None,
            repository: None,
            bin_names: vec![],
            published_by: None,
            downloads: 0,
        };
        assert!(plugin.fully_yanked());
        assert_eq!(plugin.latest.as_ref(), None);
        assert_eq!(plugin.display_version().as_deref(), Some("0.3.0"));
    }
}
