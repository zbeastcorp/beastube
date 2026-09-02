//! Filtering failures, and the recovery each one implies.
//!
//! There is one error type for the whole subsystem rather than one per module, because every
//! failure here has the same two audiences: the rule-set update path, which must decide whether to
//! keep the previous rule set, and the settings screen, which must point the user at the rule they
//! mistyped. Splitting the type further would duplicate that single decision.
//!
//! Three conventions are load-bearing:
//!
//! * **Prose lives in `Display`, machine values live in [`DomainError::params`].** A regex compile
//!   error is English text produced by the `regex` crate; it belongs in the diagnostic chain, never
//!   in an i18n parameter that the UI would render verbatim (§109). Every `params` entry here is a
//!   count, an enum discriminant or a version string.
//! * **Untrusted strings are truncated before they enter an error.** A pattern comes from a
//!   downloaded rule set or a hand-edited settings file, so [`shorten`] bounds what a hostile input
//!   can push into a log line or an error payload.
//! * **The same underlying problem recovers differently depending on where it was found.** A bad
//!   pattern typed by the user is fixed by editing settings; the identical pattern inside a
//!   candidate rule set is recovered by keeping the previous set. That is why
//!   [`FilterError::InvalidRule`] wraps rather than replaces the inner error: it keeps the inner
//!   classification and overrides only the recovery.

use std::collections::BTreeMap;
use std::fmt;

use beastube_core::error::{DomainError, ErrorKind, Recovery};

use crate::rule::RuleKind;

/// Settings path offered when the user's own rule list is at fault.
const CUSTOM_RULES_PATH: &str = "filtering.custom_rules";

/// i18n key for the degraded path taken when a candidate rule set is refused: the previously
/// validated set stays active, so filtering keeps working at the last known-good version.
const PREVIOUS_SET_FALLBACK: &str = "recovery.fallback.previous_rule_set";

/// Maximum number of characters of an untrusted string that may appear in an error.
///
/// Long enough to identify the offending value at a glance, short enough that a 4 KiB pattern
/// cannot be used to flood the log or the diagnostics screen.
pub const MAX_ECHOED_CHARS: usize = 64;

/// Truncates an untrusted string on a character boundary for inclusion in an error.
///
/// Truncation is marked with `…` so a reader can tell a short value from a clipped one.
#[must_use]
pub fn shorten(value: &str) -> String {
    if value.chars().count() <= MAX_ECHOED_CHARS {
        return value.to_owned();
    }
    let mut out: String = value.chars().take(MAX_ECHOED_CHARS).collect();
    out.push('…');
    out
}

/// Why a rule-set version string was refused.
///
/// A version is a database primary key, a filename-safe token and a diagnostics field at once, so
/// it is validated as strictly as an identifier rather than accepted as free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VersionProblem {
    /// The version was empty.
    Empty,
    /// The version exceeded [`crate::ruleset::MAX_VERSION_LEN`].
    TooLong,
    /// The version contained a character outside `[A-Za-z0-9.+_-]`.
    InvalidCharacter,
}

impl VersionProblem {
    /// Stable discriminant for i18n parameters and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::TooLong => "too_long",
            Self::InvalidCharacter => "invalid_character",
        }
    }
}

impl fmt::Display for VersionProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a pattern was refused as too risky to compile.
///
/// See [`crate::ruleset`] for the heuristic and its deliberate false-positive profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PatternRisk {
    /// A quantifier is applied to a group that already contains one, e.g. `(a+)+`.
    NestedQuantifier,
    /// A counted repetition asks for more copies than the ceiling permits, e.g. `a{5000}`.
    ExcessiveRepetition,
    /// The pattern uses more quantifiers than the budget allows.
    TooManyQuantifiers,
}

impl PatternRisk {
    /// Stable discriminant for i18n parameters and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NestedQuantifier => "nested_quantifier",
            Self::ExcessiveRepetition => "excessive_repetition",
            Self::TooManyQuantifiers => "too_many_quantifiers",
        }
    }
}

