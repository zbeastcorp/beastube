//! The user settings tree.
//!
//! Every struct carries `#[serde(default)]` and every field has a documented default. That is what
//! makes settings **forward and backward compatible**: a document written by a newer build loads in
//! an older one (unknown keys are ignored), and a document written by an older build loads in a
//! newer one (missing keys take their defaults). A corrupted or partially-written document
//! therefore degrades to defaults rather than preventing startup (§81).
//!
//! Defaults are chosen from measurement or from a stated constraint, never picked arbitrarily
//! (§132); the rationale is recorded on each constant.

use serde::{Deserialize, Serialize};

use crate::model::stream::Quality;

/// Colour scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Follow the operating system's light/dark preference.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
    /// True-black dark, for OLED panels where black pixels are unlit.
    Amoled,
    /// A user-defined token set.
    Custom,
}

/// Vertical spacing scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    /// Balanced spacing.
    #[default]
    Comfortable,
    /// Tighter spacing, fitting roughly a third more rows per screen.
    Compact,
}

/// How aggressively the content-filtering subsystem acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilteringMode {
    /// No filtering is applied. Rules are still loaded so the mode can be changed without a
    /// restart, but nothing is matched.
    Off,
    /// Filters what can be identified with high confidence. Anything ambiguous is allowed, because
    /// preserving playback outranks filtering aggressiveness (§10).
    #[default]
    Standard,
    /// Additionally filters lower-confidence matches. Documented as more likely to produce false
    /// positives; playback-critical requests are still never blocked.
    Strict,
}

impl FilteringMode {
    /// Stable identifier for persistence and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Standard => "standard",
            Self::Strict => "strict",
        }
    }

    /// Whether any matching should occur at all.
    #[must_use]
    pub const fn is_active(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// Appearance and layout.
///
/// `Eq` is not derived: `ui_scale` is an `f32`, and float equality is not an equivalence relation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    /// Colour scheme.
    pub theme: Theme,
    /// Accent colour as an `#rrggbb` string. Validated by the UI before it reaches a CSS variable.
    pub accent: String,
    /// Row spacing.
    pub density: Density,
    /// Start with the sidebar collapsed.
    pub sidebar_collapsed: bool,
    /// Suppress non-essential animation.
    ///
    /// `None` means "follow the OS `prefers-reduced-motion` setting", which is the correct default:
    /// a user who set it system-wide should not have to set it again here.
    pub reduced_motion: Option<bool>,
    /// UI scale multiplier, clamped to `0.75..=2.0` on load.
    pub ui_scale: f32,
    /// BCP 47 language tag for the interface, or `None` to follow the OS locale.
    pub language: Option<String>,
    /// Cast a soft glow from the video's colours behind the player on the watch page.
    ///
    /// Decorative, and on by default because it is what the surface looks like when nobody has an
    /// opinion. Suppressed independently of `reduced_motion`: the glow does not move, so someone
    /// who wants less animation has not asked to lose it.
    pub ambient_mode: bool,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            // The product's accent; a concrete default avoids an unstyled first paint.
            accent: "#ff5c5c".to_owned(),
            density: Density::Comfortable,
            sidebar_collapsed: false,
            reduced_motion: None,
            ui_scale: 1.0,
            language: None,
            ambient_mode: true,
        }
    }
}

/// Which control bar the player wears.
///
/// Not a cosmetic preference — the two are a genuine trade, which is why both exist rather than one
/// being chosen for the user (ADR-0004).
///
/// [`PlayerControls::Beastube`] crops YouTube's chrome away and draws the application's own dark
/// controls in its place. Quality is then requested by resizing the player's frame, which reaches
/// 360p through 2160p at 60fps but cannot go lower and takes a few seconds to settle.
///
/// [`PlayerControls::Youtube`] leaves the embed's own bar in place. Its gear drives the player's
/// internal quality API directly, so switching is exact, immediate, and spans the full 144p–2160p
/// range. The cost is that the panel it opens is YouTube's own document and styles itself from the
/// operating system, so it renders white over a dark application and no stylesheet here can reach
/// it; the title band and watermark return with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerControls {
    /// The application's own dark controls, with YouTube's chrome cropped away.
    Beastube,
    /// YouTube's own control bar, including its native quality menu.
    #[default]
    Youtube,
}

