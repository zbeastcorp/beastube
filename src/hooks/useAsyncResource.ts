/**
 * A cancelling async resource hook.
 *
 * Every view that fetches uses this, and it exists to enforce three behaviours that are easy to get
 * wrong per-view and painful when they are:
 *
 * 1. **Obsolete work is abandoned.** Each run gets an `AbortController`; changing the key aborts
 *    the previous one. Without this, typing five characters leaves five searches in flight and the
 *    slowest one wins — the classic reason a search box shows results for a query already replaced
 *    (§32).
 * 2. **A late response never overwrites a newer one.** Even with abort, a resolved-but-superseded
 *    promise can land after a newer one. Each run carries a token, and only the run whose token
 *    still matches is allowed to commit.
 * 3. **Loading is derived, not stored.** The hook is loading exactly when the settled result is for
 *    a different request than the current one. Storing it would mean writing state synchronously
 *    inside the effect, which causes a cascading render on every key change.
 *
 * Previous data stays visible while a new request runs, so a refetch does not blank the screen.
 *
 * The fetcher is wrapped in `useEffectEvent` so it always sees the latest render's closure without
 * becoming an effect dependency. That is what lets callers pass an inline arrow function without
 * refetching on every render, and is the supported way to do it in React 19 — mutating a ref during
 * render, the older workaround, is a genuine hazard under concurrent rendering.
 */

import { useCallback, useEffect, useEffectEvent, useRef, useState } from 'react';

import { isCancellation, normalizeError } from '@/services/ipc';
import { useProgressStore } from '@/stores/progress';
import type { ErrorPayload } from '@/types/domain';

/** The state of one async resource. */
export interface AsyncResource<T> {
  /** The most recent successful value, retained across refetches. */
  data: T | undefined;
  /** True while a fetch is in flight, including a refetch with data already shown. */
  loading: boolean;
  /** The most recent failure, cleared by a successful fetch. */
  error: ErrorPayload | null;
  /** Re-runs the fetch, abandoning any in-flight one. */
  reload: () => void;
}

/** What the last completed run produced, and which request it was for. */
interface Settled<T> {
  /** The request token this result belongs to, or `null` before anything has settled. */
  token: string | null;
  data: T | undefined;
  error: ErrorPayload | null;
}

/**
 * Runs `fetcher` whenever `key` changes.
 *
 * `key` is the identity of the request: the hook re-runs on every change, and must therefore
 * include everything the request depends on. Passing `null` means "nothing to fetch", which is how
 * a view waits for a prerequisite without a conditional hook.
 *
 * Pass `{ navigation: true }` for a view's *primary* fetch — the one whose arrival means the screen
 * the user asked for is there. Those, and only those, raise the progress bar at the top of the
 * window. Secondary fetches (suggestions, per-card state, background writes) deliberately do not,
 * because a bar that rose on every keystroke would stop meaning anything.
 */
export function useAsyncResource<T>(
  key: string | null,
  fetcher: (signal: AbortSignal) => Promise<T>,
  options?: { navigation?: boolean },
): AsyncResource<T> {
  const [nonce, setNonce] = useState(0);
  const [settled, setSettled] = useState<Settled<T>>({
    token: null,
    data: undefined,
    error: null,
  });

  // Reloading must re-run even when the key has not changed, so the nonce is part of the identity.
  const token = key === null ? null : `${key}#${nonce}`;

  // Guards against a superseded run committing after a newer one, independently of abort timing.
  const latestToken = useRef<string | null>(null);

  // Always the latest closure, never an effect dependency.
  const run = useEffectEvent((signal: AbortSignal) => fetcher(signal));

  // Read off the option rather than the store, and captured so the effect below does not depend on
  // an object identity that changes every render.
  const tracksNavigation = options?.navigation ?? false;

  useEffect(() => {
    if (token === null) return undefined;

    latestToken.current = token;
    const controller = new AbortController();

    // The store is read imperatively, never subscribed to. Subscribing here would re-render every
    // view in the application each time any other view started or finished fetching.
    let counted = false;
    if (tracksNavigation) {
      counted = true;
      useProgressStore.getState().begin();
    }
    const release = () => {
      if (!counted) return;
      counted = false;
      useProgressStore.getState().end();
    };

    void (async () => {
      try {
        const data = await run(controller.signal);
        if (latestToken.current !== token) return;
        setSettled({ token, data, error: null });
      } catch (cause) {
        if (latestToken.current !== token) return;
        const payload = normalizeError(cause);
        // A cancellation is the expected result of navigating away; surfacing it would show an
        // error for something the user themselves caused. The token is still marked settled so the
        // hook does not appear to load forever.
        setSettled((previous) => ({
          token,
          data: previous.data,
          error: isCancellation(payload) ? null : payload,
        }));
      } finally {
        release();
      }
    })();

    return () => {
      controller.abort();
      // Also released here, so navigating away mid-flight lowers the bar instead of stranding the
      // count above zero forever.
      release();
    };
  }, [token, tracksNavigation]);

  const reload = useCallback(() => {
    setNonce((current) => current + 1);
  }, []);

  return {
    data: settled.data,
    // Loading exactly when what has settled is not what is currently being asked for.
    loading: token !== null && settled.token !== token,
    error: settled.error,
    reload,
  };
}

/**
 * Debounces a rapidly-changing value.
 *
 * Used by the search field: issuing a request per keystroke wastes bandwidth and makes the
 * suggestion list flicker between partial queries.
 */
export function useDebounced<T>(value: T, delayMs: number): T {
  const [debounced, setDebounced] = useState(value);

  useEffect(() => {
    const timer = setTimeout(() => {
      setDebounced(value);
    }, delayMs);
    return () => {
      clearTimeout(timer);
    };
  }, [value, delayMs]);

  return debounced;
}
