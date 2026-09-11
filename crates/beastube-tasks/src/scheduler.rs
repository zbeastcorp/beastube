//! The scheduler: admission, dispatch, cancellation groups and shutdown.
//!
//! See the crate documentation for why admission is cumulative and why dispatch runs in a dedicated
//! task rather than through per-level semaphores.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use beastube_core::Priority;
use parking_lot::Mutex;
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

use crate::error::{RejectReason, TaskError, TaskResult};
use crate::metrics::{MetricsSnapshot, TaskMetrics};
use crate::retry::{JitterSource, SystemJitter};
use crate::task::{DetachedHandle, Retryable, TaskHandle, TaskId, TaskSpec};

/// Number of priority levels.
const LEVELS: usize = 5;

/// Index of `priority`, ascending from `Background` at 0.
const fn level(priority: Priority) -> usize {
    match priority {
        Priority::Background => 0,
        Priority::Low => 1,
        Priority::Normal => 2,
        Priority::High => 3,
        Priority::Critical => 4,
    }
}

const LEVEL_ORDER: [Priority; LEVELS] = [
    Priority::Background,
    Priority::Low,
    Priority::Normal,
    Priority::High,
    Priority::Critical,
];

/// How the scheduler is sized.
#[derive(Debug, Clone, Copy)]
pub struct SchedulerConfig {
    /// Ceiling on tasks executing at once, across every priority.
    pub max_concurrent: usize,
    /// Slots only [`Priority::Critical`] may occupy.
    pub reserved_critical: usize,
    /// Ceiling on tasks waiting at one priority. Exceeding it rejects rather than growing memory.
    pub queue_capacity: usize,
    /// How long shutdown waits for in-flight critical work before returning.
    pub shutdown_grace: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        // Eight concurrent tasks: enough to keep a media fetch, a metadata fetch and several
        // thumbnail decodes in flight without oversubscribing a laptop's cores or opening more
        // sockets than the per-host cap allows anyway.
        Self {
            max_concurrent: 8,
            // Two reserved slots covers the worst realistic simultaneous demand from playback: a
            // segment fetch plus a stream re-resolution triggered by an expiring URL.
            reserved_critical: 2,
            // Deep enough that a burst of thumbnail requests from a fast scroll is absorbed rather
            // than rejected, shallow enough that the backlog stays bounded.
            queue_capacity: 512,
            shutdown_grace: Duration::from_secs(5),
        }
    }
}

impl SchedulerConfig {
    /// The cumulative cap for `priority`: the most tasks that may run at that level or below.
    ///
    /// `cap(Critical)` is the global ceiling; every lower level is reduced by the reserved slots,
    /// which is what makes the reservation an invariant rather than a separate pool.
    #[must_use]
    fn cap(self, priority: Priority) -> usize {
        if priority == Priority::Critical {
            self.max_concurrent
        } else {
            self.max_concurrent
                .saturating_sub(self.reserved_critical)
                .max(1)
        }
    }

    /// Clamps nonsensical configuration into a workable shape.
    #[must_use]
    fn sanitized(mut self) -> Self {
        self.max_concurrent = self.max_concurrent.max(1);
        // Reserving every slot would starve everything that is not critical, which is a livelock
        // rather than a prioritization.
        self.reserved_critical = self
            .reserved_critical
            .min(self.max_concurrent.saturating_sub(1));
        self.queue_capacity = self.queue_capacity.max(1);
        self
    }
}

/// A unit of work the dispatcher can start.
///
/// The `bool` is "already cancelled". A task cancelled while still queued must still deliver
/// [`TaskError::Cancelled`] to whoever holds its handle — dropping the closure instead would drop
/// its result sender, and the awaiter would see [`TaskError::Panicked`] for what was an ordinary
/// cancellation.
type Job = Box<dyn FnOnce(bool) + Send>;

struct Pending {
    job: Job,
    token: CancellationToken,
    priority: Priority,
}

struct Shared {
    config: SchedulerConfig,
    queues: Mutex<[VecDeque<Pending>; LEVELS]>,
    metrics: Arc<TaskMetrics>,
    /// Woken whenever a task is queued or a slot frees.
    notify: Notify,
    shutting_down: AtomicBool,
    /// Cancels tasks that do not survive navigation, and everything at shutdown.
    cancellable_root: CancellationToken,
    /// Parent for playback work, which outlives view navigation.
    critical_root: CancellationToken,
    jitter: Arc<dyn JitterSource>,
}