/// Playback behaviour.
///
/// `Eq` is not derived: `volume` and `speed` are floats.
// A settings record is precisely the case where independent boolean switches are the right shape:
// each maps to one user-visible toggle and they are not mutually exclusive.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaybackSettings {
    /// Quality to request when opening a video.
    pub default_quality: Quality,
    /// Ceiling applied to adaptive selection, so `Auto` on a metered connection cannot climb to 4K.
    pub max_quality: Quality,
    /// Volume in `0.0..=1.0`.
    pub volume: f32,
    /// Whether audio is muted.
    pub muted: bool,
    /// Playback rate multiplier, clamped to `0.25..=4.0` on load.
    pub speed: f32,
    /// Start the next item automatically when one ends.
    pub autoplay_next: bool,
    /// Begin playing as soon as a video page opens.
    pub autoplay_on_open: bool,
    /// Restore the stored position when reopening a partially-watched video.
    pub resume_playback: bool,
    /// Show captions by default when a track matching the preferred language exists.
    pub captions_enabled: bool,
    /// Preferred caption language as a BCP 47 tag, or `None` to follow the interface language.
    pub caption_language: Option<String>,
    /// Preferred audio track language, or `None` to use the provider's default track.
    pub audio_language: Option<String>,
    /// Allow hardware-accelerated decoding.
    ///
    /// Exposed because a broken GPU driver is a real and common failure mode on Windows, and the
    /// only reliable user-side remedy is to fall back to software decoding.
    pub hardware_acceleration: bool,
    /// Seconds skipped by the short-seek controls.
    pub seek_step_seconds: u32,
    /// Seconds skipped by the long-seek controls.
    pub seek_step_large_seconds: u32,
    /// Which control bar the player wears. See [`PlayerControls`].
    pub player_controls: PlayerControls,
}

impl Default for PlaybackSettings {
    fn default() -> Self {
        Self {
            default_quality: Quality::Auto,
            // 1080p ceiling by default: above it, bitrate roughly doubles per tier for a difference
            // most users cannot see in a windowed desktop player. Users who want more can raise it.
            max_quality: Quality::P1080,
            volume: 1.0,
            muted: false,
            speed: 1.0,
            autoplay_next: true,
            autoplay_on_open: true,
            resume_playback: true,
            captions_enabled: false,
            caption_language: None,
            audio_language: None,
            hardware_acceleration: true,
            // Matches the long-standing convention of arrow-key = 5 s, J/L = 10 s, which users
            // arrive with from every other player.
            seek_step_seconds: 5,
            seek_step_large_seconds: 10,
            // YouTube's own bar by default. The application's dark controls are the better-looking
            // option and remain one setting away, but the embed's own gear drives the player's
            // internal quality API directly: every tier from 144p up, applied the instant it is
            // chosen, with no reload and no waiting for a buffer to drain. Quality is the thing
            // people actually reach for, so it wins the default and appearance yields to it.
            player_controls: PlayerControls::Youtube,
        }
    }
}

/// Local data retention. Defaults are chosen so the application is useful without recording
/// anything the user has not asked it to record.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacySettings {
    /// Record watch history locally.
    pub history_enabled: bool,
    /// Record search queries locally, for suggestions and re-running searches.
    pub search_history_enabled: bool,
    /// Use local history to rank the home feed.
    pub local_recommendations_enabled: bool,
    /// Start every session in incognito mode.
    pub incognito_by_default: bool,
    /// Delete history rows older than this many days. `None` keeps history indefinitely.
    pub history_retention_days: Option<u32>,
    /// Maximum number of stored search queries.
    pub max_search_history_entries: u32,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            // History and resume are the point of a local library, so they are on; every one of
            // them is local-only and can be cleared or disabled from the privacy screen.
            history_enabled: true,
            search_history_enabled: true,
            local_recommendations_enabled: true,
            incognito_by_default: false,
            history_retention_days: None,
            // Bounded so the suggestions index stays small and the table cannot grow without limit
            // on a machine used for years.
            max_search_history_entries: 500,
        }
    }
}

/// Content-filtering configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilteringSettings {
    /// Master switch. When off, no rule is evaluated.
    pub enabled: bool,
    /// How aggressive matching is.
    pub mode: FilteringMode,
    /// Fetch rule updates automatically.
    pub auto_update_rules: bool,
    /// Minimum interval between update checks, in hours.
    pub update_interval_hours: u32,
    /// Hosts and patterns the user always permits, evaluated before every block rule.
    pub allowlist: Vec<String>,
    /// Hosts and patterns the user always blocks.
    pub blocklist: Vec<String>,
    /// User-authored rules, applied after the shipped rule set.
    pub custom_rules: Vec<String>,
}

