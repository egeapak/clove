//! HTTP(S) proxy support for the GitHub client (`github` feature).
//!
//! octocrab's default client (a `hyper-util` legacy client over `hyper-rustls`)
//! connects straight to the API host and never reads the proxy environment. So a
//! user behind a company proxy could not sync. This module reads the proxy
//! environment the way curl does and, when a proxy applies, opens the
//! connection through an HTTP `CONNECT` tunnel. TLS to the API host then runs
//! inside the tunnel, so the proxy sees only the host name and port.
//!
//! The variables, in precedence order (the first non-empty one wins):
//!
//! - `HTTPS_PROXY` / `https_proxy` — the proxy for an `https://` API base;
//! - `HTTP_PROXY` / `http_proxy` — the proxy for an `http://` API base;
//! - `ALL_PROXY` / `all_proxy` — the fallback for both;
//! - `NO_PROXY` / `no_proxy` — hosts that bypass the proxy (curl rules:
//!   comma-separated domains, which also match their subdomains, IP addresses,
//!   CIDR ranges, or `*` for every host).
//!
//! A proxy URL is `http://[user:password@]host[:port]` (the scheme may be
//! omitted; the port defaults to 80). Credentials are sent as
//! `Proxy-Authorization: Basic …` on the `CONNECT` request. Any other scheme
//! (`https://`, `socks5://`, …) is a setup error, not a silent direct
//! connection.
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

/// The resolved proxy settings: which proxy serves each scheme, and which hosts
/// bypass it.
pub(crate) struct ProxyConfig {
    matcher: Matcher,
}

impl ProxyConfig {
    /// Read the proxy settings from the process environment.
    ///
    /// `Ok(None)` means no proxy variable is set, so the caller keeps the
    /// direct client. `Err` names the variable whose URL is not usable.
    pub(crate) fn from_env() -> Result<Option<Self>, String> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// [`from_env`](Self::from_env) over an injected lookup (unit tests).
    pub(crate) fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, String> {
        let https = first_set(&lookup, &HTTPS_VARS)?;
        let http = first_set(&lookup, &HTTP_VARS)?;
        let all = first_set(&lookup, &ALL_VARS)?;
        if https.is_none() && http.is_none() && all.is_none() {
            return Ok(None);
        }
        let no = NO_VARS
            .iter()
            .find_map(|key| lookup(key).filter(|val| !val.trim().is_empty()))
            .unwrap_or_default();
        let matcher = Matcher::builder()
            .https(https.unwrap_or_default())
            .http(http.unwrap_or_default())
            .all(all.unwrap_or_default())
            .no(no)
            .build();
        Ok(Some(Self { matcher }))
    }

    #[cfg(test)]
    /// The proxy URL that serves `dst`, or `None` for a direct connection
    /// (no proxy for that scheme, or the host is in `NO_PROXY`).
    pub(crate) fn proxy_for(&self, dst: &Uri) -> Option<Uri> {
        self.matcher
            .intercept(dst)
            .map(|intercept| intercept.uri().clone())
    }
}

/// The first non-empty variable of `keys`, validated as a proxy URL.
fn first_set(
    lookup: &impl Fn(&str) -> Option<String>,
    keys: &[&str],
) -> Result<Option<String>, String> {
    for key in keys {
        let Some(val) = lookup(key) else { continue };
        let val = val.trim();
        if val.is_empty() {
            continue;
        }
        validate_proxy_url(val).map_err(|why| format!("invalid proxy URL in {key}: {why}"))?;
        return Ok(Some(val.to_owned()));
    }
    Ok(None)
}

