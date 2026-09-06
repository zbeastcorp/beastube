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
use beastube_core::model::video::{Chapter, LiveStatus, VideoDetails, VideoSummary};
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
        channel_avatar: ThumbnailSet::empty(),
        channel_verified: false,
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
pub(crate) const fn is_verified(verification: Verification) -> bool {
    matches!(verification, Verification::Verified | Verification::Artist)
}

/// Builds watch-page details out of a raw `player` response.
///
/// ## Why this exists
///
/// The typed watch-page parser refuses the whole video when YouTube omits one optional section:
/// it reports `could not find secondary_info` and returns nothing, so the page opens on "BEASTUBE
/// could not read the response". It is not a transient failure and not a broken video — measured
/// against the live service, `LlAyUk-NnUw` fails that parse on every attempt while the player
/// endpoint returns a complete `videoDetails` for the same id, and the Shorts feed asks for one of
/// these per card as it scrolls, so a single unreadable id repeats the error for as long as the
/// user keeps watching.
///
/// What this reads is a flat object of strings rather than a tree of renderers, which is why it is
/// worth falling back to: there is far less of it to drift.
///
/// Returns `None` when even this cannot be read, so the caller can report the original failure
/// rather than inventing a video.
pub(crate) fn details_from_player(json: &str, id: &VideoId) -> Option<VideoDetails> {
    let root = serde_json::from_str::<serde_json::Value>(json).ok()?;
    let details = root.get("videoDetails")?;

    // A video with no title is not a video worth showing; everything else may legitimately be
    // absent and stays absent.
    let title = details.get("title")?.as_str()?.to_owned();

    let thumbnails: ThumbnailSet = details
        .pointer("/thumbnail/thumbnails")
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    Some(Thumbnail::sized(
                        entry.get("url")?.as_str()?.to_owned(),
                        u32::try_from(entry.get("width")?.as_u64()?).ok()?,
                        u32::try_from(entry.get("height")?.as_u64()?).ok()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    // The player response never says whether a video is a Short. The renditions do: short-form is
    // published portrait, so the tallest thumbnail being taller than it is wide is a measurement
    // rather than a guess, and it is the same signal the UI uses to choose a card shape.
    let is_short = thumbnails
        .renditions()
        .iter()
        .max_by_key(|thumbnail| thumbnail.width)
        .is_some_and(|thumbnail| thumbnail.height > thumbnail.width);

    let string_number = |key: &str| -> Option<u64> {
        details
            .get(key)
            .and_then(serde_json::Value::as_str)
            .and_then(|text| text.parse().ok())
    };

    let micro = root.pointer("/microformat/playerMicroformatRenderer");

    Some(VideoDetails {
        summary: VideoSummary {
            id: id.clone(),
            title,
            channel_id: details
                .get("channelId")
                .and_then(serde_json::Value::as_str)
                .and_then(|raw| ChannelId::new(raw.to_owned()).ok()),
            channel_name: details
                .get("author")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned),
            channel_avatar: ThumbnailSet::empty(),
            // The player response carries no verification badge, and a badge this code invented
            // would be a claim about someone's identity.
            channel_verified: false,
            thumbnails: if thumbnails.is_empty() {
                derived_thumbnails(id)
            } else {
                thumbnails
            },
            duration_ms: string_number("lengthSeconds").map(|seconds| seconds * 1000),
            published_at: None,
            published_text: None,
            view_count: string_number("viewCount"),
            live_status: if details
                .get("isLiveContent")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                LiveStatus::Live
            } else {
                LiveStatus::NotLive
            },
            is_short,
        },
        description: details
            .get("shortDescription")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        channel_avatar: ThumbnailSet::empty(),
        channel_subscriber_count: None,
        like_count: None,
        chapters: Vec::new(),
        captions: Vec::new(),
        category: micro
            .and_then(|m| m.get("category"))
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        is_unlisted: micro
            .and_then(|m| m.get("isUnlisted"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        is_age_restricted: false,
    })
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
        channel_avatar: channel
            .as_ref()
            .map(|tag| thumbnails(&tag.avatar))
            .unwrap_or_default(),
        channel_verified: channel
            .as_ref()
            .is_some_and(|tag| is_verified(tag.verification)),
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

    /// A player payload shaped like the live one, trimmed to the fields that are read.
    fn player_payload(title: Option<&str>, portrait: bool) -> String {
        let (w, h) = if portrait { (720, 1280) } else { (1280, 720) };
        let mut details = serde_json::json!({
            "channelId": "UCrBw0aVom9NWx8yF0liPdsQ",
            "author": "Bunviun",
            "lengthSeconds": "16",
            "viewCount": "60581",
            "shortDescription": "Hello! I am Bunviun!",
            "isLiveContent": false,
            "thumbnail": { "thumbnails": [
                { "url": "https://i.ytimg.com/vi/x/1.jpg", "width": w, "height": h },
            ]},
        });
        if let Some(title) = title {
            details["title"] = serde_json::json!(title);
        }
        serde_json::json!({
            "videoDetails": details,
            "microformat": { "playerMicroformatRenderer": {
                "category": "Gaming",
                "isUnlisted": false,
            }},
        })
        .to_string()
    }

    fn an_id() -> VideoId {
        VideoId::new("LlAyUk-NnUw").expect("a valid id")
    }

    #[test]
    fn the_player_payload_supplies_what_the_watch_page_needs() {
        let details = details_from_player(&player_payload(Some("MM2 roles edit"), true), &an_id())
            .expect("a payload with a title is usable");

        assert_eq!(details.summary.title, "MM2 roles edit");
        assert_eq!(details.summary.channel_name.as_deref(), Some("Bunviun"));
        assert_eq!(details.summary.duration_ms, Some(16_000), "seconds become milliseconds");
        assert_eq!(details.summary.view_count, Some(60_581));
        assert_eq!(details.description.as_deref(), Some("Hello! I am Bunviun!"));
        assert_eq!(details.category.as_deref(), Some("Gaming"));
        assert_eq!(details.summary.live_status, LiveStatus::NotLive);
    }

    #[test]
    fn a_portrait_rendition_marks_the_video_short() {
        let short = details_from_player(&player_payload(Some("t"), true), &an_id()).expect("usable");
        assert!(short.summary.is_short, "short-form is published portrait");

        let wide = details_from_player(&player_payload(Some("t"), false), &an_id()).expect("usable");
        assert!(!wide.summary.is_short);
    }

    #[test]
    fn a_payload_without_a_title_is_refused_rather_than_guessed_at() {
        assert!(details_from_player(&player_payload(None, false), &an_id()).is_none());
        assert!(details_from_player("{}", &an_id()).is_none());
        assert!(details_from_player("not json", &an_id()).is_none());
    }

    #[test]
    fn missing_renditions_fall_back_to_the_derived_ones() {
        let json = serde_json::json!({ "videoDetails": { "title": "t" } }).to_string();
        let details = details_from_player(&json, &an_id()).expect("a title is enough");
        assert!(
            !details.summary.thumbnails.is_empty(),
            "a video must not be recorded into history as a grey rectangle"
        );
    }

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

// -------------------------------------------------------------------------------------------
// The watch page's recommendation shelf
//
// The typed extractor parses this shelf into `VideoItem`s that are almost entirely empty: no
// channel, no view count, no date. Measured against the live service on 2026-09-04, every one of
// the twenty related items came back with `channel: None` and `view_count: None`, which is why a
// Home feed built from them showed nothing but titles while search results showed everything.
//
// The response itself carries all of it. YouTube moved this shelf to `lockupViewModel`, the same
// modern renderer the shorts shelf uses and the same one the typed parser does not understand —
// so this reads it directly, exactly as `shorts_from_search` above already does.
// -------------------------------------------------------------------------------------------

/// Reads the recommendation shelf out of a raw `next` response.
pub(crate) fn related_from_next(json: &str) -> Vec<VideoSummary> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };

    let mut lockups = Vec::new();
    collect_by_key(&root, "lockupViewModel", &mut lockups);

    let mut seen = std::collections::HashSet::new();
    lockups
        .into_iter()
        .filter_map(video_from_lockup)
        .filter(|video| seen.insert(video.id.as_str().to_owned()))
        .collect()
}

/// Every `content` string inside `value`, in document order.
fn text_contents(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (name, child) in map {
                if name == "content"
                    && let Some(text) = child.as_str()
                    && !text.trim().is_empty()
                {
                    out.push(text.to_owned());
                } else {
                    text_contents(child, out);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                text_contents(child, out);
            }
        }
        _ => {}
    }
}

