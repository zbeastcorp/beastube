//! The rule record: one filtering decision, expressed as data.
//!
//! A rule is deliberately *inert data*. It carries no compiled state and no behaviour beyond
//! answering "do you apply right now?", so that:
//!
//! * it round-trips through the `filtering_rules` table and through JSON without a lossy step,
//! * a rule set can be read, diffed and checksummed before anything is compiled or activated,
//! * the expensive part (regex compilation, bucketing) happens exactly once, at validation, and
//!   produces a [`CompiledRule`](crate::ruleset::CompiledRule) that the engine can use without
//!   further checks.
//!
//! ## Why these seven kinds and no others
//!
//! The vocabulary matches the `CHECK (kind IN (…))` constraint on `filtering_rules` one for one.
//! Adding a kind here without adding it there produces rows the database refuses to store, so the
//! two lists are kept literally identical and [`RuleKind::ALL`] is asserted against the schema
//! vocabulary in this module's tests.
//!
//! The kinds cover the three things ADR 0001 established as legitimately filterable — third-party
//! request blocking, feed content the user does not want, and creator-marked segments — and
//! nothing else. There is no rule kind that can express "suppress the provider's advertising",
//! because the subsystem does not do that.
//!
//! ## Two dimensions of inertness
//!
//! A rule can be switched off in three independent ways, and all three are checked before it is
//! ever matched: `enabled` (the user's toggle), `expires_at` (a temporary rule that lapses), and
//! `min_mode` (a rule that only applies in Strict). Keeping them separate means a rule that lapses
//! is distinguishable from one the user disabled, which the settings screen shows differently.

use std::fmt;
use std::str::FromStr;

use beastube_core::settings::FilteringMode;
use beastube_core::time_util::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::{FilterError, FilterResult};

/// What a rule does, and what it is matched against.
///
/// The serialized form is the exact vocabulary of the `filtering_rules.kind` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    /// Block requests to a host or any of its subdomains.
    BlockHost,
    /// Block requests whose full URL matches a regular expression.
    BlockUrl,
    /// Permit requests to a host or any of its subdomains, overriding every block rule.
    AllowHost,
    /// Permit requests whose full URL matches a regular expression, overriding every block rule.
    AllowUrl,
    /// Hide feed items from a channel, matched by identifier or by display name.
    HideChannel,
    /// Hide feed items whose title contains a keyword.
    HideKeyword,
    /// Offer to skip creator-marked segments of a category.
    SkipSegment,
}

impl RuleKind {
    /// Every kind, in the order the database `CHECK` constraint lists them.
    ///
    /// The order is also the bucket order inside a validated rule set, so it must stay stable.
    pub const ALL: [Self; 7] = [
        Self::BlockHost,
        Self::BlockUrl,
        Self::AllowHost,
        Self::AllowUrl,
        Self::HideChannel,
        Self::HideKeyword,
        Self::SkipSegment,
    ];

    /// Number of distinct kinds. Used to size the per-kind index in a validated set.
    pub const COUNT: usize = Self::ALL.len();

