/**
 * One in-flight fetch and one result, shared by every surface that wants a feed.
 *
 * Two problems this exists to solve, both of which the user hits directly:
 *
 * 1. **The Shorts tab was empty on arrival.** Its resource hook starts fetching when the view
 *    mounts, so opening the tab meant watching a spinner for as long as the request took — and the
 *    shorts feed is the most expensive request in the application, because it fans out across
 *    topics and related lists.
 * 2. **The same feed was fetched twice.** Home's Shorts shelf and the Shorts tab each asked for
 *    their own copy, so moving between them paid the cost again for data that was already in
 *    memory.
 *
 * So the fetch is started once — at launch, before anything asks for it — and the result is kept.
 * Arriving at either surface then reads memory rather than the network.
 *
 * ## Why a module, not a store
 *
 * Nothing re-renders on a change here; subscribers are given the promise and settle it themselves
 * through the ordinary resource hook. Putting it in a store would re-render every subscriber on
 * every write for no benefit.
 *
 * ## Staleness
 *
 * A cached batch is reused for a few minutes and then dropped, so a session that runs for hours
 * does not keep showing the feed it started with. The native side varies its topic rotation on a
 * similar timescale, so a refetch genuinely returns something different rather than the same list.
 */

import { invoke } from '@/services/ipc';
import type { VideoSummary } from '@/types/domain';

/** How long a fetched batch is served from memory before it is fetched again. */
const TTL_MS = 5 * 60 * 1000;

interface Entry {
  /** The in-flight or settled request. */
  promise: Promise<VideoSummary[]>;
  /** When it was started, for expiry. Never read for anything else. */
  startedAt: number;
}

const entries = new Map<string, Entry>();

/**
 * The shorts feed, fetched at most once per TTL however many surfaces ask.
 *
 * Callers pass the same key from every surface that should share the result.
 */
export function sharedShortsFeed(limit: number, signal?: AbortSignal): Promise<VideoSummary[]> {
  const key = `shorts:${limit}`;
  const now = Date.now();
  const existing = entries.get(key);

  if (existing && now - existing.startedAt < TTL_MS) {
    return existing.promise;
  }

  // Deliberately not passing the caller's `signal` into the shared request. The request belongs to
  // every subscriber, not to whichever one happened to start it, and navigating away from the first
  // surface must not cancel a fetch the next one is about to want.
  const promise = invoke('get_shorts_feed', { limit }).catch((cause: unknown) => {
    // A failure is not cached: the next surface to ask should get a fresh attempt rather than
    // inheriting a stale error.
    entries.delete(key);
    throw cause;
  });

  entries.set(key, { promise, startedAt: now });

  if (signal) {
    // The caller can still stop waiting; the shared request carries on for whoever else wants it.
    return new Promise<VideoSummary[]>((resolve, reject) => {
      const onAbort = () => {
        reject(new DOMException('aborted', 'AbortError'));
      };
      signal.addEventListener('abort', onAbort, { once: true });
      promise.then(resolve, reject).finally(() => {
        signal.removeEventListener('abort', onAbort);
      });
    });
  }

  return promise;
}

/**
 * Starts the shorts fetch before anything asks for it.
 *
 * Called once at launch. Failure is swallowed: this is speculative work, and a surface that later
 * wants the feed will retry through the same cache.
 */
export function preloadFeeds(limits: readonly number[]): void {
  for (const limit of limits) {
    void sharedShortsFeed(limit).catch(() => {
      // Speculative; the real request reports its own failure.
    });
  }
}

/** Drops everything. Used when the user clears data, so nothing survives that should not. */
export function clearFeedCache(): void {
  entries.clear();
}
