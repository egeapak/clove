//! The real HTTP transport: the **only** module that names `ureq`.
//!
//! Everything else in [`super`] depends on the [`Fetch`] trait, so this file is
//! the whole network surface of the host binary.

use std::time::Duration;

use super::{Fetch, FetchError};

/// How long a single request may take before it is abandoned.
///
/// Kept short deliberately. Discovery is never on a critical path — every caller
/// degrades to a warning — but the *name probe* fires up to four requests in
/// sequence, after pagination. Against a black-holing firewall (packets dropped,
/// no RST) a generous per-request timeout multiplies into a minute-plus of an
/// apparently hung command for what is an optional lookup. A connection that has
/// not answered in this long is not going to.
const TIMEOUT: Duration = Duration::from_secs(8);

/// A cap on the response body clove will read. `reverse_dependencies` pages are
/// a few tens of KB; this only stops a hostile or misconfigured endpoint from
/// streaming unboundedly into memory.
const MAX_BODY: u64 = 8 * 1024 * 1024;

/// The blocking `ureq`-backed transport.
///
/// **Blocking/sync on purpose** — correct for a CLI, and the reason [`super`]
/// must never be called from `cloved` or `clove-web`, which are tokio/axum.
pub struct UreqFetch {
    agent: ureq::Agent,
    user_agent: String,
}

impl Default for UreqFetch {
    fn default() -> Self {
        Self::new()
    }
}

impl UreqFetch {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            // Keep non-2xx responses as *values* rather than errors: a 404 is
            // meaningful data ("this crate does not exist") and ureq otherwise
            // discards the body along with the status.
            .http_status_as_error(false)
            .build();
        UreqFetch {
            agent: config.into(),
            // crates.io **requires** a User-Agent: anonymous requests get a 403
            // for every crate, including ones that plainly exist, which reads as
            // "everything is taken" rather than "you forgot a header".
            user_agent: format!(
                "clove/{} (+https://github.com/egeapak/clove)",
                env!("CARGO_PKG_VERSION")
            ),
        }
    }
}

/// How many times a retryable failure is re-attempted before giving up.
const MAX_RETRIES: u32 = 2;

/// Ceiling on an honored `Retry-After`. crates.io can ask for a long wait; a CLI
/// must not silently block for it, so a longer request is treated as "come back
/// later" and reported rather than slept through.
const MAX_BACKOFF: Duration = Duration::from_secs(5);

impl Fetch for UreqFetch {
    /// Issue the request, retrying a *transient* failure with the server's own
    /// backoff where it supplied one.
    ///
    /// crates.io rate-limits with `429` + `Retry-After`, and the name probe issues
    /// several sequential requests, so a single 429 would otherwise turn a working
    /// lookup into "the registry is unavailable". A 403 (the missing-User-Agent
    /// case) and a decode failure are *not* retryable — repeating them just wastes
    /// the user's time.
    fn get(&self, url: &str) -> Result<Option<String>, FetchError> {
        let mut attempt = 0;
        loop {
            match self.get_once(url) {
                Err(error) if error.is_retryable() && attempt < MAX_RETRIES => {
                    let backoff = error
                        .retry_after()
                        .unwrap_or_else(|| Duration::from_millis(250 * (1 << attempt)));
                    if backoff > MAX_BACKOFF {
                        return Err(error);
                    }
                    std::thread::sleep(backoff);
                    attempt += 1;
                }
                other => return other,
            }
        }
    }
}

impl UreqFetch {
    /// A single attempt, with no retry.
    fn get_once(&self, url: &str) -> Result<Option<String>, FetchError> {
        let response = self
            .agent
            .get(url)
            .header("User-Agent", &self.user_agent)
            .header("Accept", "application/json")
            .call()
            .map_err(|e| FetchError::Transport(tls_hint(&e.to_string())))?;

        let status = response.status().as_u16();
        if status == 404 {
            return Ok(None);
        }

        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);

        let body = response
            .into_body()
            .with_config()
            .limit(MAX_BODY)
            .read_to_string()
            .map_err(|e| FetchError::Decode(e.to_string()))?;

        if !(200..300).contains(&status) {
            return Err(FetchError::Status {
                code: status,
                retry_after,
                body: Some(body),
            });
        }
        Ok(Some(body))
    }
}

/// Add an actionable hint to a transport error that looks like a TLS failure.
///
/// clove verifies against **bundled** Mozilla roots (`webpki-roots`), which
/// deliberately ignore `SSL_CERT_FILE`, `SSL_CERT_DIR` and the platform trust
/// store. In an environment with a TLS-intercepting egress proxy that produces
/// an opaque handshake error here while `cargo`, `git` and `curl` all keep
/// working — so the message has to name the cause, or the caller's graceful
/// degradation ("registry unavailable") silently swallows a misconfigured or
/// intercepting proxy as an ordinary outage.
fn tls_hint(message: &str) -> String {
    let looks_like_tls = [
        "certificate",
        "tls",
        "handshake",
        "self-signed",
        "unknown ca",
    ]
    .iter()
    .any(|needle| message.to_ascii_lowercase().contains(needle));

    if looks_like_tls {
        format!(
            "{message} (clove verifies crates.io against bundled Mozilla roots, so a \
             TLS-intercepting proxy's CA is not trusted even when $SSL_CERT_FILE or the \
             system store contains it)"
        )
    } else {
        message.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_is_sent_and_identifies_clove() {
        // crates.io 403s every anonymous request, so an absent or generic
        // User-Agent turns "does this crate exist?" into "everything is taken".
        let fetch = UreqFetch::new();
        assert!(fetch.user_agent.starts_with("clove/"));
        assert!(fetch.user_agent.contains("github.com/egeapak/clove"));
    }

    #[test]
    fn tls_failures_get_an_actionable_hint() {
        let hinted = tls_hint("invalid peer certificate: UnknownIssuer");
        assert!(hinted.contains("bundled Mozilla roots"));
        assert!(hinted.contains("SSL_CERT_FILE"));

        // A non-TLS message is passed through untouched.
        assert_eq!(tls_hint("connection refused"), "connection refused");
    }

    /// A live check against real crates.io. Ignored by default — the offline
    /// fixture tests are the real coverage; run manually with
    /// `cargo test -p clove-cli --bin clove -- --ignored live_`.
    #[test]
    #[ignore = "hits the live crates.io API"]
    fn live_crates_io_probe_distinguishes_present_from_absent() {
        let fetch = UreqFetch::new();
        let client = super::super::crates_io::CratesIo::new(&fetch);

        let present = client.crate_exists("serde").expect("request ok");
        assert!(present.is_some(), "serde must exist on crates.io");

        let absent = client
            .crate_exists("clove-sync-gitlab-definitely-not-published")
            .expect("request ok");
        assert!(
            absent.is_none(),
            "an unpublished crate must probe as absent"
        );
    }
}