impl Shared {
    /// Whether a task at `priority` may start right now.
    ///
    /// The cumulative rule: for every level at or above `priority`, the number of tasks running at
    /// that level or below must stay within its cap.
    fn admits(&self, priority: Priority) -> bool {
        LEVEL_ORDER[level(priority)..].iter().all(|&candidate| {
            let running = self.metrics.in_flight_at_or_below(candidate);
            running < self.config.cap(candidate) as u64
        })
    }

    /// Takes the highest-priority startable job, if any.
    ///
    /// Jobs found already cancelled are moved into `discarded` rather than dropped, so the caller
    /// can notify their awaiters after releasing the queue lock.
    fn take_next(&self, discarded: &mut Vec<Job>) -> Option<Pending> {
        let mut queues = self.queues.lock();
        // Descending scan: the first admissible level wins. Because the caps are monotone, a level
        // that is blocked implies every level below it is blocked too, so this stops early.
        for priority in LEVEL_ORDER.iter().rev().copied() {
            if queues[level(priority)].is_empty() {
                continue;
            }
            if !self.admits(priority) {
                // Lower levels satisfy a superset of these constraints, so none of them can run.
                return None;
            }
            while let Some(pending) = queues[level(priority)].pop_front() {
                if pending.token.is_cancelled() {
                    // Cancelled while queued: hand it back so its awaiter is told, then keep
                    // scanning this level.
                    self.metrics.record_dequeued(priority);
                    self.metrics.record_cancelled_while_queued();
                    discarded.push(pending.job);
                    continue;
                }
                return Some(pending);
            }
        }
        None
    }
}

/// A prioritized, cancellable task scheduler.
///
/// Cloning shares one scheduler; it is not a copy.
#[derive(Clone)]
pub struct TaskScheduler {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for TaskScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskScheduler")
            .field("config", &self.shared.config)
            .field("metrics", &self.shared.metrics.snapshot())
            .finish()
    }
}

