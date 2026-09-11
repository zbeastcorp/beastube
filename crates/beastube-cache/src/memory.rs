//! L1: the in-process byte cache.
//!
//! ## Why a byte budget and not an entry count
//!
//! A count-bounded cache of images has no bound at all. One thousand 4 KiB avatars and one thousand
//! 2 MiB `maxresdefault` thumbnails are the same number of entries and differ by five hundred times
//! in resident memory. Because this application is expected to stay open for hours, the only
//! bound that means anything is the one measured in bytes — so the cache is built with a weigher
//! and its capacity is expressed in bytes.
//!
//! The weight of an entry is its payload plus its key plus a fixed per-entry overhead. Counting the
//! key matters: the keys here are URLs, routinely 100–200 bytes, which for a cache full of small
//! avatars is a significant fraction of the real footprint. Counting a fixed overhead matters for
//! the same reason — a cache of ten thousand 200-byte entries costs far more than 2 MB in practice.
//!
//! ## What "bounded" actually promises
//!
//! `moka` evicts asynchronously: an insert that overshoots the budget is admitted and the excess is
//! reclaimed by a housekeeping pass shortly afterwards. The cache is therefore bounded in the
//! steady state, not instantaneously. That is the correct trade — making every insert wait for a
//! synchronous eviction would put a lock on the hot path of a scrolling grid — but it is why
//! [`MemoryCache::run_pending_tasks`] exists, and why the size gauge is documented as approximate.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use moka::future::Cache;
use moka::notification::RemovalCause;

use crate::key::{CacheKey, Namespace};
use crate::stats::CacheStats;

/// Bytes charged to every entry on top of its payload and key.
///
/// Covers the `Bytes` handle, the `Arc<str>` key allocation, and the cache's own per-entry
/// bookkeeping (hash table slot, LRU links, expiry timestamps). Deliberately generous: a budget
/// that under-counts is not a budget.
pub const ENTRY_OVERHEAD_BYTES: usize = 96;

/// The in-process byte cache.
///
/// Cloning shares one cache: the inner handle is reference counted, matching the way
/// [`Database`](beastube_db::Database) is passed around.
#[derive(Clone)]
pub struct MemoryCache {
    inner: Cache<CacheKey, Bytes>,
    stats: Arc<CacheStats>,
    budget_bytes: u64,
}

// The moka cache holds arbitrary entry values; printing them would dump cached media bytes into a log.
#[allow(clippy::missing_fields_in_debug)]
impl std::fmt::Debug for MemoryCache {
    /// Reports size rather than contents: dumping cached image bytes into a log would be both
    /// enormous and, for a private browsing session, indiscreet.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryCache")
            .field("entries", &self.inner.entry_count())
            .field("bytes", &self.inner.weighted_size())
            .field("budget_bytes", &self.budget_bytes)
            .finish()
    }
}

impl MemoryCache {
    /// Builds a cache bounded by `budget_bytes` whose entries expire `ttl` after they were written.
    ///
    /// Expiry is time-to-live rather than time-to-idle. A thumbnail that is looked at every few
    /// seconds for an hour should still be re-validated eventually, because the upstream image can
    /// be replaced; time-to-idle would let a hot entry live forever.
    #[must_use]
    pub fn new(budget_bytes: u64, ttl: Duration, stats: Arc<CacheStats>) -> Self {
        let eviction_stats = Arc::clone(&stats);
        let inner = Cache::builder()
            .name("beastube-l1")
            .max_capacity(budget_bytes)
            .weigher(|key: &CacheKey, value: &Bytes| {
                // Saturating at `u32::MAX` makes an absurdly large entry look maximally expensive,
                // so it is evicted first. Saturating the other way would hide it from the budget.
                u32::try_from(value.len() + key.weight() + ENTRY_OVERHEAD_BYTES).unwrap_or(u32::MAX)
            })
            .time_to_live(ttl)
            .eviction_listener(move |_key, _value, cause| match cause {
                RemovalCause::Size => eviction_stats.record_memory_eviction(),
                RemovalCause::Expired => eviction_stats.record_memory_expiration(),
                // `Explicit` is an invalidation the application asked for and `Replaced` is an
                // overwrite; neither is memory pressure, and counting them would make the
                // diagnostics screen report eviction churn that is not happening.
                RemovalCause::Explicit | RemovalCause::Replaced => {}
            })
            .build();
        Self {
            inner,
            stats,
            budget_bytes,
        }
    }

