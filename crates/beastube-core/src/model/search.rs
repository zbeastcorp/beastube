//! Search requests and results.
//!
//! Filters are modelled as enums rather than free-form strings so that an unsupported combination
//! is a compile error rather than a silently ignored query parameter. Each provider adapter maps
//! them to its own encoding and reports, through
//! [`crate::model::search::SearchFilters::is_supported_by`]-style capability checks in the provider
//! layer, which filters it can actually honour — the UI must not offer a filter that the active
//! provider ignores.

use serde::{Deserialize, Serialize};

use crate::ids::ChannelId;
use crate::model::channel::ChannelSummary;
use crate::model::playlist::PlaylistSummary;
use crate::model::video::VideoSummary;

/// Which kind of result the user is asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchResultKind {
    /// Any kind, provider-ranked.
    #[default]
    All,
    /// Long-form videos.
    Videos,
    /// Short-form vertical videos.
    Shorts,
    /// Channels.
    Channels,
    /// Playlists.
    Playlists,
    /// Currently broadcasting streams.
    Live,
}

impl SearchResultKind {
    /// Stable identifier for routing, persistence and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Videos => "videos",
            Self::Shorts => "shorts",
            Self::Channels => "channels",
            Self::Playlists => "playlists",
            Self::Live => "live",
        }
    }

    /// Whether video-specific filters (duration, features, sort by upload date) apply.
    ///
    /// Selecting "Channels" must not leave a stale duration filter silently narrowing results.
    #[must_use]
    pub const fn accepts_video_filters(self) -> bool {
        matches!(self, Self::All | Self::Videos | Self::Shorts | Self::Live)
    }
}

/// Recency constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadDateFilter {
    /// No constraint.
    #[default]
    Any,
    /// Uploaded in the last hour.
    LastHour,
    /// Uploaded today.
    Today,
    /// Uploaded this week.
    ThisWeek,
    /// Uploaded this month.
    ThisMonth,
    /// Uploaded this year.
    ThisYear,
}

/// Duration bucket constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoDurationFilter {
    /// No constraint.
    #[default]
    Any,
    /// Under four minutes.
    Short,
    /// Four to twenty minutes.
    Medium,
    /// Over twenty minutes.
    Long,
}

/// Result ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSortOrder {
    /// Provider relevance ranking.
    #[default]
    Relevance,
    /// Newest first.
    UploadDate,
    /// Most viewed first.
    ViewCount,
    /// Highest rated first.
    Rating,
}

/// An optional feature a video must have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoFeatureFilter {
    /// Has subtitles or closed captions.
    Subtitles,
    /// Available at 1080p or above.
    HighDefinition,
    /// Available at 2160p.
    UltraHighDefinition,
    /// Has high dynamic range.
    Hdr,
    /// Is currently live.
    Live,
    /// Is 360-degree or VR content.
    Vr360,
}

/// A complete search request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SearchFilters {
    /// Which result kind to return.
    #[serde(default)]
    pub kind: SearchResultKind,
    /// Recency constraint.
    #[serde(default)]
    pub upload_date: UploadDateFilter,
    /// Duration constraint.
    #[serde(default)]
    pub duration: VideoDurationFilter,
    /// Ordering.
    #[serde(default)]
    pub sort_by: SearchSortOrder,
    /// Required features. Sorted and deduplicated by [`SearchFilters::normalized`] so that two
    /// equivalent requests produce the same cache key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<VideoFeatureFilter>,
    /// Restrict results to one channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<ChannelId>,
}

impl SearchFilters {
    /// Whether every field is at its default, meaning the query needs no filter encoding.
    #[must_use]
    pub fn is_unfiltered(&self) -> bool {
        *self == Self::default()
    }

    /// Canonicalizes the filter set.
    ///
    /// Sorts and deduplicates features, and clears video-only filters when the requested kind does
    /// not accept them. Without this, "channels, sorted by view count, under 4 minutes" would hash
    /// to a different cache key than the identical "channels" request, and the stale duration
    /// filter could reach an adapter that honours it.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.features.sort_unstable();
        self.features.dedup();
        if !self.kind.accepts_video_filters() {
            self.upload_date = UploadDateFilter::Any;
            self.duration = VideoDurationFilter::Any;
            self.features.clear();
            self.sort_by = SearchSortOrder::Relevance;
        }
        self
    }

    /// A stable string suitable for use as a cache key component.
    ///
    /// Built from normalized fields so equivalent requests collide in the cache as intended.
    #[must_use]
    pub fn cache_key(&self) -> String {
        let normalized = self.clone().normalized();
        let features: Vec<&str> = normalized
            .features
            .iter()
            .map(|f| match f {
                VideoFeatureFilter::Subtitles => "sub",
                VideoFeatureFilter::HighDefinition => "hd",
                VideoFeatureFilter::UltraHighDefinition => "uhd",
                VideoFeatureFilter::Hdr => "hdr",
                VideoFeatureFilter::Live => "live",
                VideoFeatureFilter::Vr360 => "vr",
            })
            .collect();
        format!(
            "{}|{:?}|{:?}|{:?}|{}|{}",
            normalized.kind.as_str(),
            normalized.upload_date,
            normalized.duration,
            normalized.sort_by,
            features.join(","),
            normalized
                .channel
                .as_ref()
                .map_or("", crate::ids::ChannelId::as_str)
        )
    }
}

