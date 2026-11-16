//! GitHub rate-limit hints: how long GitHub asks a client to wait.
//!
//! A rate-limited response is a `403 Forbidden` or a `429 Too Many Requests`
//! that names the wait in its headers:
//!
//! - `Retry-After: <seconds>` (the secondary rate limit), or
//! - `x-ratelimit-remaining: 0` with `x-ratelimit-reset: <epoch seconds>`
//!   (the primary rate limit).
//!
//! octocrab's error keeps only the status and the JSON body, not the headers.
//! So [`RateLimitHints`] wraps the HTTP client, below octocrab, and records the
//! wait of the last rate-limited response. The retry loop in `sync_net` takes
//! it with [`take_wait`] when a call fails with 403 or 429. A 403 with no hint
//! is a permission error, and stays one.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use http::{HeaderMap, Request, Response, StatusCode};

/// The wait of the last rate-limited response, until a retry takes it.
static LAST_WAIT: Mutex<Option<Duration>> = Mutex::new(None);

/// The wait that `headers` of a `status` response ask for, measured from
/// `now`. `None` when the response is not a rate limit.
pub(crate) fn wait_from_headers(
    status: StatusCode,
    headers: &HeaderMap,
    now: SystemTime,
) -> Option<Duration> {
    if status != StatusCode::FORBIDDEN && status != StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    let number =
        |name: &str| -> Option<u64> { headers.get(name)?.to_str().ok()?.trim().parse().ok() };
    if let Some(seconds) = number("retry-after") {
        return Some(Duration::from_secs(seconds));
    }
    if number("x-ratelimit-remaining") == Some(0) {
        let reset = UNIX_EPOCH + Duration::from_secs(number("x-ratelimit-reset")?);
        // A reset already past still means "wait for the next window": one
        // second, which also covers a clock that runs a little ahead.
        let wait = reset.duration_since(now).unwrap_or_default();
        return Some(wait + Duration::from_secs(1));
    }
    None
}

/// Take the wait the last rate-limited response asked for, if any.
pub(crate) fn take_wait() -> Option<Duration> {
    LAST_WAIT.lock().ok()?.take()
}

fn record(status: StatusCode, headers: &HeaderMap) {
    if let Some(wait) = wait_from_headers(status, headers, SystemTime::now()) {
        if let Ok(mut last) = LAST_WAIT.lock() {
            *last = Some(wait);
        }
    }
}

/// The HTTP client wrapper that records the rate-limit hints of each response.
#[derive(Clone)]
pub(crate) struct RateLimitHints<S> {
    inner: S,
}

impl<S> RateLimitHints<S> {
    pub(crate) fn new(inner: S) -> Self {
        RateLimitHints { inner }
    }
}

impl<S, ReqBody, ResBody> tower_service::Service<Request<ReqBody>> for RateLimitHints<S>
where
    S: tower_service::Service<Request<ReqBody>, Response = Response<ResBody>>,
    S::Future: Send + 'static,
{
    type Response = Response<ResBody>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<ReqBody>) -> Self::Future {
        let response = self.inner.call(request);
        Box::pin(async move {
            let response = response.await?;
            record(response.status(), response.headers());
            Ok(response)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    }

    const NOW: Duration = Duration::from_secs(1_800_000_000);

    fn wait(status: u16, pairs: &[(&'static str, &str)]) -> Option<Duration> {
        let status = StatusCode::from_u16(status).unwrap();
        wait_from_headers(status, &headers(pairs), UNIX_EPOCH + NOW)
    }

    #[test]
    fn retry_after_names_the_wait() {
        assert_eq!(
            wait(429, &[("retry-after", "7")]),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            wait(403, &[("retry-after", "60")]),
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn an_exhausted_primary_limit_waits_for_its_reset() {
        let reset = (NOW.as_secs() + 30).to_string();
        let exhausted = [
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", reset.as_str()),
        ];
        assert_eq!(wait(403, &exhausted), Some(Duration::from_secs(31)));
        // A reset in the past waits one second.
        let past = (NOW.as_secs() - 5).to_string();
        let stale = [
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", past.as_str()),
        ];
        assert_eq!(wait(429, &stale), Some(Duration::from_secs(1)));
    }

    #[test]
    fn other_responses_are_not_rate_limits() {
        // A 403 with requests left is a permission error.
        let reset = (NOW.as_secs() + 30).to_string();
        let left = [
            ("x-ratelimit-remaining", "12"),
            ("x-ratelimit-reset", reset.as_str()),
        ];
        assert_eq!(wait(403, &left), None);
        assert_eq!(wait(403, &[]), None);
        assert_eq!(wait(503, &[("retry-after", "5")]), None);
        assert_eq!(wait(429, &[("retry-after", "soon")]), None);
    }
}
