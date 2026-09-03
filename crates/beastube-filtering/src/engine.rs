//! The matching engine.
//!
//! This is where a request is allowed or blocked and where a feed item is shown or hidden — the
//! same model Brave Shields and uBlock Origin use: match the request against a rule set before it
//! leaves, and drop it when a block rule matches and no allow rule does.
//!
//! The engine is a thin layer over an already-validated, already-compiled
//! [`ValidatedRuleSet`](crate::ruleset::ValidatedRuleSet). It contributes the *policy* — ordering,
//! precedence and the safety invariants — while the rule set contributes the matching.
//!
//! ## The four safety invariants
//!
//! Filtering must never break ordinary playback (§10). These make that structural rather than
//! aspirational, and each has a test named after it:
//!
//! 1. **Disabled means allow.** With filtering off, or the mode `Off`, no rule is even consulted.
//! 2. **Never-block wins over everything.** Hosts on [`NeverBlockList`] are allowed regardless of
//!    mode, priority, or how confidently a rule matched. The worst a hostile or drifted rule set can
//!    do is fail to block something — it can never take the media path down.
//! 3. **Allow beats block**, evaluated first and independently of priority, so a user's allowlist
//!    entry cannot be out-ranked by a downloaded rule.
//! 4. **Unknown is allowed.** A request matching nothing passes. Filtering is a deny-list; treating
//!    unrecognized requests as hostile is exactly how a filter breaks something it has never seen.

use std::sync::Arc;

use beastube_core::security::is_host_within;
use beastube_core::settings::FilteringMode;
use beastube_core::time_util::Timestamp;

use crate::diagnostics::FilteringDiagnostics;
use crate::rule::RuleKind;
use crate::ruleset::ValidatedRuleSet;

/// Hosts that are never blocked, whatever the rules say.
///
/// Compiled into the binary and supplied at engine construction; a rule set cannot extend it. That
/// asymmetry is the point — a downloaded rule can never widen its own reach.
#[derive(Debug, Clone, Default)]
pub struct NeverBlockList {
    domains: Vec<String>,
}

impl NeverBlockList {
    /// An empty list. Used in tests; production always supplies the playback-critical set.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            domains: Vec::new(),
        }
    }

    /// The hosts playback and the application itself depend on.
    ///
    /// Blocking any of these yields a black player or a dead application, so no rule may.
    #[must_use]
    pub fn playback_critical() -> Self {
        Self::from_domains([
            // Media delivery and the player.
            "googlevideo.com",
            "youtube.com",
            "youtube-nocookie.com",
            "ytimg.com",
            "ggpht.com",
            // The application's own webview origin and loopback gateway.
            "localhost",
            "tauri.localhost",
        ])
    }

    /// Builds a list from registrable domains. A host matches if it equals or is a subdomain of one.
    #[must_use]
    pub fn from_domains<I, S>(domains: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            domains: domains
                .into_iter()
                .map(|domain| domain.into().to_ascii_lowercase())
                .collect(),
        }
    }

    /// Whether `host` is protected.
    ///
    /// Uses [`is_host_within`] rather than a suffix comparison, so `evilgooglevideo.com` is not
    /// protected merely because it ends with a protected domain.
    #[must_use]
    pub fn contains(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.domains
            .iter()
            .any(|domain| is_host_within(&host, domain))
    }

    /// Number of protected domains.
    #[must_use]
    pub fn len(&self) -> usize {
        self.domains.len()
    }

    /// Whether nothing is protected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.domains.is_empty()
    }
}

/// How the engine behaves when a decision is made.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Master switch. When false, every decision is [`Decision::Allow`].
    pub enabled: bool,
    /// How aggressive matching is.
    pub mode: FilteringMode,
    /// Hosts no rule may block.
    pub never_block: NeverBlockList,
}

impl EngineConfig {
    /// Builds a configuration.
    #[must_use]
    pub const fn new(enabled: bool, mode: FilteringMode, never_block: NeverBlockList) -> Self {
        Self {
            enabled,
            mode,
            never_block,
        }
    }