impl Default for FilteringSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: FilteringMode::Standard,
            auto_update_rules: true,
            // Twice daily. Rule sets change on the order of days, so more frequent checks would be
            // network traffic without benefit.
            update_interval_hours: 12,
            allowlist: Vec::new(),
            blocklist: Vec::new(),
            custom_rules: Vec::new(),
        }
    }
}

/// Network behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkSettings {
    /// Maximum simultaneous requests to one host.
    pub max_concurrent_requests: u32,
    /// Timeout for a complete metadata request, in seconds.
    pub request_timeout_seconds: u32,
    /// Timeout for establishing a connection, in seconds.
    pub connect_timeout_seconds: u32,
    /// Maximum automatic retries for a retryable failure.
    pub max_retries: u32,
    /// Prefetch metadata and thumbnails for content likely to be opened next.
    pub prefetch_enabled: bool,
    /// Suspend prefetch and other deferrable work while on battery.
    pub reduce_activity_on_battery: bool,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            // Six matches the per-host connection limit browsers settled on; beyond it, added
            // parallelism mostly increases queueing latency rather than throughput.
            max_concurrent_requests: 6,
            request_timeout_seconds: 30,
            // Short enough that an unreachable host surfaces quickly rather than appearing to hang.
            connect_timeout_seconds: 10,
            // Three attempts covers transient loss; more turns a hard failure into a long stall.
            max_retries: 3,
            prefetch_enabled: true,
            reduce_activity_on_battery: true,
        }
    }
}

/// Cache sizing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheSettings {
    /// Ceiling for in-memory cached objects, in mebibytes.
    pub memory_budget_mb: u32,
    /// Ceiling for the on-disk cache, in mebibytes.
    pub disk_budget_mb: u32,
    /// Lifetime of cached metadata, in hours.
    pub metadata_ttl_hours: u32,
    /// Lifetime of cached thumbnails, in days.
    pub thumbnail_ttl_days: u32,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            // Bounded so the resident set stays predictable during long sessions (§84). Large
            // enough to hold roughly a thousand decoded card thumbnails.
            memory_budget_mb: 192,
            disk_budget_mb: 1024,
            // Metadata such as view counts drifts slowly; a day-scale TTL avoids refetching a video
            // page the user reopens, while staying fresh enough not to look wrong.
            metadata_ttl_hours: 12,
            thumbnail_ttl_days: 30,
        }
    }
}

/// Video downloads.
///
/// Paths are stored as strings because the document crosses the IPC boundary as JSON and is
/// edited from the UI. They are checked in [`Settings::sanitized`], where anything that is not an
/// absolute path becomes "not set" rather than a relative path resolved against whatever the
/// process's working directory happens to be — Program Files, for an installed build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadSettings {
    /// Directory downloads are saved into. `None` means a `BEASTUBE` folder inside the user's
    /// Downloads folder, resolved by the shell.
    pub directory: Option<String>,
    /// Ceiling on the video height requested. `Auto` means the best available.
    pub max_quality: Quality,
    /// Path to the downloader executable (`yt-dlp`). `None` means it is discovered beside the
    /// application or on `PATH`.
    pub tool_path: Option<String>,
    /// Path to `ffmpeg`, which joins separate video and audio tracks. `None` means discovered.
    pub ffmpeg_path: Option<String>,
}

impl Default for DownloadSettings {
    fn default() -> Self {
        Self {
            directory: None,
            // The same ceiling as playback, for the same reason: above 1080p the file roughly
            // doubles per tier for a difference few screens show. Users who want more raise it.
            max_quality: Quality::P1080,
            tool_path: None,
            ffmpeg_path: None,
        }
    }
}

/// The complete settings document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Schema version, used to drive migrations.
    pub version: u32,
    /// Appearance and layout.
    pub appearance: AppearanceSettings,
    /// Playback behaviour.
    pub playback: PlaybackSettings,
    /// Local data retention.
    pub privacy: PrivacySettings,
    /// Content filtering.
    pub filtering: FilteringSettings,
    /// Network behaviour.
    pub network: NetworkSettings,
    /// Cache sizing.
    pub cache: CacheSettings,
    /// Video downloads.
    pub downloads: DownloadSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: crate::CONTRACT_VERSION,
            appearance: AppearanceSettings::default(),
            playback: PlaybackSettings::default(),
            privacy: PrivacySettings::default(),
            filtering: FilteringSettings::default(),
            network: NetworkSettings::default(),
            cache: CacheSettings::default(),
            downloads: DownloadSettings::default(),
        }
    }
}

