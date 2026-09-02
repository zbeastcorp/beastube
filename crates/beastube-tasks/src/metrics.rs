//! Scheduler counters.
//!
//! Read by the diagnostics screen, which may poll every frame, so every counter is a plain atomic
//! and a snapshot is a handful of relaxed loads. A lock here would mean the diagnostics view could
//! stall the dispatcher, which is exactly backwards — instrumentation must never be able to slow
//! the thing it instruments.
//!
//! Relaxed ordering throughout is deliberate. These are statistics, not synchronization: a snapshot
//! taken while work is in flight is a blurred photograph either way, and no correctness decision
//! anywhere in the crate reads them.

use std::sync::atomic::{AtomicU64, Ordering};

use beastube_core::Priority;
use serde::{Deserialize, Serialize};

/// Number of priorities, used to size the per-priority arrays.
const LEVELS: usize = Priority::ALL_DESCENDING.len();

/// Index of `priority` in the per-level arrays, ascending from `Background` at 0.
const fn level(priority: Priority) -> usize {
    match priority {
        Priority::Background => 0,
        Priority::Low => 1,
        Priority::Normal => 2,
        Priority::High => 3,
        Priority::Critical => 4,
    }
}

/// A per-priority breakdown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorityCounts {
    /// Deferrable maintenance.
    pub background: u64,
    /// Housekeeping.
    pub low: u64,
    /// Speculative work.
    pub normal: u64,
    /// Work the user is waiting on.
    pub high: u64,
    /// Playback-critical work.
    pub critical: u64,
}

impl PriorityCounts {
    fn from_levels(levels: &[AtomicU64; LEVELS]) -> Self {
        Self {
            background: levels[0].load(Ordering::Relaxed),
            low: levels[1].load(Ordering::Relaxed),
            normal: levels[2].load(Ordering::Relaxed),
            high: levels[3].load(Ordering::Relaxed),
            critical: levels[4].load(Ordering::Relaxed),
        }
    }

    /// Total across every priority.
    #[must_use]
    pub const fn total(self) -> u64 {
        self.background + self.low + self.normal + self.high + self.critical
    }

    /// The count for one priority.
    #[must_use]
    pub const fn get(self, priority: Priority) -> u64 {
        match priority {
            Priority::Background => self.background,
            Priority::Low => self.low,
            Priority::Normal => self.normal,
            Priority::High => self.high,
            Priority::Critical => self.critical,
        }
    }
}

/// An immutable view of the counters at one instant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    /// Tasks accepted for execution.
    pub spawned: u64,
    /// Tasks that finished with a value.
    pub completed: u64,
    /// Tasks that finished with an error, after exhausting retries.
    pub failed: u64,
    /// Tasks cancelled before finishing.
    pub cancelled: u64,
    /// Tasks that exceeded their timeout.
    pub timed_out: u64,
    /// Individual retry attempts made, not tasks retried.
    pub retried: u64,
    /// Submissions refused, by reason (queue full or shutting down).
    pub rejected: u64,
    /// Tasks currently executing, per priority.
    pub in_flight: PriorityCounts,
    /// Tasks admitted but not yet started, per priority.
    pub queued: PriorityCounts,
}

impl MetricsSnapshot {
    /// Total tasks currently executing.
    #[must_use]
    pub const fn total_in_flight(self) -> u64 {
        self.in_flight.total()
    }

    /// Total tasks waiting to start.
    #[must_use]
    pub const fn total_queued(self) -> u64 {
        self.queued.total()
    }

    /// Tasks that reached a terminal state, by any route.
    #[must_use]
    pub const fn settled(self) -> u64 {
        self.completed + self.failed + self.cancelled + self.timed_out
    }
}

/// Live scheduler counters.
///
/// Shared behind an `Arc` by the scheduler, the dispatcher and every running task, so it is
/// entirely lock-free.
#[derive(Debug, Default)]
pub struct TaskMetrics {
    spawned: AtomicU64,
    completed: AtomicU64,
    failed: AtomicU64,
    cancelled: AtomicU64,
    timed_out: AtomicU64,
    retried: AtomicU64,
    rejected: AtomicU64,
    in_flight: [AtomicU64; LEVELS],
    queued: [AtomicU64; LEVELS],
}

