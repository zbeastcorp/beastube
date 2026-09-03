//! Filtering adapters.
//!
//! The engine decides; adapters apply the decision to one surface. Keeping them apart is what makes
//! the subsystem independently replaceable (§6): the request adapter can be rewritten for a
//! different interception mechanism, and the content adapter for a different feed shape, without
//! either touching the matching logic or each other.
//!
//! [`FilteringProvider`] is the observer side: the rule-set manager pushes a newly activated engine
//! to every registered provider, so a live rule change takes effect without a restart and without
//! any adapter polling for one.

use std::sync::Arc;

use parking_lot::RwLock;

use crate::engine::{Decision, FilterEngine};

/// Receives a new engine whenever the active rule set changes.
///
/// Implementors hold the engine and consult it per request. Swapping an `Arc` is what makes a rule
/// update — or a rollback — atomic from the point of view of an in-flight request: a request either
/// sees the old engine or the new one, never a half-updated rule set.
pub trait FilteringProvider: Send + Sync + std::fmt::Debug {
    /// A short stable name, for diagnostics.
    fn name(&self) -> &'static str;

    /// Called when a new engine becomes active.
    fn set_engine(&self, engine: Arc<FilterEngine>);
}

/// Applies request decisions.
///
/// This is the adapter the WebView2 `WebResourceRequested` hook drives: for each request the
/// webview is about to make, it answers allow or block. It holds only an engine handle, so it has
/// no opinion of its own about what should be blocked — that lives entirely in the rule set.
#[derive(Debug)]
pub struct RequestFilterAdapter {
    engine: RwLock<Arc<FilterEngine>>,
}

impl RequestFilterAdapter {
    /// Binds the adapter to an engine.
    #[must_use]
    pub fn new(engine: Arc<FilterEngine>) -> Self {
        Self {
            engine: RwLock::new(engine),
        }
    }

    /// Decides whether a request may proceed.
    ///
    /// `host` should already be the parsed host of `url`. It is passed separately because the
    /// caller has usually parsed the URL anyway, and re-parsing on the request path would be waste.
    #[must_use]
    pub fn decide(&self, host: &str, url: &str) -> Decision {
        // Cloning the Arc rather than holding the lock across the decision keeps the read lock
        // held for a pointer copy, so a rule-set activation never blocks behind a slow match.
        let engine = Arc::clone(&self.engine.read());
        engine.decide(host, url)
    }

    /// Convenience for callers holding only a URL.
    ///
    /// Returns [`Decision::Allow`] for a URL with no host: a request we cannot classify is allowed,
    /// consistent with the engine's "unknown is allowed" invariant.
    #[must_use]
    pub fn decide_url(&self, url: &str) -> Decision {
        match url::Url::parse(url) {
            Ok(parsed) => match parsed.host_str() {
                Some(host) => self.decide(host, url),
                None => Decision::Allow,
            },
            Err(_) => Decision::Allow,
        }
    }
}

impl FilteringProvider for RequestFilterAdapter {
    fn name(&self) -> &'static str {
        "request"
    }

    fn set_engine(&self, engine: Arc<FilterEngine>) {
        *self.engine.write() = engine;
    }
}

/// Applies content-hiding decisions to feed items.
///
/// Separate from the request adapter because hiding and blocking are different actions with
/// different consequences: a hidden item is simply not rendered, and no request behaviour changes.
#[derive(Debug)]
pub struct ContentFilterAdapter {
    engine: RwLock<Arc<FilterEngine>>,
}

impl ContentFilterAdapter {
    /// Binds the adapter to an engine.
    #[must_use]
    pub fn new(engine: Arc<FilterEngine>) -> Self {
        Self {
            engine: RwLock::new(engine),
        }
    }

    /// Whether one item should be hidden.
    #[must_use]
    pub fn hides(&self, channel_id: Option<&str>, title: &str) -> bool {
        let engine = Arc::clone(&self.engine.read());
        engine.hides_item(channel_id, None, title)
    }

    /// Filters a page of items in place.
    ///
    /// Returns how many were removed, so the caller can decide whether to fetch another page —
    /// aggressive hiding must not leave the user with an apparently-empty feed and no continuation.
    pub fn retain<T, F>(&self, items: &mut Vec<T>, mut describe: F) -> usize
    where
        F: FnMut(&T) -> (Option<String>, String),
    {
        let engine = Arc::clone(&self.engine.read());
        let before = items.len();
        items.retain(|item| {
            let (channel, title) = describe(item);
            !engine.hides_item(channel.as_deref(), None, &title)
        });
        before - items.len()
    }
}

