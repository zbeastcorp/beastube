//! Video metadata.
//!
//! Split into two tiers on purpose:
//!
//! * [`VideoSummary`] is what a grid card needs. It is small, so a page of 50 results costs little
//!   to transfer over IPC, hold in the L1 cache, or keep in a virtualized list.
//! * [`VideoDetails`] is what the watch page needs, and is fetched only when a video is opened.
//!
//! Loading the heavy shape for list views was the single easiest way to make scrolling janky, so
//! the split is enforced by the type system rather than by convention.

use serde::{Deserialize, Serialize};

use crate::ids::{ChannelId, VideoId};
use crate::model::thumbnail::ThumbnailSet;
use crate::time_util::Timestamp;

/// Live-broadcast state of a video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveStatus {
    /// An ordinary uploaded video.
    #[default]
    NotLive,
    /// Currently broadcasting. Duration is unknown and seeking is limited to the DVR window.
    Live,
    /// Scheduled but not yet started. Not playable.
    Upcoming,
    /// A finished broadcast, now seekable end to end like a normal video.
    WasLive,
}

impl LiveStatus {
    /// Whether media can be requested for this video right now.
    #[must_use]
    pub const fn is_playable(self) -> bool {
        !matches!(self, Self::Upcoming)
    }

    /// Whether the timeline is open-ended, which changes buffering and seek-bar behaviour.
    #[must_use]
    pub const fn is_streaming_now(self) -> bool {
        matches!(self, Self::Live)
    }
}

/// The compact shape used by every list, grid and card surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoSummary {
    /// Provider identifier.
    pub id: VideoId,
    /// Video title, as supplied by the provider.
    ///
    /// Untrusted text: rendered through the UI's text path, never as HTML (§77).
    pub title: String,
    /// Owning channel, when the provider reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<ChannelId>,
    /// Channel display name. Untrusted text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_name: Option<String>,
    /// The channel's avatar renditions, when the provider attaches them to the item.
    ///
    /// Carried on the *video* rather than fetched per card: the provider already sends it with
    /// every search and related result, and a card that had to look it up would be one request per
    /// tile for a picture that arrived with the tile.
    #[serde(default, skip_serializing_if = "ThumbnailSet::is_empty")]
    pub channel_avatar: ThumbnailSet,
    /// Whether the provider marks the channel as verified.
    ///
    /// Only ever `true` when the provider said so. A badge the application invented would be a
    /// claim about someone's identity, which is the last thing to guess at (§131).
    #[serde(default)]
    pub channel_verified: bool,
    /// Available thumbnail renditions.
    #[serde(default, skip_serializing_if = "ThumbnailSet::is_empty")]
    pub thumbnails: ThumbnailSet,
    /// Duration in milliseconds. Absent for live and upcoming content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Publication time, when the provider reports an absolute date.
    ///
    /// Providers often give only a relative string ("3 weeks ago"); adapters convert what they can
    /// and leave this `None` otherwise rather than inventing a precise timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<Timestamp>,
    /// Provider-supplied relative publication text, preserved when no absolute date is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_text: Option<String>,
    /// View count, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_count: Option<u64>,
    /// Live-broadcast state.
    #[serde(default)]
    pub live_status: LiveStatus,
    /// Whether the provider classifies this as a Short.
    #[serde(default)]
    pub is_short: bool,
}

impl VideoSummary {
    /// Minimal summary for a video whose metadata has not been fetched yet.
    ///
    /// Used when the local library holds an identifier whose cached metadata was evicted: the row
    /// still renders with its stored title rather than disappearing from history.
    #[must_use]
    pub fn placeholder(id: VideoId, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            channel_id: None,
            channel_name: None,
            channel_avatar: ThumbnailSet::empty(),
            channel_verified: false,
            thumbnails: ThumbnailSet::empty(),
            duration_ms: None,
            published_at: None,
            published_text: None,
            view_count: None,
            live_status: LiveStatus::NotLive,
            is_short: false,
        }
    }

    /// Whether the item can be opened for playback.
    #[must_use]
    pub const fn is_playable(&self) -> bool {
        self.live_status.is_playable()
    }
}

/// A named position within a video.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chapter {
    /// Chapter title. Untrusted text.
    pub title: String,
    /// Offset from the start of the video, in milliseconds.
    pub start_ms: u64,
    /// Preview image, when the provider supplies one.
    #[serde(default, skip_serializing_if = "ThumbnailSet::is_empty")]
    pub thumbnails: ThumbnailSet,
}

