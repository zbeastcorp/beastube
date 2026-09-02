//! Cache failures, and why almost none of them reach the user.
//!
//! The cache holds only derived data (§71): every byte in it can be fetched again. That single
//! fact shapes this module:
//!
//! * **Corruption is not an error path, it is a miss.** [`CorruptionKind`] exists so the disk layer
//!   can say *how* an entry was damaged in a log line and a statistic, but a damaged entry is
//!   deleted and refetched rather than surfaced. [`CacheError::Corrupt`] is therefore produced by
//!   the verification routine and consumed by the layer above it; it does not escape `get`.
//! * **Everything classifies as [`ErrorKind::Cache`]**, never [`ErrorKind::FileSystem`] — even the
//!   I/O variants. [`ErrorKind::threatens_user_data`] drives escalation to a modal, and a cache
//!   write that failed threatens nothing. The subsystems that own non-disposable data (the
//!   database, the downloader) raise the filesystem alarm when the disk is genuinely full.
//! * **Errors are [`Clone`].** A single-flight fetch has many waiters and each must receive the
//!   failure. `std::io::Error` is not `Clone`, so [`CacheError::Io`] captures its
//!   [`std::io::ErrorKind`] and rendered message instead of the value itself: the kind is what the
//!   recovery decision keys off, and the message is what the diagnostics screen shows.

use std::collections::BTreeMap;
use std::io;

use beastube_core::error::{DomainError, ErrorKind, Recovery};
use beastube_core::security::{PathError, UrlError};

/// How a stored entry failed verification.
///
/// Kept separate from [`CacheError`] because the distinctions matter to an engineer reading a log
/// (bit rot behaves differently from a half-written file after a power cut) but never to the user,
/// who only sees the image reload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CorruptionKind {
    /// The file is smaller than the fixed-size entry header.
    #[error("the file is shorter than the entry header")]
    HeaderTooShort,
    /// The file does not begin with the entry magic, so it is not one of ours.
    #[error("the file does not start with the cache entry magic")]
    BadMagic,
    /// The entry was written by a different, unsupported format version.
    #[error("entry format version {found} is not supported")]
    UnsupportedVersion {
        /// Version recorded in the file.
        found: u32,
    },
    /// The file length disagrees with the lengths declared in the header — a truncated write, or
    /// trailing garbage appended by another process.
    #[error("declared {expected} bytes of body but the file holds {actual}")]
    LengthMismatch {
        /// Bytes the header says should follow it.
        expected: u64,
        /// Bytes actually present after the header.
        actual: u64,
    },
    /// The payload does not hash to the digest recorded when it was written.
    #[error("the payload checksum does not match the recorded digest")]
    ChecksumMismatch,
    /// The entry names a different key than the one being looked up.
    ///
    /// Reached only through a hash collision or a file moved between shards by hand, but checking
    /// it is what makes a collision a miss instead of silently serving the wrong image.
    #[error("the entry belongs to a different key")]
    KeyMismatch,
}

/// Why a [`ByteSource`](crate::layered::ByteSource) could not produce bytes.
///
/// Deliberately tiny and free of any dependency on the network crate: the cache defines the shape
/// it needs and the network layer maps its own richer error into it. That keeps this crate
/// independently testable, and keeps the cache from growing opinions about HTTP.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// The cancellation token passed to the fetch fired.
    #[error("the fetch was cancelled")]
    Cancelled,
    /// The fetch failed. `retryable` is the source's own judgement, which the cache forwards into
    /// [`Recovery`] rather than re-deriving from a message it cannot parse.
    #[error("fetch failed: {detail}")]
    Failed {
        /// Engineer-facing detail from the source, for the diagnostic chain.
        detail: String,
        /// Whether the source believes the same request could succeed later.
        retryable: bool,
    },
}

