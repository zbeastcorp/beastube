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

/// Thumbnail renditions for a video the extractor did not report any for.
///
/// The watch-page payload carries no thumbnail list at all — `rustypipe`'s `VideoDetails` has no
/// such field — so a video opened directly would be recorded into history as a grey rectangle. The
/// image host serves a stable, documented path per video id, and it is the same host the embedded
/// player itself loads its poster frame from, so deriving the URL asks for nothing that is not
/// already being fetched.
///
/// Only `mqdefault` is emitted. It is the one rendition present for every video and genuinely
/// 16:9; `hqdefault` and `sddefault` are 4:3 with black bars baked in, and `maxresdefault` is
/// absent for a large share of videos, so both would trade a grey card for a wrong-looking one.
///
/// The identifier is a validated [`VideoId`], so it cannot introduce a path segment.
pub(crate) fn derived_thumbnails(id: &VideoId) -> ThumbnailSet {
    ThumbnailSet::new(vec![Thumbnail::sized(
        format!("https://i.ytimg.com/vi/{}/mqdefault.jpg", id.as_str()),
        320,
        180,
    )])
}

/// Extracts short-form videos from a raw InnerTube search response.
///
/// ## Why this exists
///
/// The extractor this adapter is built on discards shorts entirely. They arrive as
/// `shortsLockupViewModel` objects inside a shelf renderer that is not one of its known variants,
/// so serde's catch-all arm swallows the whole shelf. Measured against a live `funny #shorts`
/// search: 26 lockups in the response, and zero `videoRenderer` — the typed parser returned nothing
/// at all for a query whose every result was a short.
///
/// So this reads them straight out of the JSON the extractor already fetched. It is the same public
/// endpoint, the same request, the same response; the only difference is that this reads a part of
/// it the typed layer throws away.
///
/// ## Shape
///
/// Walked rather than deserialized into a fixed struct. InnerTube nests these differently depending
/// on where a shelf lands in the response, and a walk that looks for one key by name is far harder
/// to break than a path that has to be right about every level above it.
pub(crate) fn shorts_from_search(json: &str) -> Vec<VideoSummary> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };

    let mut lockups = Vec::new();
    collect_by_key(&root, "shortsLockupViewModel", &mut lockups);

    let mut seen = std::collections::HashSet::new();
    lockups
        .into_iter()
        .filter_map(short_from_lockup)
        .filter(|video| seen.insert(video.id.as_str().to_owned()))
        .collect()
}

/// Depth-first walk collecting every value stored under `key`.
fn collect_by_key<'a>(value: &'a serde_json::Value, key: &str, out: &mut Vec<&'a serde_json::Value>) {
    match value {
        serde_json::Value::Object(map) => {
            for (name, child) in map {
                if name == key {
                    out.push(child);
                } else {
                    collect_by_key(child, key, out);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_by_key(child, key, out);
            }
        }
        _ => {}
    }
}

/// Builds a summary from one lockup, or `None` if it carries no usable identifier.
fn short_from_lockup(lockup: &serde_json::Value) -> Option<VideoSummary> {
    let endpoint = lockup.pointer("/onTap/innertubeCommand/reelWatchEndpoint")?;
    let id = VideoId::new(endpoint.get("videoId")?.as_str()?).ok()?;

    // The overlay text is what the card shows; the accessibility text is the same title with the
    // view count appended, so it is only a fallback.
    let title = lockup
        .pointer("/overlayMetadata/primaryText/content")
        .and_then(serde_json::Value::as_str)
        .or_else(|| lockup.get("accessibilityText").and_then(serde_json::Value::as_str))
        .unwrap_or_default()
        .to_owned();

    let thumbnails = endpoint
        .pointer("/thumbnail/thumbnails")
        .and_then(serde_json::Value::as_array)
        .map_or_else(ThumbnailSet::empty, |list| {
            ThumbnailSet::new(
                list.iter()
                    .filter_map(|entry| {
                        let url = entry.get("url")?.as_str()?;
                        let width = u32::try_from(entry.get("width")?.as_u64()?).ok()?;
                        let height = u32::try_from(entry.get("height")?.as_u64()?).ok()?;
                        Some(Thumbnail::sized(url, width, height))
                    })
                    .collect(),
            )
        });

    let view_count = lockup
        .pointer("/overlayMetadata/secondaryText/content")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_compact_count);

    Some(VideoSummary {
        id,
        title,
        channel_id: None,
        // The lockup carries no channel. Absent rather than invented: the card simply shows the
        // title and the view count, which is what YouTube's own shorts shelf shows.
        channel_name: None,
        thumbnails,
        duration_ms: None,
        published_at: None,
        published_text: None,
        view_count,
        live_status: LiveStatus::NotLive,
        is_short: true,
    })
}

