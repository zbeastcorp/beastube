//! Creator-marked segment skipping.
//!
//! Pure logic: given a set of segments and a playhead position, decide whether to skip and where
//! to. No network, no storage — the caller fetches segments and applies the decision, so this is
//! testable without either.
//!
//! Segments come from community submissions, which means they are untrusted and frequently
//! malformed: reversed, overlapping, zero-length, or extending past the end of the video. Rather
//! than trusting them, [`SegmentSkipper::new`] normalizes the set once — dropping the nonsense and
//! merging what overlaps — so the per-frame lookup is a simple scan over well-formed data.

use serde::{Deserialize, Serialize};

/// What a segment asks the player to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentAction {
    /// Jump past the segment.
    Skip,
    /// A single point of interest, not a range. Offered as a jump target, never skipped
    /// automatically — skipping *to* the highlight of a video the user chose to open would be
    /// hostile.
    Poi,
    /// A chapter boundary. Never skipped; used to populate the chapter list when the provider
    /// supplies none.
    Chapter,
}

impl SegmentAction {
    /// Whether a segment with this action may be skipped automatically.
    #[must_use]
    pub const fn is_skippable(self) -> bool {
        matches!(self, Self::Skip)
    }
}

/// One marked region of a video.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// Category slug, e.g. `sponsor`, `intro`, `outro`, `selfpromo`.
    ///
    /// Kept as an opaque string rather than an enum: the upstream category list grows, and an
    /// unknown category should be ignorable rather than a parse failure.
    pub category: String,
    /// Start offset in milliseconds.
    pub start_ms: u64,
    /// End offset in milliseconds, inclusive of the skip target.
    pub end_ms: u64,
    /// What to do with it.
    pub action: SegmentAction,
}

impl Segment {
    /// Whether this segment is structurally usable.
    ///
    /// A reversed or zero-length range is dropped rather than clamped: it carries no information,
    /// and a zero-length "skip" would fire repeatedly at one position.
    #[must_use]
    pub const fn is_well_formed(&self) -> bool {
        self.end_ms > self.start_ms
    }

    /// Whether `position_ms` falls inside this segment.
    #[must_use]
    pub const fn contains(&self, position_ms: u64) -> bool {
        position_ms >= self.start_ms && position_ms < self.end_ms
    }

    /// Length in milliseconds.
    #[must_use]
    pub const fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// Shortest segment worth skipping, in milliseconds.
///
/// Below roughly a second the seek itself costs more than the content skipped, and the resulting
/// stutter is more disruptive than the segment would have been.
pub const MIN_SKIPPABLE_MS: u64 = 1_000;

/// Decides where to skip, given a normalized segment set.
#[derive(Debug, Clone, Default)]
pub struct SegmentSkipper {
    /// Skippable segments, sorted by start and non-overlapping.
    skippable: Vec<Segment>,
    /// Points of interest and chapters, retained for the UI but never skipped.
    markers: Vec<Segment>,
}

impl SegmentSkipper {
    /// Normalizes `segments` into a usable set.
    ///
    /// Drops malformed and too-short segments, clamps to `duration_ms` when known, sorts by start,
    /// and merges overlapping skippable ranges so that skipping never lands inside another segment
    /// and immediately skip again.
    #[must_use]
    pub fn new(segments: Vec<Segment>, duration_ms: Option<u64>) -> Self {
        let mut skippable: Vec<Segment> = Vec::new();
        let mut markers: Vec<Segment> = Vec::new();

        for mut segment in segments {
            if let Some(duration) = duration_ms {
                if segment.start_ms >= duration {
                    // Entirely past the end: nothing to skip.
                    continue;
                }
                segment.end_ms = segment.end_ms.min(duration);
            }
            if !segment.is_well_formed() {
                continue;
            }
            if segment.action.is_skippable() {
                if segment.duration_ms() >= MIN_SKIPPABLE_MS {
                    skippable.push(segment);
                }
            } else {
                markers.push(segment);
            }
        }

        skippable.sort_by_key(|segment| (segment.start_ms, segment.end_ms));
        markers.sort_by_key(|segment| (segment.start_ms, segment.end_ms));

        // Merge overlapping and touching ranges. Without this, skipping out of one segment can land
        // inside the next, producing a visible double-jump.
        let mut merged: Vec<Segment> = Vec::with_capacity(skippable.len());
        for segment in skippable {
            match merged.last_mut() {
                Some(previous) if segment.start_ms <= previous.end_ms => {
                    previous.end_ms = previous.end_ms.max(segment.end_ms);
                }
                _ => merged.push(segment),
            }
        }

        Self {
            skippable: merged,
            markers,
        }
    }

