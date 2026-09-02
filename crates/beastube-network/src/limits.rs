//! Per-host concurrency limiting, ordered by [`Priority`].
//!
//! ## Why a limit per host rather than one global limit
//!
//! A global cap starves whichever host is unlucky: a burst of thumbnail fetches from `ytimg.com`
//! would queue the media segment from `googlevideo.com` behind it, and the user sees a stall while
//! the link is idle. Bounding each host separately keeps unrelated hosts independent, which is the
//! property playback needs.
//!
//! ## Why six
//!
//! Six is the per-origin connection limit browsers converged on, and it is what CDNs are tuned
//! for. Below it, a page of thumbnails serialises visibly. Above it, added parallelism mostly
//! converts into queueing latency at the far end plus a higher chance of being rate-limited, since
//! the connections share one bottleneck link anyway. The limit is matched to
//! `pool_max_idle_per_host` on the shared client (see [`crate::client`]) so that a request which
//! wins a slot finds a warm connection instead of paying a TLS handshake. It is configurable via
//! [`beastube_core::settings::NetworkSettings::max_concurrent_requests`].
//!
//! ## Why not `tokio::sync::Semaphore`
//!
//! A semaphore hands out permits in FIFO order. That is the wrong order here: a prefetch that
//! queued a moment before a playback segment would be served first, which is exactly the inversion
//! [`Priority`] exists to prevent (§31). This gate keeps an explicit priority queue and hands a
//! released slot to the most urgent waiter, breaking ties in arrival order so that equal-priority
//! work stays fair.
//!
//! ## Cancellation
//!
//! Dropping the future returned by [`HostLimiter::acquire`] is the cancellation path, and it is
//! exact in both directions: a waiter that never received a slot removes itself from the queue,
//! and a waiter that was handed a slot in the instant before it was dropped passes that slot on to
//! the next waiter rather than leaking it.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

use beastube_core::Priority;
use parking_lot::Mutex;
use tokio::sync::oneshot;
use url::Url;

/// Default simultaneous requests permitted to one host. See the module documentation for why six.
pub const DEFAULT_MAX_CONCURRENT_PER_HOST: usize = 6;

/// Bounds simultaneous requests per host, serving waiters in priority order.
///
/// Cloning is cheap and shares the same state, so this is passed by value into tasks rather than
/// being wrapped in another `Arc`.
#[derive(Debug, Clone)]
pub struct HostLimiter {
    shared: Arc<LimiterShared>,
}

#[derive(Debug)]
struct LimiterShared {
    limit: usize,
    hosts: Mutex<HashMap<String, Arc<HostGate>>>,
}

#[derive(Debug)]
struct HostGate {
    /// The key this gate is filed under, so retiring it needs no reverse lookup.
    key: String,
    state: Mutex<GateState>,
}

#[derive(Debug)]
struct GateState {
    /// Slots not currently held by a permit.
    available: usize,
    /// Waiters in service order. Entries whose id is absent from `waiters` are tombstones left by
    /// a cancelled waiter; a `BinaryHeap` cannot remove from the middle, so they are skipped when
    /// popped instead.
    order: BinaryHeap<QueuedWaiter>,
    /// Live waiters by id. Presence here is the authoritative "not yet granted" flag.
    waiters: HashMap<u64, oneshot::Sender<()>>,
    next_id: u64,
}

/// A queued waiter, ordered so that `BinaryHeap::pop` yields the most urgent, oldest waiter.
///
/// Field order is the comparison order: [`Priority`] first, then `Reverse(id)` so that a smaller
/// id — an earlier arrival — compares greater and therefore wins a tie.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct QueuedWaiter {
    priority: Priority,
    arrival: Reverse<u64>,
    id: u64,
}

