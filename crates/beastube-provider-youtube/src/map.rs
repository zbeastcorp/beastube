//! Translation from the extractor's types into the domain model.
//!
//! This module is the entire blast radius of an upstream schema change. Everything above the
//! provider layer speaks [`beastube_core::model`]; nothing above it names `rustypipe`, YouTube, or
//! any wire shape.
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
use beastube_core::model::channel::{ChannelLink, ChannelSummary};
use beastube_core::model::playlist::PlaylistSummary;
use beastube_core::model::thumbnail::{Thumbnail, ThumbnailSet};
use beastube_core::model::video::{
    AudioTrack, CaptionTrack, Chapter, Cue, LiveStatus, VideoDetails, VideoSummary,
};
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
/// The extractor this adapter is built on discards shorts entirely. They arrive as
/// `shortsLockupViewModel` objects inside a shelf renderer that is not one of its known variants,
/// so serde's catch-all arm swallows the whole shelf. Measured against a live `funny #shorts`
/// search: 26 lockups in the response, and zero `videoRenderer` — the typed parser returned nothing
/// at all for a query whose every result was a short. A channel's Shorts tab behaves the same way:
/// 48 lockups in the response, 0 items out of the typed parser.
///
/// Shared by both, because it does not care which endpoint produced the JSON — it looks for one
/// key by name, wherever that key happens to sit.
///
/// So this reads them straight out of the JSON the extractor already fetched. It is the same public
/// endpoint, the same request, the same response; the only difference is that this reads a part of
/// it the typed layer throws away.
///
/// Walked rather than deserialized into a fixed struct. InnerTube nests these differently depending
/// on where a shelf lands in the response, and a walk that looks for one key by name is far harder
/// to break than a path that has to be right about every level above it.
pub(crate) fn shorts_from_json(json: &str) -> Vec<VideoSummary> {
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

/// Reads every video out of a response built from the **legacy** card renderers.
///
/// The provider is midway through replacing one card shape with another and different surfaces are
/// at different points in that migration. A channel grid is `lockupViewModel`; the category hubs
/// behind Explore still send `videoRenderer` and `gridVideoRenderer`, the shape search uses.
/// Measured against the live News hub: 183 legacy cards, zero lockups.
///
/// The legacy shape is the richer of the two, and worth reading on its own terms rather than
/// flattening into the newer one: it carries an **exact** view count ("1,740 views") where a lockup
/// only ever gives a rounded "1.7K", and it names the uploader and their channel id on every card.
///
/// Items that are not playable videos are dropped rather than mapped: a shelf on these pages also
/// carries channels and playlists, and a card with no video id is not a video.
pub(crate) fn videos_from_renderers(json: &str) -> Vec<VideoSummary> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };

    let mut cards = Vec::new();
    collect_by_key(&root, "videoRenderer", &mut cards);
    collect_by_key(&root, "gridVideoRenderer", &mut cards);

    let mut seen = std::collections::HashSet::new();
    cards
        .into_iter()
        .filter_map(video_from_renderer)
        .filter(|video| seen.insert(video.id.as_str().to_owned()))
        .collect()
}