impl FilteringProvider for ContentFilterAdapter {
    fn name(&self) -> &'static str {
        "content"
    }

    fn set_engine(&self, engine: Arc<FilterEngine>) {
        *self.engine.write() = engine;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use beastube_core::settings::FilteringMode;

    use super::*;
    use crate::diagnostics::FilteringDiagnostics;
    use crate::engine::{EngineConfig, NeverBlockList};
    use crate::rule::{Rule, RuleKind};
    use crate::ruleset::{RuleSet, RuleSetSource};

    fn engine(rules: Vec<Rule>) -> Arc<FilterEngine> {
        let validated = RuleSet::new("test", RuleSetSource::Builtin)
            .with_rules(rules)
            .validate()
            .expect("rule set validates");
        Arc::new(FilterEngine::new(
            Arc::new(validated),
            EngineConfig::new(
                true,
                FilteringMode::Standard,
                NeverBlockList::playback_critical(),
            ),
            Arc::new(FilteringDiagnostics::new()),
        ))
    }

    #[test]
    fn the_request_adapter_applies_the_engines_decision() {
        let adapter = RequestFilterAdapter::new(engine(vec![Rule::new(
            RuleKind::BlockHost,
            "doubleclick.net",
        )]));

        assert!(
            adapter
                .decide_url("https://stats.doubleclick.net/x")
                .is_blocked()
        );
        assert!(adapter.decide_url("https://example.com/x").is_allowed());
    }

    #[test]
    fn a_url_that_cannot_be_parsed_is_allowed() {
        // Consistent with the engine's "unknown is allowed" invariant: an unclassifiable request
        // must not be dropped.
        let adapter = RequestFilterAdapter::new(engine(vec![Rule::new(
            RuleKind::BlockHost,
            "doubleclick.net",
        )]));

        for odd in ["not a url", "", "data:text/plain,hello", "about:blank"] {
            assert!(
                adapter.decide_url(odd).is_allowed(),
                "{odd:?} should be allowed rather than dropped"
            );
        }
    }

    #[test]
    fn swapping_the_engine_takes_effect_immediately() {
        let adapter = RequestFilterAdapter::new(engine(vec![]));
        assert!(adapter.decide_url("https://doubleclick.net/x").is_allowed());

        adapter.set_engine(engine(vec![Rule::new(
            RuleKind::BlockHost,
            "doubleclick.net",
        )]));
        assert!(
            adapter.decide_url("https://doubleclick.net/x").is_blocked(),
            "a rule update must take effect without a restart"
        );

        // And a rollback must be equally immediate.
        adapter.set_engine(engine(vec![]));
        assert!(adapter.decide_url("https://doubleclick.net/x").is_allowed());
    }

    #[test]
    fn the_content_adapter_hides_matching_items() {
        let adapter = ContentFilterAdapter::new(engine(vec![
            Rule::new(RuleKind::HideChannel, "UCspam"),
            Rule::new(RuleKind::HideKeyword, "giveaway"),
        ]));

        assert!(adapter.hides(Some("UCspam"), "Anything"));
        assert!(adapter.hides(None, "Free GIVEAWAY now"));
        assert!(!adapter.hides(Some("UCgood"), "An ordinary video"));
    }

    #[test]
    fn retain_reports_how_many_it_removed() {
        // The caller needs the count: hiding most of a page must trigger fetching another, or the
        // user is left with an apparently-empty feed.
        let adapter =
            ContentFilterAdapter::new(engine(vec![Rule::new(RuleKind::HideKeyword, "spam")]));

        let mut items = vec![
            ("UCa".to_owned(), "Good video".to_owned()),
            ("UCb".to_owned(), "Spam video".to_owned()),
            ("UCc".to_owned(), "More SPAM".to_owned()),
            ("UCd".to_owned(), "Also fine".to_owned()),
        ];

        let removed = adapter.retain(&mut items, |item| (Some(item.0.clone()), item.1.clone()));
        assert_eq!(removed, 2);
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .all(|item| !item.1.to_lowercase().contains("spam"))
        );
    }

    #[test]
    fn retain_on_an_empty_page_removes_nothing() {
        let adapter =
            ContentFilterAdapter::new(engine(vec![Rule::new(RuleKind::HideKeyword, "spam")]));
        let mut items: Vec<(String, String)> = Vec::new();
        assert_eq!(
            adapter.retain(&mut items, |i| (Some(i.0.clone()), i.1.clone())),
            0
        );
    }

    #[test]
    fn adapters_report_stable_names() {
        let engine = engine(vec![]);
        assert_eq!(
            RequestFilterAdapter::new(Arc::clone(&engine)).name(),
            "request"
        );
        assert_eq!(ContentFilterAdapter::new(engine).name(), "content");
    }
}
