//! The error contract shared by every subsystem.
//!
//! Each crate defines its own `thiserror` enum (`NetworkError`, `ProviderError`, `PlaybackError`,
//! …) so that no god-enum accumulates every failure mode in the workspace. What they share is the
//! *wire* shape defined here: [`ErrorPayload`], produced by implementing [`DomainError`].
//!
//! Three properties the type system enforces:
//!
//! * **No English prose crosses the IPC boundary.** Rust supplies a stable [`DomainError::code`]
//!   and an i18n [`ErrorPayload::message_key`]; the UI renders the sentence, so every
//!   user-facing string passes through the localization layer.
//! * **Every error states its recovery strategy.** [`Recovery`] is machine-readable, so the retry
//!   logic in `beastube-network` and the error surfaces in the UI act on the same decision instead
//!   of re-deriving it from a string.
//! * **Diagnostic detail is separated from the user-facing message.** [`ErrorPayload::diagnostic`]
//!   holds engineer-facing context for the diagnostics screen and local log; it is never
//!   transmitted anywhere and never rendered as the primary message.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Top-level classification of a failure, matching the subsystem that produced it.
///
/// The UI uses this to choose a presentation surface: a [`ErrorKind::Network`] failure becomes an
/// inline offline banner, whereas [`ErrorKind::Database`] escalates to a modal because local data
/// integrity is at stake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Transport-level failure: DNS, TCP, TLS, timeout, connection reset, offline.
    Network,
    /// The provider responded, but the response could not be used: schema drift, unavailable
    /// content, geo/age restriction, upstream rejection.
    Provider,
    /// Media pipeline failure: decode error, unsupported codec, MSE append failure, stall.
    Playback,
    /// SQLite failure: locked, corrupt, migration failure, constraint violation.
    Database,
    /// Cache read/write/corruption failure. Always recoverable by rebuilding: caches are
    /// disposable by design.
    Cache,
    /// Filesystem failure: permission denied, disk full, invalid path, missing directory.
    FileSystem,
    /// The operation was refused by the local permission model (Tauri capability, path scope).
    Permission,
    /// Invalid or unusable configuration/settings state.
    Configuration,
    /// Content-filtering subsystem failure: invalid rule set, failed update, rollback.
    Filtering,
    /// Update check, download, verification or installation failure.
    Update,
}