impl HostLimiter {
    /// Creates a limiter admitting `limit` simultaneous requests per host.
    ///
    /// `limit` is clamped to at least one: a limiter that admits nothing would deadlock every
    /// request rather than failing visibly.
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            shared: Arc::new(LimiterShared {
                limit: limit.max(1),
                hosts: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The per-host limit in force.
    #[must_use]
    pub fn limit(&self) -> usize {
        self.shared.limit
    }

    /// Number of hosts currently holding queue state.
    ///
    /// Exposed for the diagnostics screen and for the test that asserts idle hosts are retired: a
    /// provider response can name arbitrarily many hostnames, so this map must not be a place
    /// where memory accumulates for the life of the process.
    #[must_use]
    pub fn tracked_hosts(&self) -> usize {
        self.shared.hosts.lock().len()
    }

    /// Waits for a slot on `key`, then returns the permit that holds it.
    ///
    /// Dropping the returned future before it completes is the cancellation path; see the module
    /// documentation. The permit releases its slot when dropped.
    pub async fn acquire(&self, key: &str, priority: Priority) -> HostPermit {
        loop {
            let gate = self.shared.gate_for(key);
            let registration = {
                let mut state = gate.state.lock();
                if state.available > 0 {
                    state.available -= 1;
                    None
                } else {
                    Some(state.enqueue(priority))
                }
            };

            let Some((id, receiver)) = registration else {
                return HostPermit {
                    shared: Arc::clone(&self.shared),
                    gate,
                };
            };

            // The gate moves into the guard: while this task is parked, the guard must hold the
            // only reference besides the map's, or the idle-gate retirement below cannot tell a
            // busy gate from an abandoned one.
            let mut guard = WaiterGuard {
                shared: Arc::clone(&self.shared),
                gate: Some(gate),
                id,
            };
            let granted = receiver.await.is_ok();
            let recovered = guard.gate.take();
            drop(guard);

            if let Some(gate) = recovered
                && granted
            {
                return HostPermit {
                    shared: Arc::clone(&self.shared),
                    gate,
                };
            }
            // The sender vanished without granting, which can only happen if the gate was retired
            // underneath us. Re-register against whatever gate exists now.
        }
    }

    /// Extracts the limiter key for `url`: the authority, host plus port.
    ///
    /// Port is included because two ports on one machine are two servers as far as connection
    /// limits are concerned, and because the connection pool keys the same way — a limit that
    /// disagreed with the pool would either admit requests the pool cannot serve warm, or hold
    /// back requests it could.
    #[must_use]
    pub fn key_for(url: &Url) -> String {
        match (url.host_str(), url.port_or_known_default()) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (Some(host), None) => host.to_owned(),
            // A URL without a host never reaches here: validation rejects it first. Falling back
            // to the scheme keeps every such request in one bucket rather than panicking.
            (None, _) => url.scheme().to_owned(),
        }
    }
}

impl Default for HostLimiter {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_CONCURRENT_PER_HOST)
    }
}

impl GateState {
    /// Registers a waiter and returns its id and the channel it will be woken on.
    fn enqueue(&mut self, priority: Priority) -> (u64, oneshot::Receiver<()>) {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let (sender, receiver) = oneshot::channel();
        self.waiters.insert(id, sender);
        self.order.push(QueuedWaiter {
            priority,
            arrival: Reverse(id),
            id,
        });
        (id, receiver)
    }

    /// Hands a freed slot directly to the most urgent waiter.
    ///
    /// Returns `true` when a waiter took the slot. The hand-off is direct rather than "increment
    /// the count and wake someone" so that a request arriving in the same instant cannot barge
    /// ahead of a higher-priority waiter that was already queued.
    fn grant(&mut self) -> bool {
        while let Some(queued) = self.order.pop() {
            if let Some(sender) = self.waiters.remove(&queued.id) {
                // A failed send means the waiter was dropped between being chosen and being woken.
                // Its guard has not run yet — it cannot, we hold the lock — and when it does it
                // will find itself absent from `waiters` and pass the slot on. The slot is
                // accounted for either way, so this arm must not put it back.
                let _ = sender.send(());
                return true;
            }
        }
        false
    }

    fn is_idle(&self, limit: usize) -> bool {
        self.available >= limit && self.waiters.is_empty()
    }
}

