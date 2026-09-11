//! Filtering diagnostics.
//!
//! ## What this deliberately does not record
//!
//! Counts only. No URL, host, video identifier, channel or title is stored, logged or exposed —
//! not even in memory, and not even when a developer panel is open.
//!
//! That constraint is the whole design. A filtering layer sees every request the application makes,
//! so a diagnostics feature that recorded *what* was filtered would quietly turn the privacy story
//! inside out: the application would hold a complete browsing log, in the one subsystem best placed
//! to build one. Counters answer the questions that actually matter for diagnosis — is filtering
//! on, is the rule set current, is it matching anything, did it roll back — without any of that.
//!
//! There is a test asserting the public snapshot contains no string that could identify content.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use beastube_core::settings::FilteringMode;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::engine::{AllowReason, Decision};

/// Rule counts by category.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleCounts {
    /// Every rule in the set, enabled or not.
    pub total: usize,
    /// Allow rules (host and URL).
    pub allow: usize,
    /// Block rules (host and URL).
    pub block: usize,
    /// Content-hiding rules (channel and keyword).
    pub hide: usize,
    /// Segment-skipping rules.
    pub segment: usize,
}

/// The diagnostics snapshot shown on the developer screen and returned over IPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilteringSnapshot {
    /// Whether filtering is switched on.
    pub enabled: bool,
    /// The active mode.
    pub mode: String,
    /// Version string of the active rule set, or `None` before one is activated.
    pub active_rule_version: Option<String>,
    /// Checksum of the active rule set, so a support question can confirm which set is loaded.
    pub active_checksum: Option<String>,
    /// Rule counts by category.
    pub counts: RuleCounts,
    /// Requests evaluated since start.
    pub evaluated: u64,
    /// Requests blocked since start.
    pub blocked: u64,
    /// Requests allowed because an allow rule matched.
    pub allowed_by_rule: u64,
    /// Requests allowed because the host is playback-critical.
    pub allowed_never_block: u64,
    /// Rule-set updates that failed validation and were not activated.
    pub failed_updates: u64,
    /// Whether the active set is the result of a rollback.
    pub rolled_back: bool,
    /// Rollbacks performed since start.
    pub rollbacks: u64,
    /// i18n key describing why the last rollback happened, if there was one.
    pub rollback_reason_key: Option<String>,
    /// When the active rule set was installed, in Unix milliseconds, or `None` if never.
    pub last_updated_at: Option<i64>,
}

/// Live filtering counters.
///
/// Shared behind an `Arc` by the engine, the rule-set manager and the IPC layer. Counters are
/// atomics so the engine can record a decision on the request path without taking a lock.
#[derive(Debug)]
pub struct FilteringDiagnostics {
    enabled: AtomicBool,
    mode: RwLock<FilteringMode>,
    active: RwLock<Option<ActiveSet>>,
    evaluated: AtomicU64,
    blocked: AtomicU64,
    allowed_by_rule: AtomicU64,
    allowed_never_block: AtomicU64,
    failed_updates: AtomicU64,
    rollbacks: AtomicU64,
    rolled_back: AtomicBool,
    rollback_reason_key: RwLock<Option<String>>,
    last_updated_at: AtomicI64,
}

#[derive(Debug, Clone)]
struct ActiveSet {
    version: String,
    checksum: String,
    counts: RuleCounts,
}

impl Default for FilteringDiagnostics {
    fn default() -> Self {
        Self::new()
    }
}

