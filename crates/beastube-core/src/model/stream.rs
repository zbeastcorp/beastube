//! Media stream descriptors.
//!
//! These types describe *what the provider offered*, not what the player chose. Two consequences
//! shape the design:
//!
//! * **The quality list is derived, never assumed.** [`StreamSet::available_qualities`] reports
//!   only tiers that actually have a stream behind them, so the UI cannot present a 4K option for a
//!   video that has none (§131, no fake features).
//! * **Byte ranges are preserved.** Adaptive streams are served as a single file with an
//!   initialization segment and an index at known offsets. Carrying [`ByteRange`] through the model
//!   is what lets the playback layer emit a DASH manifest with `SegmentBase`/`Initialization`
//!   instead of downloading whole tracks.

use serde::{Deserialize, Serialize};

use crate::time_util::Timestamp;

/// An inclusive byte range, matching HTTP `Range: bytes=start-end` semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRange {
    /// First byte offset, inclusive.
    pub start: u64,
    /// Last byte offset, inclusive.
    pub end: u64,
}

impl ByteRange {
    /// Creates a range, returning `None` if `end` precedes `start`.
    ///
    /// A reversed range in a provider response is schema drift, not a recoverable value; rejecting
    /// it here prevents an underflowing length calculation downstream.
    #[must_use]
    pub const fn new(start: u64, end: u64) -> Option<Self> {
        if end < start {
            None
        } else {
            Some(Self { start, end })
        }
    }

    /// Number of bytes covered, inclusive of both endpoints.
    #[must_use]
    pub const fn len(self) -> u64 {
        self.end - self.start + 1
    }

    /// Always `false`: an inclusive range covers at least one byte by construction.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        false
    }

    /// Renders as an HTTP `Range` header value, e.g. `bytes=0-1023`.
    #[must_use]
    pub fn to_header_value(self) -> String {
        format!("bytes={}-{}", self.start, self.end)
    }
}

/// Video codec family.
///
/// The exact RFC 6381 codec string is preserved separately in [`VideoStream::codecs`]; this enum
/// exists for decisions (hardware-decode likelihood, container choice, filtering), where matching
/// on a free-form string would be fragile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    /// H.264 / AVC. Broadest hardware decode support.
    H264,
    /// VP9. Widely hardware-decoded on modern GPUs.
    Vp9,
    /// AV1. Hardware decode is common on recent GPUs and expensive in software.
    Av1,
    /// A codec this build does not model. Playability is decided by the media engine, not by us.
    Other,
}

impl VideoCodec {
    /// Stable identifier for logs and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Vp9 => "vp9",
            Self::Av1 => "av1",
            Self::Other => "other",
        }
    }

    /// Classifies an RFC 6381 codec string such as `avc1.640028`, `vp09.00.10.08` or `av01.0.05M.08`.
    #[must_use]
    pub fn from_codec_string(codecs: &str) -> Self {
        let lower = codecs.to_ascii_lowercase();
        if lower.starts_with("avc1") || lower.starts_with("avc3") || lower.starts_with("h264") {
            Self::H264
        } else if lower.starts_with("vp9") || lower.starts_with("vp09") {
            Self::Vp9
        } else if lower.starts_with("av01") || lower.starts_with("av1") {
            Self::Av1
        } else {
            Self::Other
        }
    }

    /// Whether software decoding this codec is likely to be expensive at high resolution.
    ///
    /// Used only to *rank* equivalent streams when hardware acceleration is known to be
    /// unavailable; it never suppresses a stream, because the media engine is the authority on what
    /// it can actually decode.
    #[must_use]
    pub const fn costly_in_software(self) -> bool {
        matches!(self, Self::Av1 | Self::Vp9)
    }
}

/// Audio codec family.
///
/// See [`VideoCodec`] for why the family is modelled separately from the codec string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCodec {
    /// AAC, typically in an MP4 container.
    Aac,
    /// Opus, typically in a WebM container.
    Opus,
    /// A codec this build does not model.
    Other,
}