/// One heterogeneous search result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SearchItem {
    /// A video result.
    Video(VideoSummary),
    /// A channel result.
    Channel(ChannelSummary),
    /// A playlist result.
    Playlist(PlaylistSummary),
}

impl SearchItem {
    /// Stable discriminant for logs and diagnostics.
    #[must_use]
    pub const fn kind_str(&self) -> &'static str {
        match self {
            Self::Video(_) => "video",
            Self::Channel(_) => "channel",
            Self::Playlist(_) => "playlist",
        }
    }

    /// The contained video, if this is a video result.
    #[must_use]
    pub const fn as_video(&self) -> Option<&VideoSummary> {
        match self {
            Self::Video(video) => Some(video),
            _ => None,
        }
    }
}

/// A page of search results plus the query that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResults {
    /// The query text this page answers, echoed back so a late response can be discarded when the
    /// user has since typed something else.
    pub query: String,
    /// The normalized filters that produced it.
    pub filters: SearchFilters,
    /// The results.
    pub page: crate::model::Page<SearchItem>,
    /// Provider's estimate of total matches, when reported. Usually approximate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_total: Option<u64>,
    /// A spelling correction the provider applied or suggests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrected_query: Option<String>,
}

/// One autocomplete suggestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    /// Suggested query text. Untrusted text.
    pub text: String,
    /// Whether this came from the user's own local search history rather than the provider.
    ///
    /// Local suggestions are rendered differently and are suppressed entirely in incognito mode.
    #[serde(default)]
    pub from_history: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_sorts_and_deduplicates_features() {
        let filters = SearchFilters {
            features: vec![
                VideoFeatureFilter::Hdr,
                VideoFeatureFilter::Subtitles,
                VideoFeatureFilter::Hdr,
            ],
            ..Default::default()
        }
        .normalized();
        assert_eq!(
            filters.features,
            vec![VideoFeatureFilter::Subtitles, VideoFeatureFilter::Hdr]
        );
    }

    #[test]
    fn normalization_clears_video_filters_for_non_video_kinds() {
        let filters = SearchFilters {
            kind: SearchResultKind::Channels,
            duration: VideoDurationFilter::Long,
            upload_date: UploadDateFilter::ThisWeek,
            sort_by: SearchSortOrder::ViewCount,
            features: vec![VideoFeatureFilter::Hdr],
            channel: None,
        }
        .normalized();

        assert_eq!(filters.duration, VideoDurationFilter::Any);
        assert_eq!(filters.upload_date, UploadDateFilter::Any);
        assert_eq!(filters.sort_by, SearchSortOrder::Relevance);
        assert!(filters.features.is_empty());
        assert_eq!(filters.kind, SearchResultKind::Channels);
    }

    #[test]
    fn normalization_preserves_video_filters_for_video_kinds() {
        let filters = SearchFilters {
            kind: SearchResultKind::Videos,
            duration: VideoDurationFilter::Long,
            ..Default::default()
        }
        .normalized();
        assert_eq!(filters.duration, VideoDurationFilter::Long);
    }

    #[test]
    fn equivalent_requests_share_a_cache_key() {
        let a = SearchFilters {
            kind: SearchResultKind::Channels,
            duration: VideoDurationFilter::Short,
            ..Default::default()
        };
        let b = SearchFilters {
            kind: SearchResultKind::Channels,
            ..Default::default()
        };
        assert_eq!(
            a.cache_key(),
            b.cache_key(),
            "a filter the kind ignores must not fragment the cache"
        );
    }

    #[test]
    fn differing_requests_do_not_share_a_cache_key() {
        let videos = SearchFilters {
            kind: SearchResultKind::Videos,
            ..Default::default()
        };
        let shorts = SearchFilters {
            kind: SearchResultKind::Shorts,
            ..Default::default()
        };
        assert_ne!(videos.cache_key(), shorts.cache_key());

        let scoped = SearchFilters {
            channel: Some(ChannelId::new("UCabc").unwrap()),
            ..Default::default()
        };
        assert_ne!(scoped.cache_key(), SearchFilters::default().cache_key());
    }

    #[test]
    fn feature_order_does_not_affect_the_cache_key() {
        let a = SearchFilters {
            kind: SearchResultKind::Videos,
            features: vec![VideoFeatureFilter::Hdr, VideoFeatureFilter::Subtitles],
            ..Default::default()
        };
        let b = SearchFilters {
            kind: SearchResultKind::Videos,
            features: vec![VideoFeatureFilter::Subtitles, VideoFeatureFilter::Hdr],
            ..Default::default()
        };
        assert_eq!(a.cache_key(), b.cache_key());
    }

    #[test]
    fn default_filters_are_unfiltered() {
        assert!(SearchFilters::default().is_unfiltered());
        assert!(
            !SearchFilters {
                kind: SearchResultKind::Shorts,
                ..Default::default()
            }
            .is_unfiltered()
        );
    }

    #[test]
    fn search_items_are_tagged_on_the_wire() {
        let item = SearchItem::Video(VideoSummary::placeholder(
            crate::ids::VideoId::new("dQw4w9WgXcQ").unwrap(),
            "Test",
        ));
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains(r#""type":"video""#), "{json}");
        let back: SearchItem = serde_json::from_str(&json).unwrap();
        assert!(back.as_video().is_some());
        assert_eq!(back.kind_str(), "video");
    }
}