/// Builds a summary from one legacy card, or `None` if it is not a usable video.
fn video_from_renderer(card: &serde_json::Value) -> Option<VideoSummary> {
    let id = VideoId::new(card.get("videoId")?.as_str()?).ok()?;

    // Titles arrive as runs on every card measured (183 of 183), but `simpleText` is the older
    // spelling of the same field and costs one line to accept.
    let title = card
        .pointer("/title/runs/0/text")
        .or_else(|| card.pointer("/title/simpleText"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if title.is_empty() {
        return None;
    }

    // Live is declared by the overlay's style, not inferred from a missing duration. Both signals
    // agree on 23 of the News hub's cards, and on one more they do not: a stream that is live *and*
    // reports elapsed time. Reading the style gets that card right.
    let live = {
        let mut overlays = Vec::new();
        collect_by_key(card, "thumbnailOverlayTimeStatusRenderer", &mut overlays);
        overlays.iter().any(|overlay| {
            overlay
                .get("style")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|style| style == "LIVE")
        })
    };

    let duration_ms = card
        .pointer("/lengthText/simpleText")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_duration_text);

    // The exact count first: this shape publishes "1,740 views" where a lockup would only round it
    // to "1.7K". The abbreviated field is the fallback, not the preference.
    let view_count = card
        .pointer("/viewCountText/simpleText")
        .or_else(|| card.pointer("/shortViewCountText/simpleText"))
        .and_then(serde_json::Value::as_str)
        .and_then(parse_compact_count);

    let published_text = card
        .pointer("/publishedTimeText/simpleText")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);

    let channel_name = card
        .pointer("/ownerText/runs/0/text")
        .or_else(|| card.pointer("/longBylineText/runs/0/text"))
        .or_else(|| card.pointer("/shortBylineText/runs/0/text"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);

    let channel_id = card
        .pointer("/ownerText/runs/0/navigationEndpoint/browseEndpoint/browseId")
        .or_else(|| {
            card.pointer("/longBylineText/runs/0/navigationEndpoint/browseEndpoint/browseId")
        })
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| ChannelId::new(raw).ok());

    // The tick is a badge on the owner rather than a property of the video.
    let channel_verified = {
        let mut styles = Vec::new();
        collect_by_key(card, "metadataBadgeRenderer", &mut styles);
        styles.iter().any(|badge| {
            badge
                .get("style")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|style| style.contains("VERIFIED"))
        })
    };

    Some(VideoSummary {
        id,
        title,
        channel_id,
        channel_name,
        channel_avatar: sources_to_thumbnails(
            card.pointer("/avatar/decoratedAvatarViewModel/avatar/avatarViewModel/image/sources"),
        ),
        channel_verified,
        thumbnails: sources_to_thumbnails(card.pointer("/thumbnail/thumbnails")),
        duration_ms,
        // Relative text only, as everywhere else on these surfaces.
        published_at: None,
        published_text,
        view_count,
        live_status: if live {
            LiveStatus::Live
        } else {
            LiveStatus::NotLive
        },
        is_short: false,
    })
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

/// Maps the About tab's links, keeping only those the application can actually open.
///
/// The opener admits `https` and nothing else, on purpose — see
/// [`beastube_core::security::validate_external_url`]. Owners publish `http` links, and some
/// publish `mailto:` or worse. An `http` one is upgraded, since the destination is the same
/// public page either way, and anything that is not one of those two schemes is dropped: a missing
/// link beats a link that opens onto an error.
pub(crate) fn channel_links(source: &[(String, String)]) -> Vec<ChannelLink> {
    source
        .iter()
        .filter_map(|(title, url)| {
            let trimmed = url.trim();
            let https = if let Some(rest) = trimmed.strip_prefix("http://") {
                format!("https://{rest}")
            } else if trimmed.starts_with("https://") {
                trimmed.to_owned()
            } else {
                return None;
            };
            // Validated here rather than only at the point of opening, so a malformed link never
            // becomes a button that fails when pressed.
            beastube_core::security::validate_external_url(&https).ok()?;
            Some(ChannelLink {
                title: title.trim().to_owned(),
                url: https,
            })
        })
        .collect()
}

/// The channel's declared country as an ISO 3166-1 alpha-2 code.
///
/// The code rather than the extractor's `name()`, which is English only. The surface localises it
/// with `Intl.DisplayNames`, so a Spanish or Hindi reader sees the country in their own language
/// instead of "United States" in the middle of a translated page.
pub(crate) fn country_code(country: Option<rustypipe::param::Country>) -> Option<String> {
    // Through serde rather than `Debug`: the enum declares `rename_all = "UPPERCASE"`, so this is
    // the spelling it defines rather than one inferred from how a variant happens to print.
    let value = serde_json::to_value(country?).ok()?;
    value.as_str().map(str::to_owned)
}

/// Converts the channel's creation date into the domain timestamp.
///
/// The provider publishes a day, not an instant, so this is that day at midnight UTC. Callers
/// render it as a date; treating it as a time would invent a precision the source does not have.
pub(crate) fn joined_at(date: Option<time::Date>) -> Option<Timestamp> {
    date.map(|day| Timestamp::from(day.midnight().assume_utc()))
}