    /// An empty skipper, for videos with no segments.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            skippable: Vec::new(),
            markers: Vec::new(),
        }
    }

    /// Whether there is anything to skip.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.skippable.is_empty()
    }

    /// Number of skippable segments after normalization.
    #[must_use]
    pub fn len(&self) -> usize {
        self.skippable.len()
    }

    /// The skippable segments, sorted and merged.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.skippable
    }

    /// Points of interest and chapter markers, which are never skipped.
    #[must_use]
    pub fn markers(&self) -> &[Segment] {
        &self.markers
    }

    /// The position to jump to, if `position_ms` is inside a skippable segment.
    ///
    /// Returns `None` when the playhead is not inside one, which is the common case and must stay
    /// cheap: it is consulted on every time update.
    #[must_use]
    pub fn skip_target(&self, position_ms: u64) -> Option<SkipTarget> {
        self.skippable
            .iter()
            .find(|segment| segment.contains(position_ms))
            .map(|segment| SkipTarget {
                to_ms: segment.end_ms,
                category: segment.category.clone(),
                skipped_ms: segment.end_ms.saturating_sub(position_ms),
            })
    }

    /// The next skippable segment starting at or after `position_ms`.
    ///
    /// Used to show a "skip" affordance shortly before a segment when automatic skipping is off.
    #[must_use]
    pub fn next_segment(&self, position_ms: u64) -> Option<&Segment> {
        self.skippable
            .iter()
            .find(|segment| segment.start_ms >= position_ms)
    }

    /// Total time skippable across the whole video.
    #[must_use]
    pub fn total_skippable_ms(&self) -> u64 {
        self.skippable.iter().map(Segment::duration_ms).sum::<u64>()
    }

    /// Keeps only segments whose category is in `enabled`.
    ///
    /// Applied after normalization so the user's category preferences do not have to be threaded
    /// through parsing.
    #[must_use]
    pub fn with_categories(mut self, enabled: &[String]) -> Self {
        self.skippable
            .retain(|segment| enabled.iter().any(|category| category == &segment.category));
        self
    }
}

