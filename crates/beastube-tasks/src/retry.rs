//! Retry policy: bounded exponential backoff with full jitter.
//!
//! ## Why full jitter
//!
//! Plain exponential backoff synchronizes clients. When a server returns 503 to twenty in-flight
//! requests, all twenty back off by the same amount and retry in the same millisecond, reproducing
//! the load that caused the failure. *Full* jitter — a uniform draw from `[0, backoff]` rather than
//! a small perturbation around `backoff` — spreads the retries across the whole window and is what
//! actually breaks the synchronization.
//!
//! The cost is that an individual retry can fire almost immediately. That is acceptable here
//! because the attempt count is bounded regardless: the worst case is a few fast attempts,
//! not an unbounded hot loop.
//!
//! ## Why the jitter source is injectable
//!
//! Randomness makes tests either flaky or vacuous. [`JitterSource`] lets a test install
//! [`FixedJitter`] and assert exact delays, while production uses [`SystemJitter`].
//!
//! [`SystemJitter`] is a small xorshift generator rather than a dependency on `rand`. It is used
//! **only** to spread retry timing and never for anything security-relevant — no token, key, nonce
//! or identifier is derived from it. Pulling in a cryptographic RNG for this would be a dependency
//! bought for nothing.

use std::cell::Cell;
use std::time::Duration;

/// Hard ceiling on attempts, whatever a policy asks for.
///
/// Retrying is never unbounded. A caller that sets a larger `max_attempts` is clamped rather
/// than trusted, so a configuration mistake cannot turn a persistent failure into an infinite loop.
pub const MAX_ATTEMPT_CEILING: u32 = 10;

/// Default number of attempts, including the first.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// Default delay before the second attempt.
pub const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_millis(250);

/// Default ceiling on a single backoff interval.
///
/// Beyond roughly ten seconds a retry stops feeling like recovery and starts feeling like a hang,
/// so the UI is better served by reporting the failure and offering a manual retry.
pub const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(10);

/// Default growth factor between attempts.
pub const DEFAULT_MULTIPLIER: u32 = 2;

/// Supplies the random fraction used for full jitter.
pub trait JitterSource: Send + Sync + std::fmt::Debug {
    /// A value in `[0.0, 1.0]`, scaling the computed backoff.
    fn fraction(&self) -> f64;
}

/// Production jitter: a xorshift64\* generator seeded from the clock and the thread.
///
/// Not cryptographically secure, and deliberately so — see the module docs. Thread-local state
/// keeps it lock-free, and each thread seeds independently so two threads do not produce the same
/// sequence.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemJitter;

thread_local! {
    static XORSHIFT_STATE: Cell<u64> = const { Cell::new(0) };
}

impl JitterSource for SystemJitter {
    fn fraction(&self) -> f64 {
        XORSHIFT_STATE.with(|state| {
            let mut x = state.get();
            if x == 0 {
                // Seed lazily from the clock, mixed with the address of the thread-local so two
                // threads entering in the same nanosecond still diverge.
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0x9E37_79B9_7F4A_7C15, |d| {
                        // Truncating to the low 64 bits is fine: this seeds a
                        // jitter generator, where only spread matters.
                        #[allow(clippy::cast_possible_truncation)]
                        {
                            d.as_nanos() as u64
                        }
                    });
                let address = std::ptr::from_ref(state) as u64;
                x = nanos ^ address.rotate_left(17);
                if x == 0 {
                    x = 0x9E37_79B9_7F4A_7C15;
                }
            }
            // xorshift64*
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            state.set(x);
            let value = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
            // Take the high 53 bits, the range f64 represents exactly.
            #[allow(clippy::cast_precision_loss)]
            {
                (value >> 11) as f64 / (1u64 << 53) as f64
            }
        })
    }
}

/// Deterministic jitter for tests.
#[derive(Debug, Clone, Copy)]
pub struct FixedJitter(f64);

impl FixedJitter {
    /// A source that always returns `fraction`, clamped into `[0.0, 1.0]`.
    #[must_use]
    pub fn new(fraction: f64) -> Self {
        Self(if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        })
    }

    /// Always returns the full backoff, making delays exactly the unjittered schedule.
    #[must_use]
    pub const fn full() -> Self {
        Self(1.0)
    }

    /// Always returns zero delay.
    #[must_use]
    pub const fn none() -> Self {
        Self(0.0)
    }
}

