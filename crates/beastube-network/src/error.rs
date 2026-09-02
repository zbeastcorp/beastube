//! Network failures and their recovery strategies.
//!
//! Every failure the transport layer can produce is classified here once, so that the retry loop in
//! [`crate::retry`], the connectivity tracker in [`crate::status`] and the UI all act on the same
//! decision instead of each re-deriving it from a `reqwest::Error`'s predicate methods.
//!
//! ## Why classification lives in the error, not at the call site
//!
//! `reqwest` reports "is this a timeout?" through `Error::is_timeout()`, which is only meaningful
//! next to `is_connect()`, `is_body()` and the response status. Answering "should this be retried?"
//! from a call site therefore means repeating a five-way match, and any call site that forgets one
//! arm silently retries something it should not — a 404, or a `POST`. Collapsing the transport
//! error into [`NetworkError`] at the boundary makes that mistake unrepresentable: the retry policy
//! matches on a closed enum the compiler checks.
//!
//! ## Shared failures
//!
//! `reqwest::Error` is not `Clone`, yet [`crate::dedup`] hands one request's outcome to several
//! waiters. [`NetworkError::Shared`] is the answer: it keeps the original behind an `Arc` and
//! delegates [`DomainError::kind`], [`DomainError::code`] and [`DomainError::recovery`] to it, so a
//! waiter on a deduplicated request sees exactly the classification a sole caller would have seen.
//! Use [`NetworkError::root`] when matching on the concrete variant.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use beastube_core::error::{DomainError, ErrorKind, Recovery, source_chain};
use beastube_core::security::UrlError;

use crate::retry;

/// Convenience alias for network results.
pub type NetworkResult<T> = Result<T, NetworkError>;

/// A transport-layer failure.
#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    /// The URL failed [`beastube_core::security::validate_fetch_url`] and was never sent.
    ///
    /// This is an error rather than a warning-and-proceed: a URL that fails validation came from a
    /// provider response or an imported file, and fetching it anyway is precisely the SSRF the
    /// validator exists to prevent (§78).
    #[error("refusing to fetch {url}")]
    Url {
        /// The rejected URL, truncated for the diagnostic log.
        url: String,
        /// Why the validator rejected it.
        #[source]
        source: UrlError,
    },

    /// The connection could not be established: DNS failure, refused TCP connect, TLS failure.
    #[error("could not connect to {host}")]
    Connect {
        /// Host that could not be reached.
        host: String,
        /// Underlying transport error.
        #[source]
        source: reqwest::Error,
    },

    /// The request did not complete within its deadline.
    ///
    /// Distinct from [`NetworkError::Connect`] because it says the peer is reachable but slow,
    /// which the connectivity tracker must not count towards going offline.
    #[error("request to {host} timed out after {elapsed_ms}ms")]
    Timeout {
        /// Host that was being contacted.
        host: String,
        /// Wall-clock time spent before giving up.
        elapsed_ms: u64,
    },

    /// The server answered with a status the caller cannot use.
    #[error("{host} answered {status}")]
    Status {
        /// Host that answered.
        host: String,
        /// The HTTP status code.
        status: u16,
        /// `Retry-After`, already parsed and clamped, when the response carried a usable one.
        retry_after_ms: Option<u64>,
    },

    /// The response body failed part-way through: a reset connection, or a decompression error.
    ///
    /// Not retried automatically: an interrupted body may have been partially consumed by the
    /// caller (a stream), so restarting is the caller's decision, not ours.
    #[error("response body from {host} failed")]
    Body {
        /// Host that was being read from.
        host: String,
        /// Underlying transport error.
        #[source]
        source: reqwest::Error,
    },

    /// A buffered response exceeded the ceiling on in-memory bodies.
    ///
    /// A metadata response is kilobytes; anything approaching the ceiling is either schema drift or
    /// a hostile endpoint trying to exhaust memory. Media is fetched with
    /// [`crate::request::RequestBuilder::send_stream`], which has no such ceiling because it never
    /// buffers.
    #[error("response body from {host} exceeded the {limit_bytes} byte ceiling")]
    BodyTooLarge {
        /// Host that was being read from.
        host: String,
        /// The ceiling that was exceeded.
        limit_bytes: u64,
    },

    /// The caller's cancellation token fired, or every waiter on a shared request went away.
    #[error("request to {host} was cancelled")]
    Cancelled {
        /// Host the cancelled request was addressed to.
        host: String,
    },

    /// A connect failure observed while connectivity was already known to be down.
    ///
    /// Reported separately from [`NetworkError::Connect`] so the UI can render "waiting for a
    /// connection" instead of a per-request failure, and so the recovery strategy is
    /// [`Recovery::AwaitConnectivity`] rather than an immediate retry that cannot succeed.
    #[error("offline; {host} is unreachable")]
    Offline {
        /// Host that was being contacted.
        host: String,
    },

    /// Automatic retries were exhausted.
    ///
    /// The final failure is preserved as the source so diagnostics show *what* kept failing, while
    /// the recovery strategy becomes [`Recovery::RetryManual`]: we have already spent the automatic
    /// budget, and spinning further would turn a hard failure into a long stall (§75).
    #[error("gave up on {host} after {attempts} attempts")]
    RetriesExhausted {
        /// Host that kept failing.
        host: String,
        /// Attempts made, including the first.
        attempts: u32,
        /// The failure of the final attempt.
        #[source]
        source: Box<NetworkError>,
    },

    /// A caller-supplied header name or value was not a valid HTTP header.
    #[error("invalid header `{name}`")]
    Header {
        /// The offending header name.
        name: String,
    },

    /// The shared `reqwest::Client` could not be constructed.
    #[error("the HTTP client could not be built")]
    ClientBuild {
        /// Underlying builder error.
        #[source]
        source: reqwest::Error,
    },

    /// The manager is shutting down and will not start new work.
    #[error("the network manager is shutting down")]
    ShuttingDown,

    /// The failure of a request whose outcome was shared by several waiters.
    ///
    /// See the module documentation: classification delegates to `inner`, so this wrapper is
    /// transparent to everything except a direct `match` on the variant.
    #[error("shared request failed: {origin}")]
    Shared {
        /// Rendered `Display` chain of the original failure, so the diagnostic survives the
        /// `Arc` that the source chain cannot walk into.
        origin: String,
        /// The original failure, kept for classification.
        inner: Arc<NetworkError>,
    },
}