/// Parses `"131M views"` into a number.
///
/// Approximate by construction — the provider rounds it before it is ever sent — so this recovers
/// the same approximation rather than pretending to a precision the source does not have.
fn parse_compact_count(text: &str) -> Option<u64> {
    /// A view count beyond this is not a real figure; the bound keeps the cast defined without
    /// needing an exact `u64::MAX` an `f64` cannot represent anyway.
    const CEILING: f64 = 1e15;

    let token = text.split_whitespace().next()?;
    let (digits, multiplier) = match token.chars().last()? {
        'K' | 'k' => (&token[..token.len() - 1], 1_000_f64),
        'M' | 'm' => (&token[..token.len() - 1], 1_000_000_f64),
        'B' | 'b' => (&token[..token.len() - 1], 1_000_000_000_f64),
        _ => (token, 1_f64),
    };
    let value: f64 = digits.replace(',', "").parse().ok()?;
    let scaled = value * multiplier;
    // Rejected rather than saturated: a non-finite or negative view count is a parse that went
    // wrong, and a wrong number on a card is worse than no number.
    if !scaled.is_finite() || scaled < 0.0 || scaled > CEILING {
        return None;
    }
    // Truncation is the point — the source is already a rounded figure like "131M".
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(scaled as u64)
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
    fn a_derived_thumbnail_names_the_video_and_nothing_else() {
        // The watch-page payload carries no thumbnails, so this is what history rows get. A
        // validated id cannot introduce a path segment, and the assertion pins that.
        let id = VideoId::new("dQw4w9WgXcQ").expect("valid");
        let set = derived_thumbnails(&id);
        let best = set.largest().expect("one rendition");
        assert_eq!(best.url, "https://i.ytimg.com/vi/dQw4w9WgXcQ/mqdefault.jpg");
        assert_eq!((best.width, best.height), (Some(320), Some(180)));
        assert_eq!(set.len(), 1, "one rendition, the only universally present one");
    }

    #[test]
    fn compact_view_counts_parse_the_way_the_provider_writes_them() {
        assert_eq!(parse_compact_count("131M views"), Some(131_000_000));
        assert_eq!(parse_compact_count("2.4K views"), Some(2_400));
        assert_eq!(parse_compact_count("1.2B views"), Some(1_200_000_000));
        assert_eq!(parse_compact_count("874 views"), Some(874));
        assert_eq!(parse_compact_count("1,024 views"), Some(1_024));
    }

    #[test]
    fn unparseable_counts_are_absent_rather_than_wrong() {
        // A wrong number on a card is worse than no number.
        assert_eq!(parse_compact_count(""), None);
        assert_eq!(parse_compact_count("lots of views"), None);
        assert_eq!(parse_compact_count("-5 views"), None);
    }

    #[test]
    fn a_shorts_lockup_becomes_a_short() {
        // Shaped from a real response: the id and thumbnail live on the reel endpoint, the title
        // and view count on the overlay.
        let json = serde_json::json!({
            "contents": [{ "shortsLockupViewModel": {
                "accessibilityText": "fallback title",
                "onTap": { "innertubeCommand": { "reelWatchEndpoint": {
                    "videoId": "iuef391OhRU",
                    "thumbnail": { "thumbnails": [
                        { "url": "https://i.ytimg.com/vi/iuef391OhRU/hq.jpg", "width": 405, "height": 720 }
                    ]}
                }}},
                "overlayMetadata": {
                    "primaryText": { "content": "South indian #shorts" },
                    "secondaryText": { "content": "131M views" }
                }
            }}]
        })
        .to_string();

        let shorts = shorts_from_search(&json);
        assert_eq!(shorts.len(), 1);
        let short = &shorts[0];
        assert_eq!(short.id.as_str(), "iuef391OhRU");
        assert_eq!(short.title, "South indian #shorts");
        assert_eq!(short.view_count, Some(131_000_000));
        assert!(short.is_short, "everything in a shorts shelf is short-form");
        assert_eq!(short.thumbnails.len(), 1);
    }

    #[test]
    fn a_lockup_without_an_identifier_is_skipped_not_faked() {
        let json = serde_json::json!({ "x": { "shortsLockupViewModel": { "accessibilityText": "t" } } })
            .to_string();
        assert!(shorts_from_search(&json).is_empty());
        assert!(shorts_from_search("not json").is_empty());
    }

    #[test]
    fn the_same_short_in_two_shelves_appears_once() {
        let lockup = serde_json::json!({ "shortsLockupViewModel": {
            "onTap": { "innertubeCommand": { "reelWatchEndpoint": { "videoId": "iuef391OhRU" }}},
            "overlayMetadata": { "primaryText": { "content": "t" } }
        }});
        let json = serde_json::json!({ "a": [lockup.clone()], "b": [lockup] }).to_string();
        assert_eq!(shorts_from_search(&json).len(), 1);
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
