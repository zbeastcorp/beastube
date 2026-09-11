/**
 * Who made this short.
 *
 * The shorts feed comes back as `shortsLockupViewModel` entries, and those carry an id, a title, a
 * thumbnail and a view count — no channel, no avatar. YouTube's own Shorts surface shows both, so
 * getting them means one extra request per short, against the video itself.
 *
 * The feed holds forty. Fetching a channel for all of them would be forty requests for information
 * needed by one. So this is called for the short on screen and prefetched exactly one ahead, which
 * makes it one request per short the viewer actually reaches, arriving before they get there.
 *
 * Scrolling back to a short must not re-request its channel, and the feed is deliberately easy to
 * scroll back through. Entries are kept for the session and bounded, because a long session of
 * shorts is exactly the case that would otherwise grow without limit.
 */

import { invoke } from '@/services/ipc';
import { bestThumbnailFor, type VideoId } from '@/types/domain';

/** How many channels are remembered before the oldest are dropped. */
const MAX_ENTRIES = 400;

/** Rendition width to ask for. The avatar is drawn at 32px, so this covers a 2x display. */
const AVATAR_WIDTH = 72;

export interface ShortChannel {
  /** The video this belongs to. Carried so a late response cannot be shown against a newer short. */
  videoId: VideoId;
  name: string | null;
  avatarUrl: string | null;
}

const cache = new Map<VideoId, ShortChannel>();
const inFlight = new Map<VideoId, Promise<ShortChannel>>();

/** What is already known, without fetching. Used to render the first frame without a flash. */
export function cachedShortChannel(videoId: VideoId | undefined): ShortChannel | undefined {
  return videoId === undefined ? undefined : cache.get(videoId);
}

/** The channel behind a short, fetched once and then remembered. */
export function shortChannel(videoId: VideoId, signal?: AbortSignal): Promise<ShortChannel> {
  const hit = cache.get(videoId);
  if (hit) return Promise.resolve(hit);

  const running = inFlight.get(videoId);
  if (running) return running;

  // The caller's signal is deliberately not passed through. Scrolling past a short before its
  // channel arrives should still fill the cache — the viewer will very likely scroll back, and the
  // request is already paid for.
  const request = invoke('get_video', { videoId })
    .then((details): ShortChannel => {
      const avatar = details.channel_avatar
        ? bestThumbnailFor(details.channel_avatar, AVATAR_WIDTH)
        : undefined;
      const entry: ShortChannel = {
        videoId,
        name: details.channel_name ?? null,
        avatarUrl: avatar?.url ?? null,
      };
      // Oldest-first eviction. `Map` iterates in insertion order, so the first key is the oldest.
      if (cache.size >= MAX_ENTRIES) {
        const oldest = cache.keys().next();
        if (!oldest.done) cache.delete(oldest.value);
      }
      cache.set(videoId, entry);
      return entry;
    })
    .catch((cause: unknown) => {
      // A short whose channel cannot be fetched simply shows no channel row. Not cached, so a
      // transient failure does not stick for the session.
      if (signal?.aborted !== true) {
        // Nothing to report: the row is optional and its absence is already the failure state.
      }
      throw cause;
    })
    .finally(() => {
      inFlight.delete(videoId);
    });

  inFlight.set(videoId, request);
  return request;
}

/** Warms the cache for a short the viewer has not reached yet. Failure is ignored. */
export function prefetchShortChannel(videoId: VideoId | undefined): void {
  if (videoId === undefined || cache.has(videoId) || inFlight.has(videoId)) return;
  void shortChannel(videoId).catch(() => {
    // Speculative.
  });
}