impl NetworkError {
    /// The concrete failure underneath any [`NetworkError::Shared`] or
    /// [`NetworkError::RetriesExhausted`] wrapper.
    ///
    /// Use this when matching on a variant. Do **not** use it to derive a recovery strategy: the
    /// wrappers deliberately change the recovery (retries already spent), which is exactly the
    /// information this method discards.
    #[must_use]
    pub fn root(&self) -> &Self {
        match self {
            Self::Shared { inner, .. } => inner.root(),
            Self::RetriesExhausted { source, .. } => source.root(),
            other => other,
        }
    }

    /// Whether repeating the same request could plausibly succeed.
    ///
    /// This is the single definition the retry loop consults. Note what is *absent*: every 4xx
    /// except 429 is non-transient, because a repeated request produces a repeated rejection while
    /// adding load to an upstream that is already refusing us.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self.root() {
            Self::Connect { .. } | Self::Timeout { .. } => true,
            Self::Status { status, .. } => retry::is_retryable_status(*status),
            _ => false,
        }
    }

    /// The HTTP status, when the failure was a response rather than a transport error.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self.root() {
            Self::Status { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// The delay the server asked us to wait, when it sent a usable `Retry-After`.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self.root() {
            Self::Status { retry_after_ms, .. } => retry_after_ms.map(Duration::from_millis),
            _ => None,
        }
    }

    /// Whether this failure is a caller-initiated cancellation rather than a fault.
    ///
    /// Cancellation is counted separately in [`crate::metrics`] and must never move the
    /// connectivity tracker: a user navigating away is not evidence that the network is down.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self.root(), Self::Cancelled { .. } | Self::ShuttingDown)
    }

    /// Whether this failure is evidence that no route to the network exists.
    ///
    /// Only a failure to *establish* a connection qualifies. A timeout does not: a saturated link
    /// or a slow origin times out while connectivity is perfectly fine, and treating that as
    /// offline would flip the whole UI into cached mode over one slow response.
    #[must_use]
    pub fn is_connect_failure(&self) -> bool {
        matches!(self.root(), Self::Connect { .. } | Self::Offline { .. })
    }

    /// Wraps a failure for delivery to a waiter on a deduplicated request.
    #[must_use]
    pub fn shared(inner: &Arc<Self>) -> Self {
        Self::Shared {
            origin: source_chain(inner.as_ref()),
            inner: Arc::clone(inner),
        }
    }

    /// Classifies a `reqwest` failure into the closed set the retry policy understands.
    ///
    /// Ordering matters: `is_connect()` is checked before `is_timeout()` because a connect attempt
    /// that hits the connect timeout reports *both*, and it is the connect-ness that the
    /// connectivity tracker needs.
    // Consumed by `client.rs`, which lands with the request pipeline.
    #[allow(dead_code)]
    pub(crate) fn from_reqwest(host: &str, elapsed: Duration, source: reqwest::Error) -> Self {
        if source.is_connect() {
            return Self::Connect {
                host: host.to_owned(),
                source,
            };
        }
        if source.is_timeout() {
            return Self::Timeout {
                host: host.to_owned(),
                elapsed_ms: duration_as_millis(elapsed),
            };
        }
        Self::Body {
            host: host.to_owned(),
            source,
        }
    }
}