impl ErrorKind {
    /// Stable lowercase identifier, used as the first segment of an error code and log target.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Provider => "provider",
            Self::Playback => "playback",
            Self::Database => "database",
            Self::Cache => "cache",
            Self::FileSystem => "filesystem",
            Self::Permission => "permission",
            Self::Configuration => "configuration",
            Self::Filtering => "filtering",
            Self::Update => "update",
        }
    }

    /// Whether a failure of this kind threatens user-created local data.
    ///
    /// Used to decide escalation: losing a cache entry is invisible, losing the library is not.
    #[must_use]
    pub const fn threatens_user_data(self) -> bool {
        matches!(self, Self::Database | Self::FileSystem)
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the system (or the user) can do about a failure.
///
/// This is deliberately an instruction rather than a description. `beastube-network` reads it to
/// decide whether to schedule a retry, and the UI reads it to decide which affordance to render;
/// both act on the same value so their behaviour cannot drift apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "strategy", rename_all = "snake_case")]
pub enum Recovery {
    /// The caller should retry without involving the user.
    ///
    /// `delay_ms` is the backoff already computed by the producer (including jitter), so the
    /// consumer does not re-derive a policy the producer has better information about.
    RetryAutomatic {
        /// Delay before the next attempt, in milliseconds.
        delay_ms: u64,
        /// Attempts already made, for surfacing progress and enforcing a ceiling.
        attempts_made: u32,
        /// Hard ceiling on attempts. Retrying is never unbounded.
        max_attempts: u32,
    },
    /// Retrying may succeed but should be user-initiated (avoids hammering an upstream that is
    /// deliberately rejecting us).
    RetryManual,
    /// A degraded path is available and will be, or has been, taken.
    Fallback {
        /// i18n key describing the fallback, e.g. `recovery.fallback.lower_quality`.
        message_key: String,
    },
    /// The user must change a setting before the operation can succeed.
    AdjustSettings {
        /// Dot path into the settings tree, e.g. `playback.hardware_acceleration`.
        settings_path: String,
    },
    /// Local derived data must be discarded and rebuilt. Never destroys user-created data.
    RebuildLocalData {
        /// Which store to rebuild, e.g. `cache.thumbnails`, `cache.metadata`.
        store: String,
    },
    /// Nothing to do until connectivity returns; the operation will be resumed automatically.
    AwaitConnectivity,
    /// Genuinely unrecoverable. The UI reports and moves on rather than offering a false retry.
    Unrecoverable,
}

impl Recovery {
    /// Whether this strategy implies the operation will be retried without user action.
    #[must_use]
    pub const fn is_automatic(&self) -> bool {
        matches!(self, Self::RetryAutomatic { .. } | Self::AwaitConnectivity)
    }

    /// Whether the UI should offer a retry affordance.
    #[must_use]
    pub const fn offers_retry(&self) -> bool {
        matches!(self, Self::RetryManual | Self::RetryAutomatic { .. })
    }
}

/// The serialized form of a failure as it crosses the IPC boundary.
///
/// Construct via [`DomainError::to_payload`] rather than by hand, so that the code and message key
/// stay consistent with the producing error type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorPayload {
    /// Subsystem classification.
    pub kind: ErrorKind,
    /// Stable machine-readable code, e.g. `network.timeout`. Never localized, never reworded;
    /// tests and telemetry-free diagnostics key off this.
    pub code: String,
    /// i18n message key the UI resolves, e.g. `error.network.timeout`.
    pub message_key: String,
    /// Interpolation parameters for `message_key`, e.g. `{"seconds": "30"}`.
    ///
    /// Ordered so that serialized payloads are byte-stable, which keeps snapshot tests meaningful.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    /// What to do about it.
    pub recovery: Recovery,
    /// Engineer-facing detail: the `Display` chain of the underlying error.
    ///
    /// Shown only in the diagnostics screen and written to the local log. It is never uploaded —
    /// the application performs no telemetry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
    /// Correlates this failure with a task, request or playback session for log cross-referencing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl ErrorPayload {
    /// Attaches engineer-facing detail.
    #[must_use]
    pub fn with_diagnostic(mut self, diagnostic: impl Into<String>) -> Self {
        self.diagnostic = Some(diagnostic.into());
        self
    }

    /// Attaches a correlation identifier.
    #[must_use]
    pub fn with_correlation_id(mut self, id: impl Into<String>) -> Self {
        self.correlation_id = Some(id.into());
        self
    }

    /// Adds one i18n interpolation parameter.
    #[must_use]
    pub fn with_param(mut self, key: impl Into<String>, value: impl fmt::Display) -> Self {
        self.params.insert(key.into(), value.to_string());
        self
    }

    /// Whether the producer expects this operation to be retried automatically.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        self.recovery.is_automatic()
    }
}

