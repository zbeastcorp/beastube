//! Translation from the extractor's types into the domain model.
//!
//! This module is the entire blast radius of an upstream schema change. Everything above the
//! provider layer speaks [`beastube_core::model`]; nothing above it names `rustypipe`, YouTube, or
//! any wire shape (§118).
//!
//! Two rules run throughout:
//!
//! * **A malformed item is dropped, not fatal.** One unparseable result must not fail a page of
//!   twenty. Identifiers are re-validated here because upstream types are plain `String`, so a
//!   drifted response cannot smuggle a hostile id into a cache path or a URL.
//! * **Absent stays absent.** Where the extractor cannot determine a value it yields `None`, and
//!   that is carried through rather than defaulted to zero — the UI hides the field instead of
//!   asserting "0 views".

use beastube_core::ids::{ChannelId, PlaylistId, VideoId};
use beastube_core::model::SearchItem;
use beastube_core::model::channel::ChannelSummary;
use beastube_core::model::playlist::PlaylistSummary;
use beastube_core::model::thumbnail::{Thumbnail, ThumbnailSet};
use beastube_core::model::video::{Chapter, LiveStatus, VideoSummary};
use beastube_core::time_util::Timestamp;
use rustypipe::model::{
    ChannelItem, ChannelTag, PlaylistItem, Thumbnail as YtThumbnail, Verification, VideoItem,
    YouTubeItem,
};

/// Converts a thumbnail list, preserving dimensions so the UI can pick a rendition by width.
pub(crate) fn thumbnails(source: &[YtThumbnail]) -> ThumbnailSet {
    source
        .iter()
        .map(|thumbnail| Thumbnail::sized(thumbnail.url.clone(), thumbnail.width, thumbnail.height))
        .collect()
}

/// Converts a publication date into the domain timestamp.
fn published_at(date: Option<time::OffsetDateTime>) -> Option<Timestamp> {
    date.map(Timestamp::from)
}

/// Whether the extractor marked the channel as verified.
///
/// The artist badge is treated as verification too: both mean "the provider vouches for this
/// identity", which is the only claim the UI makes.
const fn is_verified(verification: Verification) -> bool {
    matches!(verification, Verification::Verified | Verification::Artist)
}

/// Converts a video item, returning `None` if its identifier fails validation.
pub(crate) fn video_summary(item: VideoItem) -> Option<VideoSummary> {
    let id = VideoId::new(item.id).ok()?;
    let channel = item.channel;

    Some(VideoSummary {
        id,
        title: item.name,
        channel_id: channel
            .as_ref()
            .and_then(|tag| ChannelId::new(tag.id.clone()).ok()),
        channel_name: channel.as_ref().map(|tag| tag.name.clone()),
        thumbnails: thumbnails(&item.thumbnail),
        // The extractor reports whole seconds; the domain model is milliseconds throughout.
        duration_ms: item.duration.map(|seconds| u64::from(seconds) * 1000),
        published_at: published_at(item.publish_date),
        published_text: item.publish_date_txt,
        view_count: item.view_count,
        live_status: if item.is_live {
            LiveStatus::Live
        } else {
            LiveStatus::NotLive
        },
        is_short: item.is_short,
    })
}

/// Converts a channel item, returning `None` if its identifier fails validation.
pub(crate) fn channel_summary(item: ChannelItem) -> Option<ChannelSummary> {
    Some(ChannelSummary {
        id: ChannelId::new(item.id).ok()?,
        name: item.name,
        avatar: thumbnails(&item.avatar),
        subscriber_count: item.subscriber_count,
        // Stored without the leading `@`; the UI adds it back when displaying.
        handle: item
            .handle
            .map(|handle| handle.trim_start_matches('@').to_owned()),
        is_verified: is_verified(item.verification),
    })
}

/// Converts a channel tag (the compact form embedded in other responses).
///
/// Used by the channel surfaces that land next; kept here because it belongs with the other
/// mappings rather than being rediscovered later.
#[allow(dead_code)]
pub(crate) fn channel_tag(tag: ChannelTag) -> Option<ChannelSummary> {
    Some(ChannelSummary {
        id: ChannelId::new(tag.id).ok()?,
        name: tag.name,
        avatar: thumbnails(&tag.avatar),
        subscriber_count: tag.subscriber_count,
        handle: None,
        is_verified: is_verified(tag.verification),
    })
}

