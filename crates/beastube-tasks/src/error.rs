//! Scheduling failures and their recovery strategies.
//!
//! [`TaskError`] is generic over the task's own error type rather than swallowing it into a
//! `Box<dyn Error>`. That is what lets [`TaskError::Failed`] *delegate* its
//! [`DomainError`] implementation to the wrapped error: a media fetch that exhausts its retries
//! still reports `network.timeout`, not `tasks.something`, so the UI surfaces the failure the user
//! actually experienced instead of the plumbing that carried it.
//!
//! ## Classifying the scheduler's own failures
//!
//! Cancellation, timeout, rejection and panic are not failures *of* a subsystem — they are
//! statements about the application's own runtime posture, so they are reported as
//! [`ErrorKind::Configuration`]. `beastube-core` deliberately has no `Task` kind: adding one would
//! invite every subsystem to report "a task failed" instead of naming the work that failed, which
//! is precisely the information the error contract exists to preserve.
//!
//! [`TaskError::Cancelled`] is a normal outcome, not a fault. Callers should discard it with
//! [`TaskError::is_cancelled`] before an error ever reaches the UI; a user who navigates away
//! should not be shown an error about the work they navigated away from.

use std::collections::BTreeMap;

use beastube_core::Priority;
use beastube_core::error::{DomainError, ErrorKind, Recovery};
use serde::{Deserialize, Serialize};

/// Why the scheduler refused to admit a task.
///
/// Refusal happens at submission and is reported synchronously, so a caller learns that its work
/// was never started instead of awaiting a handle that will never resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum RejectReason {
    /// The scheduler is shutting down and no longer admits work.
    #[error("the scheduler is shutting down")]
    ShuttingDown,

    /// The pending queue for this priority is full.
    ///
    /// Queues are bounded *per priority*, so a flood of [`Priority::Background`] work exhausts only
    /// its own queue. It can neither displace queued playback work nor grow memory without bound.
    #[error("the {priority} queue is full at {capacity} entries")]
    QueueFull {
        /// Priority whose queue was full.
        priority: Priority,
        /// Capacity of that queue.
        capacity: usize,
    },
}

impl RejectReason {
    /// Stable code fragment for this reason.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ShuttingDown => "rejected_shutting_down",
            Self::QueueFull { .. } => "rejected_queue_full",
        }
    }

    /// Whether resubmitting the same work later could succeed.
    ///
    /// Saturation clears as in-flight work drains; shutdown does not.
    #[must_use]
    pub const fn is_transient(self) -> bool {
        matches!(self, Self::QueueFull { .. })
    }
}

/// Why a scheduled task did not produce a value.
///
/// `E` is the task body's own error type. It is preserved rather than erased so that callers can
/// match on it and so that [`DomainError`] can be delegated to it.
#[derive(Debug, thiserror::Error)]
pub enum TaskError<E> {
    /// The task body failed, and either the error was not retryable or the attempt ceiling was
    /// reached.
    #[error("task failed after {attempts} attempt(s)")]
    Failed {
        /// Attempts actually made, always at least 1 and never more than the policy's ceiling.
        attempts: u32,
        /// The final error from the task body.
        #[source]
        source: E,
    },

    /// The task's token was cancelled before it produced a value.
    ///
    /// This is the expected outcome of navigating away or shutting down, not a fault.
    #[error("task was cancelled")]
    Cancelled,

    /// An attempt exceeded its time budget.
    ///
    /// The budget is per *attempt* — see [`crate::task::TaskSpec::with_timeout`].
    #[error("task attempt exceeded its {timeout_ms} ms budget")]
    TimedOut {
        /// The budget that elapsed, in milliseconds.
        timeout_ms: u64,
    },

    /// The scheduler refused to admit the task.
    #[error("task was not admitted: {0}")]
    Rejected(RejectReason),

    /// The task body panicked, or its runtime went away before it could report.
    ///
    /// Release builds use `panic = "abort"`, so in a shipped binary a panicking task takes the
    /// process with it and this variant is unreachable. It exists for development and test builds,
    /// where unwinding is enabled and a panicking task must not silently hang its awaiter.
    #[error("task panicked or its runtime stopped")]
    Panicked,
}

impl<E> TaskError<E> {
    /// Whether this is a cancellation rather than a fault.
    ///
    /// Callers should use this to drop the outcome silently instead of reporting it.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    /// Whether an attempt ran out of time.
    #[must_use]
    pub const fn is_timeout(&self) -> bool {
        matches!(self, Self::TimedOut { .. })
    }

