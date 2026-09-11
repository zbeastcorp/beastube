/**
 * The TypeScript mirror of `beastube-core`'s domain model.
 *
 * These types are hand-written rather than generated. The tradeoff is deliberate: a code generator
 * would add a build step, a dependency and a class of "the generator is stale" failures, for types
 * that change rarely and are read constantly. Hand-writing them keeps the wire contract legible on
 * both sides.
 *
 * Drift is *not* currently caught by a test, and this comment used to claim it was — it described
 * `tests/fixtures/contract/*.json`, a Rust test serializing each shape and a TypeScript test
 * parsing it. No such fixture or test has ever existed here. A comment promising a safety net that
 * is not there is worse than no comment, because it is read as a reason to relax.
 *
 * What actually holds the two sides together today is this file being written by hand against
 * `beastube-core`, and a rename on either side producing a compile error at the first call site
 * that touches the field. That is real but partial: it will not catch a `serde` rename or a new
 * enum variant. A contract fixture remains worth building.
 *
 * Conventions carried over from Rust:
 * - Timestamps are Unix milliseconds (`number`), directly usable as `new Date(value)`.
 * - Durations and positions are milliseconds.
 * - An absent optional field means *unknown*, not zero. Render nothing rather than a confident 0.
 */

// ---------------------------------------------------------------------------------------------
// Identifiers
//
// Branded so a ChannelId cannot be passed where a VideoId is expected. The brand exists only at
// compile time; at runtime these are plain strings.
// ---------------------------------------------------------------------------------------------

declare const brand: unique symbol;
type Brand<T, B> = T & { readonly [brand]: B };

/** Provider identifier of a video. */
export type VideoId = Brand<string, 'VideoId'>;
/** Provider identifier of a channel. */
export type ChannelId = Brand<string, 'ChannelId'>;
/** Provider identifier of a remote playlist. */
export type PlaylistId = Brand<string, 'PlaylistId'>;
/** Row identifier of a locally created playlist. */
export type LocalPlaylistId = Brand<number, 'LocalPlaylistId'>;

/** The alphabet Rust validates identifiers against (RFC 4648 URL/filename-safe). */
const ID_PATTERN = /^[A-Za-z0-9_-]+$/;

/** Longest identifier accepted, matching the Rust bound for videos and channels. */
const ID_MAX_LENGTH = 64;

/**
 * Validates and brands a video identifier.
 *
 * Mirrors the Rust validation so an identifier built from user input or a deep link is rejected
 * here, before it reaches IPC — the same check runs again in Rust, since the frontend is not a
 * trusted validator.
 */
export function toVideoId(raw: string): VideoId | null {
  return raw.length > 0 && raw.length <= ID_MAX_LENGTH && ID_PATTERN.test(raw)
    ? (raw as VideoId)
    : null;
}

/** Validates and brands a channel identifier. */
export function toChannelId(raw: string): ChannelId | null {
  return raw.length > 0 && raw.length <= ID_MAX_LENGTH && ID_PATTERN.test(raw)
    ? (raw as ChannelId)
    : null;
}

/** Validates and brands a playlist identifier (longer bound, matching Rust). */
export function toPlaylistId(raw: string): PlaylistId | null {
  return raw.length > 0 && raw.length <= 128 && ID_PATTERN.test(raw) ? (raw as PlaylistId) : null;
}

/** Brands a local playlist row id. */
export function toLocalPlaylistId(raw: number): LocalPlaylistId {
  return raw as LocalPlaylistId;
}

// ---------------------------------------------------------------------------------------------
// Media
// ---------------------------------------------------------------------------------------------

/** One rendition of a thumbnail. */
export interface Thumbnail {
  url: string;
  width?: number;
  height?: number;
}

/** Renditions of one image, ascending by width. */
export type ThumbnailSet = Thumbnail[];

/** YouTube's original ceiling for a Short, and still the length nothing longer can be. */
const SHORT_MAX_DURATION_MS = 60_000;

