/**
 * When the feeds were last asked to start over.
 *
 * YouTube gives you a different home feed every time you click Home, and a different set of shorts
 * every time you open Shorts. Ours cached a batch for five minutes and served it to anyone who
 * asked, so clicking Home from the watch page returned you to the identical grid you left — which
 * makes the application feel like it has a fixed amount of content in it.
 *
 * So the nav entries bump this number, and the feed views fold it into their request key. A bumped
 * revision is a new key, a new key is a new fetch, and the native side varies its topic rotation
 * and its seeds per call, so the fetch genuinely returns something different rather than the same
 * list again.
 *
 * ## Stale while revalidate
 *
 * Bumping this does not blank anything. `feedCache` keeps the previous batch and swaps the new one
 * in when it arrives, so a click gives you a feed instantly *and* a different feed a moment later.
 *
 * ## Why a revision rather than calling `reload()`
 *
 * Because the click happens in the sidebar and the fetch happens in a view that may not be mounted
 * yet. A number in a store is readable by whichever view mounts next; a `reload` handle only exists
 * while its own view is on screen.
 */

import { create } from 'zustand';

interface FeedState {
  /** Bumped to mean "fetch again"; what you already have stays on screen meanwhile. */
  revision: number;
  /** Called when the user asks for a feed they may already be looking at. */
  refresh: () => void;
}

export const useFeedStore = create<FeedState>((set) => ({
  revision: 0,
  refresh: () => {
    // Deliberately does NOT drop the cached feed. The revision alone tells the cache to fetch
    // again; the old batch stays readable and on screen for as long as that takes. Dropping it
    // first is what turned every click into a skeleton for the length of the slowest request in
    // the application — the refresh was working, it just had nothing to show while it worked.
    set((state) => ({ revision: state.revision + 1 }));
  },
}));
