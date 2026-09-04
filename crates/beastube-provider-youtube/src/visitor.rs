//! A visitor-data ID of our own, so the extractor never has to fetch one.
//!
//! Every InnerTube request the extractor makes wants a `visitorData` ID in its context. When none
//! is configured it fetches one itself — from `music.youtube.com`, with redirects disabled — and
//! that endpoint now answers `302 Found`, which the extractor turns into a **panic** (`unwrap` in
//! `visitor_data.rs`). Measured against the live service on 2026-09-04: the first search cost
//! 3.4 s and the first video 2.2 s, against 130–400 ms once warm. The difference is that fetch
//! failing, the panic being caught, and the request being retried.
//!
//! The extractor also refreshes the ID every fifty requests from a detached task, which panics the
//! same way. Under a release profile with `panic = "abort"` that detached panic ends the process.
//!
//! Supplying the ID ourselves removes both: the extractor's cache is never consulted when a query
//! carries one (`RustyPipeQuery::visitor_data`), so neither the inline fetch nor the detached
//! refresh ever runs. This module fetches the ID the same way a browser gets one — the YouTube
//! home page, redirects followed, consent pre-answered — and reads it out of the page's config.
//!
//! ## Rotation
//!
//! One ID per launch would work, but the extractor rotates every fifty requests for a reason: an
//! ID that has made thousands of requests starts being rate-limited, and rotating also means no
//! single ID accumulates a whole session's worth of activity. The same cadence is kept here,
//! refreshed in the background and swapped in atomically, so a request never waits for it.
//!
//! ## Failure
//!
//! If the page cannot be fetched or parsed, [`VisitorDataPool::current`] returns `None` and the
//! query is built without an ID, which is exactly the extractor's original behaviour — still
//! guarded by the provider's unwind catch. Nothing here is a new way to fail; it is only a way to
//! stop paying for the old one.

use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use regex::Regex;

/// Where a browser gets its visitor ID: the front page.
const HOME_URL: &str = "https://www.youtube.com/";

/// The cookie that answers the consent interstitial, so the front page is served rather than a
/// redirect to the consent form. The same value the extractor sends.
const CONSENT_COOKIE: &str = "SOCS=CAISAiAD";

/// The desktop user agent the extractor presents, kept identical so the ID is issued for the same
/// kind of client that will use it.
const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0";

/// Requests an ID may make before a fresh one is fetched. Matches the extractor's own policy.
const ROTATE_AFTER: u32 = 50;

/// How long the fetch may take. It is one page; anything longer is a stalled connection.
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);

/// A visitor-data ID, refreshed on a request budget.
#[derive(Clone)]
pub(crate) struct VisitorDataPool {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    /// The ID currently handed to queries. `None` until the first fetch lands or after one fails
    /// before any succeeded.
    current: RwLock<Option<String>>,
    /// Requests served with the current ID.
    uses: AtomicU32,
    /// Whether a background refresh is already running, so a burst of requests crossing the
    /// budget together starts one fetch rather than dozens.
    refreshing: AtomicBool,
    /// Serialises the *inline* fetch, so concurrent first requests share one page load rather
    /// than each making their own.
    first_fetch: tokio::sync::Mutex<()>,
}

impl VisitorDataPool {
    /// Builds a pool with its own HTTP client.
    ///
    /// Its own rather than the extractor's because the extractor disables redirects, which is the
    /// very thing that turns the front page into a `302`; this client follows them.
    ///
    /// # Errors
    ///
    /// Returns the client-construction error, which in practice means TLS could not initialise.
    pub(crate) fn new() -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .redirect(reqwest::redirect::Policy::limited(5))
            .timeout(FETCH_TIMEOUT)
            .build()?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                current: RwLock::new(None),
                uses: AtomicU32::new(0),
                refreshing: AtomicBool::new(false),
                first_fetch: tokio::sync::Mutex::new(()),
            }),
        })
    }

    /// The ID to attach to the next query, fetching one first if none exists yet.
    ///
    /// Counts the use and starts a background refresh when the budget is spent. The refresh never
    /// blocks a caller: the current ID keeps serving until the replacement has arrived.
    pub(crate) async fn current(&self) -> Option<String> {
        if let Some(id) = self.read() {
            if self.note_use() {
                self.spawn_refresh();
            }
            return Some(id);
        }

        // Nothing yet. One caller fetches; the rest wait on the lock and then find the answer.
        let _guard = self.inner.first_fetch.lock().await;
        if let Some(id) = self.read() {
            if self.note_use() {
                self.spawn_refresh();
            }
            return Some(id);
        }
        let fetched = fetch(&self.inner.http).await;
        if let Some(id) = &fetched {
            self.store(id.clone());
            self.note_use();
        }
        fetched
    }

    /// Fetches an ID ahead of the first request, so that request does not pay for it.
    ///
    /// Failure is silent: the first real query fetches again, and if that fails too it proceeds
    /// without an ID exactly as before.
    pub(crate) async fn warm(&self) {
        let _ = self.current().await;
    }

    fn read(&self) -> Option<String> {
        // A poisoned lock means a panic while holding it, which no code path here can produce; the
        // data is a plain string either way, so it is read rather than propagated.
        self.inner
            .current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn store(&self, id: String) {
        *self
            .inner
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(id);
        self.inner.uses.store(0, Ordering::Relaxed);
    }

    /// Records one use. Returns `true` exactly once per budget: when this use crossed the line and
    /// no refresh is already running. Pure bookkeeping, so it can be tested without a network.
    fn note_use(&self) -> bool {
        let used = self.inner.uses.fetch_add(1, Ordering::Relaxed) + 1;
        if used < ROTATE_AFTER {
            return false;
        }
        self.inner
            .refreshing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Fetches a replacement in the background and swaps it in when it lands.
    fn spawn_refresh(&self) {
        let pool = self.clone();
        tokio::spawn(async move {
            // No unwrap anywhere on this path: a failed refresh keeps the old ID, which still
            // works, and the next budget boundary tries again.
            if let Some(id) = fetch(&pool.inner.http).await {
                pool.store(id);
            } else {
                tracing::debug!("visitor data refresh failed; keeping the current id");
            }
            pool.inner.refreshing.store(false, Ordering::Release);
        });
    }
}

impl std::fmt::Debug for VisitorDataPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisitorDataPool")
            .field("has_id", &self.read().is_some())
            .field("uses", &self.inner.uses.load(Ordering::Relaxed))
            .finish()
    }
}