impl TaskScheduler {
    /// Starts a scheduler with the default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(SchedulerConfig::default())
    }

    /// Starts a scheduler with an explicit configuration.
    #[must_use]
    pub fn with_config(config: SchedulerConfig) -> Self {
        Self::with_config_and_jitter(config, Arc::new(SystemJitter))
    }

    /// Starts a scheduler with an explicit jitter source, for deterministic tests.
    #[must_use]
    pub fn with_config_and_jitter(config: SchedulerConfig, jitter: Arc<dyn JitterSource>) -> Self {
        let shared = Arc::new(Shared {
            config: config.sanitized(),
            queues: Mutex::new(std::array::from_fn(|_| VecDeque::new())),
            metrics: Arc::new(TaskMetrics::new()),
            notify: Notify::new(),
            shutting_down: AtomicBool::new(false),
            cancellable_root: CancellationToken::new(),
            critical_root: CancellationToken::new(),
            jitter,
        });
        spawn_dispatcher(Arc::clone(&shared));
        Self { shared }
    }

    /// The scheduler's configuration, after sanitization.
    #[must_use]
    pub fn config(&self) -> SchedulerConfig {
        self.shared.config
    }

    /// A snapshot of the counters.
    #[must_use]
    pub fn metrics(&self) -> MetricsSnapshot {
        self.shared.metrics.snapshot()
    }

    /// Whether the scheduler has begun shutting down.
    #[must_use]
    pub fn is_shutting_down(&self) -> bool {
        self.shared.shutting_down.load(Ordering::Acquire)
    }

    /// Creates a cancellation group.
    ///
    /// Cancelling the group cancels every task submitted through it whose priority does not survive
    /// navigation. Playback work attaches to the critical root instead, so it is untouched.
    #[must_use]
    pub fn group(&self) -> TaskGroup {
        TaskGroup {
            scheduler: self.clone(),
            token: self.shared.cancellable_root.child_token(),
        }
    }

    /// Submits a task and returns a handle to its outcome.
    ///
    /// # Errors
    ///
    /// Returns [`RejectReason`] if the scheduler is shutting down or the priority's queue is full.
    /// A rejection means the task never ran, so the caller can act immediately rather than awaiting
    /// a handle that will never resolve.
    pub fn spawn<F, Fut, T, E>(
        &self,
        spec: TaskSpec,
        body: F,
    ) -> Result<TaskHandle<T, E>, RejectReason>
    where
        F: Fn(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>> + Send,
        T: Send + 'static,
        E: Retryable + Send + 'static,
    {
        let parent = self.parent_for(spec.priority());
        self.spawn_in(spec, parent, body)
    }

    /// Submits a task without retaining a handle to its outcome.
    ///
    /// # Errors
    ///
    /// Returns [`RejectReason`] as [`TaskScheduler::spawn`] does.
    pub fn spawn_detached<F, Fut, T, E>(
        &self,
        spec: TaskSpec,
        body: F,
    ) -> Result<DetachedHandle, RejectReason>
    where
        F: Fn(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>> + Send,
        T: Send + 'static,
        E: Retryable + Send + 'static,
    {
        let handle = self.spawn(spec, body)?;
        Ok(DetachedHandle::new(handle.id(), handle.token()))
    }

    /// The cancellation parent a task at `priority` should hang from.
    fn parent_for(&self, priority: Priority) -> CancellationToken {
        if priority.survives_navigation() {
            self.shared.critical_root.clone()
        } else {
            self.shared.cancellable_root.clone()
        }
    }

    // `parent` is cloned into the child token rather than consumed, but taking it by value keeps every caller free to hand over a temporary.
    #[allow(clippy::needless_pass_by_value)]
    fn spawn_in<F, Fut, T, E>(
        &self,
        spec: TaskSpec,
        parent: CancellationToken,
        body: F,
    ) -> Result<TaskHandle<T, E>, RejectReason>
    where
        F: Fn(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>> + Send,
        T: Send + 'static,
        E: Retryable + Send + 'static,
    {
        if self.is_shutting_down() {
            self.shared.metrics.record_rejected();
            return Err(RejectReason::ShuttingDown);
        }

        let priority = spec.priority();
        let label = spec.label();
        let token = parent.child_token();
        let id = TaskId::next();
        let (tx, rx) = oneshot::channel();

        let shared = Arc::clone(&self.shared);
        let task_token = token.clone();
        // The closure is built here, where `T` and `E` are still known, and boxed as `FnOnce()`
        // only afterwards. Monomorphizing before erasure is what lets one untyped job queue carry
        // tasks of every result type without boxing the values themselves.
        let job: Job = Box::new(move |already_cancelled: bool| {
            if already_cancelled {
                // Never started, so no in-flight gauge to decrement; the queue counters were
                // already adjusted by the dispatcher.
                let _ = tx.send(Err(TaskError::Cancelled));
                return;
            }
            tokio::spawn(async move {
                let outcome = run_attempts(&shared, &spec, &task_token, body).await;
                match &outcome {
                    Ok(_) => shared.metrics.record_completed(priority),
                    Err(TaskError::Cancelled) => shared.metrics.record_cancelled(priority),
                    Err(TaskError::TimedOut { .. }) => shared.metrics.record_timed_out(priority),
                    Err(_) => shared.metrics.record_failed(priority),
                }
                // A dropped receiver is the fire-and-forget case, not an error.
                let _ = tx.send(outcome);
                // A slot just freed; wake the dispatcher rather than letting it wait for the next
                // submission to notice.
                shared.notify.notify_one();
            });
        });

        {
            let mut queues = self.shared.queues.lock();
            let queue = &mut queues[level(priority)];
            if queue.len() >= self.shared.config.queue_capacity {
                drop(queues);
                tracing::debug!(task = %id, label, %priority, "rejected: queue full");
                self.shared.metrics.record_rejected();
                return Err(RejectReason::QueueFull {
                    priority,
                    capacity: self.shared.config.queue_capacity,
                });
            }
            queue.push_back(Pending {
                job,
                token: token.clone(),
                priority,
            });
        }

        self.shared.metrics.record_queued(priority);
        self.shared.notify.notify_one();
        Ok(TaskHandle::new(id, priority, token, rx))
    }

    /// Cancels every task that does not survive navigation, leaving playback untouched.
    ///
    /// Called when the user navigates away from a view.
    pub fn cancel_navigable(&self) {
        self.shared.cancellable_root.cancel();
    }

    /// Stops admitting work, cancels what can be cancelled, and waits briefly for critical work.
    ///
    /// Returns once in-flight critical tasks finish or the grace period elapses, whichever comes
    /// first. It never hangs: an unresponsive task costs the grace period, not the shutdown.
    pub async fn shutdown(&self) {
        self.shared.shutting_down.store(true, Ordering::Release);
        // Non-critical work stops immediately; the queue is drained so nothing new starts.
        self.shared.cancellable_root.cancel();
        {
            let mut queues = self.shared.queues.lock();
            for priority in LEVEL_ORDER {
                if priority.survives_navigation() {
                    continue;
                }
                for pending in queues[level(priority)].drain(..) {
                    self.shared.metrics.record_dequeued(priority);
                    self.shared.metrics.record_cancelled_while_queued();
                    // Tell the awaiter it was cancelled rather than leaving it on a dropped sender.
                    (pending.job)(true);
                }
            }
        }

        let deadline = tokio::time::Instant::now() + self.shared.config.shutdown_grace;
        while self.shared.metrics.in_flight_at(Priority::Critical) > 0 {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Whatever is still running has had its chance.
        self.shared.critical_root.cancel();
        self.shared.notify.notify_waiters();
    }
}

impl Default for TaskScheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs a task body, applying the per-attempt timeout and the retry policy.
///
/// Cancellation is checked with `biased` select arms so that a token cancelled at the same moment
/// the body completes resolves as a cancellation rather than racing — a task the caller has
/// abandoned must not deliver a value that nothing is waiting for.
///
/// A timeout is retried like any other transient failure when the policy allows it: a request that
/// ran out of time is exactly the kind of failure a second attempt can fix. When retries are
/// exhausted the timeout, not the last body error, is what surfaces.
async fn run_attempts<F, Fut, T, E>(
    shared: &Shared,
    spec: &TaskSpec,
    token: &CancellationToken,
    body: F,
) -> TaskResult<T, E>
where
    F: Fn(CancellationToken) -> Fut + Send,
    Fut: Future<Output = Result<T, E>> + Send,
    E: Retryable,
{
    let policy = spec.retry();
    let mut attempts: u32 = 0;

    loop {
        if token.is_cancelled() {
            return Err(TaskError::Cancelled);
        }
        attempts = attempts.saturating_add(1);

        let attempt = body(token.clone());
        let attempt_outcome: Result<Result<T, E>, ()> = match spec.timeout() {
            Some(budget) => {
                tokio::select! {
                    biased;
                    () = token.cancelled() => return Err(TaskError::Cancelled),
                    settled = tokio::time::timeout(budget, attempt) => settled.map_err(|_| ()),
                }
            }
            None => {
                tokio::select! {
                    biased;
                    () = token.cancelled() => return Err(TaskError::Cancelled),
                    settled = attempt => Ok(settled),
                }
            }
        };

        // Decide whether to try again, and with which terminal error if not.
        let terminal: TaskError<E> = match attempt_outcome {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(error)) => {
                if error.is_retryable() {
                    TaskError::Failed {
                        attempts,
                        source: error,
                    }
                } else {
                    // Not worth repeating: fail now rather than burning the policy's attempts on a
                    // request guaranteed to fail identically.
                    return Err(TaskError::Failed {
                        attempts,
                        source: error,
                    });
                }
            }
            Err(()) => TaskError::TimedOut {
                timeout_ms: spec.timeout().map_or(0, |budget| {
                    u64::try_from(budget.as_millis()).unwrap_or(u64::MAX)
                }),
            },
        };

        let Some(delay) = policy.delay_after(attempts, shared.jitter.as_ref()) else {
            return Err(terminal);
        };

        shared.metrics.record_retry();
        tokio::select! {
            biased;
            () = token.cancelled() => return Err(TaskError::Cancelled),
            () = tokio::time::sleep(delay) => {}
        }
    }
}

/// The dispatcher: the single decision point for what runs next.
fn spawn_dispatcher(shared: Arc<Shared>) {
    tokio::spawn(async move {
        loop {
            // Start everything currently admissible before waiting again, so a burst of
            // submissions does not require one wake-up per task.
            let mut discarded: Vec<Job> = Vec::new();
            while let Some(pending) = shared.take_next(&mut discarded) {
                shared.metrics.record_started(pending.priority);
                (pending.job)(false);
            }
            // Notify abandoned awaiters outside the queue lock.
            for job in discarded.drain(..) {
                job(true);
            }

            if shared.shutting_down.load(Ordering::Acquire) {
                let empty = {
                    let queues = shared.queues.lock();
                    queues.iter().all(VecDeque::is_empty)
                };
                if empty {
                    break;
                }
            }

            shared.notify.notified().await;
        }
    });
}

/// A cancellation scope for a set of related tasks.
///
/// Dropping a group does not cancel it — cancellation is always explicit, so a group held in a
/// short-lived local does not silently kill the work it started.
#[derive(Debug, Clone)]
pub struct TaskGroup {
    scheduler: TaskScheduler,
    token: CancellationToken,
}

impl TaskGroup {
    /// Submits a task into this group.
    ///
    /// # Errors
    ///
    /// Returns [`RejectReason`] as [`TaskScheduler::spawn`] does.
    pub fn spawn<F, Fut, T, E>(
        &self,
        spec: TaskSpec,
        body: F,
    ) -> Result<TaskHandle<T, E>, RejectReason>
    where
        F: Fn(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>> + Send,
        T: Send + 'static,
        E: Retryable + Send + 'static,
    {
        // Critical work ignores the group and hangs off the critical root, so cancelling the group
        // cannot stop playback (Priority::survives_navigation).
        let parent = if spec.priority().survives_navigation() {
            self.scheduler.shared.critical_root.clone()
        } else {
            self.token.clone()
        };
        self.scheduler.spawn_in(spec, parent, body)
    }

    /// Cancels every non-surviving task in this group.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Whether this group has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// A child token, for work that should stop with this group but is not submitted through it.
    #[must_use]
    pub fn child_token(&self) -> CancellationToken {
        self.token.child_token()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use crate::retry::{FixedJitter, RetryPolicy};

    use super::*;

    /// An error whose retryability the test controls.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct TestError {
        retryable: bool,
    }

    impl Retryable for TestError {
        fn is_retryable(&self) -> bool {
            self.retryable
        }
    }

    fn scheduler(max_concurrent: usize, reserved_critical: usize) -> TaskScheduler {
        TaskScheduler::with_config_and_jitter(
            SchedulerConfig {
                max_concurrent,
                reserved_critical,
                queue_capacity: 4096,
                shutdown_grace: Duration::from_millis(200),
            },
            // Deterministic: a retry waits exactly the computed backoff.
            Arc::new(FixedJitter::full()),
        )
    }

    /// Waits until `predicate` holds, or fails the test.
    ///
    /// Under a paused clock the sleep advances time instantly, so this costs no wall time.
    async fn until(mut predicate: impl FnMut() -> bool) {
        for _ in 0..10_000 {
            if predicate() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("condition was never reached");
    }

    #[tokio::test(start_paused = true)]
    async fn a_task_runs_and_reports_its_value() {
        let scheduler = scheduler(4, 1);
        let handle = scheduler
            .spawn(TaskSpec::normal("unit"), |_token| async {
                Ok::<_, TestError>(7)
            })
            .expect("admitted");

        assert_eq!(handle.join().await.expect("succeeded"), 7);
        assert_eq!(scheduler.metrics().completed, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_thousand_background_tasks_cannot_starve_a_critical_one() {
        // The property the whole dispatcher exists for.
        let scheduler = scheduler(4, 2);
        let released = Arc::new(CancellationToken::new());

        for _ in 0..1000 {
            let gate = Arc::clone(&released);
            scheduler
                .spawn_detached(TaskSpec::background("flood"), move |_token| {
                    let gate = Arc::clone(&gate);
                    async move {
                        gate.cancelled().await;
                        Ok::<_, TestError>(())
                    }
                })
                .expect("admitted");
        }

        // Let the dispatcher fill every slot it is allowed to.
        until(|| scheduler.metrics().in_flight.background > 0).await;

        let critical = scheduler
            .spawn(TaskSpec::critical("segment"), |_token| async {
                Ok::<_, TestError>("played")
            })
            .expect("admitted");

        // Must complete while a thousand background tasks are still parked.
        assert_eq!(critical.join().await.expect("succeeded"), "played");
        assert!(
            scheduler.metrics().queued.background > 0,
            "the flood should still be queued, proving the critical task jumped it"
        );

        released.cancel();
        scheduler.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn reserved_slots_are_never_occupied_by_lower_priorities() {
        let scheduler = scheduler(4, 2);
        let gate = Arc::new(CancellationToken::new());

        for _ in 0..20 {
            let gate = Arc::clone(&gate);
            scheduler
                .spawn_detached(TaskSpec::normal("filler"), move |_token| {
                    let gate = Arc::clone(&gate);
                    async move {
                        gate.cancelled().await;
                        Ok::<_, TestError>(())
                    }
                })
                .expect("admitted");
        }

        // Wait for the dispatcher to reach its steady state: submission bumps the queue gauge
        // synchronously, so waiting on `queued > 0` would return before anything had started.
        until(|| scheduler.metrics().in_flight.normal == 2).await;
        assert_eq!(
            scheduler.metrics().in_flight.normal,
            2,
            "max_concurrent 4 minus 2 reserved leaves exactly 2 slots for non-critical work"
        );
        assert!(
            scheduler.metrics().queued.normal > 0,
            "the remainder must still be waiting, not oversubscribing the reservation"
        );

        gate.cancel();
        scheduler.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_a_handle_stops_its_task_and_no_other() {
        let scheduler = scheduler(4, 1);
        let survivor_ran = Arc::new(AtomicBool::new(false));

        let victim = scheduler
            .spawn(TaskSpec::normal("victim"), move |token| async move {
                token.cancelled().await;
                Ok::<_, TestError>(())
            })
            .expect("admitted");

        let flag = Arc::clone(&survivor_ran);
        let survivor = scheduler
            .spawn(TaskSpec::normal("survivor"), move |_token| {
                let flag = Arc::clone(&flag);
                async move {
                    flag.store(true, Ordering::SeqCst);
                    Ok::<_, TestError>(())
                }
            })
            .expect("admitted");

        victim.cancel();
        assert!(matches!(victim.join().await, Err(TaskError::Cancelled)));

        assert!(
            survivor.join().await.is_ok(),
            "a sibling must be unaffected"
        );
        assert!(survivor_ran.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_a_group_spares_playback_work() {
        // Navigating away must not stop the video.
        let scheduler = scheduler(6, 2);
        let group = scheduler.group();

        let normal = group
            .spawn(TaskSpec::normal("view-data"), |token| async move {
                token.cancelled().await;
                Ok::<_, TestError>("cancelled")
            })
            .expect("admitted");

        let critical = group
            .spawn(TaskSpec::critical("playback"), |_token| async {
                Ok::<_, TestError>("kept playing")
            })
            .expect("admitted");

        group.cancel();

        assert!(matches!(normal.join().await, Err(TaskError::Cancelled)));
        assert_eq!(
            critical.join().await.expect("critical survived"),
            "kept playing",
            "playback attaches to the critical root, not to the group"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_task_cancelled_while_queued_never_starts() {
        let scheduler = scheduler(1, 0);
        let gate = Arc::new(CancellationToken::new());

        let gate_for_blocker = Arc::clone(&gate);
        scheduler
            .spawn_detached(TaskSpec::normal("blocker"), move |_token| {
                let gate = Arc::clone(&gate_for_blocker);
                async move {
                    gate.cancelled().await;
                    Ok::<_, TestError>(())
                }
            })
            .expect("admitted");

        until(|| scheduler.metrics().in_flight.normal == 1).await;

        let started = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&started);
        let queued = scheduler
            .spawn(TaskSpec::normal("queued"), move |_token| {
                let flag = Arc::clone(&flag);
                async move {
                    flag.store(true, Ordering::SeqCst);
                    Ok::<_, TestError>(())
                }
            })
            .expect("admitted");

        queued.cancel();
        gate.cancel();

        until(|| scheduler.metrics().total_in_flight() == 0).await;
        assert!(
            !started.load(Ordering::SeqCst),
            "a task cancelled while queued must never run its body"
        );
        assert!(scheduler.metrics().cancelled >= 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_attempt_that_overruns_its_budget_times_out() {
        let scheduler = scheduler(4, 1);
        let handle = scheduler
            .spawn(
                TaskSpec::high("slow").with_timeout(Duration::from_millis(50)),
                |_token| async {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    Ok::<_, TestError>(())
                },
            )
            .expect("admitted");

        assert!(matches!(
            handle.join().await,
            Err(TaskError::TimedOut { timeout_ms: 50 })
        ));
        assert_eq!(scheduler.metrics().timed_out, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_retryable_error_is_retried_up_to_the_policy_and_then_reported() {
        let scheduler = scheduler(4, 1);
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);

        let handle = scheduler
            .spawn(
                TaskSpec::high("flaky").with_retry(RetryPolicy::with_attempts(3)),
                move |_token| {
                    let counter = Arc::clone(&counter);
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Err::<(), _>(TestError { retryable: true })
                    }
                },
            )
            .expect("admitted");

        match handle.join().await {
            Err(TaskError::Failed { attempts: made, .. }) => assert_eq!(made, 3),
            other => panic!("expected a failure after three attempts, got {other:?}"),
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 3, "exactly three attempts");
        assert_eq!(
            scheduler.metrics().retried,
            2,
            "two retries follow the first attempt"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_non_retryable_error_fails_on_the_first_attempt() {
        let scheduler = scheduler(4, 1);
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);

        let handle = scheduler
            .spawn(
                TaskSpec::high("fatal").with_retry(RetryPolicy::with_attempts(5)),
                move |_token| {
                    let counter = Arc::clone(&counter);
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Err::<(), _>(TestError { retryable: false })
                    }
                },
            )
            .expect("admitted");

        assert!(handle.join().await.is_err());
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "a request guaranteed to fail identically must not be repeated"
        );
        assert_eq!(scheduler.metrics().retried, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_retry_succeeds_once_the_transient_failure_clears() {
        let scheduler = scheduler(4, 1);
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);

        let handle = scheduler
            .spawn(
                TaskSpec::high("recovers").with_retry(RetryPolicy::with_attempts(4)),
                move |_token| {
                    let counter = Arc::clone(&counter);
                    async move {
                        if counter.fetch_add(1, Ordering::SeqCst) < 2 {
                            Err(TestError { retryable: true })
                        } else {
                            Ok("recovered")
                        }
                    }
                },
            )
            .expect("admitted");

        assert_eq!(
            handle.join().await.expect("eventually succeeded"),
            "recovered"
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert_eq!(scheduler.metrics().completed, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_interrupts_a_backoff_wait() {
        // A cancelled task must not sit out a thirty-second backoff before noticing.
        let scheduler = scheduler(4, 1);
        let handle = scheduler
            .spawn(
                TaskSpec::high("backing-off").with_retry(
                    RetryPolicy::with_attempts(5).initial_backoff(Duration::from_secs(30)),
                ),
                |_token| async { Err::<(), _>(TestError { retryable: true }) },
            )
            .expect("admitted");

        until(|| scheduler.metrics().retried >= 1).await;
        handle.cancel();
        assert!(matches!(handle.join().await, Err(TaskError::Cancelled)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_queue_rejects_rather_than_growing_without_bound() {
        let scheduler = TaskScheduler::with_config_and_jitter(
            SchedulerConfig {
                max_concurrent: 1,
                reserved_critical: 0,
                queue_capacity: 2,
                shutdown_grace: Duration::from_millis(50),
            },
            Arc::new(FixedJitter::none()),
        );

        let gate = Arc::new(CancellationToken::new());
        let mut rejections = 0;
        for _ in 0..10 {
            let gate = Arc::clone(&gate);
            let result = scheduler.spawn_detached(TaskSpec::low("flood"), move |_token| {
                let gate = Arc::clone(&gate);
                async move {
                    gate.cancelled().await;
                    Ok::<_, TestError>(())
                }
            });
            if let Err(RejectReason::QueueFull { priority, capacity }) = result {
                assert_eq!(priority, Priority::Low);
                assert_eq!(capacity, 2);
                rejections += 1;
            }
        }

        assert!(
            rejections > 0,
            "a bounded queue must eventually refuse work"
        );
        assert_eq!(scheduler.metrics().rejected, rejections);
        gate.cancel();
        scheduler.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_queue_at_one_priority_does_not_block_another() {
        let scheduler = TaskScheduler::with_config_and_jitter(
            SchedulerConfig {
                max_concurrent: 2,
                reserved_critical: 1,
                queue_capacity: 2,
                shutdown_grace: Duration::from_millis(50),
            },
            Arc::new(FixedJitter::none()),
        );

        let gate = Arc::new(CancellationToken::new());
        for _ in 0..10 {
            let gate = Arc::clone(&gate);
            let _ = scheduler.spawn_detached(TaskSpec::background("flood"), move |_token| {
                let gate = Arc::clone(&gate);
                async move {
                    gate.cancelled().await;
                    Ok::<_, TestError>(())
                }
            });
        }

        // The background queue is full; critical work must still be admitted.
        let critical = scheduler
            .spawn(TaskSpec::critical("segment"), |_token| async {
                Ok::<_, TestError>(())
            })
            .expect("critical must be admitted even when another queue is full");
        assert!(critical.join().await.is_ok());

        gate.cancel();
        scheduler.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_refuses_new_work_and_returns_promptly() {
        let scheduler = scheduler(4, 1);
        scheduler.shutdown().await;

        assert!(scheduler.is_shutting_down());
        let rejected = scheduler.spawn(TaskSpec::normal("late"), |_token| async {
            Ok::<_, TestError>(())
        });
        assert!(matches!(
            rejected.map(|_| ()),
            Err(RejectReason::ShuttingDown)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_does_not_hang_on_an_unresponsive_task() {
        // The grace period is a bound, not a hope: a task ignoring its token costs the grace
        // period and nothing more.
        let scheduler = scheduler(4, 2);
        scheduler
            .spawn_detached(TaskSpec::critical("stubborn"), |_token| async {
                // Deliberately ignores cancellation.
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok::<_, TestError>(())
            })
            .expect("admitted");

        until(|| scheduler.metrics().in_flight.critical == 1).await;
        scheduler.shutdown().await;
        assert!(scheduler.is_shutting_down());
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_drains_queued_non_critical_work() {
        let scheduler = scheduler(1, 0);
        let gate = Arc::new(CancellationToken::new());

        let gate_for_blocker = Arc::clone(&gate);
        scheduler
            .spawn_detached(TaskSpec::normal("blocker"), move |_token| {
                let gate = Arc::clone(&gate_for_blocker);
                async move {
                    gate.cancelled().await;
                    Ok::<_, TestError>(())
                }
            })
            .expect("admitted");
        until(|| scheduler.metrics().in_flight.normal == 1).await;

        for _ in 0..5 {
            let _ = scheduler.spawn_detached(TaskSpec::low("queued"), |_token| async {
                Ok::<_, TestError>(())
            });
        }
        until(|| scheduler.metrics().queued.low == 5).await;

        gate.cancel();
        scheduler.shutdown().await;
        assert_eq!(
            scheduler.metrics().total_queued(),
            0,
            "queued work must be drained, not left dangling"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_task_releases_its_resources() {
        // Guards against a leak where a cancelled task's captured state is never dropped.
        struct Guard(Arc<AtomicUsize>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let scheduler = scheduler(4, 1);
        let drops = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&drops);

        let handle = scheduler
            .spawn(TaskSpec::normal("guarded"), move |token| {
                let guard = Guard(Arc::clone(&counter));
                async move {
                    let _guard = guard;
                    token.cancelled().await;
                    Ok::<_, TestError>(())
                }
            })
            .expect("admitted");

        // The guard is built by the body, so wait until the task is genuinely running; cancelling
        // it while still queued would exercise a different path entirely.
        until(|| scheduler.metrics().in_flight.normal == 1).await;
        handle.cancel();
        assert!(matches!(handle.join().await, Err(TaskError::Cancelled)));
        until(|| drops.load(Ordering::SeqCst) >= 1).await;
        assert!(
            drops.load(Ordering::SeqCst) >= 1,
            "captured state must be dropped"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_task_cancelled_while_queued_still_reports_cancellation() {
        // Regression: the queued job used to be dropped outright, taking its result sender with
        // it, so the awaiter saw `Panicked` for what was an ordinary cancellation.
        let scheduler = scheduler(1, 0);
        let gate = Arc::new(CancellationToken::new());

        let gate_for_blocker = Arc::clone(&gate);
        scheduler
            .spawn_detached(TaskSpec::normal("blocker"), move |_token| {
                let gate = Arc::clone(&gate_for_blocker);
                async move {
                    gate.cancelled().await;
                    Ok::<_, TestError>(())
                }
            })
            .expect("admitted");
        until(|| scheduler.metrics().in_flight.normal == 1).await;

        let queued = scheduler
            .spawn(TaskSpec::normal("queued"), |_token| async {
                Ok::<_, TestError>(())
            })
            .expect("admitted");
        queued.cancel();
        gate.cancel();

        assert!(
            matches!(queued.join().await, Err(TaskError::Cancelled)),
            "a task cancelled before it started must report Cancelled, not Panicked"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn metrics_balance_after_a_mixed_workload() {
        let scheduler = scheduler(6, 2);
        let mut handles = Vec::new();

        for index in 0..12 {
            let spec = match index % 4 {
                0 => TaskSpec::critical("c"),
                1 => TaskSpec::high("h"),
                2 => TaskSpec::normal("n"),
                _ => TaskSpec::low("l"),
            };
            let fail = index % 3 == 0;
            handles.push(
                scheduler
                    .spawn(spec, move |_token| async move {
                        if fail {
                            Err(TestError { retryable: false })
                        } else {
                            Ok(index)
                        }
                    })
                    .expect("admitted"),
            );
        }

        for handle in handles {
            let _ = handle.join().await;
        }
        until(|| scheduler.metrics().total_in_flight() == 0).await;

        let snapshot = scheduler.metrics();
        assert_eq!(snapshot.spawned, 12);
        assert_eq!(
            snapshot.settled(),
            12,
            "every task reached a terminal state exactly once"
        );
        assert_eq!(snapshot.failed, 4);
        assert_eq!(snapshot.completed, 8);
        assert_eq!(snapshot.total_queued(), 0);
    }

    #[test]
    fn configuration_is_clamped_into_a_workable_shape() {
        // Reserving every slot would starve everything non-critical: a livelock, not a priority.
        let config = SchedulerConfig {
            max_concurrent: 0,
            reserved_critical: 99,
            queue_capacity: 0,
            shutdown_grace: Duration::ZERO,
        }
        .sanitized();

        assert_eq!(config.max_concurrent, 1);
        assert_eq!(config.reserved_critical, 0);
        assert_eq!(config.queue_capacity, 1);
        assert!(config.cap(Priority::Critical) >= config.cap(Priority::Background));
    }

    #[test]
    fn caps_are_monotone_so_the_dispatch_scan_can_stop_early() {
        // The descending scan returns as soon as a level is blocked, which is only sound if a
        // blocked level implies every lower level is blocked too.
        let config = SchedulerConfig {
            max_concurrent: 8,
            reserved_critical: 2,
            queue_capacity: 16,
            shutdown_grace: Duration::ZERO,
        }
        .sanitized();

        for pair in LEVEL_ORDER.windows(2) {
            let (lower, higher) = (pair[0], pair[1]);
            assert!(
                config.cap(lower) <= config.cap(higher),
                "cap({lower}) must not exceed cap({higher})"
            );
        }
        assert_eq!(config.cap(Priority::Critical), 8);
        assert_eq!(config.cap(Priority::High), 6);
    }
}