    /// Stable identifier, identical to the value stored in `filtering_rules.kind`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BlockHost => "block_host",
            Self::BlockUrl => "block_url",
            Self::AllowHost => "allow_host",
            Self::AllowUrl => "allow_url",
            Self::HideChannel => "hide_channel",
            Self::HideKeyword => "hide_keyword",
            Self::SkipSegment => "skip_segment",
        }
    }

    /// Dense index into a per-kind table, matching [`RuleKind::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::BlockHost => 0,
            Self::BlockUrl => 1,
            Self::AllowHost => 2,
            Self::AllowUrl => 3,
            Self::HideChannel => 4,
            Self::HideKeyword => 5,
            Self::SkipSegment => 6,
        }
    }

    /// Whether a match permits a request. Allow kinds can only ever produce an allow decision.
    #[must_use]
    pub const fn is_allow(self) -> bool {
        matches!(self, Self::AllowHost | Self::AllowUrl)
    }

    /// Whether a match denies a request.
    #[must_use]
    pub const fn is_block(self) -> bool {
        matches!(self, Self::BlockHost | Self::BlockUrl)
    }

    /// Whether this kind participates in request filtering at all.
    #[must_use]
    pub const fn filters_requests(self) -> bool {
        self.is_allow() || self.is_block()
    }

    /// Whether this kind hides items from feeds rather than blocking requests.
    #[must_use]
    pub const fn filters_content(self) -> bool {
        matches!(self, Self::HideChannel | Self::HideKeyword)
    }

    /// Whether the pattern is a regular expression rather than a literal.
    ///
    /// Only the URL kinds are regex-bearing; every other kind matches literally, which is what
    /// keeps a typo in a host rule from silently becoming a wildcard.
    #[must_use]
    pub const fn is_regex(self) -> bool {
        matches!(self, Self::BlockUrl | Self::AllowUrl)
    }
}

impl fmt::Display for RuleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RuleKind {
    type Err = FilterError;

    fn from_str(s: &str) -> FilterResult<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == s)
            .ok_or_else(|| FilterError::unknown_enum_value("rule_kind", s))
    }
}

/// The lowest [`FilteringMode`] at which a rule applies.
///
/// This is how Standard and Strict differ without maintaining two rule sets: one set is shipped,
/// and the lower-confidence half of it is marked `Strict`. A mode change therefore takes effect
/// immediately and needs no rule reload, which is what [`FilteringSettings`] promises when it says
/// rules stay loaded while filtering is off.
///
/// [`FilteringSettings`]: beastube_core::settings::FilteringSettings
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RuleMode {
    /// Applies in both Standard and Strict. High-confidence matches only.
    #[default]
    Standard,
    /// Applies in Strict only. Documented as more likely to produce false positives.
    Strict,
}

impl RuleMode {
    /// Stable identifier, identical to the value stored in `filtering_rules.min_mode`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Strict => "strict",
        }
    }

    /// Whether a rule with this minimum applies while the application runs in `mode`.
    ///
    /// [`FilteringMode::Off`] disables everything: rules stay loaded so the mode can be changed
    /// without a restart, but nothing matches.
    #[must_use]
    pub const fn applies_in(self, mode: FilteringMode) -> bool {
        match mode {
            FilteringMode::Off => false,
            FilteringMode::Standard => matches!(self, Self::Standard),
            FilteringMode::Strict => true,
        }
    }
}

impl fmt::Display for RuleMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RuleMode {
    type Err = FilterError;

    fn from_str(s: &str) -> FilterResult<Self> {
        match s {
            "standard" => Ok(Self::Standard),
            "strict" => Ok(Self::Strict),
            other => Err(FilterError::unknown_enum_value("min_mode", other)),
        }
    }
}

/// One filtering rule, exactly as stored in `filtering_rules`.
///
/// Constructed through [`Rule::new`] and the `with_*` builders rather than by literal, so that a
/// future field cannot be forgotten at a call site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// What the rule does.
    pub kind: RuleKind,
    /// The pattern, interpreted according to [`Rule::kind`]. Untrusted text until validated.
    pub pattern: String,
    /// Tie-break within a kind: higher wins.
    ///
    /// Priority orders rules *within* the allow pass and *within* the block pass. It can never
    /// promote a block rule over an allow rule — see [`crate::engine`] for why that ordering is
    /// not negotiable.
    #[serde(default)]
    pub priority: i32,
    /// Lowest filtering mode at which the rule applies.
    #[serde(default)]
    pub min_mode: RuleMode,
    /// The user's per-rule switch. A disabled rule is still validated, because it can be
    /// re-enabled without another validation pass.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// Instant after which the rule stops applying, or `None` for a permanent rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
}

/// serde default for [`Rule::enabled`]: a rule absent the field is on, matching the schema's
/// `DEFAULT 1`.
const fn enabled_by_default() -> bool {
    true
}