/**
 * Whether a video should be *presented* as short-form.
 *
 * Three signals, because no one of them is sufficient:
 *
 * 1. **The extractor's marker**, when set. It is authoritative — but it is derived from the
 *    renderer shape the response happened to use, so the same video arrives marked from a
 *    shorts-filtered search and unmarked from a related-videos list.
 * 2. **The thumbnail's dimensions**, when they are portrait. Not decisive on their own either: some
 *    renditions of a short are padded to 16:9 with the portrait frame boxed inside them, so a
 *    landscape rendition does not mean a landscape video.
 * 3. **Duration**, under a minute. Nothing longer is a Short, and in practice this is what catches
 *    the ones the first two miss.
 *
 * ## Why the third signal is acceptable here and not everywhere
 *
 * It admits false positives: a genuinely landscape forty-second video is treated as short-form. The
 * cost of that is a card in the wrong shape or an item in the Shorts shelf instead of the grid —
 * visible, minor, recoverable. The cost of a false *negative* is the bug being fixed: a portrait
 * video with grey bars either side of it in a landscape card.
 *
 * The Shorts tab does not use this. There a false positive means a landscape video in a vertical
 * player with black bars top and bottom, which is worse than a shorter feed, so it holds out for
 * the marker alone.
 */
export function isPortraitVideo(
  video: Pick<VideoSummary, 'thumbnails' | 'is_short' | 'duration_ms'>,
): boolean {
  if (video.is_short === true) return true;
  // Comfortably below square, so a 4:3 or 1:1 thumbnail is not mistaken for a short.
  if (videoAspectRatio(video) < 0.9) return true;
  return (
    video.duration_ms !== undefined &&
    video.duration_ms > 0 &&
    video.duration_ms <= SHORT_MAX_DURATION_MS
  );
}

/**
 * The aspect ratio to give a video's player frame, as a plain number.
 *
 * The IFrame Player API cannot report a video's intrinsic dimensions — the embed is a cross-origin
 * iframe, so its `<video>` element and `videoWidth`/`videoHeight` are unreachable — which leaves the
 * thumbnail as the only signal available. Not every short is 9:16; measuring real YouTube showed a
 * 3:4 reel in a container sized to match it, not letterboxed inside a forced portrait box.
 *
 * Guarded on two sides. Renditions without both dimensions are skipped, and a ratio outside a
 * plausible band is rejected as a padded placeholder rather than trusted — some renditions are 4:3
 * with black bars baked in, and one code path reports a hardcoded 320x180 for every video. When
 * nothing usable survives, `is_short` decides the fallback.
 */
export function videoAspectRatio(video: Pick<VideoSummary, 'thumbnails' | 'is_short'>): number {
  const fallback = video.is_short === true ? 9 / 16 : 16 / 9;

  const sized = (video.thumbnails ?? []).filter(
    (thumbnail) => thumbnail.width !== undefined && thumbnail.height !== undefined,
  );
  // The largest, because a small rendition is the one most likely to be a padded square.
  const best = sized.reduce<Thumbnail | undefined>(
    (widest, thumbnail) =>
      widest === undefined || (thumbnail.width ?? 0) > (widest.width ?? 0) ? thumbnail : widest,
    undefined,
  );
  if (best?.width === undefined || best.height === undefined || best.height <= 0) return fallback;

  const ratio = best.width / best.height;
  return ratio >= 0.3 && ratio <= 2.5 ? ratio : fallback;
}

/**
 * The smallest rendition at least `targetWidth` wide, falling back to the largest available.
 *
 * Mirrors `ThumbnailSet::best_for_width`. Selecting the smallest sufficient rendition rather than
 * the largest is what keeps a virtualized grid from pulling oversized images for small cards.
 */
/** Below this width, what came back is the provider's grey "no thumbnail" placeholder. */
export const PLACEHOLDER_WIDTH_THRESHOLD = 160;