impl LimiterShared {
    fn gate_for(&self, key: &str) -> Arc<HostGate> {
        let mut hosts = self.hosts.lock();
        if let Some(existing) = hosts.get(key) {
            return Arc::clone(existing);
        }
        let gate = Arc::new(HostGate {
            key: key.to_owned(),
            state: Mutex::new(GateState {
                available: self.limit,
                order: BinaryHeap::new(),
                waiters: HashMap::new(),
                next_id: 0,
            }),
        });
        hosts.insert(key.to_owned(), Arc::clone(&gate));
        gate
    }

    /// Returns a slot, either to the next waiter or to the pool.
    fn release(&self, gate: &Arc<HostGate>) {
        let idle = {
            let mut state = gate.state.lock();
            if state.grant() {
                return;
            }
            state.available += 1;
            state.is_idle(self.limit)
        };
        if idle {
            self.retire(gate);
        }
    }

    /// Drops an idle gate from the map so hostnames do not accumulate for the life of the process.
    ///
    /// The strong-count test is what makes this safe. A task can only obtain this gate by cloning
    /// the `Arc` out of `hosts`, which requires the lock held here, so once the count is down to
    /// the map's reference plus the caller's, no new acquirer can appear behind us. Callers
    /// therefore must hold exactly one reference — which is why [`HostLimiter::acquire`] moves the
    /// gate into the waiter guard instead of keeping a second handle across the await.
    fn retire(&self, gate: &Arc<HostGate>) {
        let mut hosts = self.hosts.lock();
        if Arc::strong_count(gate) > 2 {
            return;
        }
        if !gate.state.lock().is_idle(self.limit) {
            return;
        }
        if hosts
            .get(&gate.key)
            .is_some_and(|held| Arc::ptr_eq(held, gate))
        {
            hosts.remove(&gate.key);
        }
    }
}

/// Holds one slot on a host for as long as it lives.
#[derive(Debug)]
pub struct HostPermit {
    shared: Arc<LimiterShared>,
    gate: Arc<HostGate>,
}

impl HostPermit {
    /// The limiter key this permit is held against.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.gate.key
    }
}

impl Drop for HostPermit {
    fn drop(&mut self) {
        self.shared.release(&self.gate);
    }
}

