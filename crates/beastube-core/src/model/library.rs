//! The local library: history, watch positions and bookmarks.
//!
//! Everything here exists only on this machine. There is no account, no sync and no server (§13).
//!
//! The interesting logic is resume behaviour. Naively storing "last position" and always seeking
//! there produces two bad outcomes users notice immediately:
//!
//! * A video watched to the end reopens at the final frame, showing a frozen last frame instead of
//!   replaying (§47).
//! * A video abandoned after three seconds reopens three seconds in, which is indistinguishable
//!   from the start but breaks the "play from the beginning" expectation.
//!
//! [`PlaybackPosition::resume_at_ms`] encodes both exceptions in one place so every entry point —
//! the watch page, the mini-player, autoplay, the command palette — behaves identically.

use serde::{Deserialize, Serialize};

use crate::ids::{ChannelId, VideoId};
use crate::model::thumbnail::ThumbnailSet;
use crate::time_util::Timestamp;

/// Fraction of a video that counts as watched.
///
/// Set at 95% because outros, end cards and credits routinely occupy the last few percent; a
/// stricter threshold would leave videos permanently "in progress" after the user has finished
/// with them.
pub const COMPLETION_THRESHOLD_FRACTION: f64 = 0.95;

/// Minimum stored position that is worth resuming from.
///
/// Below this, resuming is indistinguishable from starting over but violates the expectation that
/// a barely-touched video plays from the beginning.
pub const MIN_RESUME_POSITION_MS: u64 = 15_000;

/// Minimum remaining time for a resume to be worthwhile.
///
/// Inside this window the video is effectively finished, so it restarts rather than resuming to
/// show a few seconds of credits.
pub const MIN_REMAINING_MS: u64 = 20_000;

/// How far through a video the user has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchState {
    /// Never opened.
    #[default]
    Unwatched,
    /// Opened and left partway through.
    InProgress,
    /// Watched past the completion threshold.
    Completed,
}

/// A stored playback position for one video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackPosition {
    /// Last known playhead position, in milliseconds.
    pub position_ms: u64,
    /// Total duration, when known. Without it, completion cannot be determined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// When the position was last written.
    pub updated_at: Timestamp,
}

impl PlaybackPosition {
    /// Fraction watched in `0.0..=1.0`, or `None` when the duration is unknown.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn progress_fraction(&self) -> Option<f64> {
        let duration = self.duration_ms.filter(|&d| d > 0)?;
        Some((self.position_ms as f64 / duration as f64).clamp(0.0, 1.0))
    }

    /// Whether the video counts as watched.
    ///
    /// A position at or beyond the duration is complete even if the fraction computation would
    /// disagree, which covers rows written with a slightly stale duration.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        match self.duration_ms.filter(|&d| d > 0) {
            Some(duration) => {
                self.position_ms >= duration
                    || self
                        .progress_fraction()
                        .is_some_and(|f| f >= COMPLETION_THRESHOLD_FRACTION)
                    || duration.saturating_sub(self.position_ms) <= MIN_REMAINING_MS
            }
            None => false,
        }
    }

    /// Derived watch state.
    #[must_use]
    pub fn state(&self) -> WatchState {
        if self.is_complete() {
            WatchState::Completed
        } else if self.position_ms > 0 {
            WatchState::InProgress
        } else {
            WatchState::Unwatched
        }
    }

    /// The position playback should actually start from.
    ///
    /// Returns `None` — meaning "start from the beginning" — when the video is complete or when
    /// too little was watched to be worth resuming. This is the single place that decision is made.
    #[must_use]
    pub fn resume_at_ms(&self) -> Option<u64> {
        if self.is_complete() || self.position_ms < MIN_RESUME_POSITION_MS {
            return None;
        }
        Some(self.position_ms)
    }
}

/// One row of local watch history.
///
/// Metadata is denormalized so history renders offline and after a cache clear, which is the whole
/// point of a local-first library (§72).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// The video.
    pub video_id: VideoId,
    /// Title as it was when watched. Untrusted text.
    pub title: String,
    /// Owning channel, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<ChannelId>,
    /// Channel name as it was when watched. Untrusted text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_name: Option<String>,
    /// Thumbnails as they were when watched.
    #[serde(default, skip_serializing_if = "ThumbnailSet::is_empty")]
    pub thumbnails: ThumbnailSet,
    /// Stored playback position.
    pub position: PlaybackPosition,
    /// First time this video was opened.
    pub first_watched_at: Timestamp,
    /// Most recent time this video was opened.
    pub last_watched_at: Timestamp,
    /// How many times playback has been started for this video.
    pub play_count: u32,
}

impl HistoryEntry {
    /// Whether this entry belongs in the "Continue watching" surface.
    ///
    /// Requires a resumable position *and* a known duration, so a partially-watched live stream or
    /// a video of unknown length does not appear with a meaningless progress bar.
    #[must_use]
    pub fn is_resumable(&self) -> bool {
        self.position.duration_ms.is_some() && self.position.resume_at_ms().is_some()
    }

    /// Derived watch state.
    #[must_use]
    pub fn state(&self) -> WatchState {
        self.position.state()
    }
}

