/**
 * What the active provider can actually do.
 *
 * `get_provider_capabilities` has been in the IPC contract from the start and nothing ever called
 * it — so ADR 0001's third constraint, that controls render only where the adapter reports support,
 * was an invariant on paper and nowhere else. This is the frontend half.
 *
 * The concrete case that exposed it: the extractor's remote-playlist parser no longer matches
 * YouTube's response, and the provider says so by reporting `playlists: false`. Its own module
 * comment claims "the UI hides the playlist surface rather than" showing it — but search results
 * went on rendering playlist cards that linked to a route which rendered Not Found. A card that
 * looks like a destination and is not one is worse than no card.
 *
 * Capabilities are fixed for the life of the process, so they are fetched once at launch and read
 * from memory afterwards. Until the answer arrives every capability reads as **false**.
 *
 * That direction is deliberate. Guessing "supported" and being wrong shows a control that leads
 * nowhere; guessing "unsupported" and being wrong briefly omits one that appears a moment later.
 * The second is a smaller lie, and it is the one that cannot strand anybody.
 */

import { invoke, type ProviderCapabilities } from '@/services/ipc';

/**
 * What is assumed before the provider has answered.
 *
 * Every field false: see the note above on why the conservative direction is the safe one.
 */
const UNKNOWN: ProviderCapabilities = {
  search_videos: false,
  search_channels: false,
  search_playlists: false,
  search_shorts: false,
  suggestions: false,
  video_details: false,
  related_videos: false,
  channel_details: false,
  channel_videos: false,
  channel_shorts: false,
  playlists: false,
  discovery_feed: false,
  explore: false,
  captions: false,
  chapters: false,
  search_filters: false,
  pagination: false,
};

let known: ProviderCapabilities | null = null;
let inFlight: Promise<ProviderCapabilities> | null = null;

/** What the provider reported, or all-false until it has. Safe to call during render. */
export function capabilities(): ProviderCapabilities {
  return known ?? UNKNOWN;
}

/**
 * Fetches the capability set once.
 *
 * Called at launch. A failure is not cached, so a later caller retries rather than inheriting a
 * conservative answer for the whole session.
 */
export function loadCapabilities(): Promise<ProviderCapabilities> {
  if (known) return Promise.resolve(known);
  inFlight ??= invoke('get_provider_capabilities', undefined)
    .then((reported) => {
      known = reported;
      return reported;
    })
    .finally(() => {
      inFlight = null;
    });
  return inFlight;
}
