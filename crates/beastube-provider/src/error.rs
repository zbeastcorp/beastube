//! Provider errors.
//!
//! The important distinction is between "this content cannot be shown" and "the extractor is
//! broken". They look similar at the call site and mean opposite things: the first is a normal
//! outcome the user should see explained, the second is an early warning that the external service
//! changed and the adapter needs updating (§119).
//!
//! [`ProviderError::SchemaDrift`] exists for exactly that second case, and is deliberately never
//! folded into a generic parse failure.

use std::collections::BTreeMap;

use beastube_core::error::{DomainError, ErrorKind, Recovery};

/// Why a specific item cannot be shown.
///
/// These are outcomes, not faults: each has a distinct explanation the UI renders, and none of them
/// indicates anything wrong with the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unavailability {
    /// No such item, or it was deleted.
    NotFound,
    /// Withheld in the user's region.
    GeoBlocked,
    /// Requires confirming the viewer's age.
    AgeRestricted,
    /// Restricted to channel members.
    MembersOnly,
    /// Requires a purchase or subscription.
    Paid,
    /// The uploader made it private.
    Private,
    /// Scheduled but not yet broadcasting.
    NotYetBroadcast,
    /// The provider declined to say why.
    Unspecified,
}

impl Unavailability {
    /// Stable identifier, forming the i18n key suffix and the diagnostics code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::GeoBlocked => "geo_blocked",
            Self::AgeRestricted => "age_restricted",
            Self::MembersOnly => "members_only",
            Self::Paid => "paid",
            Self::Private => "private",
            Self::NotYetBroadcast => "not_yet_broadcast",
            Self::Unspecified => "unavailable",
        }
    }

    /// Whether waiting or retrying could ever change the outcome.
    ///
    /// Only an unstarted broadcast can: everything else is a property of the content or the viewer,
    /// so offering a retry would be a lie.
    #[must_use]
    pub const fn could_change(self) -> bool {
        matches!(self, Self::NotYetBroadcast)
    }
}

/// A provider-layer failure.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// The item exists in principle but cannot be shown.
    #[error("content is unavailable: {reason:?}")]
    Unavailable {
        /// Why.
        reason: Unavailability,
    },

    /// The provider answered, but not in a shape this build understands.
    ///
    /// This is the early-warning signal that the external service changed (§119). It is kept
    /// separate from every other parse failure precisely so it can be counted and surfaced on the
    /// diagnostics screen rather than blending into generic noise.
    #[error("could not read the provider response for {operation}")]
    SchemaDrift {
        /// Which operation failed to parse, e.g. `search`, `playlist`.
        operation: &'static str,
        /// Engineer-facing detail from the extractor.
        detail: String,
    },

    /// The provider refused the request rate.
    #[error("the provider is rate limiting requests")]
    RateLimited {
        /// Seconds to wait before retrying, when the provider says.
        retry_after_secs: Option<u64>,
    },

    /// A transport failure reaching the provider.
    #[error("could not reach the provider")]
    Transport {
        /// Engineer-facing detail.
        detail: String,
    },

    /// The operation is not implemented by this adapter.
    ///
    /// Callers should not reach this: [`crate::ProviderCapabilities`] declares what is supported
    /// and the UI hides the rest. It exists so that a caller which ignores capabilities fails
    /// loudly rather than silently returning nothing.
    #[error("{operation} is not supported by the {provider} provider")]
    Unsupported {
        /// Which operation was attempted.
        operation: &'static str,
        /// Which adapter refused it.
        provider: &'static str,
    },

    /// A caller-supplied argument was invalid.
    #[error("invalid {field}: {reason}")]
    InvalidInput {
        /// Which argument.
        field: &'static str,
        /// Why it was rejected.
        reason: String,
    },

    /// The request was cancelled before it completed.
    #[error("the request was cancelled")]
    Cancelled,
}

impl ProviderError {
    /// Whether repeating the request could plausibly succeed.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        match self {
            Self::Transport { .. } | Self::RateLimited { .. } => true,
            Self::Unavailable { reason } => reason.could_change(),
            // Schema drift will fail identically until the adapter is updated; retrying it just
            // multiplies the load and the log noise.
            Self::SchemaDrift { .. }
            | Self::Unsupported { .. }
            | Self::InvalidInput { .. }
            | Self::Cancelled => false,
        }
    }

    /// Whether this indicates the extractor needs updating rather than a content or network issue.
    #[must_use]
    pub const fn indicates_drift(&self) -> bool {
        matches!(self, Self::SchemaDrift { .. })
    }
}

impl DomainError for ProviderError {
    fn kind(&self) -> ErrorKind {
        match self {
            // A transport failure is a network problem that happens to surface here; classifying it
            // as a provider fault would send the user to the wrong explanation.
            Self::Transport { .. } => ErrorKind::Network,
            _ => ErrorKind::Provider,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Unavailable { reason } => reason.as_str(),
            Self::SchemaDrift { .. } => "schema_drift",
            Self::RateLimited { .. } => "rate_limited",
            Self::Transport { .. } => "connect_failed",
            Self::Unsupported { .. } => "unsupported",
            Self::InvalidInput { .. } => "invalid_input",
            Self::Cancelled => "cancelled",
        }
    }