/// Reads an image source list into a thumbnail set, keeping dimensions where they are given.
fn sources_to_thumbnails(sources: Option<&serde_json::Value>) -> ThumbnailSet {
    let Some(list) = sources.and_then(serde_json::Value::as_array) else {
        return ThumbnailSet::empty();
    };
    ThumbnailSet::new(
        list.iter()
            .filter_map(|entry| {
                let url = entry.get("url")?.as_str()?;
                match (
                    entry.get("width").and_then(serde_json::Value::as_u64),
                    entry.get("height").and_then(serde_json::Value::as_u64),
                ) {
                    (Some(width), Some(height)) => Some(Thumbnail::sized(
                        url,
                        u32::try_from(width).ok()?,
                        u32::try_from(height).ok()?,
                    )),
                    // A source without dimensions is still a usable picture; the renditions are
                    // listed smallest-first, so position carries the size well enough.
                    _ => Some(Thumbnail::unsized_at(url)),
                }
            })
            .collect(),
    )
}

/// Parses `3:45` or `1:02:03` into milliseconds.
fn parse_duration_text(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    if trimmed.is_empty() || !trimmed.contains(':') {
        return None;
    }
    let mut total: u64 = 0;
    for (index, part) in trimmed.split(':').enumerate() {
        let value: u64 = part.trim().parse().ok()?;
        // Every field after the first is two digits; a larger one means this is not a duration.
        if index > 0 && value > 59 {
            return None;
        }
        total = total.checked_mul(60)?.checked_add(value)?;
    }
    total.checked_mul(1000)
}