impl Rule {
    /// A permanent, enabled, Standard-mode rule at priority zero.
    #[must_use]
    pub fn new(kind: RuleKind, pattern: impl Into<String>) -> Self {
        Self {
            kind,
            pattern: pattern.into(),
            priority: 0,
            min_mode: RuleMode::Standard,
            enabled: true,
            expires_at: None,
        }
    }

    /// Sets the tie-break priority.
    #[must_use]
    pub const fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// Restricts the rule to a minimum filtering mode.
    #[must_use]
    pub const fn with_min_mode(mut self, min_mode: RuleMode) -> Self {
        self.min_mode = min_mode;
        self
    }

    /// Gives the rule an expiry instant.
    #[must_use]
    pub const fn with_expiry(mut self, expires_at: Timestamp) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    /// Marks the rule switched off.
    #[must_use]
    pub const fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// Whether the rule has lapsed as of `now`.
    ///
    /// The boundary is inclusive: a rule expiring exactly at `now` is already expired, so a rule
    /// created with a zero lifetime never fires.
    #[must_use]
    pub fn is_expired(&self, now: Timestamp) -> bool {
        self.expires_at.is_some_and(|expiry| expiry <= now)
    }

    /// Whether the rule may be matched at all, given the current mode and clock.
    ///
    /// Every caller goes through this rather than testing the three switches itself; that is what
    /// keeps "a Strict rule is inert in Standard mode" from being re-derived, and mis-derived, at
    /// each match site.
    #[must_use]
    pub fn applies(&self, mode: FilteringMode, now: Timestamp) -> bool {
        self.enabled && self.min_mode.applies_in(mode) && !self.is_expired(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vocabulary of `CHECK (kind IN (…))` in `migrations/0001_initial.sql`, copied verbatim.
    const SCHEMA_KINDS: [&str; 7] = [
        "block_host",
        "block_url",
        "allow_host",
        "allow_url",
        "hide_channel",
        "hide_keyword",
        "skip_segment",
    ];

    #[test]
    fn kinds_match_the_database_vocabulary_exactly() {
        let ours: Vec<&str> = RuleKind::ALL.iter().map(|kind| kind.as_str()).collect();
        assert_eq!(
            ours, SCHEMA_KINDS,
            "a kind the schema rejects would produce rows that cannot be stored"
        );
    }

    #[test]
    fn kinds_round_trip_through_text_and_json() {
        for kind in RuleKind::ALL {
            assert_eq!(kind.as_str().parse::<RuleKind>().unwrap(), kind);
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert_eq!(serde_json::from_str::<RuleKind>(&json).unwrap(), kind);
        }
    }

    #[test]
    fn indexes_are_dense_and_unique() {
        let mut seen = [false; RuleKind::COUNT];
        for kind in RuleKind::ALL {
            let index = kind.index();
            assert!(index < RuleKind::COUNT);
            assert!(!seen[index], "{kind} reuses index {index}");
            seen[index] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn an_unknown_kind_is_rejected_rather_than_defaulted() {
        let err = "block_advertising".parse::<RuleKind>().unwrap_err();
        assert_eq!(
            err.to_string(),
            "`block_advertising` is not a valid rule_kind"
        );
        assert!("".parse::<RuleKind>().is_err());
        assert!("BLOCK_HOST".parse::<RuleKind>().is_err());
    }

    #[test]
    fn allow_and_block_kinds_are_disjoint() {
        for kind in RuleKind::ALL {
            assert!(
                !(kind.is_allow() && kind.is_block()),
                "{kind} cannot be both"
            );
            assert_eq!(kind.filters_requests(), kind.is_allow() || kind.is_block());
            assert!(!(kind.filters_requests() && kind.filters_content()));
        }
        assert!(RuleKind::SkipSegment.filters_requests().eq(&false));
    }

    #[test]
    fn only_url_kinds_carry_a_regex() {
        for kind in RuleKind::ALL {
            assert_eq!(
                kind.is_regex(),
                matches!(kind, RuleKind::BlockUrl | RuleKind::AllowUrl),
                "{kind}"
            );
        }
    }

    #[test]
    fn a_strict_rule_is_inert_in_standard_mode() {
        assert!(!RuleMode::Strict.applies_in(FilteringMode::Standard));
        assert!(RuleMode::Strict.applies_in(FilteringMode::Strict));
        assert!(RuleMode::Standard.applies_in(FilteringMode::Standard));
        assert!(RuleMode::Standard.applies_in(FilteringMode::Strict));
    }

    #[test]
    fn nothing_applies_while_filtering_is_off() {
        for min_mode in [RuleMode::Standard, RuleMode::Strict] {
            assert!(!min_mode.applies_in(FilteringMode::Off));
        }
    }

    #[test]
    fn min_mode_round_trips_and_rejects_unknown_values() {
        assert_eq!("standard".parse::<RuleMode>().unwrap(), RuleMode::Standard);
        assert_eq!("strict".parse::<RuleMode>().unwrap(), RuleMode::Strict);
        assert!("aggressive".parse::<RuleMode>().is_err());
        assert_eq!(RuleMode::default(), RuleMode::Standard);
    }

    #[test]
    fn expiry_is_inclusive_so_a_zero_lifetime_rule_never_fires() {
        let now = Timestamp::from_millis(1_000);
        let rule = Rule::new(RuleKind::BlockHost, "tracker.example").with_expiry(now);
        assert!(rule.is_expired(now));
        assert!(!rule.applies(FilteringMode::Standard, now));
        assert!(rule.applies(
            FilteringMode::Standard,
            Timestamp::from_millis(now.as_millis() - 1)
        ));
    }

    #[test]
    fn a_permanent_rule_never_expires_even_at_the_end_of_time() {
        let rule = Rule::new(RuleKind::BlockHost, "tracker.example");
        assert!(!rule.is_expired(Timestamp::from_millis(i64::MAX)));
    }

    #[test]
    fn the_three_switches_are_independent() {
        let now = Timestamp::from_millis(10_000);
        let base = Rule::new(RuleKind::BlockHost, "tracker.example");

        assert!(base.applies(FilteringMode::Standard, now));
        assert!(
            !base
                .clone()
                .disabled()
                .applies(FilteringMode::Standard, now)
        );
        assert!(
            !base
                .clone()
                .with_min_mode(RuleMode::Strict)
                .applies(FilteringMode::Standard, now)
        );
        assert!(
            !base
                .clone()
                .with_expiry(Timestamp::from_millis(9_999))
                .applies(FilteringMode::Standard, now)
        );
        // …and a lapsed rule stays lapsed even in Strict mode.
        assert!(
            !base
                .with_expiry(Timestamp::from_millis(9_999))
                .applies(FilteringMode::Strict, now)
        );
    }

    #[test]
    fn a_rule_document_defaults_the_optional_fields() {
        let rule: Rule =
            serde_json::from_str(r#"{"kind":"hide_keyword","pattern":"reaction"}"#).unwrap();
        assert_eq!(rule.kind, RuleKind::HideKeyword);
        assert_eq!(rule.priority, 0);
        assert_eq!(rule.min_mode, RuleMode::Standard);
        assert!(
            rule.enabled,
            "an absent `enabled` matches the schema default"
        );
        assert_eq!(rule.expires_at, None);
    }

    #[test]
    fn rules_round_trip_through_json() {
        let rule = Rule::new(RuleKind::AllowHost, "googlevideo.com")
            .with_priority(-5)
            .with_min_mode(RuleMode::Strict)
            .with_expiry(Timestamp::from_millis(1_700_000_000_000));
        let json = serde_json::to_string(&rule).unwrap();
        assert_eq!(serde_json::from_str::<Rule>(&json).unwrap(), rule);
    }
}