impl FetchError {
    /// Builds a retryable failure (timeout, connection reset, 5xx).
    #[must_use]
    pub fn retryable(detail: impl Into<String>) -> Self {
        Self::Failed {
            detail: detail.into(),
            retryable: true,
        }
    }

    /// Builds a permanent failure (404, malformed response, rejected request).
    #[must_use]
    pub fn permanent(detail: impl Into<String>) -> Self {
        Self::Failed {
            detail: detail.into(),
            retryable: false,
        }
    }

    /// Whether this failure was a cancellation rather than a fault.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

/// A cache-layer failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CacheError {
    /// A filesystem operation on the cache tree failed.
    #[error("cache {operation} failed at `{path}`: {detail}")]
    Io {
        /// What was being attempted: `read`, `write`, `rename`, `create_dir`, `scan`, `remove`.
        operation: &'static str,
        /// Path involved, for the local diagnostics screen only.
        path: String,
        /// Classification the recovery decision keys off.
        kind: io::ErrorKind,
        /// Rendered message from the underlying error.
        detail: String,
    },

    /// A stored entry failed verification and was discarded.
    ///
    /// Produced by the verification routine and handled inside the disk layer; callers of
    /// [`DiskCache::get`](crate::disk::DiskCache::get) observe a miss instead.
    #[error("cache entry in namespace `{namespace}` is damaged")]
    Corrupt {
        /// Namespace the damaged entry belonged to.
        namespace: String,
        /// How it was damaged.
        #[source]
        kind: CorruptionKind,
    },

    /// The key exceeded [`CacheKey::MAX_LEN`](crate::key::CacheKey::MAX_LEN).
    ///
    /// A bound rather than a hash-and-forget because an unbounded key is stored verbatim in every
    /// entry header, and a provider that starts emitting megabyte URLs must not be able to inflate
    /// the cache from the outside.
    #[error("cache key is {len} bytes, maximum is {max}")]
    KeyTooLong {
        /// Length that was offered.
        len: usize,
        /// Maximum permitted length.
        max: usize,
    },

    /// A namespace name is not usable as a directory name.
    #[error("`{name}` is not a usable cache namespace")]
    InvalidNamespace {
        /// The rejected name.
        name: String,
    },

    /// A path could not be resolved inside the cache root.
    ///
    /// Unreachable for keys built through [`CacheKey`](crate::key::CacheKey), whose on-disk name is
    /// a hash digest; it exists so that the guarantee is enforced by a check rather than by an
    /// argument about the hash's alphabet.
    #[error("cache path escapes the cache root")]
    UnsafePath {
        /// Why the path was rejected.
        #[source]
        source: PathError,
    },

    /// The value is larger than the per-entry ceiling, so it was not stored.
    #[error("entry is {size} bytes, over the {max} byte per-entry ceiling")]
    EntryTooLarge {
        /// Size of the value offered.
        size: u64,
        /// Configured ceiling.
        max: u64,
    },

    /// The byte source failed.
    #[error("the byte source failed")]
    Source(
        /// The source's own failure.
        #[source]
        FetchError,
    ),

    /// The caller's cancellation token fired before the value was available.
    #[error("the cache operation was cancelled")]
    Cancelled,

    /// A thumbnail set offered no rendition to fetch.
    #[error("no thumbnail rendition is available")]
    NoRendition,

    /// A URL was rejected before it could be fetched.
    ///
    /// Thumbnail URLs arrive inside provider responses, so they are untrusted input; validating
    /// here is what stops a drifted or hostile response from steering the fetcher at the local
    /// media gateway (§78).
    #[error("the URL was rejected before fetching")]
    UnsafeUrl {
        /// Why the URL was rejected.
        #[source]
        source: UrlError,
    },
}