export function bestThumbnailFor(set: ThumbnailSet, targetWidth: number): Thumbnail | undefined {
  const sufficient = set.find((t) => t.width !== undefined && t.width >= targetWidth);
  if (sufficient) return sufficient;
  const sized = [...set].reverse().find((t) => t.width !== undefined);
  return sized ?? set.at(-1);
}

/** Live-broadcast state. */
export type LiveStatus = 'not_live' | 'live' | 'upcoming' | 'was_live';

/** Video quality tier. `auto` is a selection mode, not a resolution. */
export type Quality =
  'auto' | '144p' | '240p' | '360p' | '480p' | '720p' | '1080p' | '1440p' | '2160p';

/** Video codec family. */
export type VideoCodec = 'h264' | 'vp9' | 'av1' | 'other';

/** Audio codec family. */
export type AudioCodec = 'aac' | 'opus' | 'other';

/** The compact shape used by every card and list. */
export interface VideoSummary {
  id: VideoId;
  title: string;
  channel_id?: ChannelId;
  channel_name?: string;
  /**
   * The channel's avatar, when the provider attaches one to the item.
   *
   * Carried on the video rather than looked up per card: the provider already sends it with every
   * search and related result, so a card that fetched it would be one request per tile for a
   * picture that arrived with the tile.
   */
  channel_avatar?: ThumbnailSet;
  /** Whether the provider marks the channel as verified. Never inferred. */
  channel_verified?: boolean;
  thumbnails?: ThumbnailSet;
  duration_ms?: number;
  published_at?: number;
  published_text?: string;
  view_count?: number;
  live_status?: LiveStatus;
  is_short?: boolean;
}

/** A named position within a video. */
export interface Chapter {
  title: string;
  start_ms: number;
  thumbnails?: ThumbnailSet;
}

/**
 * One line of a subtitle track, with the window it is shown for.
 *
 * BEASTUBE draws captions itself: the embedded player draws its own inside a cross-origin frame,
 * where nothing outside can move, resize or restyle them.
 */
export interface Cue {
  start_ms: number;
  end_ms: number;
  /** Untrusted text. Rendered as text, never as HTML. */
  text: string;
}

/** A subtitle track. */
export interface CaptionTrack {
  language_code: string;
  language_name: string;
  url: string;
  is_auto_generated?: boolean;
}

/**
 * One of the audio tracks a video offers.
 *
 * Videos are increasingly published with the original audio plus dubs in other languages. A video
 * with a single track reports none of these: there is nothing to choose between.
 */
export interface AudioTrack {
  /** Provider identifier, e.g. `es.3`. Opaque. */
  id: string;
  language_code?: string;
  /** Display name, already localized upstream. Rendered as opaque text. */
  language_name: string;
  is_default?: boolean;
  /** The video's own audio rather than a translation of it. */
  is_original?: boolean;
}

/** The full shape used by the watch page. Flattens {@link VideoSummary} on the wire. */
export interface VideoDetails extends VideoSummary {
  description?: string;
  channel_avatar?: ThumbnailSet;
  channel_subscriber_count?: number;
  like_count?: number;
  chapters?: Chapter[];
  captions?: CaptionTrack[];
  /** Alternative audio tracks, when the video has more than one. Absent when there is no choice. */
  audio_tracks?: AudioTrack[];
  category?: string;
  is_unlisted?: boolean;
  is_age_restricted?: boolean;
}

/** Channel tabs that actually have content. */
export type ChannelTab = 'videos' | 'shorts' | 'live' | 'playlists';

/**
 * The browsable categories, mirroring `ExploreCategory` in the core crate.
 *
 * The site's Explore list also carries Trending, Movies & TV and Podcasts. None of the three can be
 * served without an account or returns any video at all, so none is a member here — see the Rust
 * enum for the measurements behind that.
 */
export type ExploreCategory =
  'music' | 'gaming' | 'live' | 'news' | 'sport' | 'learning' | 'fashion';