/// Milliseconds of a duration, saturating rather than wrapping.
///
/// A duration long enough to overflow `u64` milliseconds cannot arise from a request, but the
/// conversion is written to saturate anyway so no arithmetic here can panic on a strange clock.
pub(crate) fn duration_as_millis(value: Duration) -> u64 {
    u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
}

impl DomainError for NetworkError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Network
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Url { .. } => "invalid_url",
            Self::Connect { .. } => "connect_failed",
            Self::Timeout { .. } => "timeout",
            Self::Status { .. } => "http_status",
            Self::Body { .. } => "body_failed",
            Self::BodyTooLarge { .. } => "body_too_large",
            Self::Cancelled { .. } => "cancelled",
            Self::Offline { .. } => "offline",
            Self::RetriesExhausted { .. } => "retries_exhausted",
            Self::Header { .. } => "invalid_header",
            Self::ClientBuild { .. } => "client_build_failed",
            Self::ShuttingDown => "shutting_down",
            Self::Shared { inner, .. } => inner.code(),
        }
    }

    fn recovery(&self) -> Recovery {
        match self {
            // Nothing about the request will change on a repeat, and the UI has no affordance to
            // offer: a rejected URL is a bug or an attack, not a user-fixable condition.
            Self::Url { .. }
            | Self::Header { .. }
            | Self::ClientBuild { .. }
            | Self::Cancelled { .. }
            | Self::ShuttingDown => Recovery::Unrecoverable,

            // Transport faults are what the automatic budget exists for. The delay published here
            // is the policy's first backoff step, so a caller retrying by hand lands on the same
            // schedule the internal loop would have used.
            Self::Connect { .. } | Self::Timeout { .. } => automatic(None),

            Self::Status {
                status,
                retry_after_ms,
                ..
            } => {
                if retry::is_retryable_status(*status) {
                    automatic(*retry_after_ms)
                } else if *status >= 500 {
                    // 501/505 and friends: the server understood us and cannot serve it. Repeating
                    // automatically is pointless, but a later manual attempt may hit a fixed peer.
                    Recovery::RetryManual
                } else {
                    Recovery::Unrecoverable
                }
            }

            // The automatic budget is spent; further attempts must be user-initiated.
            Self::RetriesExhausted { .. } | Self::Body { .. } | Self::BodyTooLarge { .. } => {
                Recovery::RetryManual
            }

            // The operation resumes on its own once a route exists; there is nothing to retry now.
            Self::Offline { .. } => Recovery::AwaitConnectivity,

            Self::Shared { inner, .. } => inner.recovery(),
        }
    }

    fn params(&self) -> BTreeMap<String, String> {
        let mut params = BTreeMap::new();
        match self {
            Self::Url { url, .. } => {
                params.insert("url".to_owned(), url.clone());
            }
            Self::Connect { host, .. }
            | Self::Body { host, .. }
            | Self::Cancelled { host }
            | Self::Offline { host } => {
                params.insert("host".to_owned(), host.clone());
            }
            Self::Timeout { host, elapsed_ms } => {
                params.insert("host".to_owned(), host.clone());
                params.insert("elapsed_ms".to_owned(), elapsed_ms.to_string());
            }
            Self::Status { host, status, .. } => {
                params.insert("host".to_owned(), host.clone());
                params.insert("status".to_owned(), status.to_string());
            }
            Self::BodyTooLarge { host, limit_bytes } => {
                params.insert("host".to_owned(), host.clone());
                params.insert("limit_bytes".to_owned(), limit_bytes.to_string());
            }
            Self::RetriesExhausted { host, attempts, .. } => {
                params.insert("host".to_owned(), host.clone());
                params.insert("attempts".to_owned(), attempts.to_string());
            }
            Self::Header { name } => {
                params.insert("name".to_owned(), name.clone());
            }
            Self::Shared { inner, .. } => return inner.params(),
            Self::ClientBuild { .. } | Self::ShuttingDown => {}
        }
        params
    }
}

