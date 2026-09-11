//! Cache instrumentation.
//!
//! The diagnostics screen (everything stays local, nothing is uploaded) needs to answer three
//! questions without guessing: *is the cache helping*, *is it staying inside its budget*, and *is
//! the disk giving back what was written to it*. Those are the three groups of counters below.
//!
//! ## Why atomics and not a lock
//!
//! Every layer touches these on its hot path, from many tasks at once. A mutex here would serialize
//! cache hits — the one operation that must never wait. All counters use [`Ordering::Relaxed`]:
//! they are diagnostics, no other memory is published through them, and paying for acquire/release
//! ordering on a lookup counter would be a real cost for no observable benefit. The consequence is
//! that a snapshot taken during heavy traffic may be internally skewed by a few events, which is
//! irrelevant for the ratios the screen renders.
//!
//! ## Counters versus gauges
//!
//! Hits, misses, evictions and corruption events are monotonic counters. Bytes and entry counts are
//! gauges the layers write as they change, because deriving them from counters would drift: the
//! memory cache evicts asynchronously, so only the cache itself knows its current size.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use beastube_core::events::CacheChanged;
use serde::{Deserialize, Serialize};

/// Shared, lock-free cache instrumentation.
///
/// One instance is created per [`LayeredCache`](crate::layered::LayeredCache) and shared with both
/// layers, so a single snapshot describes the whole stack rather than three unrelated views.
#[derive(Debug, Default)]
pub struct CacheStats {
    memory_hits: AtomicU64,
    memory_misses: AtomicU64,
    memory_evictions: AtomicU64,
    memory_expirations: AtomicU64,
    memory_bytes: AtomicU64,
    memory_entries: AtomicU64,

    disk_hits: AtomicU64,
    disk_misses: AtomicU64,
    disk_evictions: AtomicU64,
    disk_expirations: AtomicU64,
    disk_bytes: AtomicU64,
    disk_entries: AtomicU64,
    disk_bytes_reclaimed: AtomicU64,

    source_fetches: AtomicU64,
    source_failures: AtomicU64,
    coalesced_waits: AtomicU64,
    corruption_events: AtomicU64,
    prefetches_skipped: AtomicU64,
    write_failures: AtomicU64,
}