impl CacheError {
    /// Wraps an [`io::Error`], capturing what is needed for classification and diagnostics.
    ///
    /// The error value itself is not retained: see the module docs on why [`CacheError`] is
    /// [`Clone`].
    #[must_use]
    pub fn io(operation: &'static str, path: impl std::fmt::Display, source: &io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_string(),
            kind: source.kind(),
            detail: source.to_string(),
        }
    }

    /// Whether this failure is the caller's own cancellation rather than a fault.
    ///
    /// Call sites use it to skip the log line and the failure statistic: a cancelled prefetch is
    /// the system working as designed, and counting it as a failure would make the diagnostics
    /// screen alarming during ordinary scrolling.
    #[must_use]
    pub const fn is_cancellation(&self) -> bool {
        matches!(self, Self::Cancelled | Self::Source(FetchError::Cancelled))
    }

    /// Whether the value can still be served even though the cache could not store it.
    ///
    /// True for the failures that describe the *cache's* inability to keep a value, as opposed to
    /// an inability to produce one.
    #[must_use]
    pub const fn is_storage_only(&self) -> bool {
        matches!(
            self,
            Self::Io { .. } | Self::EntryTooLarge { .. } | Self::Corrupt { .. }
        )
    }
}

impl From<FetchError> for CacheError {
    /// Maps a source failure into a cache failure.
    ///
    /// Written by hand rather than derived with `#[from]` so that a cancelled fetch collapses into
    /// [`CacheError::Cancelled`]. Without it, cancellation would arrive at the UI as a source
    /// failure and be offered a retry button for something the user themselves cancelled.
    fn from(value: FetchError) -> Self {
        match value {
            FetchError::Cancelled => Self::Cancelled,
            // Named rather than wildcarded so a future FetchError variant forces a decision here
            // instead of silently inheriting "source failure" semantics.
            other @ FetchError::Failed { .. } => Self::Source(other),
        }
    }
}

impl From<PathError> for CacheError {
    fn from(source: PathError) -> Self {
        Self::UnsafePath { source }
    }
}

impl DomainError for CacheError {
    /// Always [`ErrorKind::Cache`], including for I/O failures.
    ///
    /// See the module docs: [`ErrorKind::FileSystem`] escalates because it threatens user data, and
    /// nothing in this crate does.
    fn kind(&self) -> ErrorKind {
        ErrorKind::Cache
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "io_failed",
            Self::Corrupt { .. } => "corrupt",
            Self::KeyTooLong { .. } => "key_too_long",
            Self::InvalidNamespace { .. } => "invalid_namespace",
            Self::UnsafePath { .. } => "unsafe_path",
            Self::EntryTooLarge { .. } => "entry_too_large",
            Self::Source(_) => "source_failed",
            Self::Cancelled => "cancelled",
            Self::NoRendition => "no_rendition",
            Self::UnsafeUrl { .. } => "unsafe_url",
        }
    }

    fn recovery(&self) -> Recovery {
        match self {
            // A cache file that vanished under us is simply gone; rebuild the store rather than
            // retrying a read that will fail identically.
            Self::Io {
                kind: io::ErrorKind::NotFound,
                ..
            }
            | Self::Corrupt { .. } => Recovery::RebuildLocalData {
                store: self.store_path(),
            },
            // Permission problems do not clear on their own, and hammering the path helps nobody.
            Self::Io {
                kind: io::ErrorKind::PermissionDenied,
                ..
            } => Recovery::RetryManual,
            // On Windows the common transient cause is an antivirus scanner holding the file open
            // for a few hundred milliseconds. Retrying briefly turns that into an invisible hiccup.
            Self::Io { .. } => Recovery::RetryAutomatic {
                delay_ms: 250,
                attempts_made: 1,
                max_attempts: 3,
            },
            // The value exists, it simply will not be cached. Serve it and move on.
            Self::EntryTooLarge { .. } => Recovery::Fallback {
                message_key: "recovery.fallback.uncached".to_owned(),
            },
            Self::NoRendition => Recovery::Fallback {
                message_key: "recovery.fallback.placeholder_image".to_owned(),
            },
            // Forward the source's own judgement instead of second-guessing it: the network layer
            // knows whether it saw a timeout or a 404.
            Self::Source(FetchError::Failed { retryable, .. }) => {
                if *retryable {
                    Recovery::RetryAutomatic {
                        delay_ms: 500,
                        attempts_made: 1,
                        max_attempts: 3,
                    }
                } else {
                    Recovery::RetryManual
                }
            }
            // Nothing to recover: the caller asked for the work to stop, or the input was invalid
            // and will be invalid again.
            Self::Source(FetchError::Cancelled)
            | Self::Cancelled
            | Self::KeyTooLong { .. }
            | Self::InvalidNamespace { .. }
            | Self::UnsafePath { .. }
            | Self::UnsafeUrl { .. } => Recovery::Unrecoverable,
        }
    }

    fn params(&self) -> BTreeMap<String, String> {
        let mut params = BTreeMap::new();
        match self {
            Self::Corrupt { namespace, .. } => {
                params.insert("namespace".to_owned(), namespace.clone());
            }
            Self::InvalidNamespace { name } => {
                params.insert("namespace".to_owned(), name.clone());
            }
            Self::EntryTooLarge { size, max } => {
                params.insert("size".to_owned(), size.to_string());
                params.insert("max".to_owned(), max.to_string());
            }
            Self::KeyTooLong { len, max } => {
                params.insert("length".to_owned(), len.to_string());
                params.insert("max".to_owned(), max.to_string());
            }
            // Remaining variants carry no interpolation parameters.
            _ => {}
        }
        params
    }
}