    /// The production default: filtering on, standard mode, playback hosts protected.
    #[must_use]
    pub fn standard() -> Self {
        Self::new(
            true,
            FilteringMode::Standard,
            NeverBlockList::playback_critical(),
        )
    }

    /// Whether any matching should occur.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.enabled && self.mode.is_active()
    }
}

/// What to do with a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Let it through.
    Allow,
    /// Drop it.
    ///
    /// The interception layer answers with a synthesized empty 200 rather than a network error:
    /// a hard failure makes some pages retry in a loop, which is louder than the request it
    /// replaced.
    Block {
        /// Index of the matching rule within the active set, for diagnostics. Never a URL.
        rule_index: usize,
    },
}

impl Decision {
    /// Whether the request should proceed.
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }

    /// Whether the request should be dropped.
    #[must_use]
    pub const fn is_blocked(self) -> bool {
        matches!(self, Self::Block { .. })
    }
}

/// Why a request was allowed.
///
/// Carries no URL or host — only which class of decision was reached, so enabling diagnostics never
/// turns the filtering layer into a browsing log (§99).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowReason {
    /// Filtering is off, or the mode is `Off`.
    Disabled,
    /// The host is playback-critical.
    NeverBlock,
    /// An allow rule matched.
    AllowRule,
    /// Nothing matched. The common case.
    NoMatch,
}

/// An immutable matcher over one activated rule set.
///
/// Built once per activation and shared behind an `Arc`. Matching takes `&self` and allocates
/// nothing on the allow path, which is the overwhelmingly common one.
#[derive(Debug)]
pub struct FilterEngine {
    rule_set: Arc<ValidatedRuleSet>,
    config: EngineConfig,
    diagnostics: Arc<FilteringDiagnostics>,
}

impl FilterEngine {
    /// Binds a validated rule set to a configuration.
    #[must_use]
    pub fn new(
        rule_set: Arc<ValidatedRuleSet>,
        config: EngineConfig,
        diagnostics: Arc<FilteringDiagnostics>,
    ) -> Self {
        Self {
            rule_set,
            config,
            diagnostics,
        }
    }

    /// The configuration this engine was built with.
    #[must_use]
    pub const fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// The rule set backing this engine.
    #[must_use]
    pub const fn rule_set(&self) -> &Arc<ValidatedRuleSet> {
        &self.rule_set
    }

    /// Decides whether a request may proceed.
    ///
    /// `host` is the request's host; `url` is the full URL, used by URL-pattern rules. The caller
    /// has usually parsed the URL already, so passing both avoids re-parsing on the request path.
    #[must_use]
    pub fn decide(&self, host: &str, url: &str) -> Decision {
        self.decide_at(host, url, Timestamp::now())
    }

    /// [`FilterEngine::decide`] with an explicit clock, so expiry can be tested without waiting.
    #[must_use]
    pub fn decide_at(&self, host: &str, url: &str, now: Timestamp) -> Decision {
        let (decision, reason) = self.evaluate(host, url, now);
        self.diagnostics.record_decision(decision, reason);
        decision
    }

    fn evaluate(&self, host: &str, url: &str, now: Timestamp) -> (Decision, AllowReason) {
        // 1. Disabled means allow, at no matching cost at all.
        if !self.config.is_active() {
            return (Decision::Allow, AllowReason::Disabled);
        }

        // 2. Playback-critical hosts are never blocked, by any rule, in any mode.
        if self.config.never_block.contains(host) {
            return (Decision::Allow, AllowReason::NeverBlock);
        }

        // 3. Allow rules first, independent of priority, so a user's allowlist entry cannot be
        //    out-ranked by a downloaded block rule.
        if self
            .first_match(host, url, RuleKind::AllowHost, RuleKind::AllowUrl, now)
            .is_some()
        {
            return (Decision::Allow, AllowReason::AllowRule);
        }

        if let Some(rule_index) =
            self.first_match(host, url, RuleKind::BlockHost, RuleKind::BlockUrl, now)
        {
            return (Decision::Block { rule_index }, AllowReason::NoMatch);
        }

        // 4. Unknown is allowed.
        (Decision::Allow, AllowReason::NoMatch)
    }