impl AudioCodec {
    /// Stable identifier for logs and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::Opus => "opus",
            Self::Other => "other",
        }
    }

    /// Classifies an RFC 6381 codec string such as `mp4a.40.2` or `opus`.
    #[must_use]
    pub fn from_codec_string(codecs: &str) -> Self {
        let lower = codecs.to_ascii_lowercase();
        if lower.starts_with("mp4a") || lower.starts_with("aac") {
            Self::Aac
        } else if lower.starts_with("opus") {
            Self::Opus
        } else {
            Self::Other
        }
    }
}

/// A selectable video quality tier.
///
/// [`Quality::Auto`] is a *selection mode*, not a stream property: it delegates the choice to the
/// adaptive bitrate controller. Every other variant pins a tier.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    /// Let the ABR controller choose, adapting to measured throughput.
    #[default]
    Auto,
    /// 144p.
    #[serde(rename = "144p")]
    P144,
    /// 240p.
    #[serde(rename = "240p")]
    P240,
    /// 360p.
    #[serde(rename = "360p")]
    P360,
    /// 480p.
    #[serde(rename = "480p")]
    P480,
    /// 720p.
    #[serde(rename = "720p")]
    P720,
    /// 1080p.
    #[serde(rename = "1080p")]
    P1080,
    /// 1440p.
    #[serde(rename = "1440p")]
    P1440,
    /// 2160p (4K).
    #[serde(rename = "2160p")]
    P2160,
}

impl Quality {
    /// Every pinned tier, ascending. Excludes [`Quality::Auto`].
    pub const TIERS: [Self; 8] = [
        Self::P144,
        Self::P240,
        Self::P360,
        Self::P480,
        Self::P720,
        Self::P1080,
        Self::P1440,
        Self::P2160,
    ];

    /// Nominal vertical resolution, or `None` for [`Quality::Auto`].
    #[must_use]
    pub const fn height(self) -> Option<u32> {
        match self {
            Self::Auto => None,
            Self::P144 => Some(144),
            Self::P240 => Some(240),
            Self::P360 => Some(360),
            Self::P480 => Some(480),
            Self::P720 => Some(720),
            Self::P1080 => Some(1080),
            Self::P1440 => Some(1440),
            Self::P2160 => Some(2160),
        }
    }

    /// Stable identifier for logs, settings persistence and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::P144 => "144p",
            Self::P240 => "240p",
            Self::P360 => "360p",
            Self::P480 => "480p",
            Self::P720 => "720p",
            Self::P1080 => "1080p",
            Self::P1440 => "1440p",
            Self::P2160 => "2160p",
        }
    }

    /// Buckets a stream's pixel height into a tier.
    ///
    /// Non-16:9 videos report heights that match no tier exactly (a vertical Short may be 1920 tall
    /// while being a "1080p" stream). The bucket is therefore the highest tier **not exceeding**
    /// the height, with 144p as the floor, so an unusual aspect ratio never lands a stream outside
    /// the selectable list.
    #[must_use]
    pub fn from_height(height: u32) -> Self {
        Self::TIERS
            .into_iter()
            .rev()
            .find(|tier| tier.height().is_some_and(|h| height >= h))
            .unwrap_or(Self::P144)
    }
}

/// One adaptive video-only stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoStream {
    /// Provider-assigned format identifier, used for diagnostics and stream re-selection.
    pub itag: u32,
    /// Absolute media URL. Time-limited and frequently bound to the requesting IP.
    pub url: String,
    /// MIME type without parameters, e.g. `video/mp4`.
    pub mime_type: String,
    /// Full RFC 6381 codec string, e.g. `avc1.640028`. Required verbatim by the DASH manifest.
    pub codecs: String,
    /// Codec family, classified from [`VideoStream::codecs`].
    pub codec: VideoCodec,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Frames per second, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    /// Average bitrate in bits per second, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u64>,
    /// Total size in bytes, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_length: Option<u64>,
    /// Byte range of the initialization segment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_range: Option<ByteRange>,
    /// Byte range of the segment index (`sidx`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_range: Option<ByteRange>,
    /// Whether the stream carries high dynamic range.
    #[serde(default)]
    pub hdr: bool,
}