impl fmt::Display for PatternRisk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a creator-marked segment was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmentProblem {
    /// The segment ends before it starts, which no clamping can repair.
    EndBeforeStart,
    /// The category was empty.
    EmptyCategory,
    /// The category exceeded [`crate::segment::MAX_CATEGORY_LEN`].
    CategoryTooLong,
    /// The category contained a character outside `[a-z0-9_-]`.
    InvalidCharacter,
}

impl SegmentProblem {
    /// Stable discriminant for i18n parameters and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EndBeforeStart => "end_before_start",
            Self::EmptyCategory => "empty_category",
            Self::CategoryTooLong => "category_too_long",
            Self::InvalidCharacter => "invalid_character",
        }
    }
}

impl fmt::Display for SegmentProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A content-filtering failure.
#[derive(Debug, thiserror::Error)]
pub enum FilterError {
    /// A rule inside a candidate rule set failed validation.
    ///
    /// The wrapper exists to add the rule's position without losing the inner classification: the
    /// dotted code stays that of the inner error (`filtering.unsafe_pattern`, say) so telemetry-free
    /// diagnostics and tests key off the real cause, while the recovery becomes "keep the previous
    /// rule set", because that is what the update path can actually do about it.
    #[error("rule {index} failed validation")]
    InvalidRule {
        /// Zero-based position of the rule within the candidate set.
        index: usize,
        /// The rule-level failure.
        #[source]
        source: Box<FilterError>,
    },

    /// A rule pattern was empty. An empty pattern would match everything or nothing depending on
    /// the kind, and both readings are wrong, so it is refused rather than guessed at.
    #[error("{kind} rule has an empty pattern")]
    EmptyPattern {
        /// The rule kind whose pattern was empty.
        kind: RuleKind,
    },

    /// A rule pattern exceeded [`crate::ruleset::MAX_PATTERN_LEN`].
    #[error("{kind} rule pattern is {len} characters, maximum is {max}")]
    PatternTooLong {
        /// The rule kind that was rejected.
        kind: RuleKind,
        /// Actual length in characters.
        len: usize,
        /// Maximum permitted length.
        max: usize,
    },

    /// A rule pattern could not be interpreted for its kind: a host pattern containing a path, a
    /// regex the engine refuses to compile, a control character anywhere.
    #[error("{kind} rule pattern is malformed: {detail}")]
    MalformedPattern {
        /// The rule kind that was rejected.
        kind: RuleKind,
        /// Engineer-facing explanation. Never rendered to the user: it is not localizable.
        detail: String,
    },

    /// A rule pattern was syntactically valid but refused by the safety heuristic.
    #[error("{kind} rule pattern refused as unsafe: {risk}")]
    UnsafePattern {
        /// The rule kind that was rejected.
        kind: RuleKind,
        /// Which risk the heuristic detected.
        risk: PatternRisk,
    },

    /// The candidate rule set holds more rules than [`crate::ruleset::MAX_RULES`].
    #[error("rule set holds {count} rules, maximum is {max}")]
    TooManyRules {
        /// Number of rules offered.
        count: usize,
        /// Maximum permitted.
        max: usize,
    },

    /// The candidate rule set's version string is unusable.
    #[error("rule set version is invalid: {problem}")]
    InvalidVersion {
        /// Which check failed.
        problem: VersionProblem,
    },

    /// The rule set's contents do not hash to the checksum that accompanied it.
    ///
    /// This detects truncation and accidental corruption, not tampering — see
    /// [`crate::ruleset::RuleSet::checksum`] for why the distinction matters.
    #[error("rule set checksum mismatch: expected {expected}, computed {actual}")]
    ChecksumMismatch {
        /// Checksum supplied with the set.
        expected: String,
        /// Checksum computed from the set's contents.
        actual: String,
    },