/// Check that `val` is a proxy URL the `CONNECT` tunnel can use. The message
/// never echoes the value, which may carry a password.
fn validate_proxy_url(val: &str) -> Result<(), String> {
    let uri: Uri = val
        .parse()
        .map_err(|_| "not a URL; expected `http://[user:password@]host[:port]`".to_owned())?;
    match uri.scheme_str() {
        None | Some("http") => {}
        Some(other) => {
            return Err(format!(
                "the `{other}` scheme is not supported; use an `http://` proxy URL \
                 (clove opens an HTTP CONNECT tunnel through it)"
            ))
        }
    }
    let authority = uri
        .authority()
        .ok_or_else(|| "no host; expected `http://[user:password@]host[:port]`".to_owned())?;
    let host_port = authority
        .as_str()
        .rsplit_once('@')
        .map_or(authority.as_str(), |(_, rest)| rest);
    if host_port.is_empty() || host_port.starts_with(':') {
        return Err("no host; expected `http://[user:password@]host[:port]`".to_owned());
    }
    Ok(())
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
                .map_err(|err| BoxError::from(describe_tunnel_error(&proxy, &err)))
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
/// matcher strips them from the URL).
fn describe_tunnel_error(proxy: &Uri, err: &(dyn std::error::Error + 'static)) -> String {
    let mut text = format!("proxy CONNECT through {proxy} failed: {err}");
    let mut cause = err.source();
    while let Some(inner) = cause {
        text.push_str(": ");
        text.push_str(&inner.to_string());
        cause = inner.source();
    }
    if text.contains("proxy authorization required") {
        text.push_str(" (put the credentials in the proxy URL: `http://user:password@host:port`)");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(vars: &[(&str, &str)]) -> Result<Option<ProxyConfig>, String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        ProxyConfig::from_lookup(|key| map.get(key).cloned())
    }

    fn proxy(cfg: &ProxyConfig, url: &str) -> Option<String> {
        cfg.proxy_for(&url.parse().unwrap()).map(|u| u.to_string())
    }

    #[test]
    fn no_proxy_variable_keeps_the_direct_client() {
        assert!(config(&[]).unwrap().is_none());
        assert!(config(&[("HTTPS_PROXY", "  ")]).unwrap().is_none());
        // NO_PROXY alone configures no proxy.
        assert!(config(&[("NO_PROXY", "example.com")]).unwrap().is_none());
    }

    #[test]
    fn https_proxy_serves_https_and_http_proxy_serves_http() {
        let cfg = config(&[
            ("HTTPS_PROXY", "http://secure.proxy:3128"),
            ("HTTP_PROXY", "http://plain.proxy:8080"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            proxy(&cfg, "https://api.github.com/repos"),
            Some("http://secure.proxy:3128/".to_owned())
        );
        assert_eq!(
            proxy(&cfg, "http://ghe.local/api/v3"),
            Some("http://plain.proxy:8080/".to_owned())
        );
    }

    #[test]
    fn lowercase_names_and_all_proxy_are_read() {
        let cfg = config(&[("https_proxy", "proxy.corp:3128")])
            .unwrap()
            .unwrap();
        assert_eq!(
            proxy(&cfg, "https://api.github.com/"),
            Some("http://proxy.corp:3128/".to_owned())
        );
        // An https_proxy does not serve an http:// base.
        assert_eq!(proxy(&cfg, "http://api.github.com/"), None);

        let cfg = config(&[("ALL_PROXY", "http://all.proxy:1")])
            .unwrap()
            .unwrap();
        assert_eq!(
            proxy(&cfg, "http://ghe.local/"),
            Some("http://all.proxy:1/".to_owned())
        );
        assert_eq!(
            proxy(&cfg, "https://ghe.local/"),
            Some("http://all.proxy:1/".to_owned())
        );
    }

    #[test]
    fn uppercase_wins_over_lowercase() {
        let cfg = config(&[
            ("HTTPS_PROXY", "http://upper:1"),
            ("https_proxy", "http://lower:2"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            proxy(&cfg, "https://api.github.com/"),
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
            let cfg = config(&[("HTTPS_PROXY", "http://p:3128"), (key, list)])
                .unwrap()
                .unwrap();
            assert_eq!(proxy(&cfg, "https://api.github.com/"), None, "{key}={list}");
        }
        let cfg = config(&[
            ("HTTPS_PROXY", "http://p:3128"),
            ("NO_PROXY", "10.0.0.0/8,notgithub.com"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(proxy(&cfg, "https://10.1.2.3/"), None);
        assert_eq!(
            proxy(&cfg, "https://api.github.com/"),
            Some("http://p:3128/".to_owned())
        );
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
            ("HTTP_PROXY", "http://", "not a URL"),
            ("ALL_PROXY", "http://user:secret@:8080", "no host"),
            ("HTTPS_PROXY", "not a url", "not a URL"),
        ] {
            let Err(err) = config(&[(key, val)]) else {
                panic!("{key}={val} must be rejected");
            };
            assert!(
                err.starts_with(&format!("invalid proxy URL in {key}: ")),
                "{err}"
            );
            assert!(err.contains(needle), "{key}={val}: {err}");
            assert!(!err.contains("secret"), "the password must not leak: {err}");
        }
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
