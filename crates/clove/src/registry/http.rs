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

/// Environment variables naming an additional CA bundle, in the order the rest
/// of the Rust/`curl` ecosystem checks them.
///
/// `cargo` honours `CARGO_HTTP_CAINFO`; `curl`, `git` and OpenSSL honour
/// `SSL_CERT_FILE`. In a TLS-intercepting environment — a corporate egress
/// proxy, or the agent proxy this repository is developed behind — those are set
/// and everything else works, while `clove plugin list --all` returns an opaque
/// failure that degrades to a warning and `plugin install` fails outright. That
/// makes the feature look broken in exactly the environments least able to
/// diagnose it.
const CA_BUNDLE_VARS: [&str; 3] = ["CLOVE_CAINFO", "CARGO_HTTP_CAINFO", "SSL_CERT_FILE"];

/// The roots to verify against: the bundled set, plus any bundle the environment
/// names.
///
/// Additive, never a replacement: a bundle that fails to parse, or a variable
/// pointing at nothing, must not quietly *narrow* the trust set — that would
/// turn a misconfiguration into "nothing verifies" rather than a clear failure.
fn root_certificates() -> ureq::tls::RootCerts {
    let bundle = CA_BUNDLE_VARS
        .iter()
        .find_map(std::env::var_os)
        .map(std::path::PathBuf::from);
    root_certificates_from(bundle.as_deref())
}

/// [`root_certificates`] with the bundle path supplied, so the additive property
/// is testable without mutating this process's environment.
fn root_certificates_from(bundle: Option<&std::path::Path>) -> ureq::tls::RootCerts {
    let mut roots: Vec<ureq::tls::Certificate<'static>> = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .map(|der| ureq::tls::Certificate::from_der(der.as_ref()).to_owned())
        .collect();

    if let Some(pem) = bundle.and_then(|path| std::fs::read(path).ok()) {
        for item in ureq::tls::parse_pem(&pem).flatten() {
            if let ureq::tls::PemItem::Certificate(cert) = item {
                roots.push(cert);
            }
        }
    }

    ureq::tls::RootCerts::Specific(std::sync::Arc::new(roots))
}

impl UreqFetch {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(root_certificates())
                    .build(),
            )
            .timeout_global(Some(TIMEOUT))
            // crates.io needs no redirects at all. A permissive limit only widens
            // what a hostile or misconfigured registry root can reach: ureq applies
            // no scheme or host policy to a `Location`, so an https->http downgrade
            // or a hop to a link-local address would be followed. Nothing is echoed
            // back to the user (the response body reaches only serde_json, and
            // `FetchError`'s Display prints the status alone), so this is blind at
            // worst — but there is no reason to allow ten hops.
            .max_redirects(0)
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
                // A *transport* failure is not retried: `TIMEOUT` was chosen short
                // precisely because the probe issues several requests in sequence,
                // and retrying a timeout spends that budget again — three 8s
                // timeouts per request turns an optional lookup into ~50s of
                // apparent hang. A 429/5xx is different: the server answered, fast,
                // and told us to come back.
                Err(error) if error.is_retryable_status() && attempt < MAX_RETRIES => {
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

    #[test]
    fn an_environment_ca_bundle_adds_to_the_bundled_roots_and_never_narrows_them() {
        // The property that matters. A bundle that fails to parse, or a variable
        // pointing at nothing, must not quietly *narrow* the trust set — that
        // turns a misconfiguration into "nothing verifies at all", which is both
        // a worse failure and a much harder one to diagnose than a plain
        // certificate error.
        let count = |roots: &ureq::tls::RootCerts| match roots {
            ureq::tls::RootCerts::Specific(certs) => certs.len(),
            _ => panic!("expected an explicit root set"),
        };

        let baseline = count(&root_certificates_from(None));
        assert!(baseline > 0, "the bundled roots must be present");

        let dir = tempfile::tempdir().unwrap();

        // A variable naming a file that does not exist is ignored, not fatal.
        let missing = dir.path().join("nope.pem");
        assert_eq!(count(&root_certificates_from(Some(&missing))), baseline);

        // A file that is not PEM at all contributes nothing, and takes nothing.
        let garbage = dir.path().join("garbage.pem");
        std::fs::write(&garbage, b"this is not a certificate").unwrap();
        assert_eq!(
            count(&root_certificates_from(Some(&garbage))),
            baseline,
            "an unparseable bundle must not reduce the trust set"
        );

        // A real bundle is *added* to the built-in roots, not swapped for them —
        // so a corporate CA does not cost you the public ones.
        let extra = dir.path().join("extra.pem");
        std::fs::write(&extra, SELF_SIGNED_PEM).unwrap();
        assert_eq!(
            count(&root_certificates_from(Some(&extra))),
            baseline + 1,
            "an environment CA must extend the trust set"
        );
    }

    /// A throwaway self-signed certificate, only ever parsed — never trusted by
    /// anything but this test's arithmetic.
    const SELF_SIGNED_PEM: &str = "-----BEGIN CERTIFICATE-----\n\
MIIBhDCCASugAwIBAgIUbN3reOdgv7zCeE6MSKbUbu/ux0IwCgYIKoZIzj0EAwIw\n\
GDEWMBQGA1UEAwwNY2xvdmUtdGVzdC1jYTAeFw0yNjA4MjgxMDM0MTFaFw0zNjA4\n\
MjUxMDM0MTFaMBgxFjAUBgNVBAMMDWNsb3ZlLXRlc3QtY2EwWTATBgcqhkjOPQIB\n\
BggqhkjOPQMBBwNCAAQ019MLglYI2yKiibKXEg8N7xCpOAQN0kxIcdu9+EOvcyzi\n\
YnS5vtMW4IEnvDJHoYTLuI9tNlbZJ1H3vybg04+lo1MwUTAdBgNVHQ4EFgQUiyh/\n\
t+D4Lb7f3rGDhzEmbLHnShEwHwYDVR0jBBgwFoAUiyh/t+D4Lb7f3rGDhzEmbLHn\n\
ShEwDwYDVR0TAQH/BAUwAwEB/zAKBggqhkjOPQQDAgNHADBEAiA2C9G6KQ9ExCVz\n\
C4UV47ZnXdr8UQNzJLnRZWNCU44+iQIgJGPGcxqHPKOvdDP/uZg/N0UUTO1rS0LC\n\
X5fIM+2FHJc=\n\
-----END CERTIFICATE-----\n";
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