impl TaskMetrics {
    /// Fresh counters, all zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a task entering the pending queue.
    pub fn record_queued(&self, priority: Priority) {
        self.queued[level(priority)].fetch_add(1, Ordering::Relaxed);
    }

    /// Records a task leaving the pending queue and starting.
    pub fn record_started(&self, priority: Priority) {
        // Saturating rather than wrapping: an underflow here would print an absurd count on the
        // diagnostics screen and send someone hunting a bug that is only in the instrumentation.
        decrement(&self.queued[level(priority)]);
        self.in_flight[level(priority)].fetch_add(1, Ordering::Relaxed);
        self.spawned.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a task leaving the pending queue without starting (cancelled while queued).
    pub fn record_dequeued(&self, priority: Priority) {
        decrement(&self.queued[level(priority)]);
    }

    /// Records a task finishing successfully.
    pub fn record_completed(&self, priority: Priority) {
        decrement(&self.in_flight[level(priority)]);
        self.completed.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a task finishing with an error.
    pub fn record_failed(&self, priority: Priority) {
        decrement(&self.in_flight[level(priority)]);
        self.failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a running task being cancelled.
    pub fn record_cancelled(&self, priority: Priority) {
        decrement(&self.in_flight[level(priority)]);
        self.cancelled.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a task cancelled before it ever started.
    pub fn record_cancelled_while_queued(&self) {
        self.cancelled.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a task exceeding its timeout.
    pub fn record_timed_out(&self, priority: Priority) {
        decrement(&self.in_flight[level(priority)]);
        self.timed_out.fetch_add(1, Ordering::Relaxed);
    }

    /// Records one retry attempt.
    pub fn record_retry(&self) {
        self.retried.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a submission refused.
    pub fn record_rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    /// Reads every counter.
    #[must_use]
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            spawned: self.spawned.load(Ordering::Relaxed),
            completed: self.completed.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            cancelled: self.cancelled.load(Ordering::Relaxed),
            timed_out: self.timed_out.load(Ordering::Relaxed),
            retried: self.retried.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            in_flight: PriorityCounts::from_levels(&self.in_flight),
            queued: PriorityCounts::from_levels(&self.queued),
        }
    }

    /// Tasks currently executing at `priority` or below.
    ///
    /// This is the quantity the cumulative admission policy tests against, so it lives here rather
    /// than being recomputed from a snapshot: taking a whole snapshot on every admission decision
    /// would read ten atomics where four suffice.
    #[must_use]
    pub fn in_flight_at_or_below(&self, priority: Priority) -> u64 {
        self.in_flight[..=level(priority)]
            .iter()
            .map(|counter| counter.load(Ordering::Relaxed))
            .sum()
    }

    /// Tasks currently executing at `priority`.
    #[must_use]
    pub fn in_flight_at(&self, priority: Priority) -> u64 {
        self.in_flight[level(priority)].load(Ordering::Relaxed)
    }
}

/// Decrements without wrapping below zero.
fn decrement(counter: &AtomicU64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_sub(1))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_task_lifecycle_leaves_the_gauges_at_zero() {
        let metrics = TaskMetrics::new();
        metrics.record_queued(Priority::Normal);
        metrics.record_started(Priority::Normal);
        metrics.record_completed(Priority::Normal);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.spawned, 1);
        assert_eq!(snapshot.completed, 1);
        assert_eq!(
            snapshot.total_in_flight(),
            0,
            "in-flight is a gauge, not a total"
        );
        assert_eq!(snapshot.total_queued(), 0);
    }

    #[test]
    fn counts_are_tracked_per_priority() {
        let metrics = TaskMetrics::new();
        metrics.record_queued(Priority::Critical);
        metrics.record_started(Priority::Critical);
        metrics.record_queued(Priority::Background);
        metrics.record_started(Priority::Background);
        metrics.record_queued(Priority::Background);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.in_flight.critical, 1);
        assert_eq!(snapshot.in_flight.background, 1);
        assert_eq!(snapshot.queued.background, 1);
        assert_eq!(snapshot.in_flight.get(Priority::Critical), 1);
        assert_eq!(snapshot.total_in_flight(), 2);
    }

    #[test]
    fn cumulative_in_flight_counts_the_level_and_everything_below_it() {
        // This is the quantity the admission policy tests against, so it must include lower levels.
        let metrics = TaskMetrics::new();
        for priority in [
            Priority::Background,
            Priority::Low,
            Priority::Normal,
            Priority::High,
        ] {
            metrics.record_queued(priority);
            metrics.record_started(priority);
        }

        assert_eq!(metrics.in_flight_at_or_below(Priority::Background), 1);
        assert_eq!(metrics.in_flight_at_or_below(Priority::Low), 2);
        assert_eq!(metrics.in_flight_at_or_below(Priority::Normal), 3);
        assert_eq!(metrics.in_flight_at_or_below(Priority::High), 4);
        assert_eq!(
            metrics.in_flight_at_or_below(Priority::Critical),
            4,
            "no critical work is running, so the total is unchanged"
        );
    }

    #[test]
    fn gauges_never_underflow_on_an_unbalanced_decrement() {
        // A bug elsewhere must not turn into an 18-quintillion count on the diagnostics screen.
        let metrics = TaskMetrics::new();
        metrics.record_completed(Priority::Normal);
        metrics.record_dequeued(Priority::Normal);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.in_flight.normal, 0);
        assert_eq!(snapshot.queued.normal, 0);
    }

