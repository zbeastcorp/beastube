//! Request and task priority.
//!
//! One ordering serves the network queue, the task scheduler and the prefetcher, so that a
//! prefetch can never be scheduled ahead of the segment the user is currently watching. The
//! ordering is derived from the enum declaration order, with [`Priority::Critical`] the greatest,
//! so `BinaryHeap` pops the most urgent work first without a custom comparator.

use serde::{Deserialize, Serialize};

/// Scheduling priority, ordered from least to most urgent.
///
/// Assignments used across the application (§31):
///
/// | Work | Priority |
/// |---|---|
/// | In-flight media segment, player init | [`Priority::Critical`] |
/// | Current search, visible thumbnails, opened video metadata | [`Priority::High`] |
/// | Prefetch of likely-next content, related videos | [`Priority::Normal`] |
/// | Cache cleanup, thumbnail eviction | [`Priority::Low`] |
/// | Database maintenance, rule updates, integrity checks | [`Priority::Background`] |
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Deferrable indefinitely; runs only when nothing else wants the resource.
    Background,
    /// Housekeeping that improves future performance but is invisible now.
    Low,
    /// Speculative work for the near future: prefetch, related content.
    #[default]
    Normal,
    /// Work the user is waiting on right now.
    High,
    /// Playback-critical work. Starving this causes a visible stall.
    Critical,
}

impl Priority {
    /// Every priority, most urgent first. Used by schedulers to drain queues in order.
    pub const ALL_DESCENDING: [Self; 5] = [
        Self::Critical,
        Self::High,
        Self::Normal,
        Self::Low,
        Self::Background,
    ];

    /// Stable lowercase identifier for logs, metrics and the diagnostics screen.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    /// Whether work at this priority may be deferred while the device is on battery saver or the
    /// window is hidden (§129).
    ///
    /// Playback-critical and user-blocking work is never deferred: a background-throttling policy
    /// that stalls the video is a worse outcome than the power it saves.
    #[must_use]
    pub const fn deferrable_on_battery(self) -> bool {
        matches!(self, Self::Background | Self::Low | Self::Normal)
    }

    /// Whether this work should survive a navigation away from the view that requested it.
    ///
    /// Speculative and housekeeping work is cancelled on navigation (§32); work the user is
    /// waiting on, and playback, is not.
    #[must_use]
    pub const fn survives_navigation(self) -> bool {
        matches!(self, Self::Critical)
    }
}

impl std::fmt::Display for Priority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BinaryHeap;

    #[test]
    fn critical_outranks_everything() {
        assert!(Priority::Critical > Priority::High);
        assert!(Priority::High > Priority::Normal);
        assert!(Priority::Normal > Priority::Low);
        assert!(Priority::Low > Priority::Background);
    }

    #[test]
    fn binary_heap_pops_most_urgent_first() {
        let mut heap = BinaryHeap::from([
            Priority::Low,
            Priority::Critical,
            Priority::Background,
            Priority::High,
            Priority::Normal,
        ]);
        let drained: Vec<_> = std::iter::from_fn(|| heap.pop()).collect();
        assert_eq!(drained, Priority::ALL_DESCENDING.to_vec());
    }

    #[test]
    fn playback_work_is_never_deferred_or_cancelled() {
        assert!(!Priority::Critical.deferrable_on_battery());
        assert!(Priority::Critical.survives_navigation());
        // User-blocking work must not be throttled on battery either.
        assert!(!Priority::High.deferrable_on_battery());
    }

    #[test]
    fn speculative_work_is_cancelled_on_navigation() {
        assert!(!Priority::Normal.survives_navigation());
        assert!(!Priority::Low.survives_navigation());
        assert!(!Priority::Background.survives_navigation());
    }

    #[test]
    fn default_is_normal() {
        assert_eq!(Priority::default(), Priority::Normal);
    }

    #[test]
    fn displays_as_its_stable_identifier() {
        assert_eq!(Priority::Critical.to_string(), "critical");
        assert_eq!(format!("{}", Priority::Background), "background");
    }

    #[test]
    fn serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&Priority::Critical).unwrap(),
            "\"critical\""
        );
        let parsed: Priority = serde_json::from_str("\"background\"").unwrap();
        assert_eq!(parsed, Priority::Background);
    }
}
