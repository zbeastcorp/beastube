/**
 * What the active playback adapter can do.
 *
 * The UI is capability-gated (§131, ADR-0001): a control renders only where the corresponding flag
 * is true, so a feature the adapter cannot deliver is *absent* rather than present and inert. This
 * module is the single place the UI learns those flags, which is what keeps adapter identity out of
 * the components — nothing outside here knows or asks which adapter is running.
 *
 * The flags below are facts about the IFrame Player API, not aspirations:
 *
 * * `setPlaybackQuality`, `getPlaybackQuality` and `getAvailableQualityLevels` have been documented
 *   no-ops since 2025, so there is no quality selector and no quality readout.
 * * The API exposes no buffer level and no decoded/dropped frame counts, so there are no playback
 *   metrics.
 * * Rate, volume, mute, fullscreen and captions are real, and position polling is accurate enough
 *   to drive creator-marked segment skipping.
 */

import type { PlaybackCapabilities } from '@/types/domain';

/** Capabilities of the sanctioned embedded player, the production default adapter. */
export const IFRAME_CAPABILITIES: PlaybackCapabilities = {
  quality_selection: false,
  reports_available_qualities: false,
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