/// Whether a metadata part describes *when* rather than *how many*.
///
/// Covers the four shapes the shelf uses: `1d ago`, `Streamed 2mo ago`, `12K watching` for a live
/// item and `Premieres in 2 hours` for a scheduled one. Anything else in that position is the
/// view count.
fn is_time_text(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("ago")
        || lower.contains("streamed")
        || lower.contains("premiere")
        || lower.contains("watching")
        || lower.contains("waiting")
}

/// Builds a summary from one video lockup, or `None` if it is not a video.
fn video_from_lockup(lockup: &serde_json::Value) -> Option<VideoSummary> {
    // Playlists and channels use the same renderer; only a video lockup names a video.
    if let Some(kind) = lockup.get("contentType").and_then(serde_json::Value::as_str)
        && !kind.contains("VIDEO")
    {
        return None;
    }
    let id = VideoId::new(lockup.get("contentId")?.as_str()?).ok()?;

    let metadata = lockup.pointer("/metadata/lockupMetadataViewModel");
    let title = metadata
        .and_then(|block| block.pointer("/title/content"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // A lockup with no title is a shape this parser does not understand, and a card that renders
    // as an empty box is worse than one fewer recommendation.
    if title.is_empty() {
        return None;
    }

    let thumbnails =
        sources_to_thumbnails(lockup.pointer("/contentImage/thumbnailViewModel/image/sources"));

    // The duration sits in the thumbnail's bottom overlay, as the badge text the card draws.
    let mut overlay_text = Vec::new();
    if let Some(overlays) = lockup.pointer("/contentImage/thumbnailViewModel/overlays") {
        text_contents(overlays, &mut overlay_text);
    }
    let duration_ms = overlay_text
        .iter()
        .find_map(|text| parse_duration_text(text));

    // The avatar block also carries the channel identifier, which is what makes the name a link.
    let avatar_block = metadata.and_then(|block| {
        let mut found = Vec::new();
        collect_by_key(block, "decoratedAvatarViewModel", &mut found);
        found.into_iter().next()
    });
    let channel_avatar = sources_to_thumbnails(
        avatar_block.and_then(|block| block.pointer("/avatar/avatarViewModel/image/sources")),
    );
    let channel_id = avatar_block
        .and_then(|block| {
            block.pointer(
                "/rendererContext/commandContext/onTap/innertubeCommand/browseEndpoint/browseId",
            )
        })
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| ChannelId::new(raw).ok());

    // The rows are the channel, then views and date. Read as a flat list and classified by what
    // each string says, because a live item has no date and an upcoming one has no view count —
    // indexing by position would put a date where a count belongs.
    let rows = metadata.and_then(|block| {
        block.pointer("/metadata/contentMetadataViewModel/metadataRows")
    });
    let mut parts = Vec::new();
    if let Some(rows) = rows {
        text_contents(rows, &mut parts);
    }

    // The first row is the channel; everything after it is the count and the date, in that order
    // but not reliably present. Read live, the shelf gives `["MrBeast 2", "24M", "1d ago"]` — note
    // the bare count, with no "views" word at all, which is why this cannot key on that word the
    // way the search mapping can.
    let channel_name = parts.first().cloned();
    let published_text = parts.iter().skip(1).find(|text| is_time_text(text)).cloned();
    let view_count = parts
        .iter()
        .skip(1)
        .filter(|text| !is_time_text(text))
        .find_map(|text| parse_compact_count(text));

    // The verified tick is an attachment run on the channel row, named by its client resource.
    let channel_verified = rows.is_some_and(|rows| {
        let mut names = Vec::new();
        collect_by_key(rows, "imageName", &mut names);
        names
            .into_iter()
            .filter_map(serde_json::Value::as_str)
            .any(|name| name.contains("CHECK_CIRCLE"))
    });

    Some(VideoSummary {
        id,
        title,
        channel_id,
        channel_name,
        channel_avatar,
        channel_verified,
        thumbnails,
        duration_ms,
        // The shelf gives a relative string only. Inventing an absolute date from it would claim a
        // precision the response does not have.
        published_at: None,
        published_text,
        view_count,
        live_status: LiveStatus::NotLive,
        is_short: false,
    })
}

#[cfg(test)]
mod related_tests {
    use super::*;

    /// One lockup in the shape the live `next` response uses, reduced to the fields read.
    fn lockup() -> serde_json::Value {
        serde_json::json!({
            "contentId": "dQw4w9WgXcQ",
            "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
            "contentImage": {
                "thumbnailViewModel": {
                    "image": { "sources": [
                        { "url": "https://i.ytimg.com/vi/x/hq.jpg", "width": 336, "height": 188 }
                    ]},
                    "overlays": [
                        { "thumbnailBottomOverlayViewModel": { "badges": [
                            { "thumbnailBadgeViewModel": { "text": { "content": "12:34" } } }
                        ]}}
                    ]
                }
            },
            "metadata": { "lockupMetadataViewModel": {
                "title": { "content": "A recommended video" },
                "image": { "decoratedAvatarViewModel": {
                    "avatar": { "avatarViewModel": { "image": { "sources": [
                        { "url": "https://yt3.ggpht.com/avatar=s68", "width": 68, "height": 68 }
                    ]}}},
                    "rendererContext": { "commandContext": { "onTap": { "innertubeCommand": {
                        "browseEndpoint": { "browseId": "UCSfxFZFzcpYMbOB3A1rWHAg" }
                    }}}}
                }},
                "metadata": { "contentMetadataViewModel": { "metadataRows": [
                    { "metadataParts": [ { "text": {
                        "content": "SlayyPop",
                        "attachmentRuns": [ { "element": { "type": { "imageType": { "image": {
                            "sources": [ { "clientResource": { "imageName": "CHECK_CIRCLE_FILLED" } } ]
                        }}}}}]
                    }}]},
                    { "metadataParts": [
                        { "text": { "content": "1.2M views" } },
                        { "text": { "content": "3 days ago" } }
                    ]}
                ]}}
            }}
        })
    }

    #[test]
    fn a_related_lockup_yields_everything_a_card_draws() {
        let summary = video_from_lockup(&lockup()).expect("a video summary");

        assert_eq!(summary.id.as_str(), "dQw4w9WgXcQ");
        assert_eq!(summary.title, "A recommended video");
        assert_eq!(summary.channel_name.as_deref(), Some("SlayyPop"));
        assert_eq!(
            summary.channel_id.as_ref().map(ChannelId::as_str),
            Some("UCSfxFZFzcpYMbOB3A1rWHAg")
        );
        assert_eq!(summary.channel_avatar.len(), 1);
        assert!(summary.channel_verified, "the tick is an attachment run");
        assert_eq!(summary.view_count, Some(1_200_000));
        assert_eq!(summary.published_text.as_deref(), Some("3 days ago"));
        assert_eq!(summary.duration_ms, Some(754_000));
        assert_eq!(summary.thumbnails.len(), 1);
    }

    #[test]
    fn an_unverified_channel_gets_no_tick() {
        let mut json = lockup();
        json["metadata"]["lockupMetadataViewModel"]["metadata"]["contentMetadataViewModel"]
            ["metadataRows"][0]["metadataParts"][0]["text"]
            .as_object_mut()
            .unwrap()
            .remove("attachmentRuns");

        let summary = video_from_lockup(&json).expect("a video summary");
        assert!(!summary.channel_verified);
        assert_eq!(summary.channel_name.as_deref(), Some("SlayyPop"));
    }

    #[test]
    fn a_live_item_has_no_date_and_still_parses() {
        let mut json = lockup();
        json["metadata"]["lockupMetadataViewModel"]["metadata"]["contentMetadataViewModel"]
            ["metadataRows"][1] = serde_json::json!({
            "metadataParts": [ { "text": { "content": "12,345 watching" } } ]
        });

        let summary = video_from_lockup(&json).expect("a video summary");
        // The watcher line takes the slot the date would occupy, which is where YouTube puts it.
        assert_eq!(summary.published_text.as_deref(), Some("12,345 watching"));
        assert_eq!(
            summary.view_count, None,
            "watchers are people now, not total views; reading it as a view count would be a              number the card states and the provider never said"
        );
        assert_eq!(
            summary.channel_name.as_deref(),
            Some("SlayyPop"),
            "the channel must not be mistaken for the watcher line"
        );
    }

    #[test]
    fn playlists_and_titleless_shapes_are_skipped() {
        let mut playlist = lockup();
        playlist["contentType"] = serde_json::json!("LOCKUP_CONTENT_TYPE_PLAYLIST");
        assert!(video_from_lockup(&playlist).is_none());

        let mut untitled = lockup();
        untitled["metadata"]["lockupMetadataViewModel"]["title"]["content"] =
            serde_json::json!("");
        assert!(video_from_lockup(&untitled).is_none());
    }

    #[test]
    fn durations_parse_in_both_shapes_and_reject_anything_else() {
        assert_eq!(parse_duration_text("3:45"), Some(225_000));
        assert_eq!(parse_duration_text("1:02:03"), Some(3_723_000));
        assert_eq!(parse_duration_text("LIVE"), None);
        assert_eq!(parse_duration_text("1.2M views"), None);
        // Minutes cannot exceed 59; a value that does is some other number with a colon in it.
        assert_eq!(parse_duration_text("1:99"), None);
    }

    #[test]
    fn the_whole_shelf_is_read_and_deduplicated() {
        let response = serde_json::json!({
            "contents": { "twoColumnWatchNextResults": { "secondaryResults": { "results": [
                { "lockupViewModel": lockup() },
                { "lockupViewModel": lockup() }
            ]}}}
        });

        let videos = related_from_next(&response.to_string());
        assert_eq!(videos.len(), 1, "the same video must not appear twice");
        assert_eq!(videos[0].channel_name.as_deref(), Some("SlayyPop"));
    }

    #[test]
    fn a_response_that_is_not_json_yields_nothing_rather_than_panicking() {
        assert!(related_from_next("<html>error</html>").is_empty());
    }
}