impl CacheError {
    /// The `store` value used in [`Recovery::RebuildLocalData`].
    ///
    /// Namespaced where the namespace is known so that recovery rebuilds only the affected slice —
    /// a damaged thumbnail must not discard cached metadata as collateral.
    fn store_path(&self) -> String {
        match self {
            Self::Corrupt { namespace, .. } => format!("cache.{namespace}"),
            _ => "cache".to_owned(),
        }
    }
}

/// Convenience alias for cache results.
pub type CacheResult<T> = Result<T, CacheError>;

#[cfg(test)]
mod tests {
    use super::*;

    fn io_error(kind: io::ErrorKind) -> CacheError {
        CacheError::io("write", "C:\\cache\\x.bcx", &io::Error::new(kind, "boom"))
    }

    #[test]
    fn every_failure_classifies_as_a_cache_failure() {
        for error in [
            io_error(io::ErrorKind::Other),
            CacheError::Corrupt {
                namespace: "thumbnails".to_owned(),
                kind: CorruptionKind::ChecksumMismatch,
            },
            CacheError::Cancelled,
            CacheError::NoRendition,
        ] {
            assert_eq!(
                error.kind(),
                ErrorKind::Cache,
                "{error:?} must not escalate past the cache"
            );
            assert!(
                !error.kind().threatens_user_data(),
                "cache data is disposable and must never trigger the data-loss surface"
            );
            assert!(error.full_code().starts_with("cache."));
            assert_eq!(error.message_key(), format!("error.{}", error.full_code()));
        }
    }

    #[test]
    fn corruption_rebuilds_only_the_affected_namespace() {
        let error = CacheError::Corrupt {
            namespace: "thumbnails".to_owned(),
            kind: CorruptionKind::ChecksumMismatch,
        };
        assert_eq!(
            error.recovery(),
            Recovery::RebuildLocalData {
                store: "cache.thumbnails".to_owned()
            },
            "one bad thumbnail must not discard cached metadata"
        );
        assert_eq!(
            error
                .to_payload()
                .params
                .get("namespace")
                .map(String::as_str),
            Some("thumbnails")
        );
    }

    #[test]
    fn cancellation_is_never_offered_a_retry() {
        for error in [
            CacheError::Cancelled,
            CacheError::Source(FetchError::Cancelled),
        ] {
            assert!(error.is_cancellation());
            assert_eq!(error.recovery(), Recovery::Unrecoverable);
            assert!(!error.recovery().offers_retry());
        }
    }

