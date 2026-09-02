//! Retry policy: when to try again, and how long to wait.
//!
//! ## What is retried, and what is deliberately not
//!
//! Only two things justify an automatic retry: a transport fault (the request never reached a
//! server that could answer) and an explicit "come back later" from a server that did. Everything
//! else is excluded on purpose:
//!
//! * **No 4xx except 429.** A 403 or a 404 is a decision, not a hiccup. Repeating it produces the
//!   same answer while adding load to a peer that is already refusing us — and against a provider
//!   that rate-limits by request count, retrying a 403 is how a soft block becomes a hard one.
//! * **No non-idempotent methods.** A `POST` that times out may have been applied server-side; the
//!   response was lost, not the effect. Replaying it can duplicate the effect, and the transport
//!   layer has no way to know whether that is safe. `GET`, `HEAD`, `PUT` and `DELETE` are
//!   idempotent by definition (RFC 9110 §9.2.2) and are replayed freely.
//! * **No unbounded attempts.** The budget is small (three attempts by default) because a user
//!   waiting on a search would rather see a failure they can retry than a spinner that resolves
//!   after half a minute of invisible backoff (§75).
//!
//! ## Why equal jitter
//!
//! Backoff without jitter synchronises clients: everyone that failed at `t` retries at `t+300ms`,
//! reproducing the overload that caused the failure. Three schemes are common — no jitter, full
//! jitter (`rand(0, backoff)`) and equal jitter (`backoff/2 + rand(0, backoff/2)`). Full jitter
//! decorrelates best but can schedule a retry almost immediately, which for a 503 from an
//! overloaded origin means hitting it again before it has recovered. Equal jitter keeps a
//! guaranteed minimum spacing *and* decorrelates, which is the trade this application wants.
//!
//! ## Why there is no random-number dependency
//!
//! Jitter needs decorrelation, not unpredictability: nothing about it is security-sensitive. A
//! seeded xorshift is a few lines and keeps the dependency surface of a networking crate smaller,
//! which matters more than the quality of the distribution here.

use std::cell::Cell;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::Method;

use crate::error::NetworkError;

/// Attempts made before giving up, counting the first try.
///
/// Three covers the transient loss that retrying is for (a dropped SYN, one unlucky edge node)
/// without turning a hard failure into a long stall. It matches
/// [`beastube_core::settings::NetworkSettings::max_retries`]'s default.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// Backoff before the second attempt, doubling from there.
///
/// Short enough to be invisible when the first attempt lost a packet, long enough that a
/// momentarily overloaded origin is not hit again within the same tens of milliseconds.
pub const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_millis(300);

/// Ceiling on a single backoff, before jitter.
pub const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(10);

/// Longest `Retry-After` we will wait out rather than surfacing the failure.
///
/// A server may legitimately ask for minutes or hours. Blocking a request for that long is not a
/// retry, it is a hang: the caller is told to try again later instead, so the UI can show a real
/// state rather than a spinner. The cap is also what stops a hostile endpoint from parking our
/// tasks with `Retry-After: 86400`.
pub const MAX_HONOURED_RETRY_AFTER: Duration = Duration::from_secs(60);

/// The statuses that justify an automatic retry.
///
/// `429` because the server is explicitly asking us to come back; `500`, `502`, `503` and `504`
/// because they are the shapes a transient upstream fault takes behind a load balancer. `408` is
/// excluded despite being a timeout: it is a 4xx, and the rule "never retry a 4xx other than 429"
/// is easier to keep true than a list of exceptions to it.
#[must_use]
pub const fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

/// What the retry loop should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Sleep for this long, then attempt again.
    After(Duration),
    /// Stop and surface the failure.
    Stop,
}