impl fmt::Display for ErrorPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.kind, self.code)?;
        if let Some(diagnostic) = &self.diagnostic {
            write!(f, ": {diagnostic}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ErrorPayload {}

/// Implemented by each subsystem's error enum to produce the shared wire shape.
///
/// Implementors supply classification, a stable code and a recovery strategy; the default
/// [`DomainError::to_payload`] assembles the payload and captures the `Display` chain as the
/// diagnostic, so no implementor has to remember to do it.
pub trait DomainError: std::error::Error {
    /// Subsystem classification for this error.
    fn kind(&self) -> ErrorKind;

    /// Stable code *within* the subsystem, e.g. `timeout`. The full code is `kind.code`.
    fn code(&self) -> &'static str;

    /// The recovery strategy the caller should apply.
    fn recovery(&self) -> Recovery;

    /// Interpolation parameters for the i18n message.
    fn params(&self) -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    /// Builds the full dotted code, e.g. `network.timeout`.
    fn full_code(&self) -> String {
        format!("{}.{}", self.kind().as_str(), self.code())
    }

    /// Builds the i18n message key, e.g. `error.network.timeout`.
    fn message_key(&self) -> String {
        format!("error.{}", self.full_code())
    }

    /// Converts into the IPC wire shape, capturing the full source chain as the diagnostic.
    ///
    /// Requires `Self: Sized` so that `&Self` can coerce to `&dyn Error` for the source walk; the
    /// trait itself stays dyn-compatible for callers that only need classification.
    fn to_payload(&self) -> ErrorPayload
    where
        Self: Sized,
    {
        ErrorPayload {
            kind: self.kind(),
            code: self.full_code(),
            message_key: self.message_key(),
            params: self.params(),
            recovery: self.recovery(),
            diagnostic: Some(source_chain(self)),
            correlation_id: None,
        }
    }
}

/// Renders an error and its `source()` chain as a single `a: b: c` string.
///
/// Used for the diagnostic field and structured logs, where the root cause matters more than the
/// outermost wrapper.
pub fn source_chain(error: &dyn std::error::Error) -> String {
    let mut out = error.to_string();
    let mut current = error.source();
    while let Some(cause) = current {
        use fmt::Write as _;
        // Writing to a String is infallible; the Result is discarded deliberately.
        let _ = write!(out, ": {cause}");
        current = cause.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("connection timed out after {seconds}s")]
    struct Timeout {
        seconds: u64,
        #[source]
        cause: Option<std::io::Error>,
    }

    impl DomainError for Timeout {
        fn kind(&self) -> ErrorKind {
            ErrorKind::Network
        }
        fn code(&self) -> &'static str {
            "timeout"
        }
        fn recovery(&self) -> Recovery {
            Recovery::RetryAutomatic {
                delay_ms: 500,
                attempts_made: 1,
                max_attempts: 4,
            }
        }
        fn params(&self) -> BTreeMap<String, String> {
            BTreeMap::from([("seconds".to_owned(), self.seconds.to_string())])
        }
    }

    #[test]
    fn payload_uses_dotted_code_and_message_key() {
        let payload = Timeout {
            seconds: 30,
            cause: None,
        }
        .to_payload();
        assert_eq!(payload.code, "network.timeout");
        assert_eq!(payload.message_key, "error.network.timeout");
        assert_eq!(payload.kind, ErrorKind::Network);
        assert_eq!(
            payload.params.get("seconds").map(String::as_str),
            Some("30")
        );
        assert!(payload.is_retryable());
    }

    #[test]
    fn diagnostic_captures_full_source_chain() {
        let payload = Timeout {
            seconds: 5,
            cause: Some(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "no route to host",
            )),
        }
        .to_payload();
        let diagnostic = payload.diagnostic.expect("diagnostic present");
        assert!(diagnostic.contains("connection timed out after 5s"));
        assert!(
            diagnostic.contains("no route to host"),
            "root cause must survive: {diagnostic}"
        );
    }

    #[test]
    fn payload_round_trips_through_json() {
        let payload = Timeout {
            seconds: 30,
            cause: None,
        }
        .to_payload()
        .with_correlation_id("task-7");
        let json = serde_json::to_string(&payload).unwrap();
        let back: ErrorPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, back);
    }

    #[test]
    fn empty_params_are_omitted_from_the_wire() {
        let payload = ErrorPayload {
            kind: ErrorKind::Cache,
            code: "cache.corrupt".to_owned(),
            message_key: "error.cache.corrupt".to_owned(),
            params: BTreeMap::new(),
            recovery: Recovery::RebuildLocalData {
                store: "cache.thumbnails".to_owned(),
            },
            diagnostic: None,
            correlation_id: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(!json.contains("params"), "{json}");
        assert!(!json.contains("diagnostic"), "{json}");
    }

    #[test]
    fn only_data_bearing_kinds_escalate() {
        assert!(ErrorKind::Database.threatens_user_data());
        assert!(ErrorKind::FileSystem.threatens_user_data());
        assert!(!ErrorKind::Cache.threatens_user_data());
        assert!(!ErrorKind::Network.threatens_user_data());
    }

    #[test]
    fn unrecoverable_offers_no_retry() {
        assert!(!Recovery::Unrecoverable.offers_retry());
        assert!(!Recovery::Unrecoverable.is_automatic());
        assert!(Recovery::RetryManual.offers_retry());
        assert!(!Recovery::RetryManual.is_automatic());
    }
}