    // Variants sharing a recovery today are still distinct failures; merging the arms would
    // couple rules expected to diverge.
    #[allow(clippy::match_same_arms)]
    fn recovery(&self) -> Recovery {
        match self {
            Self::Transport { .. } => Recovery::RetryAutomatic {
                delay_ms: 800,
                attempts_made: 1,
                max_attempts: 3,
            },
            Self::RateLimited { retry_after_secs } => Recovery::RetryAutomatic {
                // Believe the provider when it says how long; otherwise back off well clear of
                // whatever tripped the limit.
                delay_ms: retry_after_secs.unwrap_or(30).saturating_mul(1000),
                attempts_made: 1,
                max_attempts: 2,
            },
            Self::Unavailable { reason } if reason.could_change() => Recovery::RetryManual,
            // Nothing the user or the application can do; an update to the adapter is required.
            Self::SchemaDrift { .. } => Recovery::Unrecoverable,
            Self::Unavailable { .. }
            | Self::Unsupported { .. }
            | Self::InvalidInput { .. }
            | Self::Cancelled => Recovery::Unrecoverable,
        }
    }

    fn params(&self) -> BTreeMap<String, String> {
        let mut params = BTreeMap::new();
        match self {
            Self::SchemaDrift { operation, .. } => {
                params.insert("operation".to_owned(), (*operation).to_owned());
            }
            Self::Unsupported {
                operation,
                provider,
            } => {
                params.insert("operation".to_owned(), (*operation).to_owned());
                params.insert("provider".to_owned(), (*provider).to_owned());
            }
            Self::InvalidInput { field, reason } => {
                params.insert("field".to_owned(), (*field).to_owned());
                params.insert("reason".to_owned(), reason.clone());
            }
            Self::RateLimited {
                retry_after_secs: Some(seconds),
            } => {
                params.insert("seconds".to_owned(), seconds.to_string());
            }
            _ => {}
        }
        params
    }
}

/// Convenience alias for provider results.
pub type ProviderResult<T> = Result<T, ProviderError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transport_failure_is_classified_as_a_network_problem() {
        // Otherwise the user is told the content is broken when their connection is.
        let error = ProviderError::Transport {
            detail: "dns".to_owned(),
        };
        assert_eq!(error.kind(), ErrorKind::Network);
        assert_eq!(error.full_code(), "network.connect_failed");
    }

    #[test]
    fn unavailability_reasons_each_get_their_own_message_key() {
        let mut seen = std::collections::HashSet::new();
        for reason in [
            Unavailability::NotFound,
            Unavailability::GeoBlocked,
            Unavailability::AgeRestricted,
            Unavailability::MembersOnly,
            Unavailability::Paid,
            Unavailability::Private,
            Unavailability::NotYetBroadcast,
            Unavailability::Unspecified,
        ] {
            let key = ProviderError::Unavailable { reason }.message_key();
            assert!(seen.insert(key.clone()), "duplicate message key: {key}");
            assert!(key.starts_with("error.provider."));
        }
    }

    #[test]
    fn only_an_unstarted_broadcast_is_worth_retrying() {
        assert!(
            ProviderError::Unavailable {
                reason: Unavailability::NotYetBroadcast
            }
            .is_retryable()
        );
        for reason in [
            Unavailability::NotFound,
            Unavailability::GeoBlocked,
            Unavailability::AgeRestricted,
            Unavailability::Private,
        ] {
            assert!(
                !ProviderError::Unavailable { reason }.is_retryable(),
                "{reason:?} is a property of the content, so a retry would be a lie"
            );
        }
    }

    #[test]
    fn schema_drift_is_never_retried_and_is_identifiable() {
        let error = ProviderError::SchemaDrift {
            operation: "playlist",
            detail: "itemSectionRenderer empty".to_owned(),
        };
        assert!(error.indicates_drift());
        assert!(
            !error.is_retryable(),
            "it will fail identically until updated"
        );
        assert_eq!(error.recovery(), Recovery::Unrecoverable);
        assert_eq!(
            error
                .to_payload()
                .params
                .get("operation")
                .map(String::as_str),
            Some("playlist")
        );
    }

    #[test]
    fn rate_limiting_believes_the_provider_when_it_says_how_long() {
        let told = ProviderError::RateLimited {
            retry_after_secs: Some(5),
        };
        match told.recovery() {
            Recovery::RetryAutomatic { delay_ms, .. } => assert_eq!(delay_ms, 5_000),
            other => panic!("expected an automatic retry, got {other:?}"),
        }

        let untold = ProviderError::RateLimited {
            retry_after_secs: None,
        };
        match untold.recovery() {
            Recovery::RetryAutomatic { delay_ms, .. } => {
                assert!(
                    delay_ms >= 30_000,
                    "back off well clear of whatever tripped it"
                );
            }
            other => panic!("expected an automatic retry, got {other:?}"),
        }
    }

    #[test]
    fn an_absurd_retry_after_does_not_overflow() {
        let hostile = ProviderError::RateLimited {
            retry_after_secs: Some(u64::MAX),
        };
        match hostile.recovery() {
            Recovery::RetryAutomatic { delay_ms, .. } => assert_eq!(delay_ms, u64::MAX),
            other => panic!("expected an automatic retry, got {other:?}"),
        }
    }

    #[test]
    fn an_unsupported_operation_names_both_sides() {
        let payload = ProviderError::Unsupported {
            operation: "playlist",
            provider: "youtube",
        }
        .to_payload();
        assert_eq!(
            payload.params.get("operation").map(String::as_str),
            Some("playlist")
        );
        assert_eq!(
            payload.params.get("provider").map(String::as_str),
            Some("youtube")
        );
    }

    #[test]
    fn no_error_message_reaches_the_ui_as_english() {
        // Every payload carries a key, and the diagnostic is engineer-facing only.
        for error in [
            ProviderError::Cancelled,
            ProviderError::Unavailable {
                reason: Unavailability::GeoBlocked,
            },
            ProviderError::Transport {
                detail: "x".to_owned(),
            },
        ] {
            let payload = error.to_payload();
            assert!(payload.message_key.starts_with("error."));
            assert!(!payload.message_key.contains(' '));
        }
    }
}
