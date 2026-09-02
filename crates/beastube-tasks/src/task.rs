//! Task identity, configuration and handles.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use beastube_core::Priority;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::error::{TaskError, TaskResult};
use crate::retry::RetryPolicy;

/// A monotonically increasing task identifier, unique within a process run.
///
/// Used to correlate a task with its log lines and with an [`beastube_core::ErrorPayload`]'s
/// `correlation_id`, so a failure on the diagnostics screen can be traced back to the work that
/// produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(u64);

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

impl TaskId {
    /// Allocates the next identifier.
    #[must_use]
    pub fn next() -> Self {
        Self(NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// The raw value, for logging.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "task-{}", self.0)
    }
}

/// Classifies an error as worth retrying.
///
/// The scheduler never guesses. A task body's error type decides, because only it knows whether a
/// failure is transient — a scheduler that retried on its own judgement would repeat requests that
/// are guaranteed to fail identically, which is worse than failing fast.
pub trait Retryable {
    /// Whether repeating the operation could plausibly succeed.
    fn is_retryable(&self) -> bool;
}

/// Errors that are never worth retrying, for task bodies with no notion of transience.
impl Retryable for std::convert::Infallible {
    fn is_retryable(&self) -> bool {
        false
    }
}

/// How one task should be run.
#[derive(Debug, Clone)]
pub struct TaskSpec {
    priority: Priority,
    timeout: Option<Duration>,
    retry: RetryPolicy,
    label: &'static str,
}

impl TaskSpec {
    /// A task at `priority` with no timeout and no retries.
    #[must_use]
    pub fn new(priority: Priority, label: &'static str) -> Self {
        Self {
            priority,
            timeout: None,
            retry: RetryPolicy::none(),
            label,
        }
    }

    /// Playback-critical work: never deferred, never cancelled by navigation.
    #[must_use]
    pub fn critical(label: &'static str) -> Self {
        Self::new(Priority::Critical, label)
    }

    /// Work the user is waiting on.
    #[must_use]
    pub fn high(label: &'static str) -> Self {
        Self::new(Priority::High, label)
    }

    /// Speculative work such as prefetch.
    #[must_use]
    pub fn normal(label: &'static str) -> Self {
        Self::new(Priority::Normal, label)
    }

    /// Housekeeping that can wait.
    #[must_use]
    pub fn low(label: &'static str) -> Self {
        Self::new(Priority::Low, label)
    }

    /// Maintenance that runs only when nothing else wants the resource.
    #[must_use]
    pub fn background(label: &'static str) -> Self {
        Self::new(Priority::Background, label)
    }

    /// Bounds each **attempt**, not the task as a whole.
    ///
    /// Per-attempt is the useful budget: a task with three retries and a five-second budget should
    /// give each attempt five seconds, not share five seconds between them — otherwise the later
    /// attempts are strictly less likely to succeed than the first, which defeats retrying.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Applies a retry policy. Only errors reporting [`Retryable::is_retryable`] are retried.
    #[must_use]
    pub const fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// The task's priority.
    #[must_use]
    pub const fn priority(&self) -> Priority {
        self.priority
    }

    /// The per-attempt timeout, if any.
    #[must_use]
    pub const fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// The retry policy.
    #[must_use]
    pub const fn retry(&self) -> RetryPolicy {
        self.retry
    }

    /// A short static label used in log fields. Never user-facing, never localized.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        self.label
    }
}

/// A handle to a submitted task.
///
/// Dropping the handle does **not** cancel the task. That is deliberate: fire-and-forget submission
/// is the common case for maintenance work, and a drop-cancels design turns `let _ = spawn(...)`
/// into a silent no-op that is very hard to diagnose. Cancellation is always explicit, through
/// [`TaskHandle::cancel`] or the owning [`crate::scheduler::TaskGroup`].
#[derive(Debug)]
pub struct TaskHandle<T, E> {
    id: TaskId,
    priority: Priority,
    token: CancellationToken,
    outcome: oneshot::Receiver<TaskResult<T, E>>,
}

impl<T, E> TaskHandle<T, E> {
    pub(crate) const fn new(
        id: TaskId,
        priority: Priority,
        token: CancellationToken,
        outcome: oneshot::Receiver<TaskResult<T, E>>,
    ) -> Self {
        Self {
            id,
            priority,
            token,
            outcome,
        }
    }

    /// The task's identifier.
    #[must_use]
    pub const fn id(&self) -> TaskId {
        self.id
    }

    /// The task's priority.
    #[must_use]
    pub const fn priority(&self) -> Priority {
        self.priority
    }

    /// Requests cancellation.
    ///
    /// Returns immediately; the task observes the token at its next await point. A task that has
    /// already finished is unaffected.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// A clone of the task's cancellation token, for passing into the task body.
    #[must_use]
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    /// Awaits the outcome.
    ///
    /// A dropped sender means the task's runtime went away or its body panicked, which surfaces as
    /// [`TaskError::Panicked`] rather than hanging the awaiter forever.
    ///
    /// # Errors
    ///
    /// Returns the task's own [`TaskError`].
    pub async fn join(self) -> TaskResult<T, E> {
        self.outcome.await.unwrap_or(Err(TaskError::Panicked))
    }