impl Settings {
    /// Clamps every bounded field into its valid range.
    ///
    /// Applied after loading, because a settings file can be hand-edited, written by a different
    /// build, or truncated by a crash. Clamping rather than rejecting keeps a single bad value from
    /// discarding every other setting the user chose.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        self.appearance.ui_scale = clamp_finite(self.appearance.ui_scale, 0.75, 2.0, 1.0);
        self.playback.volume = clamp_finite(self.playback.volume, 0.0, 1.0, 1.0);
        self.playback.speed = clamp_finite(self.playback.speed, 0.25, 4.0, 1.0);
        self.playback.seek_step_seconds = self.playback.seek_step_seconds.clamp(1, 60);
        self.playback.seek_step_large_seconds = self.playback.seek_step_large_seconds.clamp(1, 300);
        self.network.max_concurrent_requests = self.network.max_concurrent_requests.clamp(1, 32);
        self.network.request_timeout_seconds = self.network.request_timeout_seconds.clamp(5, 300);
        self.network.connect_timeout_seconds = self.network.connect_timeout_seconds.clamp(1, 120);
        self.network.max_retries = self.network.max_retries.min(10);
        self.filtering.update_interval_hours = self.filtering.update_interval_hours.clamp(1, 168);
        self.cache.memory_budget_mb = self.cache.memory_budget_mb.clamp(32, 2048);
        self.cache.disk_budget_mb = self.cache.disk_budget_mb.clamp(64, 65_536);
        self.cache.metadata_ttl_hours = self.cache.metadata_ttl_hours.clamp(1, 720);
        self.cache.thumbnail_ttl_days = self.cache.thumbnail_ttl_days.clamp(1, 365);
        self.privacy.max_search_history_entries =
            self.privacy.max_search_history_entries.clamp(0, 10_000);
        self.downloads.directory = absolute_or_none(self.downloads.directory.take());
        self.downloads.tool_path = absolute_or_none(self.downloads.tool_path.take());
        self.downloads.ffmpeg_path = absolute_or_none(self.downloads.ffmpeg_path.take());

        // A ceiling below the default request would silently pin every video to the ceiling; the
        // two are reconciled here rather than at each playback start.
        if let (Some(default_height), Some(max_height)) = (
            self.playback.default_quality.height(),
            self.playback.max_quality.height(),
        ) && default_height > max_height
        {
            self.playback.default_quality = self.playback.max_quality;
        }
        self
    }

    /// Whether any local-history recording is enabled.
    ///
    /// Used by the privacy screen to summarize state in one line.
    #[must_use]
    pub const fn records_anything_locally(&self) -> bool {
        self.privacy.history_enabled || self.privacy.search_history_enabled
    }
}

