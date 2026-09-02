//! BEASTUBE prioritized task scheduler with cancellation, retry, timeout and metrics.
//!
//! Every piece of asynchronous work in the application — a media segment fetch, a search, a
//! thumbnail decode, a database vacuum — is submitted here rather than to `tokio::spawn` directly.
//! Spawning freely is what lets a hundred speculative prefetches queue ahead of the segment the
//! user is currently watching, and no amount of care at the call sites prevents it: the call sites
//! cannot see each other. One admission point can.
//!
//! ## The admission policy, and why it is cumulative
//!
//! A naive scheduler gives each priority its own concurrency budget. That fails in the case that
//! matters: four `Normal` prefetches plus four `Background` maintenance tasks can still fill every
//! slot the runtime has, and the [`Priority::Critical`] segment fetch waits behind them.
//!
//! So the budget here is **cumulative**. For each priority `P` the scheduler enforces
//!
//! ```text
//! (number of in-flight tasks with priority <= P) <= cap(P)
//! ```
//!
//! with `cap` non-decreasing and `cap(Critical) == max_concurrent`. A task at priority `P`
//! contributes to the count for *every* level at or above it, so low-priority work can never
//! aggregate its way into the slots meant for high-priority work. Setting
//! `cap(High) = max_concurrent - reserved_critical` therefore reserves exactly
//! `reserved_critical` slots that nothing but [`Priority::Critical`] can ever occupy — this is the
//! reserved-slot mechanism, expressed as an invariant rather than as a separate pool that could
//! drift out of sync with the main one.
//!
//! The caps are also monotone in the useful direction: if a task at `P` is inadmissible, so is
//! every task below `P`, because the lower task must satisfy a strict superset of the same
//! constraints. That turns dispatch into a single descending scan that stops at the first blocked
//! level.
//!
//! ## Why a dispatcher task rather than semaphores
//!
//! The obvious implementation is one [`tokio::sync::Semaphore`] per level. It does not work:
//! tokio's semaphore wait queue is FIFO, so a thousand `Background` tasks queued on the global
//! semaphore are woken *before* a `Critical` task that arrives afterwards. Priority inversion is
//! built into the primitive.
//!
//! Instead the scheduler owns its own pending queues — one per priority — and a single dispatcher
//! task decides what runs next. Ordering is then exactly the ordering this crate defines, and the
//! "1000 background tasks cannot starve a critical one" property is structural rather than
//! probabilistic.
//!
//! ## Cancellation shape
//!
//! Cancellation is a tree of [`tokio_util::sync::CancellationToken`]s:
//!
//! ```text
//! scheduler root
//! ├── cancellable root ── group ── group ── per-task tokens   (Background/Low/Normal/High)
//! └── critical root ─────────────────────── per-task tokens   (Critical)
//! ```
//!
//! Navigating away cancels a [`TaskGroup`], which cancels every non-surviving task beneath it in
//! one call. Playback work is deliberately attached to a *sibling* root instead, because
//! [`Priority::survives_navigation`] says it must outlive the view that started it — leaving the
//! player running while the user browses is the whole point. A per-task token hangs off whichever
//! parent applies, so [`TaskHandle::cancel`] stops one task without disturbing its siblings.
//!
//! ## What this crate deliberately does not do
//!
//! * **It does not retain [`tokio::task::JoinHandle`]s.** Lifecycle is governed by cancellation
//!   tokens; a registry of join handles would need its own cleanup path and would tempt callers
//!   into `abort()`, which drops a future at an arbitrary await point.
//! * **It does not retry on its own judgement.** Only errors the caller classifies as retryable are
//!   retried, and never more than [`retry::MAX_ATTEMPT_CEILING`] times (§75).
//! * **It does not use randomness for anything but backoff jitter.** See [`retry::SystemJitter`].

// Lint policy is set workspace-wide in Cargo.toml. This crate forbids unsafe outright: it is pure
// async orchestration over safe primitives, so an unsafe block here would always be a mistake.
#![forbid(unsafe_code)]

pub mod error;
pub mod metrics;
pub mod retry;
pub mod scheduler;
pub mod task;

pub use error::{RejectReason, TaskError, TaskResult};
pub use metrics::{MetricsSnapshot, PriorityCounts, TaskMetrics};
pub use retry::{FixedJitter, JitterSource, RetryPolicy, SystemJitter};
pub use scheduler::{SchedulerConfig, TaskGroup, TaskScheduler};
pub use task::{Retryable, TaskHandle, TaskId, TaskSpec};

// Re-exported so callers configure a task's priority without also depending on `beastube-core`
// directly; it is the same type, not a copy.
pub use beastube_core::Priority;