/// Where to jump, and what was skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkipTarget {
    /// Position to seek to, in milliseconds.
    pub to_ms: u64,
    /// Category of the segment being skipped, for the toast the UI shows.
    pub category: String,
    /// How much was skipped, for the same toast.
    pub skipped_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skip(category: &str, start_ms: u64, end_ms: u64) -> Segment {
        Segment {
            category: category.to_owned(),
            start_ms,
            end_ms,
            action: SegmentAction::Skip,
        }
    }

    #[test]
    fn a_position_inside_a_segment_yields_its_end() {
        let skipper = SegmentSkipper::new(vec![skip("sponsor", 10_000, 30_000)], Some(600_000));
        let target = skipper.skip_target(15_000).expect("inside the segment");
        assert_eq!(target.to_ms, 30_000);
        assert_eq!(target.category, "sponsor");
        assert_eq!(target.skipped_ms, 15_000);
    }

    #[test]
    fn positions_outside_every_segment_yield_nothing() {
        let skipper = SegmentSkipper::new(vec![skip("sponsor", 10_000, 30_000)], Some(600_000));
        assert!(skipper.skip_target(0).is_none());
        assert!(skipper.skip_target(9_999).is_none());
        assert!(
            skipper.skip_target(30_000).is_none(),
            "the end is exclusive, or skipping would re-fire at the landing position"
        );
        assert!(skipper.skip_target(500_000).is_none());
    }

    #[test]
    fn overlapping_segments_are_merged_into_one_jump() {
        // Without merging, skipping out of the first lands inside the second and jumps again,
        // which the viewer sees as a stutter.
        let skipper = SegmentSkipper::new(
            vec![
                skip("sponsor", 10_000, 30_000),
                skip("selfpromo", 25_000, 45_000),
            ],
            Some(600_000),
        );
        assert_eq!(skipper.len(), 1);
        assert_eq!(skipper.skip_target(15_000).expect("inside").to_ms, 45_000);
    }

    #[test]
    fn touching_segments_are_merged() {
        let skipper = SegmentSkipper::new(
            vec![skip("intro", 0, 10_000), skip("sponsor", 10_000, 20_000)],
            Some(600_000),
        );
        assert_eq!(skipper.len(), 1);
        assert_eq!(skipper.skip_target(5_000).expect("inside").to_ms, 20_000);
    }

    #[test]
    fn segments_are_normalized_regardless_of_input_order() {
        let skipper = SegmentSkipper::new(
            vec![
                skip("outro", 500_000, 540_000),
                skip("sponsor", 10_000, 30_000),
                skip("intro", 0, 5_000),
            ],
            Some(600_000),
        );
        let starts: Vec<u64> = skipper.segments().iter().map(|s| s.start_ms).collect();
        assert_eq!(starts, vec![0, 10_000, 500_000]);
    }

    #[test]
    fn malformed_segments_are_dropped_rather_than_clamped() {
        // Community submissions are untrusted input; a reversed or empty range carries no
        // information, and a zero-length skip would fire repeatedly at one position.
        let skipper = SegmentSkipper::new(
            vec![
                skip("reversed", 30_000, 10_000),
                skip("empty", 10_000, 10_000),
                skip("valid", 60_000, 90_000),
            ],
            Some(600_000),
        );
        assert_eq!(skipper.len(), 1);
        assert_eq!(skipper.segments()[0].category, "valid");
    }

    #[test]
    fn a_segment_shorter_than_the_floor_is_not_worth_skipping() {
        let skipper = SegmentSkipper::new(
            vec![skip("blip", 10_000, 10_000 + MIN_SKIPPABLE_MS - 1)],
            Some(600_000),
        );
        assert!(skipper.is_empty(), "the seek would cost more than it saves");
    }

    #[test]
    fn segments_are_clamped_to_the_video_duration() {
        let skipper = SegmentSkipper::new(vec![skip("outro", 550_000, 999_999)], Some(600_000));
        assert_eq!(skipper.segments()[0].end_ms, 600_000);
    }

    #[test]
    fn a_segment_entirely_past_the_end_is_dropped() {
        let skipper = SegmentSkipper::new(vec![skip("bogus", 700_000, 800_000)], Some(600_000));
        assert!(skipper.is_empty());
    }

    #[test]
    fn an_unknown_duration_leaves_segments_unclamped() {
        // Live content has no duration; segments must still work.
        let skipper = SegmentSkipper::new(vec![skip("sponsor", 10_000, 30_000)], None);
        assert_eq!(skipper.len(), 1);
        assert_eq!(skipper.segments()[0].end_ms, 30_000);
    }

    #[test]
    fn points_of_interest_and_chapters_are_never_skipped() {
        // Skipping *to* the highlight of a video the user chose to open would be hostile.
        let skipper = SegmentSkipper::new(
            vec![
                Segment {
                    category: "poi_highlight".to_owned(),
                    start_ms: 10_000,
                    end_ms: 11_000,
                    action: SegmentAction::Poi,
                },
                Segment {
                    category: "chapter".to_owned(),
                    start_ms: 0,
                    end_ms: 60_000,
                    action: SegmentAction::Chapter,
                },
            ],
            Some(600_000),
        );
        assert!(skipper.is_empty());
        assert_eq!(skipper.markers().len(), 2);
        assert!(skipper.skip_target(10_500).is_none());
    }

    #[test]
    fn category_filtering_applies_the_users_choice() {
        let skipper = SegmentSkipper::new(
            vec![
                skip("sponsor", 10_000, 30_000),
                skip("intro", 40_000, 60_000),
            ],
            Some(600_000),
        )
        .with_categories(&["sponsor".to_owned()]);

        assert_eq!(skipper.len(), 1);
        assert!(skipper.skip_target(15_000).is_some());
        assert!(skipper.skip_target(45_000).is_none());
    }

    #[test]
    fn the_next_segment_supports_a_manual_skip_affordance() {
        let skipper = SegmentSkipper::new(
            vec![
                skip("sponsor", 10_000, 30_000),
                skip("outro", 500_000, 540_000),
            ],
            Some(600_000),
        );
        assert_eq!(
            skipper.next_segment(0).expect("a segment ahead").start_ms,
            10_000
        );
        assert_eq!(
            skipper
                .next_segment(100_000)
                .expect("a segment ahead")
                .start_ms,
            500_000
        );
        assert!(skipper.next_segment(550_000).is_none());
    }

    #[test]
    fn total_skippable_time_sums_the_merged_set() {
        let skipper = SegmentSkipper::new(
            vec![
                skip("sponsor", 10_000, 30_000),
                skip("selfpromo", 25_000, 45_000),
                skip("outro", 500_000, 540_000),
            ],
            Some(600_000),
        );
        // Merged: 10s–45s (35s) plus 500s–540s (40s).
        assert_eq!(skipper.total_skippable_ms(), 35_000 + 40_000);
    }

    #[test]
    fn an_empty_skipper_is_safe_to_query() {
        let skipper = SegmentSkipper::empty();
        assert!(skipper.is_empty());
        assert!(skipper.skip_target(0).is_none());
        assert!(skipper.next_segment(0).is_none());
        assert_eq!(skipper.total_skippable_ms(), 0);
    }

    #[test]
    fn a_segment_covering_the_whole_video_still_terminates() {
        // Degenerate but real: a mis-submitted segment spanning everything must not loop.
        let skipper = SegmentSkipper::new(vec![skip("sponsor", 0, 600_000)], Some(600_000));
        let target = skipper.skip_target(0).expect("inside");
        assert_eq!(target.to_ms, 600_000);
        assert!(
            skipper.skip_target(target.to_ms).is_none(),
            "the landing position must not be inside a segment again"
        );
    }
}