    /// Looks up `key`, recording the outcome.
    pub async fn get(&self, key: &CacheKey) -> Option<Bytes> {
        let found = self.inner.get(key).await;
        if found.is_some() {
            self.stats.record_memory_hit();
        } else {
            self.stats.record_memory_miss();
        }
        found
    }

    /// Looks up `key` without recording a hit or a miss.
    ///
    /// Used by prefetch, which asks "is this already here?" — a question that is not a cache lookup
    /// and would otherwise inflate the hit rate with traffic the user never requested.
    pub async fn contains(&self, key: &CacheKey) -> bool {
        self.inner.get(key).await.is_some()
    }

    /// Stores `value` under `key`, replacing any previous value.
    pub async fn insert(&self, key: CacheKey, value: Bytes) {
        self.inner.insert(key, value).await;
        self.publish_usage();
    }

    /// Drops `key` if present.
    pub async fn invalidate(&self, key: &CacheKey) {
        self.inner.invalidate(key).await;
        self.publish_usage();
    }

    /// Drops every entry in `namespace`, leaving other namespaces untouched.
    ///
    /// Implemented by iterating rather than with `moka`'s predicate invalidation, which requires
    /// the cache to retain an extra timestamp per entry for the lifetime of the process. Clearing a
    /// namespace happens when the user presses a button, so paying a full scan then is a better
    /// trade than paying per-entry memory forever.
    pub async fn clear(&self, namespace: Namespace) {
        let doomed: Vec<CacheKey> = self
            .inner
            .iter()
            .filter(|(key, _)| key.namespace() == namespace)
            .map(|(key, _)| CacheKey::clone(&key))
            .collect();
        for key in doomed {
            self.inner.invalidate(&key).await;
        }
        self.publish_usage();
    }

    /// Drops every entry.
    pub async fn clear_all(&self) {
        self.inner.invalidate_all();
        self.inner.run_pending_tasks().await;
        self.publish_usage();
    }

    /// Bytes currently resident, as `moka` accounts them.
    ///
    /// Approximate between housekeeping passes; call [`MemoryCache::run_pending_tasks`] first when
    /// an exact figure is needed.
    #[must_use]
    pub fn weighted_bytes(&self) -> u64 {
        self.inner.weighted_size()
    }

    /// Entries currently resident. Approximate for the same reason as [`Self::weighted_bytes`].
    #[must_use]
    pub fn entry_count(&self) -> u64 {
        self.inner.entry_count()
    }

    /// The configured byte budget.
    #[must_use]
    pub const fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    /// Runs pending eviction and expiry work now.
    ///
    /// Exposed because the shutdown sequence and the diagnostics screen both want figures that are
    /// exact rather than eventually exact.
    pub async fn run_pending_tasks(&self) {
        self.inner.run_pending_tasks().await;
        self.publish_usage();
    }