/// How transient failures are retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts before giving up, counting the first. `1` disables retrying.
    pub max_attempts: u32,
    /// Backoff before the second attempt.
    pub initial_backoff: Duration,
    /// Ceiling on a single backoff, applied before jitter.
    pub max_backoff: Duration,
    /// Longest server-requested delay that will be waited out rather than surfaced.
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
            max_retry_after: MAX_HONOURED_RETRY_AFTER,
        }
    }
}

impl RetryPolicy {
    /// A policy with `max_attempts` and otherwise default timings.
    ///
    /// `max_attempts` is clamped to at least one: a policy that permits zero attempts would mean
    /// "never send the request", which is not something a retry policy should be able to express.
    #[must_use]
    pub fn with_max_attempts(max_attempts: u32) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            ..Self::default()
        }
    }

    /// A policy that never retries, for callers that own their own scheduling.
    #[must_use]
    pub fn none() -> Self {
        Self::with_max_attempts(1)
    }

    /// Decides whether to attempt again after `error`, having already made `attempts_made` tries.
    ///
    /// The method is part of the decision, not an afterthought: a non-idempotent request is never
    /// replayed regardless of how transient the failure looks.
    #[must_use]
    pub fn decide(
        &self,
        method: &Method,
        error: &NetworkError,
        attempts_made: u32,
    ) -> RetryDecision {
        // `attempts_made` counts attempts already *completed*, and this is only ever reached after
        // one has failed — so zero is a caller mistake, not a meaningful input. Clamping to one
        // makes a `max_attempts: 1` policy actually stop, instead of granting a retry it never
        // budgeted for.
        let attempts_made = attempts_made.max(1);
        if attempts_made >= self.max_attempts || !method.is_idempotent() || !error.is_transient() {
            return RetryDecision::Stop;
        }
        match error.retry_after() {
            // The server told us when it will be ready. Believing it beats guessing, unless it
            // asks for longer than we are willing to hold the request open.
            Some(requested) if requested > self.max_retry_after => RetryDecision::Stop,
            Some(requested) => RetryDecision::After(requested),
            None => RetryDecision::After(self.backoff(attempts_made)),
        }
    }

    /// Backoff before attempt number `attempts_made + 1`, with equal jitter applied.
    ///
    /// The exponent is computed with saturating arithmetic so a caller that passes an absurd
    /// attempt count gets the ceiling rather than a panic or a wrapped, near-zero delay.
    #[must_use]
    pub fn backoff(&self, attempts_made: u32) -> Duration {
        let scale = 2u32.saturating_pow(attempts_made.min(30));
        let uncapped = self
            .initial_backoff
            .checked_mul(scale)
            .unwrap_or(self.max_backoff);
        let capped = uncapped.min(self.max_backoff);
        apply_equal_jitter(capped)
    }
}

/// Halves `base` and adds a random share of the other half.
///
/// Returns a value in `[base/2, base]`, so the schedule keeps its shape while no two clients that
/// failed together retry together.
fn apply_equal_jitter(base: Duration) -> Duration {
    let half = base / 2;
    // `jitter_unit()` is in `[0, 1)`, so the product never exceeds `half` and the sum never
    // exceeds `base`. Multiplying a `Duration` by an `f64` is done through `mul_f64`, which
    // saturates at `Duration::MAX` rather than overflowing.
    half + half.mul_f64(jitter_unit())
}

thread_local! {
    /// Per-thread xorshift state. Thread-local rather than shared so jitter costs no
    /// synchronisation on the hot path, and so two threads backing off concurrently do not
    /// serialise on the same atomic.
    static JITTER_STATE: Cell<u64> = const { Cell::new(0) };
}

/// A pseudo-random value in `[0, 1)`.
fn jitter_unit() -> f64 {
    let bits = JITTER_STATE.with(|state| {
        let mut x = state.get();
        if x == 0 {
            x = seed();
        }
        // xorshift64: full period over the non-zero 64-bit states, which is all this needs.
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        state.set(x);
        x
    });
    // Take the top 53 bits, the exact mantissa width of an `f64`, so every representable value in
    // the interval is reachable and the division is lossless.
    // Both casts are exact: 53 bits is precisely the f64 mantissa width, and 2^53 is itself
    // representable. The attribute covers the whole expression, not just the first cast.
    #[allow(clippy::cast_precision_loss)]
    {
        let numerator = (bits >> 11) as f64;
        let denominator = (1_u64 << 53) as f64;
        numerator / denominator
    }
}