/** The compact shape used by channel cards. */
export interface ChannelSummary {
  id: ChannelId;
  name: string;
  avatar?: ThumbnailSet;
  subscriber_count?: number;
  handle?: string;
  is_verified?: boolean;
}

/** The full shape used by the channel page. */
/** A link the owner published on their About tab. Always `https`; the adapter drops the rest. */
export interface ChannelLink {
  title: string;
  url: string;
}

export interface ChannelDetails extends ChannelSummary {
  description?: string;
  banner?: ThumbnailSet;
  available_tabs?: ChannelTab[];
  video_count?: number;
  canonical_url?: string;
  links?: ChannelLink[];
  view_count?: number;
  /** Unix milliseconds at midnight UTC on the day the channel was created. Render as a date. */
  joined_at?: number;
  /** ISO 3166-1 alpha-2, localised for display rather than shown raw. */
  country?: string;
}

/** The compact shape used by playlist cards. */
export interface PlaylistSummary {
  id: PlaylistId;
  title: string;
  channel_id?: ChannelId;
  channel_name?: string;
  thumbnails?: ThumbnailSet;
  video_count?: number;
}

/** The full shape used by the playlist page. */
export interface PlaylistDetails extends PlaylistSummary {
  description?: string;
  videos?: VideoSummary[];
}

// ---------------------------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------------------------

/** An opaque provider cursor. Never interpreted by the frontend. */
export type ContinuationToken = Brand<string, 'ContinuationToken'>;

/** One page of a paginated collection. */
export interface Page<T> {
  items: T[];
  continuation?: ContinuationToken;
  total_estimate?: number;
}

// ---------------------------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------------------------

export type SearchResultKind = 'all' | 'videos' | 'shorts' | 'channels' | 'playlists' | 'live';
export type UploadDateFilter =
  'any' | 'last_hour' | 'today' | 'this_week' | 'this_month' | 'this_year';
export type VideoDurationFilter = 'any' | 'short' | 'medium' | 'long';
export type SearchSortOrder = 'relevance' | 'upload_date' | 'view_count' | 'rating';
export type VideoFeatureFilter =
  'subtitles' | 'high_definition' | 'ultra_high_definition' | 'hdr' | 'live' | 'vr360';

/** A complete search request. */
export interface SearchFilters {
  kind?: SearchResultKind;
  upload_date?: UploadDateFilter;
  duration?: VideoDurationFilter;
  sort_by?: SearchSortOrder;
  features?: VideoFeatureFilter[];
  channel?: ChannelId;
}

/** One heterogeneous search result. Externally tagged by `type`. */
export type SearchItem =
  | ({ type: 'video' } & VideoSummary)
  | ({ type: 'channel' } & ChannelSummary)
  | ({ type: 'playlist' } & PlaylistSummary);

/** A page of search results plus the query that produced it. */
export interface SearchResults {
  query: string;
  filters: SearchFilters;
  page: Page<SearchItem>;
  estimated_total?: number;
  corrected_query?: string;
}

/** One autocomplete suggestion. */
export interface Suggestion {
  text: string;
  from_history?: boolean;
}

// ---------------------------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------------------------

export type WatchState = 'unwatched' | 'in_progress' | 'completed';

/** A stored playback position. */
export interface PlaybackPosition {
  position_ms: number;
  duration_ms?: number;
  updated_at: number;
}

/** One row of local watch history. */
export interface HistoryEntry {
  video_id: VideoId;
  title: string;
  channel_id?: ChannelId;
  channel_name?: string;
  thumbnails?: ThumbnailSet;
  position: PlaybackPosition;
  /** Views when it was watched, so a history card shows the same line every other card shows. */
  view_count?: number;
  /** Publication time, when an absolute one was reported. */
  published_at?: number;
  /** The channel's avatar as it was when watched, so the card draws without a lookup. */
  channel_avatar?: ThumbnailSet;
  first_watched_at: number;
  last_watched_at: number;
  play_count: number;
}