impl CacheStats {
    /// Creates a fresh, shared instrumentation handle.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Records a value served from the memory layer.
    pub fn record_memory_hit(&self) {
        self.memory_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a lookup the memory layer could not serve.
    pub fn record_memory_miss(&self) {
        self.memory_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Records an entry the memory layer dropped to stay inside its byte budget.
    pub fn record_memory_eviction(&self) {
        self.memory_evictions.fetch_add(1, Ordering::Relaxed);
    }

    /// Records an entry the memory layer dropped because its lifetime elapsed.
    pub fn record_memory_expiration(&self) {
        self.memory_expirations.fetch_add(1, Ordering::Relaxed);
    }

    /// Publishes the memory layer's current size.
    pub fn set_memory_usage(&self, bytes: u64, entries: u64) {
        self.memory_bytes.store(bytes, Ordering::Relaxed);
        self.memory_entries.store(entries, Ordering::Relaxed);
    }

    /// Records a value served from the disk layer.
    pub fn record_disk_hit(&self) {
        self.disk_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a lookup the disk layer could not serve, for any reason including corruption.
    pub fn record_disk_miss(&self) {
        self.disk_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Records entries the disk layer deleted to stay inside its byte budget.
    pub fn record_disk_evictions(&self, entries: u64, bytes: u64) {
        self.disk_evictions.fetch_add(entries, Ordering::Relaxed);
        self.disk_bytes_reclaimed
            .fetch_add(bytes, Ordering::Relaxed);
    }

    /// Records entries the disk layer deleted because their lifetime elapsed.
    pub fn record_disk_expirations(&self, entries: u64, bytes: u64) {
        self.disk_expirations.fetch_add(entries, Ordering::Relaxed);
        self.disk_bytes_reclaimed
            .fetch_add(bytes, Ordering::Relaxed);
    }

    /// Publishes the disk layer's current size.
    pub fn set_disk_usage(&self, bytes: u64, entries: u64) {
        self.disk_bytes.store(bytes, Ordering::Relaxed);
        self.disk_entries.store(entries, Ordering::Relaxed);
    }



    /// Records a caller that joined an in-flight load instead of starting its own.
    ///
    /// This is the observable evidence that single-flight is working: in a grid where fifty cards
    /// share one avatar, forty-nine of the fifty lookups land here.
    pub fn record_coalesced_wait(&self) {
        self.coalesced_waits.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a stored entry that failed verification and was discarded.
    pub fn record_corruption(&self) {
        self.corruption_events.fetch_add(1, Ordering::Relaxed);
    }



    /// Takes a consistent-enough view of every counter.
    #[must_use]
    pub fn snapshot(&self) -> CacheStatsSnapshot {
        let load = |value: &AtomicU64| value.load(Ordering::Relaxed);
        CacheStatsSnapshot {
            memory_hits: load(&self.memory_hits),
            memory_misses: load(&self.memory_misses),
            memory_evictions: load(&self.memory_evictions),
            memory_expirations: load(&self.memory_expirations),
            memory_bytes: load(&self.memory_bytes),
            memory_entries: load(&self.memory_entries),
            disk_hits: load(&self.disk_hits),
            disk_misses: load(&self.disk_misses),
            disk_evictions: load(&self.disk_evictions),
            disk_expirations: load(&self.disk_expirations),
            disk_bytes: load(&self.disk_bytes),
            disk_entries: load(&self.disk_entries),
            disk_bytes_reclaimed: load(&self.disk_bytes_reclaimed),
            source_fetches: load(&self.source_fetches),
            source_failures: load(&self.source_failures),
            coalesced_waits: load(&self.coalesced_waits),
            corruption_events: load(&self.corruption_events),
            prefetches_skipped: load(&self.prefetches_skipped),
            write_failures: load(&self.write_failures),
        }
    }

    /// Resets every counter, leaving the size gauges alone.
    ///
    /// Used by the diagnostics screen's "reset statistics" affordance. The gauges survive because
    /// they describe what is on disk right now, which a reset button has no business changing.
    pub fn reset_counters(&self) {
        for counter in [
            &self.memory_hits,
            &self.memory_misses,
            &self.memory_evictions,
            &self.memory_expirations,
            &self.disk_hits,
            &self.disk_misses,
            &self.disk_evictions,
            &self.disk_expirations,
            &self.disk_bytes_reclaimed,
            &self.source_fetches,
            &self.source_failures,
            &self.coalesced_waits,
            &self.corruption_events,
            &self.prefetches_skipped,
            &self.write_failures,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
    }
}

/// An immutable view of the cache counters, suitable for IPC.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CacheStatsSnapshot {
    /// Lookups served from memory.
    pub memory_hits: u64,
    /// Lookups memory could not serve.
    pub memory_misses: u64,
    /// Entries memory dropped for size.
    pub memory_evictions: u64,
    /// Entries memory dropped for age.
    pub memory_expirations: u64,
    /// Bytes currently held in memory, including key overhead.
    pub memory_bytes: u64,
    /// Entries currently held in memory.
    pub memory_entries: u64,
    /// Lookups served from disk.
    pub disk_hits: u64,
    /// Lookups disk could not serve, including entries discarded as damaged.
    pub disk_misses: u64,
    /// Entries disk deleted for size.
    pub disk_evictions: u64,
    /// Entries disk deleted for age.
    pub disk_expirations: u64,
    /// Bytes currently held on disk, counting payloads and headers.
    pub disk_bytes: u64,
    /// Entries currently held on disk.
    pub disk_entries: u64,
    /// Bytes reclaimed by eviction and expiry over the session.
    pub disk_bytes_reclaimed: u64,
    /// Fetches issued to the byte source.
    pub source_fetches: u64,
    /// Byte source failures, excluding cancellations.
    pub source_failures: u64,
    /// Lookups that joined an in-flight load rather than starting one.
    pub coalesced_waits: u64,
    /// Stored entries found to be damaged and discarded.
    pub corruption_events: u64,
    /// Prefetches declined because the system was constrained.
    pub prefetches_skipped: u64,
    /// Values that were produced but could not be persisted.
    pub write_failures: u64,
}

impl CacheStatsSnapshot {
    /// Fraction of lookups the memory layer served, in `0.0..=1.0`.
    ///
    /// Returns `0.0` when nothing has been looked up, rather than `NaN`: a fresh cache should read
    /// as "no hits yet" on the diagnostics screen, not as a broken number.
    #[must_use]
    pub fn memory_hit_rate(&self) -> f64 {
        ratio(self.memory_hits, self.memory_hits + self.memory_misses)
    }

    /// Fraction of memory misses the disk layer rescued, in `0.0..=1.0`.
    #[must_use]
    pub fn disk_hit_rate(&self) -> f64 {
        ratio(self.disk_hits, self.disk_hits + self.disk_misses)
    }

    /// Fraction of lookups either layer served, in `0.0..=1.0`.
    ///
    /// This is the number that matters: it is one minus the fraction of lookups that cost a network
    /// request.
    #[must_use]
    pub fn overall_hit_rate(&self) -> f64 {
        let served = self.memory_hits + self.disk_hits;
        ratio(served, served + self.disk_misses)
    }

    /// Total bytes the cache is holding across both layers.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.memory_bytes + self.disk_bytes
    }

    /// Total entries evicted for size across both layers.
    #[must_use]
    pub const fn total_evictions(&self) -> u64 {
        self.memory_evictions + self.disk_evictions
    }

    /// Projects onto the event the UI listens for.
    ///
    /// Emitted by the maintenance task after a sweep so the storage panel updates without polling.
    #[must_use]
    pub const fn to_event(&self) -> CacheChanged {
        CacheChanged {
            disk_bytes: self.disk_bytes,
            memory_bytes: self.memory_bytes,
            evicted_entries: self.total_evictions(),
        }
    }
}

/// Divides two counts, yielding `0.0` for an empty denominator.
#[allow(
    clippy::cast_precision_loss,
    reason = "counts above 2^53 cannot occur in a session, and a displayed ratio tolerates the last bit anyway"
)]
fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_snapshot_reports_no_hits_rather_than_nan() {
        let snapshot = CacheStats::new().snapshot();
        assert_eq!(snapshot, CacheStatsSnapshot::default());
        for rate in [
            snapshot.memory_hit_rate(),
            snapshot.disk_hit_rate(),
            snapshot.overall_hit_rate(),
        ] {
            assert!(
                rate.is_finite(),
                "a ratio over an empty cache must not be NaN"
            );
            assert!((rate - 0.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn hit_rates_measure_each_layer_separately() {
        let stats = CacheStats::new();
        for _ in 0..3 {
            stats.record_memory_hit();
        }
        stats.record_memory_miss();
        stats.record_disk_hit();
        stats.record_disk_miss();
        stats.record_disk_miss();
        stats.record_disk_miss();

        let snapshot = stats.snapshot();
        assert!((snapshot.memory_hit_rate() - 0.75).abs() < 1e-9);
        assert!((snapshot.disk_hit_rate() - 0.25).abs() < 1e-9);
        // Four of seven lookups avoided the network: three from memory, one from disk.
        assert!((snapshot.overall_hit_rate() - 4.0 / 7.0).abs() < 1e-9);
    }

    #[test]
    fn gauges_are_set_not_accumulated() {
        let stats = CacheStats::new();
        stats.set_disk_usage(1000, 10);
        stats.set_disk_usage(400, 4);
        let snapshot = stats.snapshot();
        assert_eq!(snapshot.disk_bytes, 400);
        assert_eq!(snapshot.disk_entries, 4);
    }

    #[test]
    fn reclaimed_bytes_accumulate_across_evictions_and_expiries() {
        let stats = CacheStats::new();
        stats.record_disk_evictions(2, 500);
        stats.record_disk_expirations(1, 250);
        let snapshot = stats.snapshot();
        assert_eq!(snapshot.disk_evictions, 2);
        assert_eq!(snapshot.disk_expirations, 1);
        assert_eq!(snapshot.disk_bytes_reclaimed, 750);
    }

    #[test]
    fn resetting_counters_leaves_the_size_gauges_intact() {
        let stats = CacheStats::new();
        stats.record_memory_hit();
        stats.record_corruption();
        stats.set_memory_usage(4096, 2);
        stats.set_disk_usage(8192, 3);

        stats.reset_counters();

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.memory_hits, 0);
        assert_eq!(snapshot.corruption_events, 0);
        assert_eq!(
            snapshot.memory_bytes, 4096,
            "resetting statistics must not claim the cache is empty"
        );
        assert_eq!(snapshot.disk_bytes, 8192);
        assert_eq!(snapshot.total_bytes(), 12288);
    }

    #[test]
    fn the_event_carries_both_layers_and_all_evictions() {
        let stats = CacheStats::new();
        stats.set_memory_usage(64, 1);
        stats.set_disk_usage(1024, 8);
        stats.record_memory_eviction();
        stats.record_disk_evictions(3, 300);

        let event = stats.snapshot().to_event();
        assert_eq!(event.memory_bytes, 64);
        assert_eq!(event.disk_bytes, 1024);
        assert_eq!(event.evicted_entries, 4);
    }

    #[test]
    fn counters_survive_concurrent_updates_without_loss() {
        let stats = CacheStats::new();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let stats = Arc::clone(&stats);
                std::thread::spawn(move || {
                    for _ in 0..1000 {
                        stats.record_memory_hit();
                        stats.record_coalesced_wait();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("counter thread must not panic");
        }
        let snapshot = stats.snapshot();
        assert_eq!(snapshot.memory_hits, 8000);
        assert_eq!(snapshot.coalesced_waits, 8000);
    }

    #[test]
    fn the_snapshot_round_trips_through_json_for_ipc() {
        let stats = CacheStats::new();
        stats.record_disk_hit();
        stats.set_disk_usage(17, 1);
        let snapshot = stats.snapshot();
        let json = serde_json::to_string(&snapshot).expect("snapshot serializes");
        let back: CacheStatsSnapshot = serde_json::from_str(&json).expect("snapshot deserializes");
        assert_eq!(snapshot, back);
    }
}