/// Seeds the generator from the clock, mixed with the address of a stack local so that two threads
/// starting in the same nanosecond do not share a stream.
fn seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0x9E37_79B9_7F4A_7C15, |d| {
            u64::try_from(d.as_nanos() & u128::from(u64::MAX)).unwrap_or(1)
        });
    let local = 0u8;
    let address = std::ptr::from_ref(&local) as u64;
    // Any non-zero value works; the constant is the golden-ratio odd word used by splitmix64.
    let mixed = nanos ^ address.rotate_left(32) ^ 0x9E37_79B9_7F4A_7C15;
    if mixed == 0 { 1 } else { mixed }
}

/// Parses a `Retry-After` header value into a delay from `now_epoch_secs`.
///
/// RFC 9110 permits either a delay in seconds or an HTTP-date. Both appear in the wild: CDNs
/// generally send seconds, origin servers behind a cache sometimes send a date. A date already in
/// the past yields `Duration::ZERO` rather than an error, because "retry immediately" is what the
/// server meant.
///
/// Only the IMF-fixdate form (`Sun, 06 Nov 1994 08:49:37 GMT`) is accepted. RFC 9110 requires
/// senders to use it, and the two obsolete formats it also tolerates in *receivers* are ambiguous
/// enough (two-digit years) that guessing wrong is worse than falling back to our own backoff.
#[must_use]
pub fn parse_retry_after(value: &str, now_epoch_secs: i64) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        return value.parse::<u64>().ok().map(Duration::from_secs);
    }
    let target = parse_imf_fixdate(value)?;
    let delta = target.saturating_sub(now_epoch_secs).max(0);
    Some(Duration::from_secs(u64::try_from(delta).unwrap_or(0)))
}

/// Seconds since the Unix epoch for an IMF-fixdate, or `None` if the string is not one.
fn parse_imf_fixdate(value: &str) -> Option<i64> {
    // `Sun, 06 Nov 1994 08:49:37 GMT` — fixed width, so the fields are sliced by position after a
    // length check rather than split, which keeps a malformed value from being partially accepted.
    let bytes = value.as_bytes();
    if bytes.len() != 29 || bytes[3] != b',' || bytes[4] != b' ' || !value.ends_with(" GMT") {
        return None;
    }
    let day = two_digits(value.get(5..7)?)?;
    let month = month_from_abbreviation(value.get(8..11)?)?;
    let year: i64 = value.get(12..16)?.parse().ok()?;
    let hour = two_digits(value.get(17..19)?)?;
    let minute = two_digits(value.get(20..22)?)?;
    let second = two_digits(value.get(23..25)?)?;

    if day == 0 || day > 31 || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second))
}

fn two_digits(text: &str) -> Option<u8> {
    if text.len() == 2 && text.bytes().all(|b| b.is_ascii_digit()) {
        text.parse().ok()
    } else {
        None
    }
}

fn month_from_abbreviation(text: &str) -> Option<u8> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    MONTHS
        .iter()
        .position(|m| *m == text)
        .and_then(|index| u8::try_from(index + 1).ok())
}