    /// Whether the task never started because the scheduler refused it.
    #[must_use]
    pub const fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected(_))
    }

    /// The task body's own error, if the task ran and failed.
    #[must_use]
    pub const fn source_error(&self) -> Option<&E> {
        match self {
            Self::Failed { source, .. } => Some(source),
            _ => None,
        }
    }

    /// Consumes the error, yielding the task body's own error if there was one.
    #[must_use]
    pub fn into_source(self) -> Option<E> {
        match self {
            Self::Failed { source, .. } => Some(source),
            _ => None,
        }
    }

    /// Attempts actually made. Zero for work that was never started.
    // Cancelled/Panicked and Rejected both report zero, but they are different situations and are
    // listed separately so a future change to one does not silently apply to the other.
    #[allow(clippy::match_same_arms)]
    #[must_use]
    pub const fn attempts(&self) -> u32 {
        match self {
            Self::Failed { attempts, .. } => *attempts,
            Self::TimedOut { .. } => 1,
            Self::Cancelled | Self::Panicked => 0,
            Self::Rejected(_) => 0,
        }
    }
}

impl<E> From<RejectReason> for TaskError<E> {
    fn from(reason: RejectReason) -> Self {
        Self::Rejected(reason)
    }
}

impl<E> DomainError for TaskError<E>
where
    E: DomainError + 'static,
{
    fn kind(&self) -> ErrorKind {
        match self {
            // The wrapped failure names the subsystem that actually failed; the scheduler was only
            // the vehicle, so it does not overwrite that classification with its own.
            Self::Failed { source, .. } => source.kind(),
            _ => ErrorKind::Configuration,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Failed { source, .. } => source.code(),
            Self::Cancelled => "task_cancelled",
            Self::TimedOut { .. } => "task_timed_out",
            Self::Rejected(reason) => reason.code(),
            Self::Panicked => "task_panicked",
        }
    }

    // Variants that share a body today are still distinct failures;
    // merging the arms would couple rules expected to diverge.
    #[allow(clippy::match_same_arms)]
    fn recovery(&self) -> Recovery {
        match self {
            Self::Failed { source, .. } => {
                let inner = source.recovery();
                if inner.is_automatic() {
                    // The producer asked for an automatic retry, but the scheduler has already
                    // spent the whole automatic budget on this error. Handing `RetryAutomatic`
                    // back up would let a caller restart the same bounded loop indefinitely, which
                    // is exactly the unbounded retry this must not do. Escalate to the user.
                    Recovery::RetryManual
                } else {
                    inner
                }
            }
            // Nothing to recover: the caller asked for this, or is on the way out.
            Self::Cancelled | Self::Panicked | Self::Rejected(RejectReason::ShuttingDown) => {
                Recovery::Unrecoverable
            }
            // Saturation clears as in-flight work drains, so a short automatic retry is honest.
            Self::Rejected(RejectReason::QueueFull { .. }) => Recovery::RetryAutomatic {
                delay_ms: 250,
                attempts_made: 0,
                max_attempts: 3,
            },
            // A budget that elapsed once may elapse again; let the user decide rather than
            // hammering work that is evidently too slow for its deadline.
            Self::TimedOut { .. } => Recovery::RetryManual,
        }
    }

    // Variants that share a body today are still distinct failures;
    // merging the arms would couple rules expected to diverge.
    #[allow(clippy::match_same_arms)]
    fn params(&self) -> BTreeMap<String, String> {
        match self {
            Self::Failed { source, attempts } => {
                let mut params = source.params();
                params.insert("attempts".to_owned(), attempts.to_string());
                params
            }
            Self::TimedOut { timeout_ms } => {
                BTreeMap::from([("timeout_ms".to_owned(), timeout_ms.to_string())])
            }
            Self::Rejected(RejectReason::QueueFull { priority, capacity }) => BTreeMap::from([
                ("priority".to_owned(), priority.as_str().to_owned()),
                ("capacity".to_owned(), capacity.to_string()),
            ]),
            _ => BTreeMap::new(),
        }
    }
}

