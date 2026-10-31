//! HTTP(S) proxy support for the GitHub client (`github` feature).
//!
//! octocrab's default client (a `hyper-util` legacy client over `hyper-rustls`)
//! connects straight to the API host and never reads the proxy environment. So a
//! user behind a company proxy could not sync. This module reads the proxy
//! environment and, when a proxy applies to the API host, opens the
//! connection through an HTTP `CONNECT` tunnel. TLS to the API host then runs
//! inside the tunnel, so the proxy sees only the host name and port.
//!
//! The variables (for each pair the first non-empty one wins, uppercase first,
//! as in hyper-util's `Matcher::from_env`; curl prefers the lowercase name):
//!
//! - `HTTPS_PROXY` / `https_proxy` — the proxy for an `https://` API base;
//! - `HTTP_PROXY` / `http_proxy` — the proxy for an `http://` API base;
//! - `ALL_PROXY` / `all_proxy` — the fallback when the scheme's own pair is
//!   unset;
//! - `NO_PROXY` / `no_proxy` — hosts that bypass the proxy (curl rules:
//!   comma-separated domains, which also match their subdomains, IP addresses,
//!   CIDR ranges, or `*` for every host).
//!
//! Only the variable that applies to the API base is used and checked: an
//! `HTTP_PROXY` meant for other tools does not matter for an `https://` base,
//! and nothing matters for a host in `NO_PROXY`.
//!
//! A proxy URL is `http://[user:password@]host[:port]` (the scheme may be
//! omitted; the port defaults to 80; percent-encode `@`, `:` and `/` in the
//! user name or password). Credentials are sent as
//! `Proxy-Authorization: Basic …` on the `CONNECT` request. A URL that does not
//! parse, or any other scheme (`https://`, `socks5://`, …), is a setup error,
//! never a silent direct connection. The check is the matcher's own parse, so
//! what is checked is exactly what is used.
//!
//! An `http://` API base is tunnelled with `CONNECT` too (to port 80), not
//! forwarded in absolute form. A proxy that allows `CONNECT` only to TLS ports
//! refuses it; GitHub's own API is `https://`, so this affects only a plain-HTTP
//! GitHub Enterprise.
//!
//! Certificate trust does not change: the TLS roots are the platform's native
//! roots (`rustls-native-certs`), which honour `SSL_CERT_FILE` /
//! `SSL_CERT_DIR`. A proxy that re-terminates TLS works once its CA is in the
//! system store or in the file `SSL_CERT_FILE` names. Verification is never
//! disabled.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use http::Uri;
use hyper_util::client::legacy::connect::proxy::Tunnel;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::proxy::matcher::Matcher;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tower_service::Service;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The proxy variables, each with its lowercase alias, in lookup order.
const HTTPS_VARS: [&str; 2] = ["HTTPS_PROXY", "https_proxy"];
const HTTP_VARS: [&str; 2] = ["HTTP_PROXY", "http_proxy"];
const ALL_VARS: [&str; 2] = ["ALL_PROXY", "all_proxy"];
const NO_VARS: [&str; 2] = ["NO_PROXY", "no_proxy"];

/// The resolved proxy settings for one API base: the proxy that serves it,
/// and the hosts that bypass it.
pub(crate) struct ProxyConfig {
    matcher: Matcher,
}

/// The expected shape, for error messages.
const PROXY_URL_SHAPE: &str = "expected `http://[user:password@]host[:port]`";

impl ProxyConfig {
    /// The proxy settings that apply to `dst` (the API base), from the process
    /// environment.
    ///
    /// `Ok(None)` means no proxy applies (no variable for its scheme, or its
    /// host is in `NO_PROXY`), so the caller keeps the direct client. `Err`
    /// names the variable whose URL is not usable.
    pub(crate) fn for_destination(dst: &Uri) -> Result<Option<Self>, String> {
        Self::for_destination_with(|key| std::env::var(key).ok(), dst)
    }