    /// Index of the highest-priority matching host or URL rule.
    fn first_match(
        &self,
        host: &str,
        url: &str,
        host_kind: RuleKind,
        url_kind: RuleKind,
        now: Timestamp,
    ) -> Option<usize> {
        let host_hit = self
            .rule_set
            .rules_of(host_kind)
            .filter(|(_, rule)| rule.applies(self.config.mode, now))
            .filter(|(_, rule)| rule.matches_host(host))
            .max_by_key(|(_, rule)| rule.priority());

        let url_hit = self
            .rule_set
            .rules_of(url_kind)
            .filter(|(_, rule)| rule.applies(self.config.mode, now))
            .filter(|(_, rule)| rule.matches_url(url))
            .max_by_key(|(_, rule)| rule.priority());

        match (host_hit, url_hit) {
            (Some((host_index, host_rule)), Some((url_index, url_rule))) => {
                // Higher priority wins; ties go to the host rule, which is the more specific claim.
                if url_rule.priority() > host_rule.priority() {
                    Some(url_index)
                } else {
                    Some(host_index)
                }
            }
            (Some((index, _)), None) | (None, Some((index, _))) => Some(index),
            (None, None) => None,
        }
    }

    /// Decides for a bare host, with no URL to match URL rules against.
    ///
    /// Used where only a host is known — a connection-level check, or a test. URL-pattern rules
    /// cannot match here, so a host that would only be caught by one is allowed.
    #[must_use]
    pub fn evaluate_host(&self, host: &str) -> Decision {
        self.decide(host, "")
    }

    /// Whether a feed item should be hidden.
    ///
    /// Independent of request blocking: a hidden item is simply not rendered, and nothing about the
    /// requests for it changes.
    #[must_use]
    pub fn hides_item(
        &self,
        channel_id: Option<&str>,
        channel_name: Option<&str>,
        title: &str,
    ) -> bool {
        self.hides_item_at(channel_id, channel_name, title, Timestamp::now())
    }

    /// [`FilterEngine::hides_item`] with an explicit clock.
    #[must_use]
    pub fn hides_item_at(
        &self,
        channel_id: Option<&str>,
        channel_name: Option<&str>,
        title: &str,
        now: Timestamp,
    ) -> bool {
        if !self.config.is_active() {
            return false;
        }

        let hides_channel = self
            .rule_set
            .rules_of(RuleKind::HideChannel)
            .filter(|(_, rule)| rule.applies(self.config.mode, now))
            .any(|(_, rule)| rule.matches_channel(channel_id, channel_name));
        if hides_channel {
            return true;
        }

        self.rule_set
            .rules_of(RuleKind::HideKeyword)
            .filter(|(_, rule)| rule.applies(self.config.mode, now))
            .any(|(_, rule)| rule.matches_keyword(title))
    }

    /// Whether a creator-marked segment category is one the rule set asks to skip.
    #[must_use]
    pub fn skips_category(&self, category: &str) -> bool {
        let now = Timestamp::now();
        self.rule_set
            .rules_of(RuleKind::SkipSegment)
            .filter(|(_, rule)| rule.applies(self.config.mode, now))
            .any(|(_, rule)| rule.matches_category(category))
    }

    /// Rules that can participate in a request decision.
    #[must_use]
    pub fn request_rule_count(&self) -> usize {
        self.rule_set.count_of(RuleKind::BlockHost)
            + self.rule_set.count_of(RuleKind::BlockUrl)
            + self.rule_set.count_of(RuleKind::AllowHost)
            + self.rule_set.count_of(RuleKind::AllowUrl)
    }

