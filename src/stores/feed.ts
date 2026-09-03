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
 * ## Why a revision rather than calling `reload()`
 *
 * Because the click happens in the sidebar and the fetch happens in a view that may not be mounted
 * yet. A number in a store is readable by whichever view mounts next; a `reload` handle only exists
 * while its own view is on screen.
 */

import { create } from 'zustand';

import { clearFeedCache } from '@/services/feedCache';

interface FeedState {
  /** Bumped to mean "throw away what you have and fetch again". */
  revision: number;
  /** Called when the user asks for a feed they may already be looking at. */
  refresh: () => void;
}

export const useFeedStore = create<FeedState>((set) => ({
  revision: 0,
  refresh: () => {
    // The shared cache is dropped first, so the refetch cannot be answered out of it.
    clearFeedCache();
    set((state) => ({ revision: state.revision + 1 }));
  },
}));