    #[test]
    fn a_cancelled_fetch_does_not_arrive_as_a_source_failure() {
        // Without the hand-written `From`, the UI would show a retry affordance for work the user
        // cancelled themselves.
        assert_eq!(
            CacheError::from(FetchError::Cancelled),
            CacheError::Cancelled
        );
        assert!(matches!(
            CacheError::from(FetchError::permanent("404")),
            CacheError::Source(FetchError::Failed {
                retryable: false,
                ..
            })
        ));
    }

    #[test]
    fn source_retryability_is_forwarded_not_re_derived() {
        assert!(
            CacheError::Source(FetchError::retryable("timeout"))
                .recovery()
                .is_automatic()
        );
        assert_eq!(
            CacheError::Source(FetchError::permanent("gone")).recovery(),
            Recovery::RetryManual
        );
    }

    #[test]
    fn a_transient_io_error_retries_but_a_permission_error_does_not() {
        match io_error(io::ErrorKind::TimedOut).recovery() {
            Recovery::RetryAutomatic { max_attempts, .. } => assert!(
                max_attempts > 0 && max_attempts <= 5,
                "retries must be bounded"
            ),
            other => panic!("expected a bounded automatic retry, got {other:?}"),
        }
        assert_eq!(
            io_error(io::ErrorKind::PermissionDenied).recovery(),
            Recovery::RetryManual
        );
        assert_eq!(
            io_error(io::ErrorKind::NotFound).recovery(),
            Recovery::RebuildLocalData {
                store: "cache".to_owned()
            }
        );
    }

    #[test]
    fn an_oversized_entry_degrades_instead_of_failing() {
        let error = CacheError::EntryTooLarge {
            size: 100 * 1024 * 1024,
            max: 32 * 1024 * 1024,
        };
        assert!(
            matches!(error.recovery(), Recovery::Fallback { .. }),
            "the value is still usable, it just is not cached"
        );
        assert!(error.is_storage_only());
        let payload = error.to_payload();
        assert_eq!(
            payload.params.get("max").map(String::as_str),
            Some("33554432")
        );
    }

    #[test]
    fn failures_that_produced_no_value_are_not_storage_only() {
        assert!(!CacheError::Cancelled.is_storage_only());
        assert!(!CacheError::NoRendition.is_storage_only());
        assert!(!CacheError::Source(FetchError::permanent("x")).is_storage_only());
    }

    #[test]
    fn the_diagnostic_chain_reaches_the_root_cause() {
        let payload = CacheError::Corrupt {
            namespace: "metadata".to_owned(),
            kind: CorruptionKind::LengthMismatch {
                expected: 4096,
                actual: 17,
            },
        }
        .to_payload();
        let diagnostic = payload.diagnostic.unwrap_or_default();
        assert!(
            diagnostic.contains("4096") && diagnostic.contains("17"),
            "the truncation detail must survive into diagnostics: {diagnostic}"
        );
    }

    #[test]
    fn codes_are_unique_across_variants() {
        let codes = [
            io_error(io::ErrorKind::Other).code(),
            CacheError::Corrupt {
                namespace: String::new(),
                kind: CorruptionKind::BadMagic,
            }
            .code(),
            CacheError::KeyTooLong { len: 1, max: 0 }.code(),
            CacheError::InvalidNamespace {
                name: String::new(),
            }
            .code(),
            CacheError::UnsafePath {
                source: PathError::Traversal,
            }
            .code(),
            CacheError::EntryTooLarge { size: 1, max: 0 }.code(),
            CacheError::Source(FetchError::Cancelled).code(),
            CacheError::Cancelled.code(),
            CacheError::NoRendition.code(),
            CacheError::UnsafeUrl {
                source: UrlError::MissingHost,
            }
            .code(),
        ];
        let mut unique: Vec<_> = codes.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), codes.len(), "error codes must not collide");
    }
}