/// Convenience alias for the result of a scheduled task.
pub type TaskResult<T, E> = Result<T, TaskError<E>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("upstream refused the request")]
    struct Upstream {
        transient: bool,
    }

    impl DomainError for Upstream {
        fn kind(&self) -> ErrorKind {
            ErrorKind::Network
        }
        fn code(&self) -> &'static str {
            "refused"
        }
        // Variants that share a body today are still distinct failures;
        // merging the arms would couple rules expected to diverge.
        #[allow(clippy::match_same_arms)]
        fn recovery(&self) -> Recovery {
            if self.transient {
                Recovery::RetryAutomatic {
                    delay_ms: 100,
                    attempts_made: 1,
                    max_attempts: 3,
                }
            } else {
                Recovery::Unrecoverable
            }
        }
        // Variants that share a body today are still distinct failures;
        // merging the arms would couple rules expected to diverge.
        #[allow(clippy::match_same_arms)]
        fn params(&self) -> BTreeMap<String, String> {
            BTreeMap::from([("host".to_owned(), "example".to_owned())])
        }
    }

    #[test]
    fn a_wrapped_failure_reports_the_subsystem_that_failed() {
        let error: TaskError<Upstream> = TaskError::Failed {
            attempts: 3,
            source: Upstream { transient: false },
        };
        assert_eq!(error.kind(), ErrorKind::Network);
        assert_eq!(error.full_code(), "network.refused");
        assert_eq!(error.message_key(), "error.network.refused");
    }

    #[test]
    fn exhausted_automatic_retries_are_not_offered_again_automatically() {
        let error: TaskError<Upstream> = TaskError::Failed {
            attempts: 4,
            source: Upstream { transient: true },
        };
        assert_eq!(
            error.recovery(),
            Recovery::RetryManual,
            "the scheduler already spent the automatic budget; looping again would be unbounded"
        );
    }

    #[test]
    fn a_non_retryable_inner_recovery_is_passed_through_untouched() {
        let error: TaskError<Upstream> = TaskError::Failed {
            attempts: 1,
            source: Upstream { transient: false },
        };
        assert_eq!(error.recovery(), Recovery::Unrecoverable);
    }

    #[test]
    fn wrapped_params_keep_the_inner_context_and_add_the_attempt_count() {
        let error: TaskError<Upstream> = TaskError::Failed {
            attempts: 2,
            source: Upstream { transient: true },
        };
        let params = error.params();
        assert_eq!(params.get("host").map(String::as_str), Some("example"));
        assert_eq!(params.get("attempts").map(String::as_str), Some("2"));
    }

    #[test]
    fn cancellation_is_never_presented_as_something_to_retry() {
        let error: TaskError<Upstream> = TaskError::Cancelled;
        assert!(error.is_cancelled());
        assert!(!error.recovery().offers_retry());
        assert_eq!(error.full_code(), "configuration.task_cancelled");
    }

    #[test]
    fn saturation_retries_automatically_but_finitely() {
        let error: TaskError<Upstream> = TaskError::Rejected(RejectReason::QueueFull {
            priority: Priority::Background,
            capacity: 512,
        });
        match error.recovery() {
            Recovery::RetryAutomatic { max_attempts, .. } => {
                assert!(
                    max_attempts > 0 && max_attempts <= 5,
                    "retries must be bounded"
                );
            }
            other => panic!("expected a bounded automatic retry, got {other:?}"),
        }
        let params = error.params();
        assert_eq!(
            params.get("priority").map(String::as_str),
            Some("background")
        );
    }

    #[test]
    fn shutdown_rejection_is_terminal_but_saturation_is_not() {
        assert!(!RejectReason::ShuttingDown.is_transient());
        assert!(
            RejectReason::QueueFull {
                priority: Priority::Normal,
                capacity: 1,
            }
            .is_transient()
        );
    }

    #[test]
    fn timeout_reports_the_budget_that_elapsed() {
        let error: TaskError<Upstream> = TaskError::TimedOut { timeout_ms: 2_500 };
        assert!(error.is_timeout());
        assert_eq!(error.full_code(), "configuration.task_timed_out");
        assert_eq!(
            error.params().get("timeout_ms").map(String::as_str),
            Some("2500")
        );
    }

    #[test]
    fn the_diagnostic_chain_reaches_the_root_cause() {
        let payload = TaskError::Failed {
            attempts: 3,
            source: Upstream { transient: true },
        }
        .to_payload();
        let diagnostic = payload.diagnostic.expect("diagnostic present");
        assert!(diagnostic.contains("task failed after 3"), "{diagnostic}");
        assert!(
            diagnostic.contains("upstream refused the request"),
            "the root cause must survive: {diagnostic}"
        );
    }

    #[test]
    fn attempt_counts_distinguish_work_that_never_started() {
        let never_started: TaskError<Upstream> = TaskError::Rejected(RejectReason::ShuttingDown);
        assert_eq!(never_started.attempts(), 0);
        assert!(never_started.is_rejected());

        let ran: TaskError<Upstream> = TaskError::Failed {
            attempts: 5,
            source: Upstream { transient: false },
        };
        assert_eq!(ran.attempts(), 5);
        assert!(ran.source_error().is_some());
        assert!(ran.into_source().is_some());
    }

    #[test]
    fn a_reject_reason_converts_into_a_task_error() {
        let error: TaskError<Upstream> = RejectReason::ShuttingDown.into();
        assert!(error.is_rejected());
        assert!(error.into_source().is_none());
    }
}