/// A validated segment category slug.
///
/// Categories arrive from community submissions and from user-authored rules, so they are
/// untrusted text. Validating once at construction means every later comparison is a plain string
/// compare, and a malformed category is rejected at the rule-set boundary rather than silently
/// never matching.
///
/// The alphabet is deliberately narrow — lowercase ASCII, digits and underscore — which covers
/// every category the upstream vocabulary uses (`sponsor`, `selfpromo`, `music_offtopic`,
/// `poi_highlight`) while leaving no room for a pattern that could be mistaken for something else.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentCategory(String);

impl SegmentCategory {
    /// Longest accepted category.
    pub const MAX_LEN: usize = 64;

    /// Validates and normalizes a category slug.
    ///
    /// Input is lowercased first, so `Sponsor` and `sponsor` are the same category rather than two.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::InvalidSegment`] if the slug is empty, longer than
    /// [`SegmentCategory::MAX_LEN`], or contains a character outside the accepted alphabet.
    pub fn new(raw: &str) -> crate::error::FilterResult<Self> {
        use crate::error::{FilterError, SegmentProblem};

        let normalized = raw.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err(FilterError::InvalidSegment {
                problem: SegmentProblem::EmptyCategory,
            });
        }
        if normalized.len() > Self::MAX_LEN {
            return Err(FilterError::InvalidSegment {
                problem: SegmentProblem::CategoryTooLong,
            });
        }
        if !normalized
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(FilterError::InvalidSegment {
                problem: SegmentProblem::InvalidCharacter,
            });
        }
        Ok(Self(normalized))
    }

    /// The normalized slug.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SegmentCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod category_tests {
    use super::*;

    #[test]
    fn accepts_the_upstream_vocabulary() {
        for slug in [
            "sponsor",
            "selfpromo",
            "interaction",
            "intro",
            "outro",
            "preview",
            "music_offtopic",
            "filler",
            "poi_highlight",
            "chapter",
            "hook",
        ] {
            assert!(
                SegmentCategory::new(slug).is_ok(),
                "{slug} should be accepted"
            );
        }
    }

    #[test]
    fn normalizes_case_and_surrounding_space() {
        assert_eq!(
            SegmentCategory::new("  Sponsor ").unwrap().as_str(),
            "sponsor"
        );
        assert_eq!(
            SegmentCategory::new("SELFPROMO").unwrap().as_str(),
            "selfpromo"
        );
    }

    #[test]
    fn rejects_empty_and_whitespace_only() {
        assert!(SegmentCategory::new("").is_err());
        assert!(SegmentCategory::new("   ").is_err());
    }

    #[test]
    fn rejects_an_overlong_slug() {
        assert!(SegmentCategory::new(&"a".repeat(SegmentCategory::MAX_LEN + 1)).is_err());
        assert!(SegmentCategory::new(&"a".repeat(SegmentCategory::MAX_LEN)).is_ok());
    }

    #[test]
    fn rejects_characters_outside_the_alphabet() {
        // Anything that could be mistaken for a pattern, a path or a separator.
        for hostile in [
            "spon sor",
            "sponsor!",
            "spon-sor",
            "spon.sor",
            "spon/sor",
            "спонсор",
            "spon*",
        ] {
            assert!(
                SegmentCategory::new(hostile).is_err(),
                "{hostile:?} should be rejected"
            );
        }
    }
}