impl VideoStream {
    /// The quality tier this stream belongs to.
    #[must_use]
    pub fn quality(&self) -> Quality {
        Quality::from_height(self.height)
    }

    /// Whether the stream carries the byte ranges required to describe it with `SegmentBase`.
    ///
    /// A stream missing them cannot be placed in a generated DASH manifest and must be excluded,
    /// rather than producing a manifest the player fails to load.
    #[must_use]
    pub const fn supports_segment_base(&self) -> bool {
        self.init_range.is_some() && self.index_range.is_some()
    }
}

/// One adaptive audio-only stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioStream {
    /// Provider-assigned format identifier.
    pub itag: u32,
    /// Absolute media URL. Time-limited and frequently IP-bound.
    pub url: String,
    /// MIME type without parameters, e.g. `audio/webm`.
    pub mime_type: String,
    /// Full RFC 6381 codec string, e.g. `opus` or `mp4a.40.2`.
    pub codecs: String,
    /// Codec family, classified from [`AudioStream::codecs`].
    pub codec: AudioCodec,
    /// Average bitrate in bits per second, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u64>,
    /// Channel count, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u8>,
    /// Sample rate in hertz, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    /// Total size in bytes, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_length: Option<u64>,
    /// Byte range of the initialization segment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_range: Option<ByteRange>,
    /// Byte range of the segment index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_range: Option<ByteRange>,
    /// BCP 47 language tag of this audio track, when the video has multiple.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Human-readable track name, e.g. `English (original)`.
    ///
    /// Supplied by the provider and therefore already localized upstream; it is rendered as opaque
    /// text, never used as a translation key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_name: Option<String>,
    /// Whether this is the track to select when the user has expressed no preference.
    #[serde(default)]
    pub is_default: bool,
}

impl AudioStream {
    /// Whether the stream carries the byte ranges required to describe it with `SegmentBase`.
    #[must_use]
    pub const fn supports_segment_base(&self) -> bool {
        self.init_range.is_some() && self.index_range.is_some()
    }
}

/// Everything needed to start playback of one video.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamSet {
    /// Adaptive video-only streams.
    pub video: Vec<VideoStream>,
    /// Adaptive audio-only streams.
    pub audio: Vec<AudioStream>,
    /// Wall-clock time at which the media URLs stop working.
    pub expires_at: Timestamp,
    /// Total media duration in milliseconds. `None` for live content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Whether this is a live or post-live stream, which changes buffering and seek behaviour.
    #[serde(default)]
    pub is_live: bool,
}

impl StreamSet {
    /// Quality tiers that have at least one usable video stream, ascending and deduplicated.
    ///
    /// This is the only source the UI may use to populate the quality menu.
    #[must_use]
    pub fn available_qualities(&self) -> Vec<Quality> {
        let mut tiers: Vec<Quality> = self
            .video
            .iter()
            .filter(|s| s.supports_segment_base())
            .map(VideoStream::quality)
            .collect();
        tiers.sort_unstable();
        tiers.dedup();
        tiers
    }

    /// Video streams usable in a generated DASH manifest.
    pub fn usable_video(&self) -> impl Iterator<Item = &VideoStream> {
        self.video.iter().filter(|s| s.supports_segment_base())
    }

    /// Audio streams usable in a generated DASH manifest.
    pub fn usable_audio(&self) -> impl Iterator<Item = &AudioStream> {
        self.audio.iter().filter(|s| s.supports_segment_base())
    }

    /// Whether a manifest can be produced at all.
    ///
    /// Requires at least one usable stream on each side: audio-only or video-only playback of a
    /// regular video is a broken experience, and reporting the failure lets the caller fall back.
    #[must_use]
    pub fn is_playable(&self) -> bool {
        self.usable_video().next().is_some() && self.usable_audio().next().is_some()
    }

    /// Whether the media URLs have expired as of `now`.
    #[must_use]
    pub const fn is_expired(&self, now: Timestamp) -> bool {
        self.expires_at.as_millis() <= now.as_millis()
    }