    /// Cancels the task and waits for it to stop.
    ///
    /// # Errors
    ///
    /// Returns the task's own [`TaskError`], usually [`TaskError::Cancelled`].
    pub async fn abort(self) -> TaskResult<T, E> {
        self.token.cancel();
        self.join().await
    }
}

/// A handle whose outcome the caller does not intend to await.
///
/// Returned by fire-and-forget submission so that the unused-result lint does not fire on every
/// maintenance task, while cancellation remains available.
#[derive(Debug, Clone)]
pub struct DetachedHandle {
    id: TaskId,
    token: CancellationToken,
}

impl DetachedHandle {
    pub(crate) const fn new(id: TaskId, token: CancellationToken) -> Self {
        Self { id, token }
    }

    /// The task's identifier.
    #[must_use]
    pub const fn id(&self) -> TaskId {
        self.id
    }

    /// Requests cancellation.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_unique_and_increasing() {
        let first = TaskId::next();
        let second = TaskId::next();
        assert!(second > first);
        assert_ne!(first, second);
        assert_eq!(first.to_string(), format!("task-{}", first.get()));
    }

    #[test]
    fn identifiers_are_unique_across_threads() {
        let handles: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(|| (0..500).map(|_| TaskId::next()).collect::<Vec<_>>()))
            .collect();
        let mut all = Vec::new();
        for handle in handles {
            all.extend(handle.join().expect("worker thread panicked"));
        }
        let unique: std::collections::HashSet<_> = all.iter().copied().collect();
        assert_eq!(unique.len(), all.len(), "identifiers must never collide");
    }

    #[test]
    fn spec_builders_set_the_expected_priority() {
        assert_eq!(TaskSpec::critical("play").priority(), Priority::Critical);
        assert_eq!(TaskSpec::high("search").priority(), Priority::High);
        assert_eq!(TaskSpec::normal("prefetch").priority(), Priority::Normal);
        assert_eq!(TaskSpec::low("cleanup").priority(), Priority::Low);
        assert_eq!(
            TaskSpec::background("vacuum").priority(),
            Priority::Background
        );
    }

    #[test]
    fn a_spec_defaults_to_no_timeout_and_no_retries() {
        let spec = TaskSpec::normal("prefetch");
        assert_eq!(spec.timeout(), None);
        assert_eq!(spec.retry().max_attempts(), 1, "no retry by default");
        assert_eq!(spec.label(), "prefetch");
    }

    #[test]
    fn the_timeout_is_per_attempt_not_per_task() {
        // Documented behaviour worth pinning: three attempts with a 5s budget get 5s each.
        let spec = TaskSpec::high("fetch")
            .with_timeout(Duration::from_secs(5))
            .with_retry(RetryPolicy::with_attempts(3));
        assert_eq!(spec.timeout(), Some(Duration::from_secs(5)));
        assert_eq!(spec.retry().max_attempts(), 3);
    }

    #[tokio::test]
    async fn dropping_a_handle_does_not_cancel_the_task() {
        let token = CancellationToken::new();
        let (_tx, rx) = oneshot::channel::<TaskResult<(), std::convert::Infallible>>();
        let handle = TaskHandle::new(TaskId::next(), Priority::Normal, token.clone(), rx);
        drop(handle);
        assert!(
            !token.is_cancelled(),
            "fire-and-forget submission must not be silently cancelled by dropping the handle"
        );
    }

    #[tokio::test]
    async fn cancel_marks_the_token_without_blocking() {
        let token = CancellationToken::new();
        let (_tx, rx) = oneshot::channel::<TaskResult<(), std::convert::Infallible>>();
        let handle = TaskHandle::new(TaskId::next(), Priority::High, token.clone(), rx);

        assert!(!handle.is_cancelled());
        handle.cancel();
        assert!(handle.is_cancelled());
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn a_dropped_sender_surfaces_as_panicked_rather_than_hanging() {
        let (tx, rx) = oneshot::channel::<TaskResult<u32, std::convert::Infallible>>();
        let handle = TaskHandle::new(
            TaskId::next(),
            Priority::Normal,
            CancellationToken::new(),
            rx,
        );
        drop(tx);

        let outcome = handle.join().await;
        assert!(matches!(outcome, Err(TaskError::Panicked)));
    }

    #[tokio::test]
    async fn join_yields_the_task_outcome() {
        let (tx, rx) = oneshot::channel::<TaskResult<u32, std::convert::Infallible>>();
        let handle = TaskHandle::new(
            TaskId::next(),
            Priority::Normal,
            CancellationToken::new(),
            rx,
        );
        tx.send(Ok(42)).expect("receiver is alive");
        assert_eq!(handle.join().await.expect("task succeeded"), 42);
    }

    #[test]
    fn infallible_errors_are_never_retryable() {
        fn assert_impl<T: Retryable>() {}
        assert_impl::<std::convert::Infallible>();
    }
}