impl FilteringDiagnostics {
    /// Fresh counters.
    #[must_use]
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            mode: RwLock::new(FilteringMode::Standard),
            active: RwLock::new(None),
            evaluated: AtomicU64::new(0),
            blocked: AtomicU64::new(0),
            allowed_by_rule: AtomicU64::new(0),
            allowed_never_block: AtomicU64::new(0),
            failed_updates: AtomicU64::new(0),
            rollbacks: AtomicU64::new(0),
            rolled_back: AtomicBool::new(false),
            rollback_reason_key: RwLock::new(None),
            // i64::MIN stands for "never", so that a genuine epoch timestamp of 0 is not mistaken
            // for one.
            last_updated_at: AtomicI64::new(i64::MIN),
        }
    }

    /// Records the current configuration.
    pub fn set_config(&self, enabled: bool, mode: FilteringMode) {
        self.enabled.store(enabled, Ordering::Relaxed);
        *self.mode.write() = mode;
    }

    /// Records which rule set is active.
    pub fn set_active_rule_set(&self, version: &str, checksum: &str, counts: RuleCounts) {
        *self.active.write() = Some(ActiveSet {
            version: version.to_owned(),
            checksum: checksum.to_owned(),
            counts,
        });
    }

    /// Records when the active rule set was installed.
    pub fn set_last_updated(&self, millis: i64) {
        self.last_updated_at.store(millis, Ordering::Relaxed);
    }

    /// Records one request decision.
    ///
    /// Takes the decision and the reason, never the request itself.
    pub fn record_decision(&self, decision: Decision, reason: AllowReason) {
        self.evaluated.fetch_add(1, Ordering::Relaxed);
        if decision.is_blocked() {
            self.blocked.fetch_add(1, Ordering::Relaxed);
            return;
        }
        match reason {
            AllowReason::AllowRule => {
                self.allowed_by_rule.fetch_add(1, Ordering::Relaxed);
            }
            AllowReason::NeverBlock => {
                self.allowed_never_block.fetch_add(1, Ordering::Relaxed);
            }
            AllowReason::Disabled | AllowReason::NoMatch => {}
        }
    }

    /// Records a rule-set update that failed validation.
    pub fn record_failed_update(&self) {
        self.failed_updates.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a rollback to the previous rule set.
    ///
    /// Takes the record so the reason can be surfaced, but stores only its i18n key — never a URL
    /// or anything identifying content.
    pub fn record_rollback(&self, record: &crate::ruleset::RollbackRecord) {
        *self.rollback_reason_key.write() = Some(record.reason_key().to_owned());
        self.rollbacks.fetch_add(1, Ordering::Relaxed);
        self.rolled_back.store(true, Ordering::Relaxed);
    }

    /// Clears the rolled-back marker, after a later set activates cleanly.
    pub fn clear_rollback(&self) {
        self.rolled_back.store(false, Ordering::Relaxed);
        *self.rollback_reason_key.write() = None;
    }

    /// Reads every counter.
    #[must_use]
    pub fn snapshot(&self) -> FilteringSnapshot {
        let active = self.active.read().clone();
        let last_updated = self.last_updated_at.load(Ordering::Relaxed);

        FilteringSnapshot {
            enabled: self.enabled.load(Ordering::Relaxed),
            mode: self.mode.read().as_str().to_owned(),
            active_rule_version: active.as_ref().map(|set| set.version.clone()),
            active_checksum: active.as_ref().map(|set| set.checksum.clone()),
            counts: active.map(|set| set.counts).unwrap_or_default(),
            evaluated: self.evaluated.load(Ordering::Relaxed),
            blocked: self.blocked.load(Ordering::Relaxed),
            allowed_by_rule: self.allowed_by_rule.load(Ordering::Relaxed),
            allowed_never_block: self.allowed_never_block.load(Ordering::Relaxed),
            failed_updates: self.failed_updates.load(Ordering::Relaxed),
            rolled_back: self.rolled_back.load(Ordering::Relaxed),
            rollbacks: self.rollbacks.load(Ordering::Relaxed),
            rollback_reason_key: self.rollback_reason_key.read().clone(),
            last_updated_at: (last_updated != i64::MIN).then_some(last_updated),
        }
    }

    /// Resets the request counters, leaving configuration and rule-set identity in place.
    pub fn reset_counters(&self) {
        for counter in [
            &self.evaluated,
            &self.blocked,
            &self.allowed_by_rule,
            &self.allowed_never_block,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions_are_counted_by_outcome() {
        let diagnostics = FilteringDiagnostics::new();
        diagnostics.record_decision(Decision::Block { rule_index: 1 }, AllowReason::NoMatch);
        diagnostics.record_decision(Decision::Allow, AllowReason::AllowRule);
        diagnostics.record_decision(Decision::Allow, AllowReason::NeverBlock);
        diagnostics.record_decision(Decision::Allow, AllowReason::NoMatch);

        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot.evaluated, 4);
        assert_eq!(snapshot.blocked, 1);
        assert_eq!(snapshot.allowed_by_rule, 1);
        assert_eq!(snapshot.allowed_never_block, 1);
    }

    #[test]
    fn the_snapshot_contains_nothing_that_identifies_content() {
        // The load-bearing privacy test. A filtering layer sees every request the application
        // makes; if a diagnostic ever carried one, the application would hold a browsing log in
        // exactly the subsystem best placed to build one.
        let diagnostics = FilteringDiagnostics::new();
        diagnostics.set_config(true, FilteringMode::Strict);
        diagnostics.set_active_rule_set("2026.09.01", "abc123", RuleCounts::default());
        diagnostics.record_decision(Decision::Block { rule_index: 7 }, AllowReason::NoMatch);

        let json = serde_json::to_string(&diagnostics.snapshot()).expect("serializes");

        for forbidden in [
            "youtube",
            "googlevideo",
            "doubleclick",
            "http",
            "://",
            ".com",
            "watch?v=",
            "UC",
        ] {
            assert!(
                !json.contains(forbidden),
                "diagnostics leaked {forbidden:?}: {json}"
            );
        }
    }

    #[test]
    fn never_updated_is_distinguishable_from_the_epoch() {
        let diagnostics = FilteringDiagnostics::new();
        assert_eq!(diagnostics.snapshot().last_updated_at, None);

        // A genuine timestamp of 0 must not read as "never".
        diagnostics.set_last_updated(0);
        assert_eq!(diagnostics.snapshot().last_updated_at, Some(0));
    }

    #[test]
    fn a_rollback_is_visible_and_clearable() {
        let diagnostics = FilteringDiagnostics::new();
        assert!(!diagnostics.snapshot().rolled_back);

        diagnostics.record_rollback(&crate::ruleset::RollbackRecord {
            from_version: "2".to_owned(),
            to_version: "1".to_owned(),
            reason: crate::ruleset::RollbackReason::Manual,
            at: beastube_core::time_util::Timestamp::EPOCH,
        });
        let snapshot = diagnostics.snapshot();
        assert!(snapshot.rolled_back);
        assert_eq!(snapshot.rollbacks, 1);

        diagnostics.clear_rollback();
        let snapshot = diagnostics.snapshot();
        assert!(
            !snapshot.rolled_back,
            "a clean activation clears the marker"
        );
        assert_eq!(snapshot.rollbacks, 1, "the historical count is retained");
    }

    #[test]
    fn resetting_counters_keeps_the_rule_set_identity() {
        let diagnostics = FilteringDiagnostics::new();
        diagnostics.set_active_rule_set(
            "v1",
            "sum",
            RuleCounts {
                total: 5,
                ..RuleCounts::default()
            },
        );
        diagnostics.record_decision(Decision::Block { rule_index: 0 }, AllowReason::NoMatch);

        diagnostics.reset_counters();
        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot.evaluated, 0);
        assert_eq!(snapshot.blocked, 0);
        assert_eq!(snapshot.active_rule_version.as_deref(), Some("v1"));
        assert_eq!(snapshot.counts.total, 5);
    }

    #[test]
    fn counters_survive_concurrent_updates() {
        let diagnostics = std::sync::Arc::new(FilteringDiagnostics::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let diagnostics = std::sync::Arc::clone(&diagnostics);
            handles.push(std::thread::spawn(move || {
                for _ in 0..1000 {
                    diagnostics
                        .record_decision(Decision::Block { rule_index: 0 }, AllowReason::NoMatch);
                }
            }));
        }
        for handle in handles {
            handle.join().expect("worker thread panicked");
        }
        assert_eq!(diagnostics.snapshot().blocked, 8000);
    }
}