/// Fetches the front page and reads the visitor ID out of it. `None` on any failure.
async fn fetch(http: &reqwest::Client) -> Option<String> {
    let response = http
        .get(HOME_URL)
        .header(reqwest::header::COOKIE, CONSENT_COOKIE)
        .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .send()
        .await
        .map_err(|error| tracing::debug!(%error, "visitor data page request failed"))
        .ok()?;

    if !response.status().is_success() {
        tracing::debug!(status = %response.status(), "visitor data page not served");
        return None;
    }

    let html = response
        .text()
        .await
        .map_err(|error| tracing::debug!(%error, "visitor data page unreadable"))
        .ok()?;

    let id = extract(&html);
    if id.is_none() {
        tracing::debug!(bytes = html.len(), "no visitor data in the page");
    }
    id
}

/// The visitor ID embedded in the front page's configuration, in either of the two spellings the
/// page uses.
fn extract(html: &str) -> Option<String> {
    // The page carries it as `"visitorData":"…"` inside the player/response config and as
    // `"VISITOR_DATA":"…"` inside `ytcfg`. Either is the same value, URL-safe base64 with `%3D`
    // padding. Compiled per call: this runs once per fifty requests, not per request.
    let patterns = [
        r#""visitorData":"([A-Za-z0-9_\-%]+)""#,
        r#""VISITOR_DATA":"([A-Za-z0-9_\-%]+)""#,
    ];
    patterns.iter().find_map(|pattern| {
        Regex::new(pattern)
            .ok()?
            .captures(html)?
            .get(1)
            .map(|m| m.as_str().to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_is_read_from_either_config_spelling() {
        let page = r#"<script>ytcfg.set({"VISITOR_DATA":"Cgs0aGxmR3FHYlZFRSj3v_miBg%3D%3D","X":1});</script>"#;
        assert_eq!(
            extract(page).as_deref(),
            Some("Cgs0aGxmR3FHYlZFRSj3v_miBg%3D%3D")
        );

        let response = r#"{"responseContext":{"visitorData":"CgtZLXBxX0xWbGJmayj2v_miBg%3D%3D"}}"#;
        assert_eq!(
            extract(response).as_deref(),
            Some("CgtZLXBxX0xWbGJmayj2v_miBg%3D%3D")
        );
    }

    #[test]
    fn a_page_without_one_yields_nothing_rather_than_garbage() {
        assert_eq!(extract("<html>no config here</html>"), None);
        // A quote inside would be another field, never part of the ID.
        assert_eq!(extract(r#""visitorData":"""#), None);
    }

    #[test]
    fn the_budget_claims_exactly_one_refresh() {
        // No network: `note_use` is the bookkeeping alone.
        let pool = VisitorDataPool::new().unwrap();
        pool.store("seed".to_owned());

        let claimed: Vec<bool> = (0..ROTATE_AFTER + 5).map(|_| pool.note_use()).collect();
        assert_eq!(
            claimed.iter().filter(|c| **c).count(),
            1,
            "one refresh per budget however many uses cross the line: {claimed:?}"
        );
        assert!(
            claimed[usize::try_from(ROTATE_AFTER).unwrap() - 1],
            "and it is claimed on the use that spends the budget"
        );
        assert_eq!(pool.read().as_deref(), Some("seed"), "keeps serving meanwhile");
    }

    #[test]
    fn storing_a_replacement_resets_the_budget() {
        let pool = VisitorDataPool::new().unwrap();
        pool.store("first".to_owned());
        for _ in 0..ROTATE_AFTER {
            pool.note_use();
        }
        pool.store("second".to_owned());
        assert_eq!(pool.inner.uses.load(Ordering::Relaxed), 0);
        assert_eq!(pool.read().as_deref(), Some("second"));
    }
}