    /// Activation was refused because this version was rolled back earlier in the session.
    ///
    /// Without this, an updater that keeps offering the same set would reinstall it after every
    /// rollback, and the user would see playback break on a loop.
    #[error("rule set version {version} was rolled back and will not be reactivated")]
    PreviouslyRolledBack {
        /// The refused version.
        version: String,
    },

    /// A creator-marked segment could not be modelled.
    #[error("segment is invalid: {problem}")]
    InvalidSegment {
        /// Which check failed.
        problem: SegmentProblem,
    },

    /// A persisted or transmitted enum value is not one this build knows.
    ///
    /// Produced when decoding a database row or a rule-set document: the vocabulary is pinned by a
    /// `CHECK` constraint in the schema, so an unknown value means drift, corruption, or a
    /// hand-edited file.
    #[error("`{value}` is not a valid {field}")]
    UnknownEnumValue {
        /// Which vocabulary was being decoded, e.g. `rule_kind`.
        field: &'static str,
        /// The offending value, truncated by [`shorten`].
        value: String,
    },
}

impl FilterError {
    /// Wraps a rule-level failure with its position in the candidate set.
    #[must_use]
    pub fn at_rule(index: usize, source: Self) -> Self {
        Self::InvalidRule {
            index,
            source: Box::new(source),
        }
    }

    /// Builds an [`FilterError::UnknownEnumValue`], truncating the untrusted value.
    #[must_use]
    pub fn unknown_enum_value(field: &'static str, value: &str) -> Self {
        Self::UnknownEnumValue {
            field,
            value: shorten(value),
        }
    }

    /// The innermost failure, unwrapping any [`FilterError::InvalidRule`] wrappers.
    ///
    /// Callers that branch on the cause use this so that adding the position wrapper never changes
    /// their behaviour.
    #[must_use]
    pub fn root_cause(&self) -> &Self {
        match self {
            Self::InvalidRule { source, .. } => source.root_cause(),
            other => other,
        }
    }

    /// Position of the offending rule, when the failure came from validating a set.
    #[must_use]
    pub const fn rule_index(&self) -> Option<usize> {
        match self {
            Self::InvalidRule { index, .. } => Some(*index),
            _ => None,
        }
    }

    /// Whether this failure concerns one rule rather than the set as a whole.
    ///
    /// The settings screen uses it to decide between highlighting a row and showing a set-level
    /// banner.
    #[must_use]
    pub const fn is_rule_level(&self) -> bool {
        matches!(
            self.root_cause_const(),
            Self::EmptyPattern { .. }
                | Self::PatternTooLong { .. }
                | Self::MalformedPattern { .. }
                | Self::UnsafePattern { .. }
        )
    }

    /// `const`-compatible unwrap of one wrapper level, used by [`Self::is_rule_level`].
    ///
    /// Recursion is not permitted in a `const fn` here, and one level is enough: validation never
    /// nests wrappers.
    const fn root_cause_const(&self) -> &Self {
        match self {
            Self::InvalidRule { source, .. } => source,
            other => other,
        }
    }
}