    /// [`for_destination`](Self::for_destination) over an injected lookup
    /// (unit tests).
    pub(crate) fn for_destination_with(
        lookup: impl Fn(&str) -> Option<String>,
        dst: &Uri,
    ) -> Result<Option<Self>, String> {
        let scheme_vars: &[&str] = if dst.scheme_str() == Some("http") {
            &HTTP_VARS
        } else {
            &HTTPS_VARS
        };
        let Some((key, val)) =
            first_set(&lookup, scheme_vars).or_else(|| first_set(&lookup, &ALL_VARS))
        else {
            return Ok(None);
        };
        let no = first_set(&lookup, &NO_VARS)
            .map(|(_, list)| list)
            .unwrap_or_default();
        // A bypassed host never uses the proxy, so its URL does not matter.
        let probe = Matcher::builder()
            .all("http://proxy.invalid")
            .no(no.as_str())
            .build();
        if probe.intercept(dst).is_none() {
            return Ok(None);
        }
        let matcher = Matcher::builder().all(val).no(no).build();
        // The matcher drops a URL it cannot parse; that must not turn into a
        // silent direct connection. The message never echoes the value, which
        // may carry a password.
        let intercept = matcher.intercept(dst).ok_or_else(|| {
            format!(
                "invalid proxy URL in {key}: it does not parse; {PROXY_URL_SHAPE} \
                 (percent-encode `@`, `:` and `/` in the user name or password)"
            )
        })?;
        let proxy = intercept.uri();
        // The matcher splits the credentials at the first `@`; an unencoded `@`
        // in the password leaves the rest in the authority and drops the
        // credentials.
        if proxy.authority().is_some_and(|a| a.as_str().contains('@')) {
            return Err(format!(
                "invalid proxy URL in {key}: it does not parse; {PROXY_URL_SHAPE} \
                 (percent-encode `@`, `:` and `/` in the user name or password)"
            ));
        }
        if proxy.scheme_str() != Some("http") {
            let host = dst.host().unwrap_or_default();
            return Err(format!(
                "invalid proxy URL in {key}: the `{}` scheme is not supported; use an \
                 `http://` proxy URL (clove opens an HTTP CONNECT tunnel through it), \
                 or add `{host}` to NO_PROXY to connect directly",
                proxy.scheme_str().unwrap_or_default()
            ));
        }
        if proxy.host().is_none_or(str::is_empty) {
            return Err(format!(
                "invalid proxy URL in {key}: no host; {PROXY_URL_SHAPE}"
            ));
        }
        Ok(Some(Self { matcher }))
    }

    #[cfg(test)]
    /// The proxy URL that serves `dst`, or `None` for a direct connection.
    pub(crate) fn proxy_for(&self, dst: &Uri) -> Option<Uri> {
        self.matcher
            .intercept(dst)
            .map(|intercept| intercept.uri().clone())
    }
}

/// The first non-empty variable of `keys`: its name and trimmed value.
fn first_set(
    lookup: &impl Fn(&str) -> Option<String>,
    keys: &[&'static str],
) -> Option<(&'static str, String)> {
    keys.iter().find_map(|key| {
        let val = lookup(key)?;
        let val = val.trim();
        (!val.is_empty()).then(|| (*key, val.to_owned()))
    })
}

/// A connector that opens a plain TCP connection to the destination, or to the
/// proxy with an HTTP `CONNECT` tunnel to the destination when [`ProxyConfig`]
/// names one. `hyper-rustls` wraps it, so TLS runs inside the tunnel.
#[derive(Clone)]
pub(crate) struct ProxyConnector {
    http: HttpConnector,
    config: Arc<ProxyConfig>,
}

impl ProxyConnector {
    pub(crate) fn new(config: ProxyConfig) -> Self {
        let mut http = HttpConnector::new();
        // hyper-rustls hands `https://` destinations to this connector; it does
        // the TLS itself, so the TCP layer must accept them.
        http.enforce_http(false);
        Self {
            http,
            config: Arc::new(config),
        }
    }
}

impl Service<Uri> for ProxyConnector {
    type Response = TokioIo<TcpStream>;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.http.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let Some(intercept) = self.config.matcher.intercept(&dst) else {
            let connecting = self.http.call(dst);
            return Box::pin(async move { connecting.await.map_err(Into::into) });
        };
        let proxy = intercept.uri().clone();
        let mut tunnel = Tunnel::new(proxy.clone(), self.http.clone());
        let has_auth = intercept.basic_auth().is_some();
        if let Some(auth) = intercept.basic_auth() {
            tunnel = tunnel.with_auth(auth.clone());
        }
        // `Tunnel` defaults a missing port to 443; an `http://` base without a
        // port means 80, so name the port explicitly.
        let target = with_explicit_port(&dst);
        Box::pin(async move {
            let target = target?;
            let mut tunnel = tunnel;
            tunnel
                .call(target)
                .await
                .map_err(|err| BoxError::from(describe_tunnel_error(&proxy, has_auth, &err)))
        })
    }
}

