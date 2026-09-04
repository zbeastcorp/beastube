/**
 * The feeds, kept across mounts so a screen is never empty while it fetches.
 *
 * Three problems this solves, all of which the user hits directly:
 *
 * 1. **Arriving at a feed showed a spinner.** The shell keys the routed view on the route name, so
 *    every navigation unmounts the previous view and destroys its state. Home → a video → Home
 *    therefore started from nothing every single time, however recently the feed had been fetched.
 *    The value lives here instead of in the view, so a remounted view paints on its first frame.
 * 2. **The same feed was fetched twice.** Home's shorts shelf and the Shorts tab each asked for
 *    their own copy of the most expensive request in the application.
 * 3. **Refreshing blanked the screen.** Clicking Home is a request for something new, but the old
 *    feed is perfectly good until the new one arrives — dropping it first meant staring at a
 *    skeleton for the length of the request.
 *
 * ## Stale while revalidate
 *
 * That third point is the shape of the whole module. A refresh does not throw away the value; it
 * starts a new request *beside* it. Callers read the old value immediately and the new one replaces
 * it when it lands. There is never a moment with nothing to show, and there is never a stale feed
 * that stays stale.
 *
 * The `revision` is how a caller says "not the one you have". It comes from `useFeedStore` and is
 * bumped by clicking a feed's nav entry, so the same request key with a new revision means fetch
 * again rather than serve the cached answer.
 *
 * ## Why a module, not a store
 *
 * Nothing re-renders on a change here; subscribers settle the promise themselves through the
 * ordinary resource hook. Putting it in a store would re-render every subscriber on every write.
 */

import { invoke } from '@/services/ipc';
import type { RecommendedFeed } from '@/services/ipc';
import type { VideoSummary } from '@/types/domain';

/** How long a batch is served without refetching, when nobody has asked for a new one. */
const TTL_MS = 5 * 60 * 1000;

/**
 * A cache key that also says what is stored under it.
 *
 * The phantom `value` is never read at runtime. It exists so that the key and the type of the thing
 * behind it cannot drift apart: `lastFeed(shortsKey(16))` is typed as a list of videos because the
 * key says so, and there is no second place to state it and get it wrong.
 */
export interface FeedKey<T> {
  readonly id: string;
  readonly value?: T;
}

interface Entry<T> {
  /**
   * The last successful value.
   *
   * Deliberately retained across a refresh. It is what a remounted view paints on its first frame,
   * and what stays on screen while the replacement is fetched.
   */
  value?: T;
  /** The in-flight or settled request, shared by every concurrent caller. */
  promise: Promise<T>;
  /** Which revision that request was issued for. A newer one means refetch. */
  revision: number;
  /** When it started, for expiry. Set to zero on failure so the next caller retries. */
  startedAt: number;
}

// Untyped at rest, narrowed at each accessor. The map holds unrelated shapes under different keys,
// which no single type parameter can express.
const entries = new Map<string, Entry<unknown>>();

/**
 * The newest value fetched for a key, without fetching.
 *
 * Read during render to fill a view before its own request has settled. Returns `undefined` only
 * when nothing has ever been fetched for that key — the first launch, or after a data clear.
 */
export function lastFeed<T>(key: FeedKey<T>): T | undefined {
  return (entries.get(key.id) as Entry<T> | undefined)?.value;
}

/**
 * Applies a change to the entry that owns `request`, and only if it still owns it.
 *
 * A newer revision may have replaced the entry while the request was in flight; writing a stale
 * answer into the slot it no longer owns would undo the refresh the user asked for.
 */
function commit<T>(id: string, request: Promise<T>, mutate: (entry: Entry<T>) => void): void {
  const slot = entries.get(id) as Entry<T> | undefined;
  if (!slot) return;
  if (slot.promise !== request) return;
  mutate(slot);
}

/**
 * Fetches a feed, or hands back the one already in flight or already fetched.
 *
 * A new `revision` always starts a new request, and the previous value stays readable through
 * {@link lastFeed} for as long as that request takes.
 */
function feed<T>(
  key: FeedKey<T>,
  revision: number,
  fetcher: () => Promise<T>,
  signal?: AbortSignal,
): Promise<T> {
  const now = Date.now();
  const cached = entries.get(key.id) as Entry<T> | undefined;

  let promise: Promise<T> | undefined;
  if (cached) {
    const current = cached.revision === revision && now - cached.startedAt < TTL_MS;
    if (current) promise = cached.promise;
  }

  if (!promise) {
    const request = fetcher()
      .then((value) => {
        commit(key.id, request, (entry) => {
          entry.value = value;
        });
        return value;
      })
      .catch((cause: unknown) => {
        // A failure is not cached: expiring the entry lets the next caller retry. The previous good
        // value survives — a failed refresh must not empty a screen that had content.
        commit(key.id, request, (entry) => {
          entry.startedAt = 0;
        });
        throw cause;
      });

    entries.set(key.id, {
      // Carried forward, which is the entire point: the screen keeps its content.
      ...(cached?.value !== undefined ? { value: cached.value } : {}),
      promise: request,
      revision,
      startedAt: now,
    });
    promise = request;
  }

  const shared = promise;
  if (!signal) return shared;

  // The caller can stop waiting; the shared request carries on for whoever else wants it. The
  // signal is deliberately not passed into the request itself — it belongs to every subscriber,
  // not to whichever one happened to start it, and navigating away must not cancel a fetch the
  // next screen is about to want.
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => {
      reject(new DOMException('aborted', 'AbortError'));
    };
    signal.addEventListener('abort', onAbort, { once: true });
    shared.then(resolve, reject).finally(() => {
      signal.removeEventListener('abort', onAbort);
    });
  });
}

/** Cache key for the shorts feed at a given size. */
export function shortsKey(limit: number): FeedKey<VideoSummary[]> {
  return { id: `shorts:${String(limit)}` };
}

/** Cache key for the recommended feed at a given size. */
export function recommendedKey(limit: number): FeedKey<RecommendedFeed> {
  return { id: `recommended:${String(limit)}` };
}

/** The shorts feed, fetched at most once per revision however many surfaces ask. */
export function sharedShortsFeed(
  limit: number,
  revision: number,
  signal?: AbortSignal,
): Promise<VideoSummary[]> {
  return feed(shortsKey(limit), revision, () => invoke('get_shorts_feed', { limit }), signal);
}

/** The recommended feed, on the same terms. */
export function sharedRecommended(
  limit: number,
  revision: number,
  signal?: AbortSignal,
): Promise<RecommendedFeed> {
  // The revision is both the cache key's freshness marker *and* the draw selector. Refresh raises
  // it, so the request that follows asks the provider for a different set of seeds instead of
  // re-fetching the same recommendations and returning a feed the viewer has already scrolled.
  return feed(
    recommendedKey(limit),
    revision,
    () => invoke('get_recommended', { limit, variant: revision }),
    signal,
  );
}

/**
 * Starts the feed fetches before anything asks for them.
 *
 * Called once at launch. Failure is swallowed: this is speculative work, and a surface that later
 * wants a feed retries through the same cache.
 */
export function preloadFeeds(shortsLimits: readonly number[], recommendedLimit: number): void {
  for (const limit of shortsLimits) {
    void sharedShortsFeed(limit, 0).catch(() => {
      // Speculative; the real request reports its own failure.
    });
  }
  void sharedRecommended(recommendedLimit, 0).catch(() => {
    // Speculative.
  });
}

/** Drops everything. Used when the user clears data, so nothing survives that should not. */
export function clearFeedCache(): void {
  entries.clear();
}