/// A subtitle track offered for a video.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptionTrack {
    /// BCP 47 language tag, e.g. `en`, `pt-BR`.
    pub language_code: String,
    /// Provider-supplied display name, already localized upstream. Rendered as opaque text.
    pub language_name: String,
    /// URL from which the cue file can be fetched.
    pub url: String,
    /// Whether the track was machine-generated, which the UI marks so users can judge accuracy.
    #[serde(default)]
    pub is_auto_generated: bool,
}

/// One of the audio tracks a video offers.
///
/// Videos are increasingly published with the original audio plus dubs in other languages. A
/// video with a single track reports none of these: there is nothing to choose between, and an
/// audio menu holding one entry is a control that cannot do anything (§131).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioTrack {
    /// Provider identifier for the track, e.g. `es.3`. Opaque; used to ask for the track by name.
    pub id: String,
    /// BCP 47 language tag, when the provider reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_code: Option<String>,
    /// Display name, already localized upstream. Rendered as opaque text.
    pub language_name: String,
    /// Whether this is the track the provider serves by default.
    #[serde(default)]
    pub is_default: bool,
    /// Whether this is the video's original audio rather than a dub.
    ///
    /// Worth distinguishing in the menu: "original" tells a viewer which track carries the
    /// performance rather than a translation of it.
    #[serde(default)]
    pub is_original: bool,
}

/// The full shape used by the watch page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoDetails {
    /// Everything a card shows, so watch-page data can seed list caches without a second fetch.
    #[serde(flatten)]
    pub summary: VideoSummary,
    /// Full description. Untrusted text; links are extracted and validated by the UI, never
    /// rendered from provider-supplied HTML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Channel avatar renditions.
    #[serde(default, skip_serializing_if = "ThumbnailSet::is_empty")]
    pub channel_avatar: ThumbnailSet,
    /// Subscriber count of the owning channel, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_subscriber_count: Option<u64>,
    /// Like count, when reported. Dislike counts are not published by the provider and are not
    /// synthesized from third-party sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub like_count: Option<u64>,
    /// Chapters, ascending by start time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chapters: Vec<Chapter>,
    /// Available subtitle tracks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captions: Vec<CaptionTrack>,
    /// Alternative audio tracks, when the video has more than one. Empty when there is no choice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_tracks: Vec<AudioTrack>,
    /// Provider category name, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Whether the provider marks this video as unlisted.
    #[serde(default)]
    pub is_unlisted: bool,
    /// Whether the video is age-restricted, which the playback layer must handle explicitly rather
    /// than failing with an opaque error.
    #[serde(default)]
    pub is_age_restricted: bool,
}

impl VideoDetails {
    /// The chapter containing `position_ms`, if any.
    ///
    /// Assumes chapters are sorted ascending, which adapters guarantee via
    /// [`VideoDetails::sort_chapters`].
    #[must_use]
    pub fn chapter_at(&self, position_ms: u64) -> Option<&Chapter> {
        self.chapters
            .iter()
            .rev()
            .find(|chapter| chapter.start_ms <= position_ms)
    }

    /// Sorts chapters ascending by start time and drops any that begin at or beyond the video
    /// duration.
    ///
    /// Called by adapters after parsing. Chapters derived from description timestamps are
    /// user-authored and routinely out of order or past the end.
    pub fn sort_chapters(&mut self) {
        self.chapters.sort_by_key(|chapter| chapter.start_ms);
        if let Some(duration) = self.summary.duration_ms {
            self.chapters.retain(|chapter| chapter.start_ms < duration);
        }
    }