/// Days from `1970-01-01` to `year-month-day`, by Howard Hinnant's `days_from_civil`.
///
/// Written out rather than pulled from a date library because this crate needs exactly one date
/// conversion, in one header parser, and a calendar dependency in the network layer would be
/// carried by every consumer of it.
fn days_from_civil(year: i64, month: u8, day: u8) -> i64 {
    let month = i64::from(month);
    let day = i64::from(day);
    // Shift the year so that March is month 1: leap day then lands at the end of the year, which
    // removes every special case from the arithmetic below.
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn transient() -> NetworkError {
        NetworkError::Status {
            host: "example.com".to_owned(),
            status: 503,
            retry_after_ms: None,
        }
    }

    fn permanent() -> NetworkError {
        NetworkError::Status {
            host: "example.com".to_owned(),
            status: 404,
            retry_after_ms: None,
        }
    }

    #[test]
    fn a_transient_failure_is_retried_until_the_budget_runs_out() {
        let policy = RetryPolicy::with_max_attempts(3);
        assert!(matches!(
            policy.decide(&Method::GET, &transient(), 1),
            RetryDecision::After(_)
        ));
        assert!(matches!(
            policy.decide(&Method::GET, &transient(), 2),
            RetryDecision::After(_)
        ));
        assert_eq!(
            policy.decide(&Method::GET, &transient(), 3),
            RetryDecision::Stop,
            "the third attempt exhausts a three-attempt budget"
        );
    }

    #[test]
    fn a_permanent_failure_is_never_retried() {
        let policy = RetryPolicy::default();
        assert_eq!(
            policy.decide(&Method::GET, &permanent(), 0),
            RetryDecision::Stop
        );
    }

    #[test]
    fn a_non_idempotent_method_is_never_replayed() {
        let policy = RetryPolicy::default();
        for method in [Method::POST, Method::PATCH, Method::CONNECT] {
            assert_eq!(
                policy.decide(&method, &transient(), 0),
                RetryDecision::Stop,
                "{method} may have been applied server-side already"
            );
        }
        for method in [Method::GET, Method::HEAD, Method::PUT, Method::DELETE] {
            assert!(
                matches!(
                    policy.decide(&method, &transient(), 0),
                    RetryDecision::After(_)
                ),
                "{method} is idempotent and safe to replay"
            );
        }
    }

    #[test]
    fn a_disabled_policy_stops_immediately() {
        assert_eq!(
            RetryPolicy::none().decide(&Method::GET, &transient(), 1),
            RetryDecision::Stop
        );
        // Zero is not a meaningful attempt count here; it must not buy an extra retry.
        assert_eq!(
            RetryPolicy::none().decide(&Method::GET, &transient(), 0),
            RetryDecision::Stop
        );
        assert_eq!(
            RetryPolicy::with_max_attempts(0).max_attempts,
            1,
            "a zero-attempt policy would mean never sending the request"
        );
    }

    #[test]
    fn a_servers_retry_after_wins_over_our_backoff() {
        let policy = RetryPolicy::default();
        let error = NetworkError::Status {
            host: "example.com".to_owned(),
            status: 429,
            retry_after_ms: Some(7_000),
        };
        assert_eq!(
            policy.decide(&Method::GET, &error, 0),
            RetryDecision::After(Duration::from_secs(7))
        );
    }

    #[test]
    fn an_absurd_retry_after_surfaces_the_failure_instead_of_hanging() {
        let policy = RetryPolicy::default();
        let error = NetworkError::Status {
            host: "example.com".to_owned(),
            status: 429,
            retry_after_ms: Some(86_400_000),
        };
        assert_eq!(
            policy.decide(&Method::GET, &error, 0),
            RetryDecision::Stop,
            "a day-long wait is a hang, not a retry"
        );
    }

    #[test]
    fn backoff_grows_but_is_capped() {
        let policy = RetryPolicy {
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(800),
            ..RetryPolicy::default()
        };
        // Equal jitter puts every value in [base/2, base].
        for (attempts, base_ms) in [(0u32, 100u64), (1, 200), (2, 400), (3, 800), (4, 800)] {
            let delay = policy.backoff(attempts);
            assert!(
                delay >= Duration::from_millis(base_ms / 2)
                    && delay <= Duration::from_millis(base_ms),
                "backoff({attempts}) = {delay:?} outside [{}ms, {base_ms}ms]",
                base_ms / 2
            );
        }
    }

    #[test]
    fn backoff_saturates_instead_of_overflowing() {
        let policy = RetryPolicy::default();
        // An attempt count this large would wrap a naive `1 << attempts`.
        for attempts in [30u32, 31, 64, u32::MAX] {
            let delay = policy.backoff(attempts);
            assert!(
                delay <= policy.max_backoff && delay >= policy.max_backoff / 2,
                "backoff({attempts}) = {delay:?} must clamp to the ceiling"
            );
        }
    }

    #[test]
    fn jitter_decorrelates_successive_backoffs() {
        let policy = RetryPolicy {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(1),
            ..RetryPolicy::default()
        };
        let distinct: HashSet<u128> = (0..64).map(|_| policy.backoff(0).as_nanos()).collect();
        assert!(
            distinct.len() > 32,
            "64 backoffs produced only {} distinct delays; clients would retry in lockstep",
            distinct.len()
        );
    }

    #[test]
    fn jitter_never_leaves_the_unit_interval() {
        for _ in 0..10_000 {
            let value = jitter_unit();
            assert!((0.0..1.0).contains(&value), "{value} is outside [0, 1)");
        }
    }

    #[test]
    fn retry_after_accepts_delay_seconds() {
        assert_eq!(parse_retry_after("120", 0), Some(Duration::from_secs(120)));
        assert_eq!(parse_retry_after("  0  ", 0), Some(Duration::ZERO));
    }

    #[test]
    fn retry_after_accepts_an_http_date() {
        // 1994-11-06T08:49:37Z is 784_111_777 seconds after the epoch.
        let target = 784_111_777;
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", target - 30),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", target),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn a_retry_after_in_the_past_means_now_not_an_error() {
        let target = 784_111_777;
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", target + 10_000),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn retry_after_rejects_junk_instead_of_guessing() {
        for junk in [
            "",
            "   ",
            "soon",
            "-5",
            "12.5",
            "1e9",
            "Sun, 06 Nov 1994 08:49:37",           // no zone
            "Sun, 06 Xxx 1994 08:49:37 GMT",       // bad month
            "Sun, 32 Nov 1994 08:49:37 GMT",       // bad day
            "Sun, 06 Nov 1994 25:49:37 GMT",       // bad hour
            "Sunday, 06-Nov-94 08:49:37 GMT",      // RFC 850, deliberately unsupported
            "Sun Nov  6 08:49:37 1994",            // asctime, deliberately unsupported
            "Sun, 06 Nov 1994 08:49:37 GMT extra", // trailing junk
        ] {
            assert_eq!(
                parse_retry_after(junk, 0),
                None,
                "{junk:?} must be rejected"
            );
        }
    }

    #[test]
    fn retry_after_does_not_panic_on_multibyte_input() {
        // Slicing by byte position must never split a character boundary.
        for hostile in [
            "日曜日, 06 Nov 1994 08:49:37 GMT",
            "Sun, 0６ Nov 1994 08:49:37 GMT",
        ] {
            assert_eq!(parse_retry_after(hostile, 0), None);
        }
    }

    #[test]
    fn civil_dates_convert_across_leap_years_and_centuries() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        // 2000 is a leap year, 1900 is not: the century rule must survive.
        assert_eq!(
            days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28),
            2
        );
        assert_eq!(
            days_from_civil(2024, 2, 29) + 1,
            days_from_civil(2024, 3, 1)
        );
    }

    #[test]
    fn statuses_outside_the_documented_set_are_not_retryable() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(502));
        assert!(is_retryable_status(503));
        assert!(is_retryable_status(504));
        for status in [200, 204, 301, 400, 401, 403, 404, 408, 410, 451, 501, 505] {
            assert!(!is_retryable_status(status), "{status} must not be retried");
        }
    }
}
