//! What a provider can actually do.
//!
//! This is the mechanism behind §131 ("no fake features"). A provider declares its capabilities;
//! the UI renders a control only where the corresponding flag is true. A capability that is false
//! is a control that is *absent*, not one that is present and fails.
//!
//! The flags are deliberately fine-grained. "Search" is not one capability — an adapter can search
//! videos while being unable to search playlists, and the filter chips should reflect that rather
//! than offering a tab that always returns nothing.

use serde::{Deserialize, Serialize};

/// What a metadata provider supports.
// A capability set is exactly the case where independent booleans are the right shape: each maps to
// one control the UI renders or hides, and they are not mutually exclusive.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    /// Free-text search over videos.
    pub search_videos: bool,
    /// Free-text search over channels.
    pub search_channels: bool,
    /// Free-text search over playlists.
    pub search_playlists: bool,
    /// Search restricted to short-form video.
    pub search_shorts: bool,
    /// Query autocompletion.
    pub suggestions: bool,
    /// Full details for one video.
    pub video_details: bool,
    /// Videos related to one video.
    pub related_videos: bool,
    /// Channel metadata.
    pub channel_details: bool,
    /// A channel's uploads.
    pub channel_videos: bool,
    /// A channel's short-form uploads.
    pub channel_shorts: bool,
    /// Remote (provider-hosted) playlists.
    pub playlists: bool,
    /// A discovery feed with no query and no account.
    pub discovery_feed: bool,
    /// Browsable editorial categories, as the site's Explore section lists them.
    pub explore: bool,
    /// Subtitle tracks.
    pub captions: bool,
    /// Chapter markers.
    pub chapters: bool,
    /// Filtering results by upload date, duration and features.
    pub search_filters: bool,
    /// Paging beyond the first page of results.
    pub pagination: bool,
}

impl ProviderCapabilities {
    /// Nothing supported. The starting point for building one up explicitly.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            search_videos: false,
            search_channels: false,
            search_playlists: false,
            search_shorts: false,
            suggestions: false,
            video_details: false,
            related_videos: false,
            channel_details: false,
            channel_videos: false,
            channel_shorts: false,
            playlists: false,
            discovery_feed: false,
            explore: false,
            captions: false,
            chapters: false,
            search_filters: false,
            pagination: false,
        }
    }

    /// Whether the provider can answer any query at all.
    ///
    /// When false the application is effectively offline with respect to this provider, and the UI
    /// shows the local library rather than empty feeds.
    #[must_use]
    pub const fn is_usable(self) -> bool {
        self.search_videos || self.video_details || self.channel_videos
    }

    /// Whether any search surface exists, for deciding whether to render the search field at all.
    #[must_use]
    pub const fn supports_any_search(self) -> bool {
        self.search_videos || self.search_channels || self.search_playlists || self.search_shorts
    }
}

impl Default for ProviderCapabilities {
    fn default() -> Self {
        Self::none()
    }
}

/// What a playback adapter supports.
///
/// Mirrors the TypeScript `PlaybackCapabilities`; the contract fixture test keeps the two in step.
/// Several are false under the sanctioned embed adapter, which is why the player renders without a
/// buffer readout rather than with an inert one.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackCapabilities {
    /// A specific quality tier can be selected.
    pub quality_selection: bool,
    /// The list of available qualities reflects real streams.
    pub reports_available_qualities: bool,
    /// Playback rate can be changed.
    pub playback_rate: bool,
    /// Subtitle tracks can be listed and toggled.
    pub caption_control: bool,
    /// An audio track can be chosen.
    pub audio_track_selection: bool,
    /// Buffer level is observable.
    pub buffer_metrics: bool,
    /// Dropped and decoded frame counts are observable.
    pub frame_metrics: bool,
    /// Picture-in-picture can be entered.
    pub picture_in_picture: bool,
    /// The player can go fullscreen.
    pub fullscreen: bool,
    /// Volume can be set programmatically.
    pub volume_control: bool,
    /// Creator-marked segments can be skipped automatically.
    pub segment_skipping: bool,
}

