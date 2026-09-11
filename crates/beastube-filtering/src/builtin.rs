//! The rule set shipped with the application.
//!
//! Compiled in rather than downloaded, so filtering works on first launch, offline, and before any
//! update has run. A downloaded set replaces it through the normal activation path; a rollback
//! falls back to it rather than to nothing.
//!
//! ## What is in it, and what is not
//!
//! **Third-party tracking and analytics endpoints.** These are requests the application never needs
//! and that exist to profile the viewer. Blocking them is the part of the Brave/uBlock model that
//! applies cleanly here, and none of it touches media delivery.
//!
//! **Not** the provider's own advertising. Under the sanctioned embed player (ADR-0001) the media
//! path and the ad path are the same connection, so a rule aimed at one would take the other with
//! it — the player would simply stop working. The never-block list exists to make that impossible
//! even if such a rule were introduced by a downloaded set.
//!
//! ## Choosing what to include
//!
//! Every entry is a registrable domain whose sole purpose is measurement. Domains that also serve
//! content, or that a page needs to function, are deliberately absent: a filter that breaks a page
//! is worse than one that misses a beacon, and the standard mode exists to hold that line. Entries
//! that are useful but carry any risk of a false positive are marked strict, so they act only when
//! the user has explicitly asked for more aggressive filtering.

use crate::rule::{Rule, RuleKind, RuleMode};
use crate::ruleset::{RuleSet, RuleSetSource};

/// Version of the built-in set. Bumped whenever the list below changes.
pub const BUILTIN_VERSION: &str = "builtin.2026.09.03";

/// Analytics and tracking endpoints blocked in every active mode.
///
/// Each of these exists to record behaviour and serves no content the application displays.
const TRACKERS: &[&str] = [
    // Advertising exchanges and measurement.
    "doubleclick.net",
    "googleadservices.com",
    "googlesyndication.com",
    "adservice.google.com",
    "2mdn.net",
    "adnxs.com",
    "rubiconproject.com",
    "pubmatic.com",
    "criteo.com",
    "taboola.com",
    "outbrain.com",
    "scorecardresearch.com",
    "moatads.com",
    "adsafeprotected.com",
    // Behavioural analytics.
    "hotjar.com",
    "mixpanel.com",
    "segment.io",
    "amplitude.com",
    "fullstory.com",
    "mouseflow.com",
    "crazyegg.com",
    "quantserve.com",
    "chartbeat.com",
    // Social tracking pixels.
    "connect.facebook.net",
    "analytics.tiktok.com",
    "ads.linkedin.com",
    "ads-twitter.com",
]
.as_slice();

/// URL patterns blocked in every active mode.
///
/// Used where the tracking endpoint is a path on a host that also serves something else, so a host
/// rule would be too broad. Patterns are anchored to a literal host and path; the validator rejects
/// anything that could backtrack catastrophically.
const TRACKER_URLS: &[&str] = [
    // The Meta tracking pixel. `facebook.com` itself is not blocked, because a host rule there
    // would be far broader than the beacon it is aimed at.
    r"facebook\.com/tr",
    // Google's measurement collector, which shares a host with services that are not tracking.
    r"google-analytics\.com/(collect|g/collect)",
]
.as_slice();

/// Endpoints blocked only in strict mode.
///
/// These are measurement endpoints that occasionally sit on a host which also serves something
/// useful, so blocking them carries a small risk of a false positive. Standard mode leaves them
/// alone; strict mode is where the user has accepted that trade.
const STRICT_ONLY: &[&str] = [
    "google-analytics.com",
    "googletagmanager.com",
    "app-measurement.com",
    "branch.io",
    "adjust.com",
    "appsflyer.com",
]
.as_slice();

/// Builds the rule set shipped with the application.
///
/// The result is unvalidated by design: it goes through the same validation path as a downloaded
/// set, so a mistake in the list above is caught by the same checks rather than bypassing them.
#[must_use]
pub fn builtin_rule_set() -> RuleSet {
    let standard = TRACKERS
        .iter()
        .map(|domain| Rule::new(RuleKind::BlockHost, *domain));

    let urls = TRACKER_URLS
        .iter()
        .map(|pattern| Rule::new(RuleKind::BlockUrl, *pattern));

    let strict = STRICT_ONLY
        .iter()
        .map(|domain| Rule::new(RuleKind::BlockHost, *domain).with_min_mode(RuleMode::Strict));

    RuleSet::new(BUILTIN_VERSION, RuleSetSource::Builtin)
        .with_rules(standard.chain(urls).chain(strict))
}