/// Builds the automatic-retry recovery advertised to callers, honouring a server-supplied delay.
fn automatic(retry_after_ms: Option<u64>) -> Recovery {
    Recovery::RetryAutomatic {
        delay_ms: retry_after_ms
            .unwrap_or_else(|| duration_as_millis(retry::DEFAULT_INITIAL_BACKOFF)),
        attempts_made: 0,
        max_attempts: retry::DEFAULT_MAX_ATTEMPTS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(code: u16) -> NetworkError {
        NetworkError::Status {
            host: "example.com".to_owned(),
            status: code,
            retry_after_ms: None,
        }
    }

    #[test]
    fn errors_classify_as_network_failures() {
        let err = status(503);
        assert_eq!(err.kind(), ErrorKind::Network);
        assert_eq!(err.full_code(), "network.http_status");
        assert_eq!(err.message_key(), "error.network.http_status");
    }

    #[test]
    fn only_the_documented_status_codes_are_transient() {
        for code in [429, 500, 502, 503, 504] {
            assert!(status(code).is_transient(), "{code} must be retryable");
        }
        for code in [200, 301, 400, 401, 403, 404, 410, 418, 451, 501, 505] {
            assert!(!status(code).is_transient(), "{code} must not be retryable");
        }
    }

    #[test]
    fn a_client_rejection_is_unrecoverable_but_a_server_refusal_is_not() {
        assert_eq!(status(404).recovery(), Recovery::Unrecoverable);
        assert_eq!(status(403).recovery(), Recovery::Unrecoverable);
        // 501 is a server-side "no", so a later attempt against a fixed peer is worth offering.
        assert_eq!(status(501).recovery(), Recovery::RetryManual);
    }

    #[test]
    fn a_rate_limit_publishes_the_servers_own_delay() {
        let err = NetworkError::Status {
            host: "example.com".to_owned(),
            status: 429,
            retry_after_ms: Some(4_000),
        };
        match err.recovery() {
            Recovery::RetryAutomatic {
                delay_ms,
                max_attempts,
                ..
            } => {
                assert_eq!(
                    delay_ms, 4_000,
                    "the server's delay must win over our backoff"
                );
                assert!(max_attempts > 0, "retries must stay bounded");
            }
            other => panic!("expected an automatic retry, got {other:?}"),
        }
        assert_eq!(err.retry_after(), Some(Duration::from_millis(4_000)));
    }

    #[test]
    fn exhausted_retries_stop_being_automatic() {
        let err = NetworkError::RetriesExhausted {
            host: "example.com".to_owned(),
            attempts: 3,
            source: Box::new(status(503)),
        };
        assert_eq!(
            err.recovery(),
            Recovery::RetryManual,
            "spending the budget twice would turn a failure into a stall"
        );
        // Inspection still reaches the underlying cause.
        assert_eq!(err.status(), Some(503));
    }

    #[test]
    fn offline_waits_for_connectivity_instead_of_retrying() {
        let err = NetworkError::Offline {
            host: "example.com".to_owned(),
        };
        assert_eq!(err.recovery(), Recovery::AwaitConnectivity);
        assert!(err.recovery().is_automatic());
        assert!(!err.recovery().offers_retry());
        assert!(err.is_connect_failure());
    }

    #[test]
    fn a_timeout_is_not_evidence_of_being_offline() {
        let err = NetworkError::Timeout {
            host: "example.com".to_owned(),
            elapsed_ms: 30_000,
        };
        assert!(
            !err.is_connect_failure(),
            "a slow origin must not flip the app into cached mode"
        );
        assert!(err.is_transient());
    }

    #[test]
    fn cancellation_is_not_a_fault() {
        let err = NetworkError::Cancelled {
            host: "example.com".to_owned(),
        };
        assert!(err.is_cancelled());
        assert!(!err.is_transient());
        assert!(!err.is_connect_failure());
        assert_eq!(err.recovery(), Recovery::Unrecoverable);
        assert!(NetworkError::ShuttingDown.is_cancelled());
    }

    #[test]
    fn a_rejected_url_is_never_retried() {
        let err = NetworkError::Url {
            url: "http://127.0.0.1/steal".to_owned(),
            source: UrlError::HostNotAllowed {
                host: "127.0.0.1".to_owned(),
            },
        };
        assert_eq!(err.recovery(), Recovery::Unrecoverable);
        assert!(!err.is_transient());
        let payload = err.to_payload();
        assert_eq!(payload.code, "network.invalid_url");
        assert!(
            payload
                .diagnostic
                .as_deref()
                .is_some_and(|d| d.contains("127.0.0.1")),
            "the validator's reason must reach the diagnostic log"
        );
    }

    #[test]
    fn a_shared_failure_is_transparent_to_classification() {
        let inner = Arc::new(status(503));
        let shared = NetworkError::shared(&inner);

        assert_eq!(shared.code(), "http_status");
        assert_eq!(shared.kind(), ErrorKind::Network);
        assert_eq!(shared.recovery(), inner.recovery());
        assert_eq!(shared.status(), Some(503));
        assert!(shared.is_transient());
        assert_eq!(
            shared.params().get("status").map(String::as_str),
            Some("503")
        );
    }

    #[test]
    fn a_shared_failure_keeps_the_original_diagnostic() {
        let inner = Arc::new(NetworkError::Url {
            url: "ftp://example.com".to_owned(),
            source: UrlError::SchemeNotAllowed {
                scheme: "ftp".to_owned(),
            },
        });
        let shared = NetworkError::shared(&inner);
        let rendered = shared.to_string();
        assert!(
            rendered.contains("ftp://example.com") && rendered.contains("scheme"),
            "the Arc breaks the source chain, so the origin must be rendered in: {rendered}"
        );
    }

    #[test]
    fn nested_wrappers_still_resolve_to_the_root_cause() {
        let inner = Arc::new(NetworkError::RetriesExhausted {
            host: "example.com".to_owned(),
            attempts: 3,
            source: Box::new(status(504)),
        });
        let shared = NetworkError::shared(&inner);
        assert_eq!(shared.status(), Some(504));
        // The wrapper's own recovery survives delegation: retries are still spent.
        assert_eq!(shared.recovery(), Recovery::RetryManual);
    }

    #[test]
    fn payloads_carry_identifying_params_for_the_ui() {
        let payload = NetworkError::Timeout {
            host: "rr3.googlevideo.com".to_owned(),
            elapsed_ms: 30_000,
        }
        .to_payload();
        assert_eq!(
            payload.params.get("host").map(String::as_str),
            Some("rr3.googlevideo.com")
        );
        assert_eq!(
            payload.params.get("elapsed_ms").map(String::as_str),
            Some("30000")
        );
    }

    #[test]
    fn millisecond_conversion_saturates_instead_of_wrapping() {
        assert_eq!(duration_as_millis(Duration::from_millis(250)), 250);
        assert_eq!(duration_as_millis(Duration::MAX), u64::MAX);
        assert_eq!(duration_as_millis(Duration::ZERO), 0);
    }

    #[test]
    fn an_oversized_body_is_reported_with_its_ceiling() {
        let err = NetworkError::BodyTooLarge {
            host: "example.com".to_owned(),
            limit_bytes: 16 * 1024 * 1024,
        };
        assert_eq!(err.code(), "body_too_large");
        assert_eq!(err.recovery(), Recovery::RetryManual);
        assert_eq!(
            err.params().get("limit_bytes").map(String::as_str),
            Some("16777216")
        );
    }
}