impl DomainError for FilterError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Filtering
    }

    fn code(&self) -> &'static str {
        match self {
            // Delegates so the dotted code names the real cause, not the wrapper.
            Self::InvalidRule { source, .. } => source.code(),
            Self::EmptyPattern { .. } => "empty_pattern",
            Self::PatternTooLong { .. } => "pattern_too_long",
            Self::MalformedPattern { .. } => "malformed_pattern",
            Self::UnsafePattern { .. } => "unsafe_pattern",
            Self::TooManyRules { .. } => "too_many_rules",
            Self::InvalidVersion { .. } => "invalid_version",
            Self::ChecksumMismatch { .. } => "checksum_mismatch",
            Self::PreviouslyRolledBack { .. } => "previously_rolled_back",
            Self::InvalidSegment { .. } => "invalid_segment",
            Self::UnknownEnumValue { .. } => "unknown_enum_value",
        }
    }

    // Variants that share a body today are still distinct failures;
    // merging the arms would couple rules expected to diverge.
    #[allow(clippy::match_same_arms)]
    fn recovery(&self) -> Recovery {
        match self {
            // A bad rule found inside a candidate set is recovered by not activating that set. The
            // user cannot edit a downloaded rule, so offering them a settings path would be a dead
            // end (§131: no affordance that does nothing).
            Self::InvalidRule { .. }
            | Self::TooManyRules { .. }
            | Self::InvalidVersion { .. }
            | Self::ChecksumMismatch { .. }
            | Self::PreviouslyRolledBack { .. }
            | Self::UnknownEnumValue { .. } => Recovery::Fallback {
                message_key: PREVIOUS_SET_FALLBACK.to_owned(),
            },
            // The same failure reached directly means the user typed the rule.
            Self::EmptyPattern { .. }
            | Self::PatternTooLong { .. }
            | Self::MalformedPattern { .. }
            | Self::UnsafePattern { .. } => Recovery::AdjustSettings {
                settings_path: CUSTOM_RULES_PATH.to_owned(),
            },
            // One unusable segment is dropped; there is nothing to retry and nothing to adjust.
            Self::InvalidSegment { .. } => Recovery::Unrecoverable,
        }
    }

    // Variants that share a body today are still distinct failures;
    // merging the arms would couple rules expected to diverge.
    #[allow(clippy::match_same_arms)]
    fn params(&self) -> BTreeMap<String, String> {
        let mut params = BTreeMap::new();
        match self {
            Self::InvalidRule { index, source } => {
                params = source.params();
                params.insert("index".to_owned(), index.to_string());
            }
            Self::EmptyPattern { kind } => {
                params.insert("kind".to_owned(), kind.as_str().to_owned());
            }
            Self::PatternTooLong { kind, len, max } => {
                params.insert("kind".to_owned(), kind.as_str().to_owned());
                params.insert("len".to_owned(), len.to_string());
                params.insert("max".to_owned(), max.to_string());
            }
            // `detail` is deliberately absent: it is untranslatable engineer prose and reaches the
            // diagnostics screen through the source chain instead.
            Self::MalformedPattern { kind, .. } => {
                params.insert("kind".to_owned(), kind.as_str().to_owned());
            }
            Self::UnsafePattern { kind, risk } => {
                params.insert("kind".to_owned(), kind.as_str().to_owned());
                params.insert("risk".to_owned(), risk.as_str().to_owned());
            }
            Self::TooManyRules { count, max } => {
                params.insert("count".to_owned(), count.to_string());
                params.insert("max".to_owned(), max.to_string());
            }
            Self::InvalidVersion { problem } => {
                params.insert("problem".to_owned(), problem.as_str().to_owned());
            }
            Self::ChecksumMismatch { expected, actual } => {
                params.insert("expected".to_owned(), expected.clone());
                params.insert("actual".to_owned(), actual.clone());
            }
            Self::PreviouslyRolledBack { version } => {
                params.insert("version".to_owned(), version.clone());
            }
            Self::InvalidSegment { problem } => {
                params.insert("problem".to_owned(), problem.as_str().to_owned());
            }
            Self::UnknownEnumValue { field, value } => {
                params.insert("field".to_owned(), (*field).to_owned());
                params.insert("value".to_owned(), value.clone());
            }
        }
        params
    }
}