/// Removes a queued waiter that is being cancelled, or passes on a slot granted to it too late.
///
/// `gate` is `None` once the waiter has taken ownership of the outcome, which is how the guard is
/// disarmed on the success path without a separate flag.
struct WaiterGuard {
    shared: Arc<LimiterShared>,
    gate: Option<Arc<HostGate>>,
    id: u64,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        let Some(gate) = self.gate.take() else {
            return;
        };
        let idle_after_removal = {
            let mut state = gate.state.lock();
            if state.waiters.remove(&self.id).is_some() {
                Some(state.is_idle(self.shared.limit))
            } else {
                None
            }
        };
        match idle_after_removal {
            // We left the queue without ever holding a slot.
            Some(true) => self.shared.retire(&gate),
            Some(false) => {}
            // A slot was handed to us in the window before this drop. Nobody else knows it exists,
            // so returning it here is what keeps the gate from leaking capacity on cancellation.
            None => self.shared.release(&gate),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;
    use std::time::Duration;

    /// Registers a waiter deterministically: `acquire` enqueues on its first poll, so polling once
    /// and observing `Pending` proves this waiter is queued before the next one is created.
    macro_rules! pending_waiter {
        ($limiter:expr, $priority:expr) => {{
            let mut future = Box::pin($limiter.acquire("host:443", $priority));
            assert!(
                futures::poll!(future.as_mut()).is_pending(),
                "expected the waiter to queue"
            );
            future
        }};
    }

    macro_rules! expect_served {
        ($future:expr) => {
            match futures::poll!($future.as_mut()) {
                Poll::Ready(permit) => permit,
                Poll::Pending => panic!("expected this waiter to be served"),
            }
        };
    }

    #[tokio::test]
    async fn a_permit_is_available_immediately_while_under_the_limit() {
        let limiter = HostLimiter::new(2);
        let first = limiter.acquire("host:443", Priority::Normal).await;
        let second = limiter.acquire("host:443", Priority::Normal).await;
        assert_eq!(first.key(), "host:443");
        drop((first, second));
    }

    #[tokio::test]
    async fn the_limit_is_enforced_per_host_not_globally() {
        let limiter = HostLimiter::new(1);
        let held = limiter.acquire("a:443", Priority::Normal).await;
        let other = tokio::time::timeout(
            Duration::from_millis(250),
            limiter.acquire("b:443", Priority::Normal),
        )
        .await;
        assert!(other.is_ok(), "hosts must not share a budget");
        drop(held);
    }

    #[tokio::test]
    async fn a_released_slot_goes_to_the_most_urgent_waiter() {
        let limiter = HostLimiter::new(1);
        let held = limiter.acquire("host:443", Priority::Normal).await;

        let mut low = pending_waiter!(limiter, Priority::Low);
        let mut critical = pending_waiter!(limiter, Priority::Critical);
        let mut normal = pending_waiter!(limiter, Priority::Normal);

        drop(held);
        assert!(
            futures::poll!(low.as_mut()).is_pending(),
            "low priority must not be served before critical"
        );
        assert!(
            futures::poll!(normal.as_mut()).is_pending(),
            "normal priority must not be served before critical"
        );
        let critical_permit = expect_served!(critical);

        drop(critical_permit);
        assert!(
            futures::poll!(low.as_mut()).is_pending(),
            "normal outranks low for the second slot"
        );
        let normal_permit = expect_served!(normal);

        drop(normal_permit);
        drop(expect_served!(low));
    }

    #[tokio::test]
    async fn equal_priorities_are_served_in_arrival_order() {
        let limiter = HostLimiter::new(1);
        let held = limiter.acquire("host:443", Priority::Normal).await;

        let mut first = pending_waiter!(limiter, Priority::Normal);
        let mut second = pending_waiter!(limiter, Priority::Normal);

        drop(held);
        assert!(
            futures::poll!(second.as_mut()).is_pending(),
            "a tie must be broken by arrival, not by who polls first"
        );
        drop(expect_served!(first));
    }

    #[tokio::test]
    async fn a_cancelled_waiter_leaves_the_queue_without_consuming_a_slot() {
        let limiter = HostLimiter::new(1);
        let held = limiter.acquire("host:443", Priority::Normal).await;

        let abandoned = pending_waiter!(limiter, Priority::Critical);
        let mut survivor = pending_waiter!(limiter, Priority::Normal);
        drop(abandoned);

        drop(held);
        drop(expect_served!(survivor));
    }

    #[tokio::test]
    async fn a_slot_granted_to_a_waiter_that_vanished_is_passed_on() {
        let limiter = HostLimiter::new(1);
        let held = limiter.acquire("host:443", Priority::Normal).await;

        let granted = pending_waiter!(limiter, Priority::Critical);
        let mut next = pending_waiter!(limiter, Priority::Normal);

        // The slot is handed to the critical waiter, which is then dropped without ever being
        // polled again — the exact window in which a naive implementation loses the slot forever.
        drop(held);
        drop(granted);

        drop(expect_served!(next));
    }

    #[tokio::test]
    async fn every_slot_survives_a_storm_of_cancellations() {
        let limiter = HostLimiter::new(2);
        let held_a = limiter.acquire("host:443", Priority::Normal).await;
        let held_b = limiter.acquire("host:443", Priority::Normal).await;

        for _ in 0..200 {
            let waiter = pending_waiter!(limiter, Priority::High);
            drop(waiter);
        }
        drop((held_a, held_b));

        // If any slot had been lost, one of these would hang.
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            let a = limiter.acquire("host:443", Priority::Normal).await;
            let b = limiter.acquire("host:443", Priority::Normal).await;
            drop((a, b));
        })
        .await;
        assert!(result.is_ok(), "capacity was lost to cancellation");
    }

    #[tokio::test]
    async fn concurrency_never_exceeds_the_limit() {
        let limiter = HostLimiter::new(3);
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..64 {
            let limiter = limiter.clone();
            let live = Arc::clone(&live);
            let peak = Arc::clone(&peak);
            handles.push(tokio::spawn(async move {
                let permit = limiter.acquire("host:443", Priority::Normal).await;
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::task::yield_now().await;
                live.fetch_sub(1, Ordering::SeqCst);
                drop(permit);
            }));
        }
        for handle in handles {
            handle.await.expect("task panicked");
        }
        let observed = peak.load(Ordering::SeqCst);
        assert!(
            observed <= 3,
            "observed {observed} concurrent holders with a limit of 3"
        );
        assert!(
            observed > 1,
            "the test did not actually exercise concurrency"
        );
    }

    #[tokio::test]
    async fn idle_hosts_are_retired_so_the_map_cannot_grow_without_bound() {
        let limiter = HostLimiter::new(2);
        for index in 0..500 {
            let permit = limiter
                .acquire(&format!("host-{index}:443"), Priority::Low)
                .await;
            drop(permit);
        }
        assert_eq!(
            limiter.tracked_hosts(),
            0,
            "a provider naming many hostnames must not leak queue state"
        );
    }

    #[tokio::test]
    async fn a_cancelled_waiter_also_retires_the_host_it_leaves_empty() {
        let limiter = HostLimiter::new(1);
        let held = limiter.acquire("host:443", Priority::Normal).await;
        let waiter = pending_waiter!(limiter, Priority::Normal);
        drop(held);
        // `held` handed its slot to the waiter, which is then abandoned.
        drop(waiter);
        assert_eq!(limiter.tracked_hosts(), 0);
    }

    #[tokio::test]
    async fn a_busy_host_is_not_retired_from_under_its_waiters() {
        let limiter = HostLimiter::new(1);
        let held = limiter.acquire("host:443", Priority::Normal).await;
        let mut waiter = pending_waiter!(limiter, Priority::Normal);
        assert_eq!(limiter.tracked_hosts(), 1);
        drop(held);
        let permit = expect_served!(waiter);
        assert_eq!(limiter.tracked_hosts(), 1, "the slot is still held");
        drop(permit);
        assert_eq!(limiter.tracked_hosts(), 0);
    }

    #[test]
    fn a_zero_limit_is_clamped_rather_than_deadlocking() {
        assert_eq!(HostLimiter::new(0).limit(), 1);
        assert_eq!(
            HostLimiter::default().limit(),
            DEFAULT_MAX_CONCURRENT_PER_HOST
        );
    }

    #[test]
    fn keys_separate_ports_and_default_them_by_scheme() {
        let key = |raw: &str| HostLimiter::key_for(&Url::parse(raw).expect("test URL parses"));
        assert_eq!(key("https://example.com/a"), "example.com:443");
        assert_eq!(key("http://example.com/a"), "example.com:80");
        assert_eq!(key("https://example.com:8443/a"), "example.com:8443");
        assert_ne!(
            key("https://example.com/a"),
            key("https://other.example.com/a")
        );
    }

    #[test]
    fn waiter_ordering_puts_urgent_before_old() {
        let mut heap = BinaryHeap::from([
            QueuedWaiter {
                priority: Priority::Low,
                arrival: Reverse(0),
                id: 0,
            },
            QueuedWaiter {
                priority: Priority::Critical,
                arrival: Reverse(9),
                id: 9,
            },
            QueuedWaiter {
                priority: Priority::Low,
                arrival: Reverse(1),
                id: 1,
            },
        ]);
        assert_eq!(heap.pop().map(|w| w.id), Some(9));
        assert_eq!(heap.pop().map(|w| w.id), Some(0), "older wins a tie");
        assert_eq!(heap.pop().map(|w| w.id), Some(1));
    }
}