    /// Caption track best matching `preferred`, falling back to a prefix match on the primary
    /// language subtag, then to any manually authored track, then to any track at all.
    ///
    /// The fallback ladder matters: a user who asked for `pt-BR` is better served by `pt` than by
    /// nothing, and better served by a human `en` track than by a machine-generated one.
    #[must_use]
    pub fn caption_for(&self, preferred: &str) -> Option<&CaptionTrack> {
        let primary = preferred.split('-').next().unwrap_or(preferred);
        self.captions
            .iter()
            .find(|track| track.language_code.eq_ignore_ascii_case(preferred))
            .or_else(|| {
                self.captions.iter().find(|track| {
                    track
                        .language_code
                        .split('-')
                        .next()
                        .is_some_and(|code| code.eq_ignore_ascii_case(primary))
                })
            })
            .or_else(|| self.captions.iter().find(|track| !track.is_auto_generated))
            .or_else(|| self.captions.first())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details() -> VideoDetails {
        VideoDetails {
            summary: VideoSummary {
                duration_ms: Some(600_000),
                ..VideoSummary::placeholder(VideoId::new("dQw4w9WgXcQ").unwrap(), "Test")
            },
            description: None,
            channel_avatar: ThumbnailSet::empty(),
            channel_subscriber_count: None,
            like_count: None,
            audio_tracks: Vec::new(),
            chapters: vec![
                Chapter {
                    title: "Outro".to_owned(),
                    start_ms: 500_000,
                    thumbnails: ThumbnailSet::empty(),
                },
                Chapter {
                    title: "Intro".to_owned(),
                    start_ms: 0,
                    thumbnails: ThumbnailSet::empty(),
                },
                Chapter {
                    title: "Bogus past end".to_owned(),
                    start_ms: 900_000,
                    thumbnails: ThumbnailSet::empty(),
                },
            ],
            captions: vec![],
            category: None,
            is_unlisted: false,
            is_age_restricted: false,
        }
    }

    #[test]
    fn sorting_orders_chapters_and_drops_ones_past_the_end() {
        let mut d = details();
        d.sort_chapters();
        let titles: Vec<_> = d.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["Intro", "Outro"]);
    }

    #[test]
    fn chapter_lookup_finds_the_containing_chapter() {
        let mut d = details();
        d.sort_chapters();
        assert_eq!(d.chapter_at(0).unwrap().title, "Intro");
        assert_eq!(d.chapter_at(499_999).unwrap().title, "Intro");
        assert_eq!(d.chapter_at(500_000).unwrap().title, "Outro");
        assert_eq!(d.chapter_at(599_999).unwrap().title, "Outro");
    }

    #[test]
    fn chapter_lookup_before_the_first_chapter_yields_none() {
        let mut d = details();
        d.chapters = vec![Chapter {
            title: "Later".to_owned(),
            start_ms: 10_000,
            thumbnails: ThumbnailSet::empty(),
        }];
        assert!(d.chapter_at(0).is_none());
        assert!(d.chapter_at(9_999).is_none());
        assert!(d.chapter_at(10_000).is_some());
    }

    #[test]
    fn sorting_without_a_known_duration_keeps_every_chapter() {
        let mut d = details();
        d.summary.duration_ms = None;
        d.sort_chapters();
        assert_eq!(d.chapters.len(), 3);
    }

    #[test]
    fn caption_selection_walks_the_fallback_ladder() {
        let mut d = details();
        d.captions = vec![
            CaptionTrack {
                language_code: "en".to_owned(),
                language_name: "English (auto-generated)".to_owned(),
                url: "https://example.com/en".to_owned(),
                is_auto_generated: true,
            },
            CaptionTrack {
                language_code: "pt".to_owned(),
                language_name: "Portuguese".to_owned(),
                url: "https://example.com/pt".to_owned(),
                is_auto_generated: false,
            },
        ];

        // Exact match wins.
        assert_eq!(d.caption_for("pt").unwrap().language_code, "pt");
        // Region variant falls back to the primary subtag.
        assert_eq!(d.caption_for("pt-BR").unwrap().language_code, "pt");
        // Unknown language prefers a human track over a machine one.
        assert_eq!(d.caption_for("ja").unwrap().language_code, "pt");
    }

    #[test]
    fn caption_selection_returns_none_when_there_are_no_tracks() {
        assert!(details().caption_for("en").is_none());
    }

    #[test]
    fn upcoming_videos_are_not_playable() {
        assert!(!LiveStatus::Upcoming.is_playable());
        assert!(LiveStatus::Live.is_playable());
        assert!(LiveStatus::WasLive.is_playable());
        assert!(LiveStatus::NotLive.is_playable());
        assert!(LiveStatus::Live.is_streaming_now());
        assert!(!LiveStatus::WasLive.is_streaming_now());
    }

    #[test]
    fn details_flattens_the_summary_on_the_wire() {
        let json = serde_json::to_string(&details()).unwrap();
        assert!(json.contains("\"id\":\"dQw4w9WgXcQ\""), "{json}");
        assert!(
            !json.contains("\"summary\""),
            "summary must be flattened: {json}"
        );
        let back: VideoDetails = serde_json::from_str(&json).unwrap();
        assert_eq!(back.summary.id.as_str(), "dQw4w9WgXcQ");
    }
}
