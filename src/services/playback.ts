/**
 * What the active playback adapter can do.
 *
 * The UI is capability-gated (§131, ADR-0001): a control renders only where the corresponding flag
 * is true, so a feature the adapter cannot deliver is *absent* rather than present and inert. This
 * module is the single place the UI learns those flags, which is what keeps adapter identity out of
 * the components — nothing outside here knows or asks which adapter is running.
 *
 * The flags below are facts about the IFrame Player API, measured rather than assumed:
 *
 * * `setPlaybackQuality` is inert — calling it with `hd1080` on a player showing `hd720` leaves it
 *   on `hd720`. `getPlaybackQuality` and `getAvailableQualityLevels` are not: both answer, and the
 *   list is per-video rather than a fixed ladder. A tier is therefore requested by resizing the
 *   player's frame, which the embed does act on, so quality selection and the quality readout are
 *   both real. See `YouTubePlayer` and ADR-0004.
 * * The API exposes no buffer level and no decoded/dropped frame counts, so there are no playback
 *   metrics.
 * * Rate, volume, mute, fullscreen and captions are real, and position polling is accurate enough
 *   to drive creator-marked segment skipping.
 */

import type { PlaybackCapabilities } from '@/types/domain';

/** Capabilities of the sanctioned embedded player, the production default adapter. */
export const IFRAME_CAPABILITIES: PlaybackCapabilities = {
  quality_selection: true,
  reports_available_qualities: true,
  playback_rate: true,
  caption_control: true,
  audio_track_selection: false,
  buffer_metrics: false,
  frame_metrics: false,
  picture_in_picture: false,
  fullscreen: true,
  volume_control: true,
  segment_skipping: true,
};

/**
 * Capabilities of the adapter currently in use.
 *
 * A function rather than a constant because the adapter becomes a runtime choice once the
 * experimental direct-stream adapter is compiled in; callers written against this signature will
 * not change when that happens.
 */
export function activePlaybackCapabilities(): PlaybackCapabilities {
  return IFRAME_CAPABILITIES;
}

/** Human-facing name of the active adapter, for the diagnostics screen only. */
export function activePlaybackAdapterName(): string {
  return 'iframe';
}