/// How long ago `text` says a video was published, in milliseconds, or `None` if it does not say.
///
/// Approximate by construction, and only ever used for ordering. These surfaces publish prose —
/// "35 minutes ago", "Streamed 2 hours ago" — and never an absolute date, so this recovers roughly
/// what the reader is told and nothing finer. It must not be used to fill `published_at`, which is
/// a claim about *when* something happened rather than about how it should be sorted.
///
/// Months and years are the average Gregorian ones, for the same reason: a feed ordered by
/// "3 months" against "1 year" only needs those two to land on the right side of each other.
pub(crate) fn approximate_age_ms(text: &str) -> Option<u64> {
    const MINUTE: u64 = 60_000;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;

    let lower = text.to_lowercase();
    // The first number in the string. "Streamed 2 hours ago" and "2 hours ago" both give 2.
    let digits: String = lower
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    let count: u64 = digits.parse().ok()?;

    // Checked longest-first: "month" contains no other unit, but checking "day" before "today"
    // would be the same class of mistake, so the order is explicit rather than incidental.
    let unit = if lower.contains("second") {
        1_000
    } else if lower.contains("minute") {
        MINUTE
    } else if lower.contains("hour") {
        HOUR
    } else if lower.contains("day") {
        DAY
    } else if lower.contains("week") {
        7 * DAY
    } else if lower.contains("month") {
        2_629_800_000
    } else if lower.contains("year") {
        31_557_600_000
    } else {
        return None;
    };

    count.checked_mul(unit)
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
        audio_tracks: Vec::new(),
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

/// Converts the extractor's subtitle list into caption tracks.
///
/// These come from the player response rather than the watch page: the watch page does not carry
/// them, which is why the capability said captions were unavailable when in fact they were one
/// request away.
pub(crate) fn caption_tracks(subtitles: Vec<rustypipe::model::Subtitle>) -> Vec<CaptionTrack> {
    subtitles
        .into_iter()
        .map(|subtitle| CaptionTrack {
            language_code: subtitle.lang,
            language_name: subtitle.lang_name,
            url: subtitle.url,
            is_auto_generated: subtitle.auto_generated,
        })
        .collect()
}

/// Parses a `json3` caption payload into cues.
///
/// The format is a flat `events` array: a start, a duration, and the line split into segments that
/// are simply concatenated. Events with no text are the format's own timing padding and are
/// dropped, as are ones whose text is only whitespace — both would otherwise flash an empty
/// caption box on screen.
pub(crate) fn cues_from_json3(json: &str) -> Vec<Cue> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(events) = root.get("events").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };

    events
        .iter()
        .filter_map(|event| {
            let start = event.get("tStartMs")?.as_u64()?;
            let text: String = event
                .get("segs")?
                .as_array()?
                .iter()
                .filter_map(|segment| segment.get("utf8").and_then(serde_json::Value::as_str))
                .collect();
            if text.trim().is_empty() {
                return None;
            }
            // A missing duration means "until the next one"; a short floor keeps such a line on
            // screen long enough to be read rather than blinking out on the same frame.
            let duration = event
                .get("dDurationMs")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(MIN_CUE_MS)
                .max(MIN_CUE_MS);
            Some(Cue {
                start_ms: start,
                end_ms: start + duration,
                text,
            })
        })
        .collect()
}

/// Shortest time a line stays on screen, in milliseconds.
const MIN_CUE_MS: u64 = 700;