/// Converts a playlist item, returning `None` if its identifier fails validation.
pub(crate) fn playlist_summary(item: PlaylistItem) -> Option<PlaylistSummary> {
    let channel = item.channel;
    Some(PlaylistSummary {
        id: PlaylistId::new(item.id).ok()?,
        title: item.name,
        channel_id: channel
            .as_ref()
            .and_then(|tag| ChannelId::new(tag.id.clone()).ok()),
        channel_name: channel.as_ref().map(|tag| tag.name.clone()),
        thumbnails: thumbnails(&item.thumbnail),
        video_count: item.video_count,
    })
}

/// Converts a heterogeneous search result, dropping kinds the domain model does not carry.
///
/// Music-specific kinds (tracks, albums, artists) are deliberately dropped rather than coerced:
/// presenting an album as a playlist would produce a card that navigates nowhere useful.
pub(crate) fn search_item(item: YouTubeItem) -> Option<SearchItem> {
    match item {
        YouTubeItem::Video(video) => video_summary(video).map(SearchItem::Video),
        YouTubeItem::Channel(channel) => channel_summary(channel).map(SearchItem::Channel),
        YouTubeItem::Playlist(playlist) => playlist_summary(playlist).map(SearchItem::Playlist),
    }
}

/// Converts chapters, sorted ascending and with any beyond the duration dropped by the caller.
pub(crate) fn chapters(source: Vec<rustypipe::model::Chapter>) -> Vec<Chapter> {
    source
        .into_iter()
        .map(|chapter| Chapter {
            title: chapter.name,
            start_ms: u64::from(chapter.position) * 1000,
            thumbnails: thumbnails(&chapter.thumbnail),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an upstream item by deserializing it.
    ///
    /// The extractor's model types are `#[non_exhaustive]`, so a test cannot name their fields.
    /// Going through `serde` is not merely a workaround: it pins the actual wire shape, so a field
    /// rename upstream fails these tests rather than silently changing behaviour.
    fn from_json<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
        serde_json::from_value(value).expect("fixture matches the upstream shape")
    }

    fn video_json(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "name": "A video",
            "duration": 212,
            "thumbnail": [
                { "url": "https://i.ytimg.com/vi/x/hq.jpg", "width": 480, "height": 360 }
            ],
            "channel": {
                "id": "UCuAXFkgsw1L7xaCfnd5JJOw",
                "name": "A channel",
                "avatar": [],
                "verification": "verified",
                "subscriber_count": 1000
            },
            "publish_date": null,
            "publish_date_txt": "3 weeks ago",
            "view_count": 42,
            "is_live": false,
            "is_short": false,
            "is_upcoming": false,
            "short_description": null
        })
    }

    #[test]
    fn a_video_maps_with_seconds_converted_to_milliseconds() {
        let summary = video_summary(from_json(video_json("dQw4w9WgXcQ"))).expect("maps");
        assert_eq!(summary.id.as_str(), "dQw4w9WgXcQ");
        assert_eq!(
            summary.duration_ms,
            Some(212_000),
            "seconds upstream, millis in the model"
        );
        assert_eq!(summary.view_count, Some(42));
        assert_eq!(summary.channel_name.as_deref(), Some("A channel"));
        assert_eq!(summary.live_status, LiveStatus::NotLive);
        assert_eq!(summary.published_text.as_deref(), Some("3 weeks ago"));
    }

    #[test]
    fn a_hostile_identifier_drops_the_item_rather_than_propagating() {
        // Upstream ids are plain strings; a drifted response must never smuggle one into a cache
        // path or a URL.
        let long = "a".repeat(200);
        for hostile in [
            "../../etc/passwd",
            "a/b",
            "",
            "id with spaces",
            long.as_str(),
        ] {
            assert!(
                video_summary(from_json(video_json(hostile))).is_none(),
                "{hostile:?} should have been dropped"
            );
        }
    }

    #[test]
    fn an_unparseable_channel_id_leaves_the_video_usable() {
        // Losing the channel link is a better outcome than losing the video.
        let mut json = video_json("dQw4w9WgXcQ");
        json["channel"]["id"] = serde_json::json!("not a valid id");

        let summary = video_summary(from_json(json)).expect("the video still maps");
        assert_eq!(summary.channel_id, None);
        assert_eq!(
            summary.channel_name.as_deref(),
            Some("A channel"),
            "the name is still worth showing"
        );
    }

    #[test]
    fn a_livestream_maps_without_a_duration() {
        let mut json = video_json("dQw4w9WgXcQ");
        json["is_live"] = serde_json::json!(true);
        json["duration"] = serde_json::Value::Null;

        let summary = video_summary(from_json(json)).expect("maps");
        assert_eq!(summary.live_status, LiveStatus::Live);
        assert_eq!(summary.duration_ms, None);
    }

    #[test]
    fn absent_values_stay_absent_rather_than_becoming_zero() {
        let mut json = video_json("dQw4w9WgXcQ");
        json["view_count"] = serde_json::Value::Null;
        json["duration"] = serde_json::Value::Null;

        let summary = video_summary(from_json(json)).expect("maps");
        assert_eq!(
            summary.view_count, None,
            "the UI hides it rather than showing 0"
        );
        assert_eq!(summary.duration_ms, None);
    }

    #[test]
    fn a_short_is_marked_as_one() {
        let mut json = video_json("dQw4w9WgXcQ");
        json["is_short"] = serde_json::json!(true);
        assert!(video_summary(from_json(json)).expect("maps").is_short);
    }

    #[test]
    fn thumbnails_preserve_dimensions_for_rendition_selection() {
        let set = thumbnails(&[
            from_json(serde_json::json!({
                "url": "https://example.com/small.jpg", "width": 120, "height": 90
            })),
            from_json(serde_json::json!({
                "url": "https://example.com/large.jpg", "width": 1280, "height": 720
            })),
        ]);
        assert_eq!(set.len(), 2);
        assert_eq!(set.best_for_width(320).and_then(|t| t.width), Some(1280));
        assert_eq!(set.smallest().and_then(|t| t.width), Some(120));
    }

    #[test]
    fn a_handle_loses_its_at_prefix_for_storage() {
        let summary: beastube_core::model::channel::ChannelSummary =
            channel_summary(from_json(serde_json::json!({
                "id": "UCuAXFkgsw1L7xaCfnd5JJOw",
                "name": "A channel",
                "handle": "@example",
                "avatar": [],
                "verification": "none",
                "subscriber_count": null,
                "short_description": ""
            })))
            .expect("maps");

        assert_eq!(summary.handle.as_deref(), Some("example"));
        assert_eq!(summary.display_handle().as_deref(), Some("@example"));
        assert!(!summary.is_verified);
    }

    #[test]
    fn an_artist_badge_counts_as_verification() {
        let summary = channel_tag(from_json(serde_json::json!({
            "id": "UCuAXFkgsw1L7xaCfnd5JJOw",
            "name": "An artist",
            "avatar": [],
            "verification": "artist",
            "subscriber_count": null
        })))
        .expect("maps");
        assert!(
            summary.is_verified,
            "an artist badge is still the provider vouching"
        );
    }

    #[test]
    fn a_playlist_maps_with_its_owning_channel() {
        let summary = playlist_summary(from_json(serde_json::json!({
            "id": "PLrAXtmRdnEQy6nuLMfO6uKk3",
            "name": "A playlist",
            "thumbnail": [],
            "channel": {
                "id": "UCuAXFkgsw1L7xaCfnd5JJOw",
                "name": "A channel",
                "avatar": [],
                "verification": "none",
                "subscriber_count": null
            },
            "video_count": 12
        })))
        .expect("maps");

        assert_eq!(summary.title, "A playlist");
        assert_eq!(summary.video_count, Some(12));
        assert_eq!(summary.channel_name.as_deref(), Some("A channel"));
    }

    #[test]
    fn a_video_search_item_maps_to_a_video() {
        let item = search_item(from_json(serde_json::json!({
            "Video": {
            "id": "dQw4w9WgXcQ",
            "name": "A video",
            "duration": 212,
            "thumbnail": [],
            "channel": null,
            "publish_date": null,
            "publish_date_txt": null,
            "view_count": null,
            "is_live": false,
            "is_short": false,
            "is_upcoming": false,
            "short_description": null
            }
        })));
        assert!(matches!(item, Some(SearchItem::Video(_))));
    }
}