/// Number of rules in the built-in set, for the diagnostics screen and tests.
#[must_use]
pub fn builtin_rule_count() -> usize {
    TRACKERS.len() + TRACKER_URLS.len() + STRICT_ONLY.len()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use beastube_core::settings::FilteringMode;

    use super::*;
    use crate::diagnostics::FilteringDiagnostics;
    use crate::engine::{EngineConfig, FilterEngine, NeverBlockList};

    fn engine(mode: FilteringMode) -> FilterEngine {
        let validated = builtin_rule_set()
            .validate()
            .expect("the shipped rule set must validate");
        FilterEngine::new(
            Arc::new(validated),
            EngineConfig::new(true, mode, NeverBlockList::playback_critical()),
            Arc::new(FilteringDiagnostics::new()),
        )
    }

    #[test]
    fn the_shipped_rule_set_validates() {
        // If this fails, the application would ship with filtering that refuses to activate.
        let validated = builtin_rule_set().validate().expect("validates");
        assert_eq!(validated.len(), builtin_rule_count());
        assert_eq!(validated.version(), BUILTIN_VERSION);
    }

    #[test]
    fn it_contains_no_duplicates() {
        let mut seen = HashSet::new();
        for domain in TRACKERS
            .iter()
            .chain(TRACKER_URLS.iter())
            .chain(STRICT_ONLY.iter())
        {
            assert!(seen.insert(*domain), "duplicate entry: {domain}");
        }
    }

    #[test]
    fn it_blocks_a_known_tracker_in_standard_mode() {
        let engine = engine(FilteringMode::Standard);
        for host in [
            "doubleclick.net",
            "stats.g.doubleclick.net",
            "static.hotjar.com",
            "b.scorecardresearch.com",
        ] {
            assert!(
                engine.decide(host, &format!("https://{host}/x")).is_blocked(),
                "{host} should be blocked"
            );
        }
    }

    #[test]
    fn strict_only_entries_are_inert_in_standard_mode() {
        // The whole point of the two modes: standard holds the line at "no false positives".
        let standard = engine(FilteringMode::Standard);
        let strict = engine(FilteringMode::Strict);

        for host in ["google-analytics.com", "www.googletagmanager.com"] {
            assert!(
                standard.decide(host, &format!("https://{host}/x")).is_allowed(),
                "{host} must be allowed in standard mode"
            );
            assert!(
                strict.decide(host, &format!("https://{host}/x")).is_blocked(),
                "{host} must be blocked in strict mode"
            );
        }
    }

    #[test]
    fn it_never_blocks_the_media_path_in_any_mode() {
        // The load-bearing safety property: filtering must not be able to break playback.
        for mode in [FilteringMode::Standard, FilteringMode::Strict] {
            let engine = engine(mode);
            for host in [
                "www.youtube.com",
                "youtube-nocookie.com",
                "rr3---sn-abc.googlevideo.com",
                "i.ytimg.com",
                "yt3.ggpht.com",
            ] {
                assert!(
                    engine.decide(host, &format!("https://{host}/x")).is_allowed(),
                    "{host} must stay reachable in {mode:?} mode"
                );
            }
        }
    }

    #[test]
    fn a_url_pattern_blocks_the_beacon_without_blocking_its_host() {
        // The reason these are URL rules rather than host rules: the host serves other things.
        let engine = engine(FilteringMode::Standard);
        assert!(
            engine
                .decide("www.facebook.com", "https://www.facebook.com/tr?id=1")
                .is_blocked(),
            "the tracking pixel should be blocked"
        );
        assert!(
            engine
                .decide("www.facebook.com", "https://www.facebook.com/some/page")
                .is_allowed(),
            "the rest of the host must stay reachable"
        );
    }

    #[test]
    fn it_does_not_block_ordinary_hosts() {
        let engine = engine(FilteringMode::Strict);
        for host in ["example.com", "wikipedia.org", "github.com", "localhost"] {
            assert!(
                engine.decide(host, &format!("https://{host}/x")).is_allowed(),
                "{host} is not a tracker and must be allowed"
            );
        }
    }

    #[test]
    fn no_entry_is_a_bare_public_suffix() {
        // A rule on `com` or `net` would block most of the internet. The validator rejects one, but
        // catching it here names the offending entry instead of failing opaquely.
        for domain in TRACKERS.iter().chain(STRICT_ONLY.iter()) {
            assert!(
                domain.contains('.'),
                "{domain} has no dot and would match far too much"
            );
            assert!(
                !domain.contains('/'),
                "{domain} carries a path and belongs in TRACKER_URLS"
            );
        }
    }
}