/// The distinct audio tracks across a video's audio streams.
///
/// The extractor reports the track on each *stream*, and a video has several streams per track —
/// one per bitrate — so the same language arrives many times over. This collapses them to one
/// entry each, keeping the original first and the rest in the order the provider listed them,
/// which is the order the site's own menu uses.
///
/// Every track the response names is returned, including a lone one. Whether that is worth a menu
/// is the interface's decision, not this function's — and a viewer being told which language they
/// are hearing is worth something even when it is the only one on offer.
pub(crate) fn audio_tracks(streams: &[rustypipe::model::AudioStream]) -> Vec<AudioTrack> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut tracks: Vec<AudioTrack> = Vec::new();

    for stream in streams {
        let Some(track) = stream.track.as_ref() else {
            continue;
        };
        if !seen.insert(track.id.as_str()) {
            continue;
        }
        tracks.push(AudioTrack {
            id: track.id.clone(),
            language_code: track.lang.clone(),
            language_name: track.lang_name.clone(),
            is_default: track.is_default,
            is_original: matches!(
                track.track_type,
                Some(rustypipe::model::AudioTrackType::Original)
            ),
        });
    }

    // The original leads; everything else keeps the provider's order.
    tracks.sort_by_key(|track| !track.is_original);
    tracks
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
    fn cues_are_read_with_their_windows() {
        let json = serde_json::json!({ "events": [
            { "tStartMs": 1360, "dDurationMs": 1680, "segs": [{ "utf8": "[music]" }] },
            { "tStartMs": 18640, "dDurationMs": 3240,
              "segs": [{ "utf8": "We're no " }, { "utf8": "strangers" }] },
        ]})
        .to_string();

        let cues = cues_from_json3(&json);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start_ms, 1360);
        assert_eq!(cues[0].end_ms, 3040, "end is start plus duration");
        assert_eq!(
            cues[1].text, "We're no strangers",
            "segments are one line, not one cue each"
        );
    }

    #[test]
    fn empty_events_do_not_become_blank_captions() {
        let json = serde_json::json!({ "events": [
            { "tStartMs": 0, "dDurationMs": 500, "segs": [{ "utf8": "
" }] },
            { "tStartMs": 10, "dDurationMs": 500 },
            { "tStartMs": 20, "dDurationMs": 500, "segs": [{ "utf8": "real" }] },
        ]})
        .to_string();
        assert_eq!(cues_from_json3(&json).len(), 1);
    }

    #[test]
    fn a_cue_without_a_duration_still_stays_readable() {
        let json =
            serde_json::json!({ "events": [{ "tStartMs": 0, "segs": [{ "utf8": "hi" }] }] })
                .to_string();
        let cues = cues_from_json3(&json);
        assert_eq!(cues[0].end_ms, MIN_CUE_MS);
    }

    #[test]
    fn an_unreadable_payload_yields_no_cues_rather_than_failing() {
        assert!(cues_from_json3("not json").is_empty());
        assert!(cues_from_json3("{}").is_empty());
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

        let shorts = shorts_from_json(&json);
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
        assert!(shorts_from_json(&json).is_empty());
        assert!(shorts_from_json("not json").is_empty());
    }

    #[test]
    fn the_same_short_in_two_shelves_appears_once() {
        let lockup = serde_json::json!({ "shortsLockupViewModel": {
            "onTap": { "innertubeCommand": { "reelWatchEndpoint": { "videoId": "iuef391OhRU" }}},
            "overlayMetadata": { "primaryText": { "content": "t" } }
        }});
        let json = serde_json::json!({ "a": [lockup.clone()], "b": [lockup] }).to_string();
        assert_eq!(shorts_from_json(&json).len(), 1);
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
// so this reads it directly, exactly as `shorts_from_json` above already does.
// -------------------------------------------------------------------------------------------

/// Reads the recommendation shelf out of a raw `next` response.
pub(crate) fn videos_from_json(json: &str) -> Vec<VideoSummary> {
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

    // The duration is the badge drawn over the thumbnail, and it is a bare string under `text`
    // rather than the `{ content }` object every other string in a lockup uses. Scanning for
    // `content` therefore never found it: measured against the live recommendation shelf, that
    // was 0 of 26 cards with a duration when all 26 carry one.
    //
    // The badge is read by name rather than by path because the same badge appears under
    // different overlay wrappers depending on the surface, and a non-duration badge ("LIVE", a
    // members-only label) simply fails to parse and is skipped.
    let mut badges = Vec::new();
    if let Some(image) = lockup.pointer("/contentImage/thumbnailViewModel") {
        collect_by_key(image, "thumbnailBadgeViewModel", &mut badges);
    }
    let duration_ms = badges
        .iter()
        .filter_map(|badge| {
            // Live, `text` is a bare string. Elsewhere in a lockup every string is wrapped as
            // `{ content }`, and this badge has been seen both ways, so both are accepted.
            let text = badge.get("text")?;
            text.as_str()
                .or_else(|| text.get("content").and_then(serde_json::Value::as_str))
        })
        .find_map(parse_duration_text);

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

    // The metadata rows are read as rows rather than flattened into one list, because how many
    // there are is the only thing that says whether a channel name is present at all.
    //
    // Two shapes reach here, and they differ by exactly one row:
    //
    // - A recommendation shelf names the uploader: `[["Rick Astley"], ["28M", "6y ago"]]`.
    // - A channel's own grid does not, because every card on it has the same uploader — the page
    //   is that channel: `[["79M views", "6 days ago"]]`.
    //
    // Flattening and taking the first entry as the channel — which this did — reads "79M views" as
    // the uploader's name on every card of every channel page, and then looks for the view count
    // in what is left and finds nothing. Measured against the live channel grid, that is 0 of 30
    // cards with a view count and 30 of 30 with a nonsense channel name.
    //
    // Collaborations are why this counts rows instead of testing for the avatar block: a co-hosted
    // upload on a channel grid does carry a channel row ("MrBeast and Mark Rober") while carrying
    // no avatar, and a recommendation can do the same ("Shakira and 2 more").
    let rows = metadata
        .and_then(|block| block.pointer("/metadata/contentMetadataViewModel/metadataRows"))
        .and_then(serde_json::Value::as_array);

    let row_parts = |index: usize| -> Vec<String> {
        let mut out = Vec::new();
        if let Some(row) = rows.and_then(|rows| rows.get(index)) {
            text_contents(row, &mut out);
        }
        out
    };

    let row_count = rows.map_or(0, Vec::len);
    let named_channel = row_count > 1;

    // Everything after the channel row, or everything when there is no channel row.
    let stats: Vec<String> = (usize::from(named_channel)..row_count)
        .flat_map(row_parts)
        .collect();

    let channel_name = if named_channel {
        row_parts(0).into_iter().next()
    } else {
        None
    };

    // Classified by what each string says rather than by position: a live item has no date and an
    // upcoming one has no view count, so indexing would put a date where a count belongs. Read
    // live, the shelf gives a bare `"24M"` with no "views" word at all, which is why this cannot
    // key on that word the way the search mapping can.
    let published_text = stats.iter().find(|text| is_time_text(text)).cloned();
    let view_count = stats
        .iter()
        .filter(|text| !is_time_text(text))
        .find_map(|text| parse_compact_count(text));

    // The verified tick is an attachment run on the channel row, named by its client resource.
    let channel_verified = rows.is_some_and(|rows| {
        let mut names = Vec::new();
        for row in rows {
            collect_by_key(row, "imageName", &mut names);
        }
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
                            { "thumbnailBadgeViewModel": { "text": "12:34" } }
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

    fn links(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(title, url)| ((*title).to_owned(), (*url).to_owned()))
            .collect()
    }

    /// One card in the shape a **channel grid** uses, reduced to the fields read.
    ///
    /// The difference from [`lockup`] is one metadata row: a channel page does not repeat the
    /// uploader on every card, because the page is that uploader. Everything the card shows sits
    /// in a single row instead of a second one.
    fn channel_grid_lockup() -> serde_json::Value {
        serde_json::json!({
            "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
            "contentId": "gTKS8SAwUzE",
            "contentImage": {
                "thumbnailViewModel": {
                    "image": { "sources": [{ "url": "https://i.ytimg.com/vi/gTKS8SAwUzE/hq.jpg", "width": 480, "height": 360 }] },
                    "overlays": [{
                        "thumbnailBottomOverlayViewModel": {
                            "badges": [{ "thumbnailBadgeViewModel": { "text": "23:28" } }]
                        }
                    }]
                }
            },
            "metadata": {
                "lockupMetadataViewModel": {
                    "title": { "content": "I Survived The Most Extreme Places On Earth" },
                    "metadata": {
                        "contentMetadataViewModel": {
                            "metadataRows": [
                                { "metadataParts": [
                                    { "text": { "content": "79M views" } },
                                    { "text": { "content": "6 days ago" } }
                                ] }
                            ]
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn a_duration_badge_is_read_in_either_spelling() {
        // The live responses send `text` as a bare string; this fixture used to claim it was
        // `{ content }`, which is why the parser passed its tests and still returned a duration
        // for 0 of 26 live cards. Both are accepted now, and both are pinned.
        let mut wrapped = channel_grid_lockup();
        wrapped["contentImage"]["thumbnailViewModel"]["overlays"][0]
            ["thumbnailBottomOverlayViewModel"]["badges"][0]["thumbnailBadgeViewModel"]["text"] =
            serde_json::json!({ "content": "1:02:03" });

        let summary = video_from_lockup(&wrapped).expect("a video summary");
        assert_eq!(
            summary.duration_ms,
            Some((3600 + 2 * 60 + 3) * 1000),
            "the wrapped spelling must parse, hours included"
        );
    }

    #[test]
    fn a_non_duration_badge_is_skipped_rather_than_guessed_at() {
        let mut json = channel_grid_lockup();
        json["contentImage"]["thumbnailViewModel"]["overlays"][0]
            ["thumbnailBottomOverlayViewModel"]["badges"] = serde_json::json!([
            { "thumbnailBadgeViewModel": { "text": "LIVE" } },
            { "thumbnailBadgeViewModel": { "text": "23:28" } }
        ]);

        let summary = video_from_lockup(&json).expect("a video summary");
        assert_eq!(
            summary.duration_ms,
            Some(23 * 60 * 1000 + 28 * 1000),
            "a badge that is not a duration must be passed over, not parsed"
        );
    }

    #[test]
    fn a_channel_grid_card_keeps_its_views_and_date() {
        // The bug this pins: with only one metadata row, the parser used to read "79M views" as
        // the uploader's name and then find no view count at all. Measured against the live
        // channel grid that was 0 of 30 cards with a view count and 30 of 30 misnamed.
        let summary = video_from_lockup(&channel_grid_lockup()).expect("a video summary");

        assert_eq!(summary.view_count, Some(79_000_000));
        assert_eq!(summary.published_text.as_deref(), Some("6 days ago"));
        assert_eq!(summary.duration_ms, Some(23 * 60 * 1000 + 28 * 1000));
        assert_eq!(
            summary.channel_name, None,
            "a channel grid names no uploader per card, so none must be invented"
        );
    }

    #[test]
    fn a_collaboration_on_a_channel_grid_still_names_its_channel() {
        // A co-hosted upload does carry a channel row, on the channel's own grid, and carries no
        // avatar block with it — which is why the row count decides this and not the avatar.
        let mut json = channel_grid_lockup();
        json["metadata"]["lockupMetadataViewModel"]["metadata"]["contentMetadataViewModel"]
            ["metadataRows"] = serde_json::json!([
            { "metadataParts": [{ "text": { "content": "MrBeast and Mark Rober" } }] },
            { "metadataParts": [
                { "text": { "content": "131M views" } },
                { "text": { "content": "1 year ago" } }
            ] }
        ]);

        let summary = video_from_lockup(&json).expect("a video summary");
        assert_eq!(summary.channel_name.as_deref(), Some("MrBeast and Mark Rober"));
        assert_eq!(summary.view_count, Some(131_000_000));
        assert_eq!(summary.published_text.as_deref(), Some("1 year ago"));
    }

    #[test]
    fn a_recommendation_shelf_card_is_unchanged() {
        // The two-row shape must keep behaving exactly as it did; this is the shelf the parser was
        // written for, and the channel fix must not cost it its uploader.
        let summary = video_from_lockup(&lockup()).expect("a video summary");
        assert!(
            summary.channel_name.is_some(),
            "the recommendation shelf does name its uploader"
        );
    }

    #[test]
    fn a_live_card_with_no_date_does_not_borrow_one() {
        // A live item reports watchers instead of an upload date. Position-based reading would put
        // "12K watching" where the date belongs.
        let mut json = channel_grid_lockup();
        json["metadata"]["lockupMetadataViewModel"]["metadata"]["contentMetadataViewModel"]
            ["metadataRows"] = serde_json::json!([
            { "metadataParts": [{ "text": { "content": "12K watching" } }] }
        ]);

        let summary = video_from_lockup(&json).expect("a video summary");
        assert_eq!(summary.published_text.as_deref(), Some("12K watching"));
        assert_eq!(
            summary.view_count, None,
            "a watcher count is not a view count and must not be reported as one"
        );
    }

    #[test]
    fn channel_links_upgrade_plaintext_to_https() {
        // Every About-tab URL the live probe returned for MrBeast was reachable over https; the
        // provider simply reports some of them as http, which the external opener refuses.
        let mapped = channel_links(&links(&[("Twitter", "http://twitter.com/MrBeast")]));
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].url, "https://twitter.com/MrBeast");
        assert_eq!(mapped[0].title, "Twitter");
    }

    #[test]
    fn channel_links_drop_what_cannot_be_opened() {
        let mapped = channel_links(&links(&[
            ("Mail", "mailto:someone@example.com"),
            ("Script", "javascript:alert(1)"),
            ("Local", "file:///C:/Windows/System32"),
            ("Relative", "/about"),
            ("Empty", ""),
            ("Real", "https://example.com/"),
        ]));
        // Only the one the opener would actually accept survives.
        assert_eq!(mapped.len(), 1, "{mapped:?}");
        assert_eq!(mapped[0].url, "https://example.com/");
    }

    #[test]
    fn channel_links_drop_a_credentialled_url() {
        // `validate_external_url` refuses embedded credentials; this checks the filter really
        // consults it rather than only looking at the scheme.
        let mapped = channel_links(&links(&[("Sneaky", "https://user:pass@example.com/")]));
        assert!(mapped.is_empty(), "{mapped:?}");
    }

    #[test]
    fn relative_ages_order_correctly() {
        // Only the ordering matters, so the assertions are about which is newer rather than about
        // exact durations.
        let age = |text: &str| approximate_age_ms(text).expect("a parseable age");
        assert!(age("35 minutes ago") < age("2 hours ago"));
        assert!(age("2 hours ago") < age("6 days ago"));
        assert!(age("6 days ago") < age("3 weeks ago"));
        assert!(age("3 weeks ago") < age("5 months ago"));
        assert!(age("5 months ago") < age("3 years ago"));
        assert_eq!(approximate_age_ms("45 seconds ago"), Some(45_000));
        assert_eq!(approximate_age_ms("1 hour ago"), Some(3_600_000));
    }

    #[test]
    fn a_past_stream_reads_like_any_other_age() {
        // Live tabs say "Streamed 3 months ago" rather than "3 months ago", and the number is not
        // the first thing in the string.
        assert_eq!(
            approximate_age_ms("Streamed 2 hours ago"),
            approximate_age_ms("2 hours ago")
        );
        assert!(
            approximate_age_ms("Streamed 3 months ago") > approximate_age_ms("Streamed 2 days ago")
        );
    }

    #[test]
    fn text_with_no_age_in_it_yields_nothing() {
        // A watcher count is not an age, and must not be read as one: "12K watching" would
        // otherwise parse its number against whatever unit happened to match.
        assert_eq!(approximate_age_ms("12K watching"), None);
        assert_eq!(approximate_age_ms("LIVE"), None);
        assert_eq!(approximate_age_ms(""), None);
        assert_eq!(approximate_age_ms("ages ago"), None);
        assert_eq!(approximate_age_ms("Premieres tomorrow"), None);
    }

    #[test]
    fn a_join_date_becomes_midnight_utc() {
        let day = time::Date::from_calendar_date(2012, time::Month::February, 20).unwrap();
        let stamp = joined_at(Some(day)).expect("a timestamp");
        let back = time::OffsetDateTime::from_unix_timestamp(stamp.as_millis() / 1000).unwrap();
        assert_eq!(back.year(), 2012);
        assert_eq!(back.month(), time::Month::February);
        assert_eq!(back.day(), 20);
        assert_eq!((back.hour(), back.minute(), back.second()), (0, 0, 0));
        assert_eq!(joined_at(None), None);
    }

    #[test]
    fn a_country_maps_to_its_two_letter_code() {
        assert_eq!(
            country_code(Some(rustypipe::param::Country::Us)).as_deref(),
            Some("US")
        );
        assert_eq!(
            country_code(Some(rustypipe::param::Country::Gb)).as_deref(),
            Some("GB")
        );
        assert_eq!(country_code(None), None);
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

        let videos = videos_from_json(&response.to_string());
        assert_eq!(videos.len(), 1, "the same video must not appear twice");
        assert_eq!(videos[0].channel_name.as_deref(), Some("SlayyPop"));
    }

    #[test]
    fn a_response_that_is_not_json_yields_nothing_rather_than_panicking() {
        assert!(videos_from_json("<html>error</html>").is_empty());
    }
}