/// Clamps `value` into `min..=max`, substituting `fallback` for NaN and infinities.
///
/// A NaN reaching a CSS variable or an audio gain node produces silent, hard-to-trace breakage, so
/// non-finite values are replaced rather than clamped (`f32::clamp` panics on a NaN bound and
/// propagates a NaN input).
fn clamp_finite(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

/// `Some(path)` only when `value` is a non-empty absolute path.
///
/// An empty string is what a cleared text field sends, and a relative path would be resolved
/// against the working directory. Both mean "not set", which is what discovery then handles.
fn absolute_or_none(value: Option<String>) -> Option<String> {
    let value = value?;
    let trimmed = value.trim();
    (!trimmed.is_empty() && std::path::Path::new(trimmed).is_absolute())
        .then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_paths_must_be_absolute_or_are_dropped() {
        let absolute = std::env::temp_dir().join("ffmpeg").display().to_string();
        let document = serde_json::json!({
            "downloads": {"directory": "  ", "tool_path": "yt-dlp.exe", "ffmpeg_path": absolute}
        });
        let loaded: Settings = serde_json::from_value(document).unwrap();
        let loaded = loaded.sanitized();

        assert_eq!(loaded.downloads.directory, None, "blank means unset");
        assert_eq!(
            loaded.downloads.tool_path, None,
            "a bare name would resolve against the working directory"
        );
        assert_eq!(loaded.downloads.ffmpeg_path, Some(absolute));
        assert_eq!(loaded.downloads.max_quality, Quality::P1080);
    }

    #[test]
    fn defaults_are_already_sanitized() {
        let defaults = Settings::default();
        assert_eq!(defaults.clone().sanitized(), defaults);
    }

    #[test]
    fn an_empty_document_loads_as_defaults() {
        let loaded: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn a_partial_document_keeps_its_values_and_defaults_the_rest() {
        let loaded: Settings =
            serde_json::from_str(r#"{"playback":{"volume":0.5,"muted":true}}"#).unwrap();
        assert!((loaded.playback.volume - 0.5).abs() < f32::EPSILON);
        assert!(loaded.playback.muted);
        // Untouched fields still take their defaults.
        assert_eq!(loaded.playback.default_quality, Quality::Auto);
        assert_eq!(loaded.appearance.theme, Theme::System);
    }

    #[test]
    fn unknown_keys_from_a_newer_build_are_ignored() {
        let loaded: Settings = serde_json::from_str(
            r#"{"version":1,"playback":{"volume":0.25},"future_section":{"x":1},"playback_extra":true}"#,
        )
        .unwrap();
        assert!((loaded.playback.volume - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn out_of_range_values_are_clamped_not_rejected() {
        let hostile: Settings = serde_json::from_str(
            r#"{"playback":{"volume":9.0,"speed":-4.0,"seek_step_seconds":9999},
                "network":{"max_concurrent_requests":10000,"max_retries":999},
                "cache":{"memory_budget_mb":1,"disk_budget_mb":99999999}}"#,
        )
        .unwrap();
        let safe = hostile.sanitized();

        assert!((safe.playback.volume - 1.0).abs() < f32::EPSILON);
        assert!((safe.playback.speed - 0.25).abs() < f32::EPSILON);
        assert_eq!(safe.playback.seek_step_seconds, 60);
        assert_eq!(safe.network.max_concurrent_requests, 32);
        assert_eq!(safe.network.max_retries, 10);
        assert_eq!(safe.cache.memory_budget_mb, 32);
        assert_eq!(safe.cache.disk_budget_mb, 65_536);
    }

    #[test]
    fn non_finite_floats_fall_back_instead_of_propagating_nan() {
        let mut settings = Settings::default();
        settings.playback.volume = f32::NAN;
        settings.playback.speed = f32::INFINITY;
        settings.appearance.ui_scale = f32::NEG_INFINITY;

        let safe = settings.sanitized();
        assert!(safe.playback.volume.is_finite());
        assert!((safe.playback.volume - 1.0).abs() < f32::EPSILON);
        assert!((safe.playback.speed - 1.0).abs() < f32::EPSILON);
        assert!((safe.appearance.ui_scale - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_default_quality_above_the_ceiling_is_lowered_to_it() {
        let mut settings = Settings::default();
        settings.playback.default_quality = Quality::P2160;
        settings.playback.max_quality = Quality::P720;

        let safe = settings.sanitized();
        assert_eq!(safe.playback.default_quality, Quality::P720);
    }

    #[test]
    fn auto_quality_is_never_rewritten_by_the_ceiling() {
        let mut settings = Settings::default();
        settings.playback.default_quality = Quality::Auto;
        settings.playback.max_quality = Quality::P480;

        let safe = settings.sanitized();
        assert_eq!(
            safe.playback.default_quality,
            Quality::Auto,
            "Auto is a mode, not a tier, and the ceiling constrains it at selection time"
        );
    }

    #[test]
    fn settings_round_trip_through_json() {
        let settings = Settings::default();
        let json = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(settings, back);
    }

    #[test]
    fn filtering_off_disables_matching_entirely() {
        assert!(!FilteringMode::Off.is_active());
        assert!(FilteringMode::Standard.is_active());
        assert!(FilteringMode::Strict.is_active());
        assert_eq!(FilteringMode::default(), FilteringMode::Standard);
    }

    #[test]
    fn privacy_summary_reflects_both_history_switches() {
        let mut settings = Settings::default();
        assert!(settings.records_anything_locally());

        settings.privacy.history_enabled = false;
        assert!(settings.records_anything_locally());

        settings.privacy.search_history_enabled = false;
        assert!(!settings.records_anything_locally());
    }
}