/** A user-saved bookmark. */
export interface Bookmark {
  video_id: VideoId;
  title: string;
  channel_id?: ChannelId;
  channel_name?: string;
  thumbnails?: ThumbnailSet;
  note?: string;
  tags?: string[];
  timestamp_ms?: number;
  created_at: number;
}

/** A playlist the user created locally. */
export interface LocalPlaylist {
  id: LocalPlaylistId;
  name: string;
  description?: string;
  item_count: number;
  created_at: number;
  updated_at: number;
  thumbnails?: ThumbnailSet;
  is_system?: boolean;
}

/** One entry in a local playlist. */
export interface PlaylistItem {
  video: VideoSummary;
  position: number;
  added_at: number;
}

// ---------------------------------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------------------------------

/** Explicit playback lifecycle states. Mirrors `beastube_core::PlaybackState`. */
export type PlaybackState =
  'idle' | 'loading' | 'ready' | 'playing' | 'paused' | 'buffering' | 'seeking' | 'ended' | 'error';

/**
 * What the active playback adapter can actually do.
 *
 * The UI renders a control only where the corresponding flag is true. This is the mechanism behind
 * No fake features: under the IFrame adapter `buffer_metrics` is false, so the buffer
 * readout is absent rather than present and showing nothing.
 */
export interface PlaybackCapabilities {
  /** A specific quality tier can be selected. */
  quality_selection: boolean;
  /** `available_qualities` reflects real streams. */
  reports_available_qualities: boolean;
  /** Playback rate can be changed. */
  playback_rate: boolean;
  /** Subtitle tracks can be listed and toggled. */
  caption_control: boolean;
  /** An audio track can be chosen. */
  audio_track_selection: boolean;
  /** Buffer level is observable. */
  buffer_metrics: boolean;
  /** Dropped/decoded frame counts are observable. */
  frame_metrics: boolean;
  /** Picture-in-picture can be entered. */
  picture_in_picture: boolean;
  /** The element can go fullscreen. */
  fullscreen: boolean;
  /** Volume can be set programmatically. */
  volume_control: boolean;
  /** Creator-marked segments can be skipped automatically. */
  segment_skipping: boolean;
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

export type ErrorKind =
  | 'network'
  | 'provider'
  | 'playback'
  | 'database'
  | 'cache'
  | 'filesystem'
  | 'permission'
  | 'configuration'
  | 'filtering'
  | 'update';

/** What to do about a failure. Mirrors `beastube_core::error::Recovery`. */
export type Recovery =
  | { strategy: 'retry_automatic'; delay_ms: number; attempts_made: number; max_attempts: number }
  | { strategy: 'retry_manual' }
  | { strategy: 'fallback'; message_key: string }
  | { strategy: 'adjust_settings'; settings_path: string }
  | { strategy: 'rebuild_local_data'; store: string }
  | { strategy: 'await_connectivity' }
  | { strategy: 'unrecoverable' };

/**
 * A failure as it crosses IPC.
 *
 * Carries no English: `message_key` is resolved by the localization layer, and `diagnostic` is
 * engineer-facing detail shown only on the diagnostics screen.
 */
export interface ErrorPayload {
  kind: ErrorKind;
  code: string;
  message_key: string;
  params?: Record<string, string>;
  recovery: Recovery;
  diagnostic?: string;
  correlation_id?: string;
}

/** Whether the UI should offer a retry affordance. */
export function offersRetry(error: ErrorPayload): boolean {
  return (
    error.recovery.strategy === 'retry_manual' || error.recovery.strategy === 'retry_automatic'
  );
}

/** Narrows an unknown thrown value to an {@link ErrorPayload}. */
export function isErrorPayload(value: unknown): value is ErrorPayload {
  if (typeof value !== 'object' || value === null) return false;
  const candidate = value as Partial<ErrorPayload>;
  return (
    typeof candidate.kind === 'string' &&
    typeof candidate.code === 'string' &&
    typeof candidate.message_key === 'string' &&
    typeof candidate.recovery === 'object'
  );
}

// ---------------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------------

export type Theme = 'system' | 'light' | 'dark' | 'amoled' | 'custom';
export type Density = 'comfortable' | 'compact';
export type FilteringMode = 'off' | 'standard' | 'strict';

export interface AppearanceSettings {
  theme: Theme;
  accent: string;
  density: Density;
  sidebar_collapsed: boolean;
  /** `null` means "follow the OS `prefers-reduced-motion`". */
  reduced_motion: boolean | null;
  ui_scale: number;
  language: string | null;
  /** Cast a soft glow from the video's colours behind the player on the watch page. */
  ambient_mode: boolean;
}

/**
 * Which control bar the player wears.
 *
 * A genuine trade rather than a cosmetic choice (ADR-0004). `beastube` crops YouTube's chrome away
 * and draws the application's own dark controls; quality is then requested by resizing the frame,
 * which reaches 360p–2160p at 60fps but no lower and takes a few seconds to settle. `youtube`
 * leaves the embed's own bar in place, whose gear drives the player's internal quality API — exact,
 * immediate, and the full 144p–2160p range — at the cost of a white settings panel that cannot be
 * themed, and the title band returning with it.
 */
export type PlayerControls = 'beastube' | 'youtube';

export interface PlaybackSettings {
  default_quality: Quality;
  /** Which control bar the player wears. */
  player_controls: PlayerControls;
  max_quality: Quality;
  volume: number;
  muted: boolean;
  speed: number;
  autoplay_next: boolean;
  autoplay_on_open: boolean;
  resume_playback: boolean;
  captions_enabled: boolean;
  /** How far captions sit above the bottom of the picture, as a percentage of its height. */
  caption_offset_percent: number;
  /** Caption text size, as a percentage of the default. */
  caption_scale_percent: number;
  /** Opacity of the band behind caption text, as a percentage. */
  caption_background_percent: number;
  caption_language: string | null;
  audio_language: string | null;
  hardware_acceleration: boolean;
  seek_step_seconds: number;
  seek_step_large_seconds: number;
}

export interface PrivacySettings {
  history_enabled: boolean;
  search_history_enabled: boolean;
  local_recommendations_enabled: boolean;
  incognito_by_default: boolean;
  history_retention_days: number | null;
  max_search_history_entries: number;
  /** Clear the caches at startup once they exceed this many megabytes. `null` never clears. */
  cache_limit_mb: number | null;
}

export interface FilteringSettings {
  enabled: boolean;
  mode: FilteringMode;
  auto_update_rules: boolean;
  update_interval_hours: number;
  allowlist: string[];
  blocklist: string[];
  custom_rules: string[];
}

export interface NetworkSettings {
  max_concurrent_requests: number;
  request_timeout_seconds: number;
  connect_timeout_seconds: number;
  max_retries: number;
  prefetch_enabled: boolean;
  reduce_activity_on_battery: boolean;
}

export interface CacheSettings {
  memory_budget_mb: number;
  disk_budget_mb: number;
  metadata_ttl_hours: number;
  thumbnail_ttl_days: number;
}

/**
 * Video downloads.
 *
 * Paths are absolute or `null`; the native side drops anything relative on load, because a
 * relative path would resolve against the process's working directory. `null` on a tool path means
 * "find it yourself" — beside the application, then on `PATH`.
 */
export interface DownloadSettings {
  /** Where files are saved. `null` means a BEASTUBE folder in the user's Downloads. */
  directory: string | null;
  /** Ceiling on the height requested. `auto` means the best available. */
  max_quality: Quality;
  /** Path to `yt-dlp`, or `null` to discover it. */
  tool_path: string | null;
  /** Path to `ffmpeg`, or `null` to discover it. */
  ffmpeg_path: string | null;
}

/**
 * Automatic updates.
 *
 * An update installs software without asking a second time, so both fields exist to keep that from
 * becoming something the viewer cannot stop or escape.
 */
export interface UpdateSettings {
  /** Install a newer version shortly after launch, without being asked. */
  automatic: boolean;
  /**
   * A version that failed to install automatically and must not be retried on its own.
   *
   * Without it, a release that cannot install on a particular machine is downloaded again on every
   * launch — fifty megabytes each time, for ever. The manual control ignores this, so trying again
   * deliberately is always possible.
   */
  skip_version: string | null;
}

/** The complete settings document. */
export interface Settings {
  version: number;
  appearance: AppearanceSettings;
  playback: PlaybackSettings;
  privacy: PrivacySettings;
  filtering: FilteringSettings;
  network: NetworkSettings;
  cache: CacheSettings;
  downloads: DownloadSettings;
  updates: UpdateSettings;
}

// ---------------------------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------------------------

export type NetworkStatus = 'online' | 'offline' | 'metered';
export type FilterUpdateOutcome =
  'applied' | 'already_current' | 'rejected_invalid' | 'rolled_back';

export interface PlaybackStateChanged {
  session_id: string;
  video_id: VideoId;
  previous: PlaybackState;
  current: PlaybackState;
  position_ms: number;
}

export interface PlaybackFailed {
  session_id: string;
  video_id: VideoId;
  position_ms: number;
  error: ErrorPayload;
}

export interface SearchCompleted {
  query: string;
  result_count: number;
  elapsed_ms: number;
  from_cache: boolean;
}

export interface NetworkChanged {
  previous: NetworkStatus;
  current: NetworkStatus;
}

export interface CacheChanged {
  disk_bytes: number;
  memory_bytes: number;
  evicted_entries: number;
}

export interface FilterUpdated {
  outcome: FilterUpdateOutcome;
  active_version: string;
  rule_count: number;
  reason_key?: string;
}

export interface UpdateAvailable {
  version: string;
  notes?: string;
  published_at?: number;
}

export interface MaintenanceProgress {
  task: string;
  fraction?: number;
  finished: boolean;
}

/** Where a download is in its life. */
export type DownloadStatus =
  'queued' | 'starting' | 'downloading' | 'merging' | 'finished' | 'failed' | 'cancelled';

/**
 * The state of one download.
 *
 * The whole record arrives on every change rather than a delta, so a screen that mounted midway
 * through is correct as soon as the next event lands. An absent size or speed means *unknown* —
 * the tool did not report one — and must render as an indeterminate state, never as zero.
 */
export interface DownloadProgress {
  id: string;
  video_id: VideoId;
  title: string;
  status: DownloadStatus;
  downloaded_bytes?: number;
  total_bytes?: number;
  /** Completion in `0..=1`, absent when the total size is unknown. */
  fraction?: number;
  speed_bps?: number;
  eta_seconds?: number;
  /** The finished file, once there is one. */
  path?: string;
  error?: ErrorPayload;
  updated_at: number;
}

/** Whether a download is over, one way or another. Mirrors `DownloadStatus::is_terminal`. */
export function isTerminalDownload(status: DownloadStatus): boolean {
  return status === 'finished' || status === 'failed' || status === 'cancelled';
}

/**
 * Event channel names and their payloads.
 *
 * Kept in step with `AppEvent::ALL_NAMES` in Rust by the contract fixture test — adding a variant
 * on one side without the other fails that test.
 */
export interface AppEventMap {
  'playback:state-changed': PlaybackStateChanged;
  'playback:failed': PlaybackFailed;
  'search:completed': SearchCompleted;
  'network:changed': NetworkChanged;
  'cache:changed': CacheChanged;
  'filter:updated': FilterUpdated;
  'update:available': UpdateAvailable;
  'maintenance:progress': MaintenanceProgress;
  'download:progress': DownloadProgress;
}

/** Every event channel name. */
export type AppEventName = keyof AppEventMap;