    /// Rules that hide content.
    #[must_use]
    pub fn content_rule_count(&self) -> usize {
        self.rule_set.count_of(RuleKind::HideChannel)
            + self.rule_set.count_of(RuleKind::HideKeyword)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{Rule, RuleMode};
    use crate::ruleset::{RuleSet, RuleSetSource};

    fn engine_with(rules: Vec<Rule>, mode: FilteringMode) -> FilterEngine {
        engine_with_config(
            rules,
            EngineConfig::new(true, mode, NeverBlockList::playback_critical()),
        )
    }

    fn engine_with_config(rules: Vec<Rule>, config: EngineConfig) -> FilterEngine {
        let validated = RuleSet::new("test", RuleSetSource::Builtin)
            .with_rules(rules)
            .validate()
            .expect("rule set validates");
        FilterEngine::new(
            Arc::new(validated),
            config,
            Arc::new(FilteringDiagnostics::new()),
        )
    }

    fn block_host(pattern: &str) -> Rule {
        Rule::new(RuleKind::BlockHost, pattern)
    }

    fn allow_host(pattern: &str) -> Rule {
        Rule::new(RuleKind::AllowHost, pattern)
    }

    #[test]
    fn a_blocked_host_is_blocked() {
        let engine = engine_with(vec![block_host("doubleclick.net")], FilteringMode::Standard);
        assert!(
            engine
                .decide("doubleclick.net", "https://doubleclick.net/ad")
                .is_blocked()
        );
    }

    #[test]
    fn a_host_rule_covers_subdomains() {
        // This is what lets a list of a few thousand domains cover the whole ad ecosystem.
        let engine = engine_with(vec![block_host("doubleclick.net")], FilteringMode::Standard);
        for host in [
            "stats.g.doubleclick.net",
            "ad.doubleclick.net",
            "a.b.c.doubleclick.net",
        ] {
            assert!(
                engine
                    .decide(host, &format!("https://{host}/x"))
                    .is_blocked(),
                "{host} should be covered by the parent rule"
            );
        }
    }

    #[test]
    fn a_host_rule_does_not_match_a_lookalike_domain() {
        let engine = engine_with(vec![block_host("doubleclick.net")], FilteringMode::Standard);
        for host in [
            "evildoubleclick.net",
            "doubleclick.net.evil.tld",
            "notdoubleclick.net",
        ] {
            assert!(
                engine
                    .decide(host, &format!("https://{host}/x"))
                    .is_allowed(),
                "{host} must not be caught by the doubleclick.net rule"
            );
        }
    }

    #[test]
    fn an_unmatched_request_is_allowed() {
        let engine = engine_with(vec![block_host("doubleclick.net")], FilteringMode::Standard);
        assert!(
            engine
                .decide("example.com", "https://example.com/anything")
                .is_allowed()
        );
    }

    #[test]
    fn an_allow_rule_beats_a_block_rule_at_any_priority() {
        let engine = engine_with(
            vec![
                block_host("tracker.example").with_priority(1000),
                allow_host("tracker.example").with_priority(-1000),
            ],
            FilteringMode::Standard,
        );
        assert!(
            engine
                .decide("tracker.example", "https://tracker.example/x")
                .is_allowed(),
            "allow must win regardless of priority"
        );
    }

    #[test]
    fn a_playback_critical_host_is_never_blocked() {
        // Even named explicitly, at maximum priority, in strict mode.
        let engine = engine_with(
            vec![
                block_host("googlevideo.com").with_priority(i32::MAX),
                block_host("youtube.com").with_priority(i32::MAX),
                block_host("ytimg.com").with_priority(i32::MAX),
            ],
            FilteringMode::Strict,
        );
        for host in [
            "googlevideo.com",
            "rr3---sn-abc.googlevideo.com",
            "www.youtube.com",
            "i.ytimg.com",
        ] {
            assert!(
                engine
                    .decide(host, &format!("https://{host}/x"))
                    .is_allowed(),
                "{host} is playback-critical and must never be blocked"
            );
        }
    }

    #[test]
    fn never_block_protection_does_not_leak_to_lookalikes() {
        let engine = engine_with(
            vec![block_host("evilgooglevideo.com")],
            FilteringMode::Standard,
        );
        assert!(
            engine
                .decide("evilgooglevideo.com", "https://evilgooglevideo.com/x")
                .is_blocked()
        );
    }

    #[test]
    fn a_strict_rule_is_inert_in_standard_mode() {
        let standard = engine_with(
            vec![block_host("maybe-ads.example").with_min_mode(RuleMode::Strict)],
            FilteringMode::Standard,
        );
        assert!(
            standard
                .decide("maybe-ads.example", "https://maybe-ads.example/x")
                .is_allowed(),
            "a strict-only rule must not act in standard mode"
        );

        let strict = engine_with(
            vec![block_host("maybe-ads.example").with_min_mode(RuleMode::Strict)],
            FilteringMode::Strict,
        );
        assert!(
            strict
                .decide("maybe-ads.example", "https://maybe-ads.example/x")
                .is_blocked(),
            "the same rule must act in strict mode"
        );
    }

    #[test]
    fn filtering_disabled_allows_everything() {
        for config in [
            EngineConfig::new(false, FilteringMode::Strict, NeverBlockList::empty()),
            EngineConfig::new(true, FilteringMode::Off, NeverBlockList::empty()),
        ] {
            let engine = engine_with_config(vec![block_host("doubleclick.net")], config);
            assert!(
                engine
                    .decide("doubleclick.net", "https://doubleclick.net/ad")
                    .is_allowed()
            );
        }
    }

    #[test]
    fn an_empty_rule_set_allows_everything_and_hides_nothing() {
        let engine = engine_with(vec![], FilteringMode::Strict);
        assert!(
            engine
                .decide("anything.example", "https://anything.example/")
                .is_allowed()
        );
        assert!(!engine.hides_item(Some("UCanything"), None, "Any title"));
        assert_eq!(engine.request_rule_count(), 0);
    }

    #[test]
    fn content_hiding_matches_channel_and_keyword() {
        let engine = engine_with(
            vec![
                Rule::new(RuleKind::HideChannel, "UCspam"),
                Rule::new(RuleKind::HideKeyword, "crypto giveaway"),
            ],
            FilteringMode::Standard,
        );

        assert!(engine.hides_item(Some("UCspam"), None, "Anything"));
        assert!(engine.hides_item(None, None, "Huge CRYPTO GIVEAWAY today"));
        assert!(!engine.hides_item(Some("UCgood"), None, "An ordinary video"));
    }

    #[test]
    fn content_hiding_is_off_when_filtering_is_off() {
        let engine = engine_with_config(
            vec![Rule::new(RuleKind::HideChannel, "UCspam")],
            EngineConfig::new(false, FilteringMode::Strict, NeverBlockList::empty()),
        );
        assert!(!engine.hides_item(Some("UCspam"), None, "Anything"));
    }

    #[test]
    fn rule_counts_reflect_the_active_set() {
        let engine = engine_with(
            vec![
                block_host("a.example"),
                allow_host("b.example"),
                Rule::new(RuleKind::HideChannel, "UCx"),
                Rule::new(RuleKind::HideKeyword, "spam"),
            ],
            FilteringMode::Standard,
        );
        assert_eq!(engine.request_rule_count(), 2);
        assert_eq!(engine.content_rule_count(), 2);
    }

    #[test]
    fn the_never_block_list_rejects_lookalikes_directly() {
        let list = NeverBlockList::playback_critical();
        assert!(list.contains("googlevideo.com"));
        assert!(list.contains("rr1---sn-x.googlevideo.com"));
        assert!(list.contains("GOOGLEVIDEO.COM"));
        assert!(!list.contains("evilgooglevideo.com"));
        assert!(!list.contains("googlevideo.com.attacker.tld"));
        assert!(!list.is_empty());
        assert!(NeverBlockList::empty().is_empty());
    }

    #[test]
    fn decisions_are_counted_without_recording_what_was_requested() {
        let diagnostics = Arc::new(FilteringDiagnostics::new());
        let validated = RuleSet::new("test", RuleSetSource::Builtin)
            .with_rules(vec![block_host("doubleclick.net")])
            .validate()
            .expect("validates");
        let engine = FilterEngine::new(
            Arc::new(validated),
            EngineConfig::standard(),
            Arc::clone(&diagnostics),
        );

        // Called for the counter side effect; the decisions themselves are asserted elsewhere.
        let _ = engine.decide("doubleclick.net", "https://doubleclick.net/ad");
        let _ = engine.decide("example.com", "https://example.com/x");
        let _ = engine.decide("googlevideo.com", "https://googlevideo.com/videoplayback");

        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot.evaluated, 3);
        assert_eq!(snapshot.blocked, 1);
        assert_eq!(snapshot.allowed_never_block, 1);
    }
}
