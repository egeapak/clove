//! The access token of a web server (DESIGN §8.4).
//!
//! The web UI listens on a loopback TCP port, and a TCP port has no owner: any
//! local user (or any process of any user) can connect to it. The daemon's IPC
//! socket is guarded by a peer-uid check and a per-project token; without a
//! credential of its own the web port would hand the same reads and writes to
//! everyone on the machine.
//!
//! So each server has a random token. The URL the CLI prints carries it
//! (`…/?token=<hex>`); the first page load exchanges it for a `SameSite=Strict`,
//! `HttpOnly` cookie and redirects to the same URL without the token, so the
//! SPA, its API calls and its event socket need no change and the token does
//! not stay in the address bar. A script can send `Authorization: Bearer
//! <token>` or keep `?token=` on each request instead.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;

/// Random bytes in a token (hex-encoded to twice as many characters).
const TOKEN_BYTES: usize = 32;
/// The query parameter that carries the token in a printed URL.
const QUERY_KEY: &str = "token";
/// Cookie name prefix; the server's port follows, because cookies are shared
/// by every port of a host and two servers must not overwrite each other's.
const COOKIE_PREFIX: &str = "clove_web";

/// A web server's access token. Cheap to clone.
#[derive(Clone)]
pub struct WebAuth {
    token: Arc<str>,
}

impl WebAuth {
    /// A fresh random token.
    pub fn generate() -> std::io::Result<Self> {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::getrandom(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Ok(Self {
            token: token.into(),
        })
    }

    /// The token, for a client that sends it as `Authorization: Bearer`.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// `url` with the token attached: what the CLI prints and opens.
    pub fn url(&self, url: &str) -> String {
        let sep = if url.contains('?') { '&' } else { '?' };
        format!("{url}{sep}{QUERY_KEY}={}", self.token)
    }

    /// Refuse every request to `router` that does not present the token.
    pub fn protect<S>(&self, router: Router<S>) -> Router<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        router.layer(axum::middleware::from_fn_with_state(self.clone(), guard))
    }

    fn matches(&self, presented: &str) -> bool {
        clove_core::daemon_token::matches(&self.token, presented)
    }
}

async fn guard(State(auth): State<WebAuth>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    if bearer(headers).is_some_and(|t| auth.matches(t))
        || cookies(headers)
            .any(|(name, value)| name.starts_with(COOKIE_PREFIX) && auth.matches(value))
    {
        return next.run(request).await;
    }
    let query = request.uri().query().unwrap_or("");
    let presented = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == QUERY_KEY)
        .map(|(_, value)| value);
    if !presented.is_some_and(|t| auth.matches(t)) {
        return unauthorized();
    }
    let cookie = set_cookie(headers, &auth.token);
    let path = request.uri().path();
    let is_page = request.method() == Method::GET && !path.contains("/api/");
    let mut response = if is_page {
        // Take the token out of the address bar (and so out of history and
        // `Referer`): the cookie carries it from here on.
        let rest: Vec<&str> = query
            .split('&')
            .filter(|pair| !pair.is_empty() && pair.split('=').next() != Some(QUERY_KEY))
            .collect();
        let location = match rest.is_empty() {
            true => path.to_owned(),
            false => format!("{path}?{}", rest.join("&")),
        };
        match HeaderValue::from_str(&location) {
            Ok(location) => (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response(),
            Err(_) => next.run(request).await,
        }
    } else {
        next.run(request).await
    };
    if let Some(cookie) = cookie {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::WWW_AUTHENTICATE, "Bearer"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        "401: this clove web UI needs its access token.\n\
         \n\
         Open the URL that `clove serve` prints: it ends in `?token=...`. Run \
         `clove serve` again to print it (`clove serve --open` opens it). For a \
         project that the daemon serves, `clove daemon status` shows the same URL.\n\
         A script can send the token as `Authorization: Bearer <token>`.\n",
    )
        .into_response()
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

fn cookies(headers: &HeaderMap) -> impl Iterator<Item = (&str, &str)> {
    headers
        .get_all(header::COOKIE)
        .into_iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
}

/// The cookie for the server the request was sent to (named by its port).
fn set_cookie(headers: &HeaderMap, token: &str) -> Option<HeaderValue> {
    let port = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(|host| host.rsplit_once(':'))
        .map(|(_, port)| port)
        .filter(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()));
    let name = match port {
        Some(port) => format!("{COOKIE_PREFIX}_{port}"),
        None => COOKIE_PREFIX.to_owned(),
    };
    HeaderValue::from_str(&format!(
        "{name}={token}; Path=/; HttpOnly; SameSite=Strict"
    ))
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_long() {
        let (a, b) = (WebAuth::generate().unwrap(), WebAuth::generate().unwrap());
        assert_eq!(a.token().len(), TOKEN_BYTES * 2);
        assert_ne!(a.token(), b.token());
    }

    #[test]
    fn the_url_carries_the_token() {
        let auth = WebAuth {
            token: "abc".into(),
        };
        assert_eq!(
            auth.url("http://127.0.0.1:7373/"),
            "http://127.0.0.1:7373/?token=abc"
        );
        assert_eq!(auth.url("http://h/p/x/?a=1"), "http://h/p/x/?a=1&token=abc");
    }

    #[test]
    fn the_cookie_is_named_for_the_port_and_is_strict() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:7373"));
        let cookie = set_cookie(&headers, "abc").unwrap();
        assert_eq!(
            cookie.to_str().unwrap(),
            "clove_web_7373=abc; Path=/; HttpOnly; SameSite=Strict"
        );
    }
}
