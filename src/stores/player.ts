/**
 * The one playback session, held above the router.
 *
 * ## The problem this exists to solve
 *
 * The shell keys the routed view on the route name, so moving from Home to a video unmounts one
 * view and mounts another. When the player lives inside that view, every such navigation destroys
 * an `<iframe>` and builds a new one — a full embed bootstrap, every time, for the most common
 * thing anyone does in this application.
 *
 * Watch-to-watch was already fast, because the route name does not change and the player swaps with
 * `loadVideoById`. Home-to-watch was not, and that is the path a person actually takes: browse,
 * open, go back, open another. Worse, `HoverPreview` had already built a player for that exact
 * video while the pointer rested on it, and clicking threw it away.
 *
 * ## The shape
 *
 * One player is mounted once, outside the routed subtree, and never unmounts. A view that wants
 * playback registers a *slot* — an empty box in its own layout — and the host positions the player
 * over it. Switching videos is then one `loadVideoById`, which is the same cost as switching shorts
 * and is why that surface has always felt instant.
 *
 * ## Why a slot rather than a portal
 *
 * `createPortal` would move the player's DOM node into the view. Moving an `<iframe>` in the DOM
 * destroys its browsing context and reloads it, which is the exact cost being avoided — so the node
 * stays where it is and only its geometry follows the slot.
 *
 * ## Why callbacks are not in the store
 *
 * They change identity on most renders, and a store write per render would re-render every
 * subscriber. They live in a module-level registry the host reads at call time, so the reactive
 * surface stays down to "which video, and where".
 */

import { create } from 'zustand';

import type { PlaybackState, VideoId } from '@/types/domain';

/** What to play, and how to start it. */
export interface PlayerSession {
  videoId: VideoId;
  /** Where to resume from. Absent means the beginning. */
  startAtMs?: number;
  autoplay: boolean;
  /**
   * The video's own thumbnail, shown until the first frame arrives.
   *
   * The embed paints black while it buffers, and a black rectangle reads as broken rather than as
   * loading — which is exactly what "stuck on a black screen" describes. Absent until metadata has
   * arrived, which is fine: it fills in a moment later and is gone once playback starts.
   */
  posterUrl?: string;
}

/** Events the owning view wants, kept out of the reactive store deliberately. */
export interface PlayerHandlers {
  onStateChange?: (state: PlaybackState, videoId: VideoId | null) => void;
  onPosition?: (positionMs: number, durationMs: number) => void;
}

let handlers: PlayerHandlers = {};

/** Points the host's callbacks at the current owner. */
export function setPlayerHandlers(next: PlayerHandlers): void {
  handlers = next;
}

/** Read by the host when an event fires, so a re-render is never needed to keep them fresh. */
export function playerHandlers(): PlayerHandlers {
  return handlers;
}

interface PlayerState {
  /** What is playing, or `null` when no view wants the player on screen. */
  session: PlayerSession | null;
  /**
   * The most recent video, retained after the session ends.
   *
   * This is what keeps the parked player pointed at something. Without it the host would need
   * local state and an effect to remember what it was last showing, and setting that state from an
   * effect is the cascading render React warns about.
   */
  lastVideoId: VideoId | null;
  /**
   * The box the player should cover.
   *
   * Held as an element rather than a rectangle so the host can observe it: the slot resizes with
   * the window and with its own content, and re-measuring is the host's job rather than the
   * view's.
   */
  slot: HTMLElement | null;
  setSession: (session: PlayerSession | null) => void;
  setSlot: (slot: HTMLElement | null) => void;
}

export const usePlayerStore = create<PlayerState>((set) => ({
  session: null,
  lastVideoId: null,
  slot: null,
  setSession: (session) => {
    set(session === null ? { session } : { session, lastVideoId: session.videoId });
  },
  setSlot: (slot) => {
    set({ slot });
  },
}));