impl PlaybackCapabilities {
    /// Nothing supported.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            quality_selection: false,
            reports_available_qualities: false,
            playback_rate: false,
            caption_control: false,
            audio_track_selection: false,
            buffer_metrics: false,
            frame_metrics: false,
            picture_in_picture: false,
            fullscreen: false,
            volume_control: false,
            segment_skipping: false,
        }
    }

    /// What the sanctioned embed player supports.
    ///
    /// Buffer and frame metrics are absent because the embed exposes neither. Seeking, rate,
    /// volume, fullscreen and segment skipping all work, because they are driven through the
    /// player API rather than by touching the media element.
    ///
    /// Quality is selectable, though not by the obvious route. `setPlaybackQuality` really is
    /// inert, but the embed chooses its rendition from the size of its own viewport and keeps
    /// doing so during playback, so the shell requests a tier by laying the player's frame out at
    /// the matching width and scaling it back down — measured to move a live player between 360p
    /// and 2160p60 with no reload. `getPlaybackQuality` and `getAvailableQualityLevels` both
    /// answer honestly, and the latter is per-video, so the list offered is the list that exists.
    /// See ADR-0004.
    #[must_use]
    pub const fn embedded_player() -> Self {
        Self {
            quality_selection: true,
            reports_available_qualities: true,
            playback_rate: true,
            caption_control: true,
            audio_track_selection: false,
            buffer_metrics: false,
            frame_metrics: false,
            picture_in_picture: true,
            fullscreen: true,
            volume_control: true,
            segment_skipping: true,
        }
    }

    /// Whether the UI should render a quality control at all.
    #[must_use]
    pub const fn has_quality_menu(self) -> bool {
        self.quality_selection && self.reports_available_qualities
    }
}

impl Default for PlaybackCapabilities {
    fn default() -> Self {
        Self::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_capability_set_is_not_usable() {
        let none = ProviderCapabilities::none();
        assert!(!none.is_usable());
        assert!(!none.supports_any_search());
        assert_eq!(ProviderCapabilities::default(), none);
    }

    #[test]
    fn usability_only_needs_one_working_read_path() {
        let details_only = ProviderCapabilities {
            video_details: true,
            ..ProviderCapabilities::none()
        };
        assert!(details_only.is_usable());
        assert!(
            !details_only.supports_any_search(),
            "usable is not the same as searchable"
        );
    }

    #[test]
    fn the_embed_player_reports_a_quality_menu_but_no_metrics() {
        // A tier is requested by resizing the frame rather than by `setPlaybackQuality`, and the
        // embed reports the tiers each video actually has — so the menu is real. Buffer level and
        // frame counts are exposed by nothing, so those readouts stay absent (§131).
        let embed = PlaybackCapabilities::embedded_player();
        assert!(embed.has_quality_menu());
        assert!(embed.quality_selection);
        assert!(embed.reports_available_qualities);
        assert!(!embed.buffer_metrics);
        assert!(!embed.frame_metrics);
    }

    #[test]
    fn the_embed_player_supports_what_it_actually_can_do() {
        let embed = PlaybackCapabilities::embedded_player();
        assert!(embed.playback_rate);
        assert!(embed.fullscreen);
        assert!(embed.volume_control);
        assert!(embed.picture_in_picture);
        assert!(embed.segment_skipping);
    }

    #[test]
    fn a_quality_menu_needs_both_selection_and_a_real_list() {
        // Selecting a tier is useless without knowing which tiers exist, and vice versa.
        let half = PlaybackCapabilities {
            quality_selection: true,
            ..PlaybackCapabilities::none()
        };
        assert!(!half.has_quality_menu());

        let full = PlaybackCapabilities {
            quality_selection: true,
            reports_available_qualities: true,
            ..PlaybackCapabilities::none()
        };
        assert!(full.has_quality_menu());
    }

    #[test]
    fn capabilities_round_trip_for_the_ipc_boundary() {
        for capabilities in [
            ProviderCapabilities::none(),
            ProviderCapabilities {
                search_videos: true,
                pagination: true,
                ..ProviderCapabilities::none()
            },
        ] {
            let json = serde_json::to_string(&capabilities).expect("serializes");
            let back: ProviderCapabilities = serde_json::from_str(&json).expect("deserializes");
            assert_eq!(back, capabilities);
        }
    }
}