impl JitterSource for FixedJitter {
    fn fraction(&self) -> f64 {
        self.0
    }
}

/// How many times, and how far apart, to retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    max_attempts: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
    multiplier: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
            multiplier: DEFAULT_MULTIPLIER,
        }
    }
}

impl RetryPolicy {
    /// A policy that never retries: one attempt, then the error stands.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            max_attempts: 1,
            initial_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
            multiplier: 1,
        }
    }

    /// A policy with `max_attempts` total attempts, clamped to `1..=MAX_ATTEMPT_CEILING`.
    #[must_use]
    pub fn with_attempts(max_attempts: u32) -> Self {
        Self {
            max_attempts: max_attempts.clamp(1, MAX_ATTEMPT_CEILING),
            ..Self::default()
        }
    }

    /// Sets the delay before the second attempt.
    #[must_use]
    pub const fn initial_backoff(mut self, backoff: Duration) -> Self {
        self.initial_backoff = backoff;
        self
    }

    /// Sets the ceiling on any single backoff interval.
    #[must_use]
    pub const fn max_backoff(mut self, backoff: Duration) -> Self {
        self.max_backoff = backoff;
        self
    }

    /// Sets the growth factor. A multiplier below 1 is treated as 1 (constant backoff).
    #[must_use]
    pub const fn multiplier(mut self, multiplier: u32) -> Self {
        self.multiplier = if multiplier < 1 { 1 } else { multiplier };
        self
    }

    /// Total attempts this policy permits, including the first.
    #[must_use]
    pub const fn max_attempts(self) -> u32 {
        self.max_attempts
    }

    /// Whether another attempt is permitted after `attempts_made` have already been made.
    #[must_use]
    pub const fn permits_retry(self, attempts_made: u32) -> bool {
        attempts_made < self.max_attempts
    }

    /// The unjittered backoff before attempt number `attempts_made + 1`.
    ///
    /// Saturates instead of overflowing: an aggressive multiplier with a high attempt count would
    /// otherwise wrap and produce a *shorter* delay than the previous attempt.
    #[must_use]
    pub fn base_backoff(self, attempts_made: u32) -> Duration {
        if attempts_made == 0 {
            return Duration::ZERO;
        }
        let exponent = attempts_made - 1;
        let factor = u64::from(self.multiplier).saturating_pow(exponent);
        let millis = u64::try_from(self.initial_backoff.as_millis())
            .unwrap_or(u64::MAX)
            .saturating_mul(factor);
        Duration::from_millis(millis).min(self.max_backoff)
    }

    /// The actual delay to wait, with full jitter applied.
    ///
    /// Returns `None` when the policy is exhausted, so the caller cannot accidentally treat
    /// "no more attempts" as "retry immediately".
    #[must_use]
    pub fn delay_after(self, attempts_made: u32, jitter: &dyn JitterSource) -> Option<Duration> {
        if !self.permits_retry(attempts_made) {
            return None;
        }
        let base = self.base_backoff(attempts_made);
        if base.is_zero() {
            return Some(Duration::ZERO);
        }
        let fraction = {
            let raw = jitter.fraction();
            if raw.is_finite() {
                raw.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let millis = (base.as_millis() as f64 * fraction) as u64;
        Some(Duration::from_millis(millis))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_attempt_is_never_delayed() {
        assert_eq!(RetryPolicy::default().base_backoff(0), Duration::ZERO);
    }

    #[test]
    fn backoff_grows_geometrically_until_the_ceiling() {
        let policy = RetryPolicy::with_attempts(10)
            .initial_backoff(Duration::from_millis(100))
            .max_backoff(Duration::from_millis(800))
            .multiplier(2);

        assert_eq!(policy.base_backoff(1), Duration::from_millis(100));
        assert_eq!(policy.base_backoff(2), Duration::from_millis(200));
        assert_eq!(policy.base_backoff(3), Duration::from_millis(400));
        assert_eq!(policy.base_backoff(4), Duration::from_millis(800));
        assert_eq!(
            policy.base_backoff(5),
            Duration::from_millis(800),
            "clamped"
        );
    }

    #[test]
    fn an_extreme_multiplier_saturates_rather_than_wrapping() {
        // Without saturation this wraps and yields a delay shorter than the previous attempt,
        // turning backoff into a hot loop at exactly the moment it matters most.
        let policy = RetryPolicy::with_attempts(10)
            .initial_backoff(Duration::from_secs(1))
            .max_backoff(Duration::from_secs(30))
            .multiplier(1000);

        let mut previous = Duration::ZERO;
        for attempt in 1..=9 {
            let backoff = policy.base_backoff(attempt);
            assert!(
                backoff >= previous,
                "backoff must never decrease (attempt {attempt})"
            );
            assert!(backoff <= Duration::from_secs(30));
            previous = backoff;
        }
    }

    #[test]
    fn retries_stop_at_the_configured_attempt_count() {
        let policy = RetryPolicy::with_attempts(3);
        assert!(policy.permits_retry(0));
        assert!(policy.permits_retry(1));
        assert!(policy.permits_retry(2));
        assert!(!policy.permits_retry(3), "three attempts means no fourth");
    }

    #[test]
    fn the_attempt_ceiling_cannot_be_exceeded_by_configuration() {
        assert_eq!(
            RetryPolicy::with_attempts(1_000).max_attempts(),
            MAX_ATTEMPT_CEILING
        );
        assert_eq!(
            RetryPolicy::with_attempts(0).max_attempts(),
            1,
            "at least one attempt"
        );
    }

    #[test]
    fn a_none_policy_never_retries() {
        let policy = RetryPolicy::none();
        assert_eq!(policy.max_attempts(), 1);
        assert!(!policy.permits_retry(1));
        assert_eq!(policy.delay_after(1, &FixedJitter::full()), None);
    }

    #[test]
    fn an_exhausted_policy_returns_no_delay_rather_than_zero() {
        // Zero would read as "retry immediately", which is the opposite of what exhaustion means.
        let policy = RetryPolicy::with_attempts(2);
        assert!(policy.delay_after(1, &FixedJitter::full()).is_some());
        assert_eq!(policy.delay_after(2, &FixedJitter::full()), None);
    }

    #[test]
    fn full_jitter_spans_zero_to_the_base_backoff() {
        let policy = RetryPolicy::with_attempts(5).initial_backoff(Duration::from_millis(400));

        assert_eq!(
            policy.delay_after(1, &FixedJitter::full()),
            Some(Duration::from_millis(400))
        );
        assert_eq!(
            policy.delay_after(1, &FixedJitter::none()),
            Some(Duration::ZERO)
        );
        assert_eq!(
            policy.delay_after(1, &FixedJitter::new(0.5)),
            Some(Duration::from_millis(200))
        );
    }

    #[test]
    // These compare against literals the clamp returns bit-for-bit, not computed values.
    #[allow(clippy::float_cmp)]
    fn a_nonsensical_jitter_fraction_cannot_extend_the_delay() {
        #[derive(Debug)]
        struct Hostile;
        impl JitterSource for Hostile {
            fn fraction(&self) -> f64 {
                f64::NAN
            }
        }
        #[derive(Debug)]
        struct TooLarge;
        impl JitterSource for TooLarge {
            fn fraction(&self) -> f64 {
                1000.0
            }
        }

        let policy = RetryPolicy::with_attempts(5).initial_backoff(Duration::from_millis(100));
        assert_eq!(policy.delay_after(1, &Hostile), Some(Duration::ZERO));
        assert_eq!(
            policy.delay_after(1, &TooLarge),
            Some(Duration::from_millis(100)),
            "a fraction above 1 must clamp, not multiply the backoff"
        );

        assert_eq!(FixedJitter::new(f64::INFINITY).fraction(), 0.0);
        assert_eq!(FixedJitter::new(-5.0).fraction(), 0.0);
        assert_eq!(FixedJitter::new(2.0).fraction(), 1.0);
    }

    #[test]
    fn system_jitter_stays_in_range_and_actually_varies() {
        let jitter = SystemJitter;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..256 {
            let value = jitter.fraction();
            assert!((0.0..=1.0).contains(&value), "out of range: {value}");
            seen.insert(value.to_bits());
        }
        assert!(
            seen.len() > 200,
            "a jitter source that repeats itself does not decorrelate retries: {} distinct",
            seen.len()
        );
    }

    #[test]
    fn a_constant_multiplier_produces_constant_backoff() {
        let policy = RetryPolicy::with_attempts(5)
            .initial_backoff(Duration::from_millis(300))
            .multiplier(0); // clamped up to 1

        assert_eq!(policy.base_backoff(1), Duration::from_millis(300));
        assert_eq!(policy.base_backoff(3), Duration::from_millis(300));
    }
}