    #[test]
    fn every_terminal_route_is_counted_once() {
        let metrics = TaskMetrics::new();
        for priority in [
            Priority::Normal,
            Priority::Normal,
            Priority::Normal,
            Priority::Normal,
        ] {
            metrics.record_queued(priority);
            metrics.record_started(priority);
        }
        metrics.record_completed(Priority::Normal);
        metrics.record_failed(Priority::Normal);
        metrics.record_cancelled(Priority::Normal);
        metrics.record_timed_out(Priority::Normal);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.settled(), 4);
        assert_eq!(snapshot.spawned, 4);
        assert_eq!(snapshot.total_in_flight(), 0);
    }

    #[test]
    fn retries_and_rejections_are_counted_separately_from_tasks() {
        let metrics = TaskMetrics::new();
        metrics.record_retry();
        metrics.record_retry();
        metrics.record_rejected();

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.retried, 2, "retries count attempts, not tasks");
        assert_eq!(snapshot.rejected, 1);
        assert_eq!(snapshot.spawned, 0, "a rejected task was never spawned");
    }

    #[test]
    fn concurrent_updates_are_not_lost() {
        let metrics = std::sync::Arc::new(TaskMetrics::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let metrics = std::sync::Arc::clone(&metrics);
            handles.push(std::thread::spawn(move || {
                for _ in 0..1000 {
                    metrics.record_queued(Priority::Normal);
                    metrics.record_started(Priority::Normal);
                    metrics.record_completed(Priority::Normal);
                }
            }));
        }
        for handle in handles {
            handle.join().expect("worker thread panicked");
        }

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.spawned, 8000);
        assert_eq!(snapshot.completed, 8000);
        assert_eq!(snapshot.total_in_flight(), 0);
        assert_eq!(snapshot.total_queued(), 0);
    }

    #[test]
    fn snapshots_round_trip_for_the_diagnostics_screen() {
        let metrics = TaskMetrics::new();
        metrics.record_queued(Priority::High);
        let snapshot = metrics.snapshot();
        let json = serde_json::to_string(&snapshot).expect("snapshot serializes");
        let back: MetricsSnapshot = serde_json::from_str(&json).expect("snapshot deserializes");
        assert_eq!(back, snapshot);
    }
}
