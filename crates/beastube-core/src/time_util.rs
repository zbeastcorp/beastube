//! Timestamps at the storage and IPC boundary.
//!
//! One representation is used everywhere: **milliseconds since the Unix epoch, UTC, as `i64`**.
//! That single choice removes a class of bugs the alternatives invite:
//!
//! * SQLite has no date type, so a text encoding would sort lexicographically-but-not-chronologically
//!   across timezone offsets; an integer sorts correctly and indexes tightly.
//! * JavaScript's `Date` is milliseconds since the epoch, so the frontend needs no parsing step and
//!   no timezone guessing — `new Date(value)` is exact.
//! * Durations elsewhere in the model are also milliseconds, so arithmetic between a position and a
//!   timestamp never needs a unit conversion.
//!
//! Localization to the user's timezone happens in the UI, which is the only layer that knows the
//! user's locale and format preferences.

use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// A point in time, stored and transmitted as milliseconds since the Unix epoch (UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    /// The Unix epoch, `1970-01-01T00:00:00Z`.
    pub const EPOCH: Self = Self(0);

    /// Current wall-clock time.
    #[must_use]
    pub fn now() -> Self {
        Self::from(OffsetDateTime::now_utc())
    }

    /// Wraps a raw millisecond count.
    #[must_use]
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// The raw millisecond count.
    #[must_use]
    pub const fn as_millis(self) -> i64 {
        self.0
    }

    /// Seconds since the epoch, truncated toward negative infinity.
    #[must_use]
    pub const fn as_secs(self) -> i64 {
        self.0.div_euclid(1000)
    }

    /// Converts to an [`OffsetDateTime`] in UTC.
    ///
    /// # Errors
    ///
    /// Returns [`time::error::ComponentRange`] if the millisecond count is outside the range
    /// representable by [`OffsetDateTime`], which can happen for a corrupted database row.
    pub fn to_offset_date_time(self) -> Result<OffsetDateTime, time::error::ComponentRange> {
        OffsetDateTime::from_unix_timestamp_nanos(i128::from(self.0) * 1_000_000)
    }

    /// Milliseconds elapsed from `self` to `later`, saturating rather than overflowing.
    #[must_use]
    pub const fn millis_until(self, later: Self) -> i64 {
        later.0.saturating_sub(self.0)
    }

    /// Whether `self` is more than `age_ms` older than `now`.
    ///
    /// Used for cache freshness checks. A timestamp in the future (clock skew, or a row written by
    /// a machine with a wrong clock) is treated as *not* stale rather than as infinitely stale,
    /// which avoids a skewed clock causing a cache stampede.
    #[must_use]
    pub const fn is_older_than(self, age_ms: i64, now: Self) -> bool {
        self.millis_until(now) > age_ms
    }
}

impl From<OffsetDateTime> for Timestamp {
    fn from(value: OffsetDateTime) -> Self {
        // Nanoseconds since the epoch is i128, so this division cannot overflow for any
        // representable OffsetDateTime; the cast is saturating for defence in depth.
        let millis = value.unix_timestamp_nanos() / 1_000_000;
        Self(i64::try_from(millis).unwrap_or(i64::MAX))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Formats a duration in milliseconds as `H:MM:SS` or `M:SS`.
///
/// Present here rather than in the UI only for logs and diagnostics; user-facing duration strings
/// are formatted by the localization layer, which knows the locale's separators.
#[must_use]
pub fn format_duration_ms(millis: u64) -> String {
    let total_secs = millis / 1000;
    let (hours, minutes, seconds) = (total_secs / 3600, (total_secs % 3600) / 60, total_secs % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_offset_date_time() {
        let ts = Timestamp::from_millis(1_735_689_600_000); // 2025-01-01T00:00:00Z
        let odt = ts.to_offset_date_time().unwrap();
        assert_eq!(odt.year(), 2025);
        assert_eq!(Timestamp::from(odt), ts);
    }

    #[test]
    fn serializes_as_a_bare_integer_for_javascript_date() {
        let ts = Timestamp::from_millis(1_700_000_000_123);
        assert_eq!(serde_json::to_string(&ts).unwrap(), "1700000000123");
        let back: Timestamp = serde_json::from_str("1700000000123").unwrap();
        assert_eq!(back, ts);
    }

    #[test]
    fn seconds_truncate_toward_negative_infinity() {
        assert_eq!(Timestamp::from_millis(1500).as_secs(), 1);
        assert_eq!(Timestamp::from_millis(-1500).as_secs(), -2);
        assert_eq!(Timestamp::from_millis(0).as_secs(), 0);
    }

    #[test]
    fn staleness_tolerates_clock_skew() {
        let now = Timestamp::from_millis(1_000_000);
        let written_in_the_future = Timestamp::from_millis(2_000_000);
        assert!(
            !written_in_the_future.is_older_than(60_000, now),
            "a future timestamp must not be treated as stale"
        );

        let old = Timestamp::from_millis(900_000);
        assert!(old.is_older_than(60_000, now));
        assert!(!old.is_older_than(200_000, now));
    }

    #[test]
    fn duration_gap_saturates_instead_of_overflowing() {
        let min = Timestamp::from_millis(i64::MIN);
        let max = Timestamp::from_millis(i64::MAX);
        // Would panic in debug on a plain subtraction.
        assert_eq!(max.millis_until(min), i64::MIN);
        assert_eq!(min.millis_until(max), i64::MAX);
    }

    #[test]
    fn formats_durations_for_logs() {
        assert_eq!(format_duration_ms(0), "0:00");
        assert_eq!(format_duration_ms(9_000), "0:09");
        assert_eq!(format_duration_ms(61_000), "1:01");
        assert_eq!(format_duration_ms(3_600_000), "1:00:00");
        assert_eq!(format_duration_ms(3_725_000), "1:02:05");
    }
}
