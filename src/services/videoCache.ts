/**
 * Video details, fetched once and reusable by whoever asks next.
 *
 * The watch page cannot paint its title, channel, description or actions until `get_video` returns,
 * and that request only *starts* when the page mounts — which is after the click. So the first
 * moment of every video is a player with a skeleton underneath it, and the metadata drops in a beat
 * later. The request is the same one whether it is issued on click or on hover, so it is issued on
 * hover: by the time the card is clicked the answer is usually already here and the whole page
 * paints at once.
 *
 * ## Why hover and not click
 *
 * Because hover happens first, and it happens for free. A pointer resting on a card is a strong
 * signal — the hover preview already treats it as one — and the cost of being wrong is one request
 * that is thrown away, against a saving of the entire round trip when it is right.
 *
 * ## Staleness
 *
 * A view count a few minutes old is not worth a second request, but one from an hour ago is. The
 * entry is dropped after a short window and refetched on the next ask.
 */

import { invoke } from '@/services/ipc';
import type { VideoDetails, VideoId } from '@/types/domain';

/** How long a fetched result is served from memory before it is fetched again. */
const TTL_MS = 3 * 60 * 1000;

/** How many videos are remembered. Bounded so a long session cannot grow without limit. */
const MAX_ENTRIES = 200;

interface Entry {
  promise: Promise<VideoDetails>;
  startedAt: number;
}

const entries = new Map<VideoId, Entry>();

/**
 * The details for a video, shared with anything else that asked recently.
 *
 * The caller's `signal` stops *this* caller waiting; it never cancels the shared request, which
 * belongs to every subscriber rather than to whichever one happened to start it.
 */
export function videoDetails(videoId: VideoId, signal?: AbortSignal): Promise<VideoDetails> {
  const now = Date.now();
  const existing = entries.get(videoId);
  const promise =
    existing && now - existing.startedAt < TTL_MS ? existing.promise : start(videoId, now);

  if (!signal) return promise;

  return new Promise<VideoDetails>((resolve, reject) => {
    const onAbort = () => {
      reject(new DOMException('aborted', 'AbortError'));
    };
    signal.addEventListener('abort', onAbort, { once: true });
    promise.then(resolve, reject).finally(() => {
      signal.removeEventListener('abort', onAbort);
    });
  });
}

function start(videoId: VideoId, now: number): Promise<VideoDetails> {
  const promise = invoke('get_video', { videoId }).catch((cause: unknown) => {
    // A failure is not kept: the next caller should get a fresh attempt rather than inherit an
    // error from a request they had nothing to do with.
    entries.delete(videoId);
    throw cause;
  });

  // Oldest-first eviction; `Map` iterates in insertion order.
  if (entries.size >= MAX_ENTRIES) {
    const oldest = entries.keys().next();
    if (!oldest.done) entries.delete(oldest.value);
  }
  entries.set(videoId, { promise, startedAt: now });
  return promise;
}

/** Starts the fetch for a video the pointer is resting on. Failure is ignored. */
export function prefetchVideoDetails(videoId: VideoId): void {
  const existing = entries.get(videoId);
  if (existing && Date.now() - existing.startedAt < TTL_MS) return;
  void videoDetails(videoId).catch(() => {
    // Speculative; the real request reports its own failure.
  });
}

/** Drops everything. Used when the user clears data, so nothing survives that should not. */
export function clearVideoCache(): void {
  entries.clear();
}
