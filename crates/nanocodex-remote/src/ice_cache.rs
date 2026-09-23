//! Publisher-session ICE prewarming. Only this session and its admitted viewer
//! preparations own the shared HTTP future; there is no detached worker.
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use serde_json::Value;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;
use url::Url;

pub(crate) type Response = Result<Arc<Value>, Arc<reqwest::Error>>;
type Flight = Shared<BoxFuture<'static, Fetched>>;
const MARGIN_MS: u64 = 60_000;
const MAX_REUSE: Duration = Duration::from_secs(300);
const RETRY: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct Fetched {
    response: Response,
    received: Instant,
    wall_ms: u64,
}
struct Cached {
    response: Arc<Value>,
    expires_at: u64,
    until: Instant,
    refresh_at: Instant,
}
impl Cached {
    fn new(fetched: &Fetched) -> Option<Self> {
        let response = fetched.response.as_ref().ok()?;
        // A successful HTTP status alone is not a reusable ICE configuration.
        crate::ice::ice_servers(response).ok()?;
        let expires_at = response["expires_at"].as_u64()?;
        let reusable_ms = expires_at
            .checked_sub(fetched.wall_ms)?
            .checked_sub(MARGIN_MS)?;
        if reusable_ms == 0 {
            return None;
        }
        let reuse = Duration::from_millis(reusable_ms).min(MAX_REUSE);
        Some(Self {
            response: response.clone(),
            expires_at,
            until: fetched.received + reuse,
            refresh_at: fetched.received + reuse.mul_f64(0.8),
        })
    }
    fn valid(&self, now: Instant, wall_ms: u64) -> bool {
        now < self.until && self.expires_at.saturating_sub(wall_ms) > MARGIN_MS
    }
}
fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

pub(crate) struct IceCache {
    http: reqwest::Client,
    url: Url,
    token: String,
    pending: Option<Flight>,
    cached: Option<Cached>,
    refresh_attempts: u8,
    retry_at: Option<Instant>,
}
impl IceCache {
    pub(crate) fn new(http: reqwest::Client, mut base: Url, token: &str) -> Self {
        base.set_path(&format!("{}/ice", base.path()));
        Self {
            http,
            url: base,
            token: token.into(),
            pending: None,
            cached: None,
            refresh_attempts: 0,
            retry_at: None,
        }
    }
    fn start(&mut self) -> Flight {
        let http = self.http.clone();
        let url = self.url.clone();
        let token = self.token.clone();
        let flight = async move {
            let response = async {
                http.post(url)
                    .bearer_auth(token)
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<Value>()
                    .await
            }
            .await
            .map(Arc::new)
            .map_err(Arc::new);
            Fetched {
                response,
                received: Instant::now(),
                wall_ms: wall_ms(),
            }
        }
        .boxed()
        .shared();
        self.pending = Some(flight.clone());
        flight
    }
    fn finish(&mut self, fetched: Fetched) {
        self.pending = None;
        if let Some(cached) = Cached::new(&fetched) {
            self.cached = Some(cached);
            self.refresh_attempts = 0;
            self.retry_at = None;
        } else {
            // Keep an earlier *valid* entry only to its original bounds. One
            // background retry is allowed; failures never extend credential life.
            self.retry_at = Some(Instant::now() + RETRY);
        }
    }
    fn settle(&mut self) {
        if let Some(fetched) = self.pending.as_ref().and_then(|f| f.clone().now_or_never()) {
            self.finish(fetched);
        }
    }
    fn valid(&self) -> Option<&Cached> {
        self.cached
            .as_ref()
            .filter(|c| c.valid(Instant::now(), wall_ms()))
    }
    /// Called only after publication. The session select loop drives the I/O.
    pub(crate) fn prefetch(&mut self) {
        self.settle();
        if self.pending.is_none() && self.valid().is_none() {
            drop(self.start());
        }
    }
    /// Called by the session's existing one-second lifecycle tick. No timer or
    /// request survives its authorization/session scope.
    pub(crate) fn refresh(&mut self) {
        self.settle();
        let now = Instant::now();
        if self.pending.is_none()
            && self.refresh_attempts < 2
            && self.valid().is_some_and(|c| now >= c.refresh_at)
            && self.retry_at.is_none_or(|retry| now >= retry)
        {
            self.refresh_attempts += 1;
            drop(self.start());
        }
    }
    pub(crate) fn request(&mut self) -> BoxFuture<'static, Response> {
        // A viewer may have completed the shared future before next() wins the
        // select. Retire that flight first, including failed/uncacheable results.
        self.settle();
        if let Some(cached) = self.valid() {
            return futures_util::future::ready(Ok(cached.response.clone())).boxed();
        }
        self.cached = None;
        let flight = self.pending.clone().unwrap_or_else(|| self.start());
        async move { flight.await.response }.boxed()
    }
    // Cancellation-safe: do not remove the owned flight until it completes.
    pub(crate) async fn next(&mut self) {
        match self.pending.as_ref() {
            Some(flight) => {
                let fetched = flight.clone().await;
                self.finish(fetched);
            }
            None => std::future::pending().await,
        }
    }
}

#[cfg(test)]
#[path = "ice_cache_tests.rs"]
mod tests;