    /// Whether the URLs expire within `margin_ms` of `now`.
    ///
    /// Playback re-resolves the stream set *before* expiry rather than after: recovering from an
    /// expiry-induced 403 mid-segment costs a visible stall, whereas refreshing early costs one
    /// cheap metadata request.
    #[must_use]
    pub const fn expires_within(&self, margin_ms: i64, now: Timestamp) -> bool {
        self.expires_at.as_millis() - now.as_millis() <= margin_ms
    }

    /// Distinct audio languages present, in first-seen order.
    #[must_use]
    pub fn audio_languages(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for stream in &self.audio {
            if let Some(language) = &stream.language
                && !seen.iter().any(|s: &String| s == language)
            {
                seen.push(language.clone());
            }
        }
        seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(itag: u32, height: u32, ranged: bool) -> VideoStream {
        VideoStream {
            itag,
            url: format!("https://rr1.googlevideo.com/videoplayback?itag={itag}"),
            mime_type: "video/mp4".to_owned(),
            codecs: "avc1.640028".to_owned(),
            codec: VideoCodec::H264,
            width: height * 16 / 9,
            height,
            fps: Some(30),
            bitrate: Some(u64::from(height) * 3000),
            content_length: Some(1_000_000),
            init_range: ranged.then(|| ByteRange::new(0, 739).unwrap()),
            index_range: ranged.then(|| ByteRange::new(740, 1231).unwrap()),
            hdr: false,
        }
    }

    fn audio(itag: u32, ranged: bool) -> AudioStream {
        AudioStream {
            itag,
            url: format!("https://rr1.googlevideo.com/videoplayback?itag={itag}"),
            mime_type: "audio/webm".to_owned(),
            codecs: "opus".to_owned(),
            codec: AudioCodec::Opus,
            bitrate: Some(128_000),
            channels: Some(2),
            sample_rate: Some(48_000),
            content_length: Some(500_000),
            init_range: ranged.then(|| ByteRange::new(0, 258).unwrap()),
            index_range: ranged.then(|| ByteRange::new(259, 512).unwrap()),
            language: None,
            track_name: None,
            is_default: true,
        }
    }

    fn stream_set(video_streams: Vec<VideoStream>, audio_streams: Vec<AudioStream>) -> StreamSet {
        StreamSet {
            video: video_streams,
            audio: audio_streams,
            expires_at: Timestamp::from_millis(1_000_000),
            duration_ms: Some(212_000),
            is_live: false,
        }
    }

    #[test]
    fn byte_range_rejects_reversed_input() {
        assert!(ByteRange::new(10, 5).is_none());
        assert_eq!(ByteRange::new(0, 0).unwrap().len(), 1);
        assert_eq!(ByteRange::new(0, 1023).unwrap().len(), 1024);
        assert_eq!(
            ByteRange::new(0, 1023).unwrap().to_header_value(),
            "bytes=0-1023"
        );
    }

    #[test]
    fn codec_classification_covers_real_codec_strings() {
        assert_eq!(
            VideoCodec::from_codec_string("avc1.640028"),
            VideoCodec::H264
        );
        assert_eq!(
            VideoCodec::from_codec_string("avc3.42E01E"),
            VideoCodec::H264
        );
        assert_eq!(
            VideoCodec::from_codec_string("vp09.00.10.08"),
            VideoCodec::Vp9
        );
        assert_eq!(VideoCodec::from_codec_string("vp9"), VideoCodec::Vp9);
        assert_eq!(
            VideoCodec::from_codec_string("av01.0.05M.08"),
            VideoCodec::Av1
        );
        assert_eq!(
            VideoCodec::from_codec_string("hev1.1.6.L93.B0"),
            VideoCodec::Other
        );
        assert_eq!(AudioCodec::from_codec_string("mp4a.40.2"), AudioCodec::Aac);
        assert_eq!(AudioCodec::from_codec_string("opus"), AudioCodec::Opus);
        assert_eq!(AudioCodec::from_codec_string("ec-3"), AudioCodec::Other);
    }

    #[test]
    fn quality_buckets_non_standard_heights_downward() {
        assert_eq!(Quality::from_height(1080), Quality::P1080);
        assert_eq!(Quality::from_height(1078), Quality::P720);
        assert_eq!(Quality::from_height(2160), Quality::P2160);
        assert_eq!(Quality::from_height(4320), Quality::P2160);
        // A vertical Short: tall, but still bucketed rather than dropped.
        assert_eq!(Quality::from_height(1920), Quality::P1440);
        // Below the floor.
        assert_eq!(Quality::from_height(1), Quality::P144);
        assert_eq!(Quality::from_height(0), Quality::P144);
    }

    #[test]
    fn quality_ordering_allows_min_max_comparisons() {
        assert!(Quality::P2160 > Quality::P1080);
        assert!(
            Quality::Auto < Quality::P144,
            "Auto sorts before pinned tiers"
        );
        assert_eq!(Quality::default(), Quality::Auto);
    }

    #[test]
    fn quality_serializes_with_human_stable_names() {
        assert_eq!(serde_json::to_string(&Quality::P1080).unwrap(), "\"1080p\"");
        assert_eq!(serde_json::to_string(&Quality::Auto).unwrap(), "\"auto\"");
        let parsed: Quality = serde_json::from_str("\"2160p\"").unwrap();
        assert_eq!(parsed, Quality::P2160);
    }

    #[test]
    fn quality_list_excludes_streams_that_cannot_be_manifested() {
        let set = stream_set(
            vec![video(137, 1080, true), video(313, 2160, false)],
            vec![audio(251, true)],
        );
        assert_eq!(
            set.available_qualities(),
            vec![Quality::P1080],
            "a 4K stream without byte ranges must not appear in the menu"
        );
    }

    #[test]
    fn quality_list_is_sorted_and_deduplicated() {
        let set = stream_set(
            vec![
                video(137, 1080, true),
                video(248, 1080, true),
                video(136, 720, true),
            ],
            vec![audio(251, true)],
        );
        assert_eq!(
            set.available_qualities(),
            vec![Quality::P720, Quality::P1080]
        );
    }

    #[test]
    fn playability_requires_both_tracks() {
        assert!(stream_set(vec![video(137, 1080, true)], vec![audio(251, true)]).is_playable());
        assert!(!stream_set(vec![video(137, 1080, true)], vec![]).is_playable());
        assert!(!stream_set(vec![], vec![audio(251, true)]).is_playable());
        assert!(
            !stream_set(vec![video(137, 1080, false)], vec![audio(251, true)]).is_playable(),
            "unmanifestable video means unplayable"
        );
    }

    #[test]
    fn expiry_is_checked_with_a_refresh_margin() {
        let set = stream_set(vec![video(137, 1080, true)], vec![audio(251, true)]);
        let before = Timestamp::from_millis(900_000);
        let at = Timestamp::from_millis(1_000_000);
        let after = Timestamp::from_millis(1_000_001);

        assert!(!set.is_expired(before));
        assert!(set.is_expired(at), "expiry is inclusive");
        assert!(set.is_expired(after));

        assert!(set.expires_within(120_000, before));
        assert!(!set.expires_within(60_000, before));
    }

    #[test]
    fn audio_languages_are_deduplicated_in_first_seen_order() {
        let mut english = audio(251, true);
        english.language = Some("en".to_owned());
        let mut english_dub = audio(140, true);
        english_dub.language = Some("en".to_owned());
        let mut hindi = audio(249, true);
        hindi.language = Some("hi".to_owned());

        let set = stream_set(vec![], vec![english, hindi, english_dub]);
        assert_eq!(
            set.audio_languages(),
            vec!["en".to_owned(), "hi".to_owned()]
        );
    }

    #[test]
    fn stream_set_round_trips_through_json() {
        let set = stream_set(vec![video(137, 1080, true)], vec![audio(251, true)]);
        let json = serde_json::to_string(&set).unwrap();
        let back: StreamSet = serde_json::from_str(&json).unwrap();
        assert_eq!(set, back);
    }
}