    /// Pushes the current size into the shared statistics.
    fn publish_usage(&self) {
        self.stats
            .set_memory_usage(self.inner.weighted_size(), self.inner.entry_count());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOREVER: Duration = Duration::from_secs(3600);

    fn key(name: &str) -> CacheKey {
        CacheKey::new(Namespace::THUMBNAILS, name).expect("short test key")
    }

    fn payload(len: usize) -> Bytes {
        Bytes::from(vec![0xAB; len])
    }

    #[tokio::test]
    async fn a_stored_value_comes_back_and_is_counted_as_a_hit() {
        let stats = CacheStats::new();
        let cache = MemoryCache::new(1 << 20, FOREVER, Arc::clone(&stats));

        assert!(cache.get(&key("absent")).await.is_none());
        cache.insert(key("a"), payload(16)).await;
        assert_eq!(cache.get(&key("a")).await, Some(payload(16)));

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.memory_hits, 1);
        assert_eq!(snapshot.memory_misses, 1);
    }

    #[tokio::test]
    async fn contains_does_not_move_the_hit_rate() {
        let stats = CacheStats::new();
        let cache = MemoryCache::new(1 << 20, FOREVER, Arc::clone(&stats));
        cache.insert(key("a"), payload(16)).await;

        assert!(cache.contains(&key("a")).await);
        assert!(!cache.contains(&key("b")).await);

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.memory_hits, 0);
        assert_eq!(snapshot.memory_misses, 0);
    }

    #[tokio::test]
    async fn the_budget_is_measured_in_bytes_not_entries() {
        // Budget for roughly ten 1 KiB entries. A count-bounded cache would happily hold a hundred.
        let budget = 10 * (1024 + ENTRY_OVERHEAD_BYTES as u64 + 64);
        let stats = CacheStats::new();
        let cache = MemoryCache::new(budget, FOREVER, Arc::clone(&stats));

        for i in 0..200 {
            cache
                .insert(key(&format!("entry-{i}")), payload(1024))
                .await;
        }
        cache.run_pending_tasks().await;

        assert!(
            cache.weighted_bytes() <= budget,
            "resident bytes {} exceeded the {budget} byte budget",
            cache.weighted_bytes()
        );
        assert!(
            cache.entry_count() < 200,
            "nothing was evicted, so the budget is not being enforced"
        );
        assert!(
            stats.snapshot().memory_evictions > 0,
            "eviction must be visible on the diagnostics screen"
        );
    }

    #[tokio::test]
    async fn one_large_value_does_not_blow_the_budget() {
        let budget = 4096;
        let stats = CacheStats::new();
        let cache = MemoryCache::new(budget, FOREVER, Arc::clone(&stats));

        cache.insert(key("small"), payload(64)).await;
        cache.insert(key("huge"), payload(1 << 20)).await;
        cache.run_pending_tasks().await;

        assert!(
            cache.weighted_bytes() <= budget,
            "a single oversized value must not become a permanent leak"
        );
    }

    #[tokio::test]
    async fn the_key_counts_towards_the_weight() {
        // Two caches with the same payloads but wildly different key lengths must not report the
        // same footprint, or a cache of long URLs silently exceeds its budget.
        let stats = CacheStats::new();
        let cache = MemoryCache::new(1 << 30, FOREVER, Arc::clone(&stats));
        cache.insert(key("k"), payload(8)).await;
        cache.run_pending_tasks().await;
        let with_short_key = cache.weighted_bytes();

        let stats2 = CacheStats::new();
        let cache2 = MemoryCache::new(1 << 30, FOREVER, stats2);
        cache2.insert(key(&"k".repeat(2000)), payload(8)).await;
        cache2.run_pending_tasks().await;

        assert!(
            cache2.weighted_bytes() > with_short_key + 1900,
            "a 2 KB key must be charged for"
        );
    }

    #[tokio::test]
    async fn entries_expire_after_their_lifetime() {
        let stats = CacheStats::new();
        let cache = MemoryCache::new(1 << 20, Duration::from_millis(40), Arc::clone(&stats));
        cache.insert(key("a"), payload(16)).await;
        assert!(cache.get(&key("a")).await.is_some());

        tokio::time::sleep(Duration::from_millis(90)).await;

        assert!(
            cache.get(&key("a")).await.is_none(),
            "an expired entry must not be served"
        );
        cache.run_pending_tasks().await;
        assert_eq!(
            stats.snapshot().memory_expirations,
            1,
            "expiry is not eviction and must be counted separately"
        );
        assert_eq!(
            stats.snapshot().memory_evictions,
            0,
            "an idle cache must not report memory pressure"
        );
    }

    #[tokio::test]
    async fn clearing_one_namespace_leaves_the_others() {
        let stats = CacheStats::new();
        let cache = MemoryCache::new(1 << 20, FOREVER, Arc::clone(&stats));
        let thumbnail = CacheKey::new(Namespace::THUMBNAILS, "shared-name").unwrap();
        let metadata = CacheKey::new(Namespace::METADATA, "shared-name").unwrap();
        cache.insert(thumbnail.clone(), payload(8)).await;
        cache.insert(metadata.clone(), payload(8)).await;

        cache.clear(Namespace::THUMBNAILS).await;

        assert!(cache.get(&thumbnail).await.is_none());
        assert!(
            cache.get(&metadata).await.is_some(),
            "clearing thumbnails must not discard cached metadata"
        );
        assert_eq!(
            stats.snapshot().memory_evictions,
            0,
            "an explicit clear is not an eviction"
        );
    }

    #[tokio::test]
    async fn clear_all_empties_the_cache_and_the_gauge() {
        let stats = CacheStats::new();
        let cache = MemoryCache::new(1 << 20, FOREVER, Arc::clone(&stats));
        for i in 0..10 {
            cache.insert(key(&format!("k{i}")), payload(128)).await;
        }
        cache.run_pending_tasks().await;
        assert!(stats.snapshot().memory_bytes > 0);

        cache.clear_all().await;

        assert_eq!(cache.entry_count(), 0);
        assert_eq!(cache.weighted_bytes(), 0);
        assert_eq!(stats.snapshot().memory_bytes, 0);
    }

    #[tokio::test]
    async fn invalidating_a_key_leaves_its_neighbours() {
        let stats = CacheStats::new();
        let cache = MemoryCache::new(1 << 20, FOREVER, stats);
        cache.insert(key("a"), payload(8)).await;
        cache.insert(key("b"), payload(8)).await;

        cache.invalidate(&key("a")).await;

        assert!(cache.get(&key("a")).await.is_none());
        assert!(cache.get(&key("b")).await.is_some());
    }

    #[tokio::test]
    async fn overwriting_a_key_does_not_report_an_eviction() {
        let stats = CacheStats::new();
        let cache = MemoryCache::new(1 << 20, FOREVER, Arc::clone(&stats));
        cache.insert(key("a"), payload(8)).await;
        cache.insert(key("a"), payload(4096)).await;
        cache.run_pending_tasks().await;

        assert_eq!(cache.entry_count(), 1);
        assert_eq!(cache.get(&key("a")).await, Some(payload(4096)));
        assert_eq!(stats.snapshot().memory_evictions, 0);
    }

    #[tokio::test]
    async fn an_empty_value_round_trips() {
        let cache = MemoryCache::new(1 << 20, FOREVER, CacheStats::new());
        cache.insert(key("empty"), Bytes::new()).await;
        assert_eq!(cache.get(&key("empty")).await, Some(Bytes::new()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writers_leave_the_cache_consistent() {
        let stats = CacheStats::new();
        let cache = MemoryCache::new(1 << 22, FOREVER, Arc::clone(&stats));

        let handles: Vec<_> = (0..32)
            .map(|i| {
                let cache = cache.clone();
                tokio::spawn(async move {
                    for j in 0..32 {
                        cache.insert(key(&format!("k-{i}-{j}")), payload(256)).await;
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.await.expect("no writer should panic");
        }
        cache.run_pending_tasks().await;

        assert_eq!(cache.entry_count(), 32 * 32);
        assert_eq!(stats.snapshot().memory_entries, 32 * 32);
    }
}