/// A user-saved bookmark.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    /// The video.
    pub video_id: VideoId,
    /// Title at the time of saving. Untrusted text.
    pub title: String,
    /// Owning channel, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<ChannelId>,
    /// Channel name at the time of saving. Untrusted text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_name: Option<String>,
    /// Thumbnails at the time of saving.
    #[serde(default, skip_serializing_if = "ThumbnailSet::is_empty")]
    pub thumbnails: ThumbnailSet,
    /// User-written note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// User-assigned tags, lowercased and deduplicated by the storage layer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// A specific timestamp within the video, when the bookmark marks a moment rather than the
    /// whole video.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_ms: Option<u64>,
    /// When the bookmark was created.
    pub created_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(position_ms: u64, duration_ms: Option<u64>) -> PlaybackPosition {
        PlaybackPosition {
            position_ms,
            duration_ms,
            updated_at: Timestamp::from_millis(1_000),
        }
    }

    #[test]
    fn a_finished_video_restarts_instead_of_resuming_at_the_last_frame() {
        let finished = position(600_000, Some(600_000));
        assert!(finished.is_complete());
        assert_eq!(
            finished.resume_at_ms(),
            None,
            "resuming at the end would show a frozen final frame"
        );
        assert_eq!(finished.state(), WatchState::Completed);
    }

    #[test]
    fn the_completion_threshold_is_fractional_not_exact() {
        // 96% of a 10-minute video: outro territory, counts as watched.
        assert!(position(576_000, Some(600_000)).is_complete());
        // 80%: still in progress.
        assert!(!position(480_000, Some(600_000)).is_complete());
    }

    #[test]
    fn a_short_tail_counts_as_complete_even_below_the_fraction() {
        // A 4-hour video at 99.7% is under the 95% rule's reach in absolute terms but has only
        // 15 seconds left, which is not worth resuming into.
        let long = position(14_385_000, Some(14_400_000));
        assert!(long.is_complete());
        assert_eq!(long.resume_at_ms(), None);
    }

    #[test]
    fn a_barely_started_video_restarts() {
        let barely = position(3_000, Some(600_000));
        assert_eq!(barely.resume_at_ms(), None);
        assert_eq!(
            barely.state(),
            WatchState::InProgress,
            "it is still in progress; it just is not worth resuming"
        );
    }

    #[test]
    fn a_genuinely_partial_video_resumes_where_it_stopped() {
        let partial = position(120_000, Some(600_000));
        assert_eq!(partial.resume_at_ms(), Some(120_000));
        assert_eq!(partial.state(), WatchState::InProgress);
    }

    #[test]
    fn the_resume_boundary_is_exact() {
        assert_eq!(
            position(MIN_RESUME_POSITION_MS - 1, Some(600_000)).resume_at_ms(),
            None
        );
        assert_eq!(
            position(MIN_RESUME_POSITION_MS, Some(600_000)).resume_at_ms(),
            Some(MIN_RESUME_POSITION_MS)
        );
    }

    #[test]
    fn unknown_duration_never_reports_completion() {
        // Live streams and unparsed durations: no denominator, so no completion claim and no
        // progress bar.
        let live = position(3_600_000, None);
        assert!(!live.is_complete());
        assert_eq!(live.progress_fraction(), None);
        assert_eq!(live.state(), WatchState::InProgress);
        // A position is still resumable; only the progress bar is withheld.
        assert_eq!(live.resume_at_ms(), Some(3_600_000));
    }

    #[test]
    fn zero_duration_rows_do_not_divide_by_zero() {
        let corrupt = position(1_000, Some(0));
        assert_eq!(corrupt.progress_fraction(), None);
        assert!(!corrupt.is_complete());
    }

    #[test]
    fn a_position_beyond_the_duration_is_complete_not_over_one_hundred_percent() {
        let overshoot = position(700_000, Some(600_000));
        assert_eq!(overshoot.progress_fraction(), Some(1.0));
        assert!(overshoot.is_complete());
    }

    #[test]
    fn continue_watching_requires_a_known_duration() {
        let entry = |position: PlaybackPosition| HistoryEntry {
            video_id: VideoId::new("dQw4w9WgXcQ").unwrap(),
            title: "Test".to_owned(),
            channel_id: None,
            channel_name: None,
            thumbnails: ThumbnailSet::empty(),
            position,
            first_watched_at: Timestamp::from_millis(0),
            last_watched_at: Timestamp::from_millis(0),
            play_count: 1,
        };

        assert!(entry(position(120_000, Some(600_000))).is_resumable());
        assert!(
            !entry(position(3_600_000, None)).is_resumable(),
            "a live stream has no meaningful progress bar"
        );
        assert!(!entry(position(600_000, Some(600_000))).is_resumable());
        assert!(!entry(position(1_000, Some(600_000))).is_resumable());
    }

    #[test]
    fn unwatched_is_the_zero_position_state() {
        assert_eq!(position(0, Some(600_000)).state(), WatchState::Unwatched);
        assert_eq!(WatchState::default(), WatchState::Unwatched);
    }
}