/// Convenience alias for filtering results.
pub type FilterResult<T> = Result<T, FilterError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_classify_as_filtering_failures() {
        let err = FilterError::EmptyPattern {
            kind: RuleKind::BlockHost,
        };
        assert_eq!(err.kind(), ErrorKind::Filtering);
        assert_eq!(err.full_code(), "filtering.empty_pattern");
        assert_eq!(err.message_key(), "error.filtering.empty_pattern");
    }

    #[test]
    fn the_position_wrapper_keeps_the_inner_code_but_changes_the_recovery() {
        let inner = FilterError::UnsafePattern {
            kind: RuleKind::BlockUrl,
            risk: PatternRisk::NestedQuantifier,
        };
        let wrapped = FilterError::at_rule(7, inner);

        assert_eq!(
            wrapped.full_code(),
            "filtering.unsafe_pattern",
            "the wrapper must not mask which check failed"
        );
        assert_eq!(wrapped.rule_index(), Some(7));
        assert!(matches!(
            wrapped.recovery(),
            Recovery::Fallback { message_key } if message_key == PREVIOUS_SET_FALLBACK
        ));
        assert!(matches!(
            wrapped.root_cause(),
            FilterError::UnsafePattern { .. }
        ));
    }

    #[test]
    fn a_rule_the_user_typed_recovers_by_editing_settings() {
        let recovery = FilterError::MalformedPattern {
            kind: RuleKind::AllowHost,
            detail: "host pattern contains a path separator".to_owned(),
        }
        .recovery();
        assert_eq!(
            recovery,
            Recovery::AdjustSettings {
                settings_path: CUSTOM_RULES_PATH.to_owned()
            }
        );
    }

    #[test]
    fn params_never_carry_untranslatable_prose() {
        let payload = FilterError::MalformedPattern {
            kind: RuleKind::BlockUrl,
            detail: "regex parse error: unclosed group".to_owned(),
        }
        .to_payload();

        assert_eq!(
            payload.params.get("kind").map(String::as_str),
            Some("block_url")
        );
        assert!(
            !payload.params.values().any(|v| v.contains("regex parse")),
            "engineer prose must stay out of i18n parameters: {:?}",
            payload.params
        );
        assert!(
            payload
                .diagnostic
                .as_deref()
                .is_some_and(|d| d.contains("unclosed group")),
            "but it must survive in the diagnostic chain"
        );
    }

    #[test]
    fn wrapped_params_carry_both_the_cause_and_the_position() {
        let payload = FilterError::at_rule(
            3,
            FilterError::PatternTooLong {
                kind: RuleKind::HideKeyword,
                len: 900,
                max: 512,
            },
        )
        .to_payload();

        assert_eq!(payload.params.get("index").map(String::as_str), Some("3"));
        assert_eq!(payload.params.get("len").map(String::as_str), Some("900"));
        assert_eq!(payload.params.get("max").map(String::as_str), Some("512"));
    }

    #[test]
    fn untrusted_values_are_truncated_before_reaching_an_error() {
        let hostile = "x".repeat(4096);
        let err = FilterError::unknown_enum_value("rule_kind", &hostile);
        let rendered = err.to_string();
        assert!(
            rendered.chars().count() < 200,
            "a hostile value must not flood the log: {} chars",
            rendered.chars().count()
        );
        assert!(rendered.contains('…'), "truncation must be visible");
    }

    #[test]
    fn shorten_splits_on_character_boundaries() {
        let multibyte = "é".repeat(MAX_ECHOED_CHARS * 2);
        let shortened = shorten(&multibyte);
        assert_eq!(shortened.chars().count(), MAX_ECHOED_CHARS + 1);
        assert_eq!(shorten("short"), "short");
    }

    #[test]
    fn set_level_failures_are_not_reported_as_rule_level() {
        assert!(!FilterError::TooManyRules { count: 10, max: 5 }.is_rule_level());
        assert!(
            FilterError::at_rule(
                0,
                FilterError::EmptyPattern {
                    kind: RuleKind::HideChannel
                }
            )
            .is_rule_level()
        );
    }

    #[test]
    fn a_dropped_segment_offers_no_false_retry() {
        assert_eq!(
            FilterError::InvalidSegment {
                problem: SegmentProblem::EndBeforeStart
            }
            .recovery(),
            Recovery::Unrecoverable
        );
    }

    #[test]
    fn discriminants_are_stable_strings() {
        assert_eq!(VersionProblem::TooLong.as_str(), "too_long");
        assert_eq!(PatternRisk::NestedQuantifier.as_str(), "nested_quantifier");
        assert_eq!(SegmentProblem::EndBeforeStart.as_str(), "end_before_start");
    }
}