/// `dst` with its port spelled out (80 for `http`, 443 otherwise).
fn with_explicit_port(dst: &Uri) -> Result<Uri, BoxError> {
    if dst.port().is_some() {
        return Ok(dst.clone());
    }
    let host = dst.host().ok_or("destination URL has no host")?;
    let port = if dst.scheme_str() == Some("http") {
        80
    } else {
        443
    };
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let scheme = dst.scheme_str().unwrap_or("https");
    Ok(format!("{scheme}://{host}:{port}/").parse::<Uri>()?)
}

/// One line on why the `CONNECT` tunnel through `proxy` failed: the error and
/// its source chain. `TunnelError`'s own `Display` is terse ("tunnel error:
/// unsuccessful"), so name the proxy. `proxy` carries no credentials (the
/// matcher strips them from the URL). `has_auth` says whether credentials
/// were sent, which decides the hint on a 407.
fn describe_tunnel_error(
    proxy: &Uri,
    has_auth: bool,
    err: &(dyn std::error::Error + 'static),
) -> String {
    let mut text = format!("proxy CONNECT through {proxy} failed: {err}");
    let mut cause = err.source();
    while let Some(inner) = cause {
        text.push_str(": ");
        text.push_str(&inner.to_string());
        cause = inner.source();
    }
    if text.contains("proxy authorization required") {
        text.push_str(if has_auth {
            " (the proxy refused the credentials in the proxy URL)"
        } else {
            " (put the credentials in the proxy URL: `http://user:password@host:port`)"
        });
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const GITHUB: &str = "https://api.github.com";

    fn config(vars: &[(&str, &str)], base: &str) -> Result<Option<ProxyConfig>, String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        ProxyConfig::for_destination_with(|key| map.get(key).cloned(), &base.parse().unwrap())
    }

    /// The proxy that serves `base`, as a string (`None`: direct).
    fn proxy(vars: &[(&str, &str)], base: &str) -> Option<String> {
        config(vars, base)
            .unwrap()
            .and_then(|cfg| cfg.proxy_for(&base.parse().unwrap()))
            .map(|u| u.to_string())
    }

    #[test]
    fn no_proxy_variable_keeps_the_direct_client() {
        assert!(config(&[], GITHUB).unwrap().is_none());
        assert!(config(&[("HTTPS_PROXY", "  ")], GITHUB).unwrap().is_none());
        // NO_PROXY alone configures no proxy.
        assert!(config(&[("NO_PROXY", "example.com")], GITHUB)
            .unwrap()
            .is_none());
    }

    #[test]
    fn the_base_scheme_picks_the_variable() {
        let vars = [
            ("HTTPS_PROXY", "http://secure.proxy:3128"),
            ("HTTP_PROXY", "http://plain.proxy:8080"),
        ];
        assert_eq!(
            proxy(&vars, GITHUB),
            Some("http://secure.proxy:3128/".to_owned())
        );
        assert_eq!(
            proxy(&vars, "http://ghe.local/api/v3"),
            Some("http://plain.proxy:8080/".to_owned())
        );
        // An https_proxy does not serve an http:// base.
        assert_eq!(
            proxy(&[("https_proxy", "http://p:1")], "http://ghe.local/"),
            None
        );
    }

    #[test]
    fn lowercase_names_and_all_proxy_are_read() {
        assert_eq!(
            proxy(&[("https_proxy", "proxy.corp:3128")], GITHUB),
            Some("http://proxy.corp:3128/".to_owned())
        );
        for base in ["http://ghe.local/", "https://ghe.local/"] {
            assert_eq!(
                proxy(&[("all_proxy", "http://all.proxy:1")], base),
                Some("http://all.proxy:1/".to_owned()),
                "{base}"
            );
        }
        // The scheme's own variable wins over ALL_PROXY.
        assert_eq!(
            proxy(
                &[
                    ("ALL_PROXY", "http://all:1"),
                    ("HTTPS_PROXY", "http://own:2")
                ],
                GITHUB
            ),
            Some("http://own:2/".to_owned())
        );
    }

    #[test]
    fn uppercase_wins_over_lowercase() {
        assert_eq!(
            proxy(
                &[
                    ("HTTPS_PROXY", "http://upper:1"),
                    ("https_proxy", "http://lower:2")
                ],
                GITHUB
            ),
            Some("http://upper:1/".to_owned())
        );
    }

    #[test]
    fn no_proxy_hosts_bypass_the_proxy() {
        for (key, list) in [
            ("NO_PROXY", "github.com"),
            ("no_proxy", ".github.com, other.org"),
            ("NO_PROXY", "*"),
        ] {
            let vars = [("HTTPS_PROXY", "http://p:3128"), (key, list)];
            assert!(config(&vars, GITHUB).unwrap().is_none(), "{key}={list}");
        }
        let vars = [
            ("HTTPS_PROXY", "http://p:3128"),
            ("NO_PROXY", "10.0.0.0/8,notgithub.com"),
        ];
        assert!(config(&vars, "https://10.1.2.3/").unwrap().is_none());
        assert_eq!(proxy(&vars, GITHUB), Some("http://p:3128/".to_owned()));
    }

    #[test]
    fn a_variable_that_does_not_apply_is_not_checked() {
        // HTTP_PROXY is for http:// bases; a socks URL there is someone else's.
        assert!(config(&[("HTTP_PROXY", "socks5://p:1080")], GITHUB)
            .unwrap()
            .is_none());
        // ALL_PROXY is shadowed by the scheme's own variable.
        assert_eq!(
            proxy(
                &[
                    ("ALL_PROXY", "socks5://p:1080"),
                    ("HTTPS_PROXY", "http://own:2")
                ],
                GITHUB
            ),
            Some("http://own:2/".to_owned())
        );
        // A bypassed host never uses the proxy, whatever its URL.
        assert!(config(
            &[
                ("HTTPS_PROXY", "socks5://p:1080"),
                ("NO_PROXY", "api.github.com")
            ],
            GITHUB
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn an_unusable_proxy_url_names_the_variable() {
        for (key, val, needle) in [
            (
                "HTTPS_PROXY",
                "socks5://p:1080",
                "`socks5` scheme is not supported",
            ),
            (
                "https_proxy",
                "https://p:443",
                "`https` scheme is not supported",
            ),
            (
                "ALL_PROXY",
                "socks5h://user:secret@p:1080",
                "scheme is not supported",
            ),
            ("HTTPS_PROXY", "http://", "does not parse"),
            ("HTTPS_PROXY", "not a url", "does not parse"),
            // An unencoded `@` in the password: the matcher cannot use it, so
            // it must not fall through to a direct connection.
            (
                "HTTPS_PROXY",
                "http://user:p@ss@proxy:3128",
                "does not parse",
            ),
        ] {
            let Err(err) = config(&[(key, val)], GITHUB) else {
                panic!("{key}={val} must be rejected");
            };
            assert!(
                err.starts_with(&format!("invalid proxy URL in {key}: ")),
                "{err}"
            );
            assert!(err.contains(needle), "{key}={val}: {err}");
            assert!(!err.contains("secret") && !err.contains("p@ss"), "{err}");
        }
        let Err(err) = config(&[("HTTPS_PROXY", "socks5://p:1080")], GITHUB) else {
            panic!("socks5 must be rejected");
        };
        assert!(err.contains("add `api.github.com` to NO_PROXY"), "{err}");
    }

    #[test]
    fn a_407_hint_depends_on_whether_credentials_were_sent() {
        let proxy: Uri = "http://p:3128/".parse().unwrap();
        let err = std::io::Error::other("tunnel error: proxy authorization required");
        assert!(describe_tunnel_error(&proxy, false, &err).contains("put the credentials"));
        assert!(describe_tunnel_error(&proxy, true, &err).contains("refused the credentials"));
    }

    #[test]
    fn the_tunnel_target_names_its_port() {
        let port = |u: &str| with_explicit_port(&u.parse().unwrap()).unwrap().to_string();
        assert_eq!(port("http://ghe.local/api"), "http://ghe.local:80/");
        assert_eq!(
            port("https://api.github.com"),
            "https://api.github.com:443/"
        );
        assert_eq!(port("https://h:8443/x"), "https://h:8443/x");
    }
}
