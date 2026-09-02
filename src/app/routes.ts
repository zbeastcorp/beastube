/**
 * Route definitions.
 *
 * A discriminated union rather than a path-string router. The difference matters in practice:
 * `navigate({ name: 'watch', videoId })` cannot be called without a video id, and adding a required
 * param to a route immediately fails every call site that omits it. A string-based router defers
 * both of those to runtime.
 *
 * ## Why hashes
 *
 * URLs are `#/watch/dQw4w9WgXcQ`. Path routing would need the host serving the bundle to rewrite
 * unknown paths to `index.html`; under Tauri the frontend is served by a custom protocol handler,
 * so a deep path after a devtools reload would 404. Hash routing behaves identically under the Vite
 * dev server and the packaged protocol handler, with no rewrite rule to keep in sync.
 *
 * There are no aesthetics at stake — the address bar is not visible in a desktop window.
 */

import type { ChannelId, ChannelTab, LocalPlaylistId, PlaylistId, VideoId } from '@/types/domain';
import {
  toChannelId,
  toLocalPlaylistId,
  toPlaylistId,
  toVideoId,
  type SearchResultKind,
} from '@/types/domain';

/** Which settings pane is open. */
export type SettingsSection =
  'appearance' | 'playback' | 'privacy' | 'filtering' | 'network' | 'cache' | 'shortcuts' | 'about';

const SETTINGS_SECTIONS: readonly SettingsSection[] = [
  'appearance',
  'playback',
  'privacy',
  'filtering',
  'network',
  'cache',
  'shortcuts',
  'about',
];

const CHANNEL_TABS: readonly ChannelTab[] = ['videos', 'shorts', 'live', 'playlists'];

const SEARCH_KINDS: readonly SearchResultKind[] = [
  'all',
  'videos',
  'shorts',
  'channels',
  'playlists',
  'live',
];

/** Every destination in the application. */
export type Route =
  | { name: 'home' }
  | { name: 'shorts'; videoId?: VideoId }
  | { name: 'search'; query: string; kind?: SearchResultKind }
  | { name: 'watch'; videoId: VideoId; startAtMs?: number; playlistId?: LocalPlaylistId }
  | { name: 'channel'; channelId: ChannelId; tab?: ChannelTab }
  | { name: 'playlist'; playlistId: PlaylistId }
  | { name: 'localPlaylist'; id: LocalPlaylistId }
  | { name: 'history' }
  | { name: 'library' }
  | { name: 'bookmarks' }
  | { name: 'settings'; section?: SettingsSection }
  | { name: 'diagnostics' }
  | { name: 'notFound'; path: string };

/** A route's discriminant. */
export type RouteName = Route['name'];

/** The route shown before the first navigation resolves. */
export const HOME: Route = { name: 'home' };

/**
 * Serializes a route to a hash fragment.
 *
 * Path segments are percent-encoded. Identifiers are already restricted to the URL-safe alphabet by
 * their branded constructors, so encoding is belt-and-braces there; the search query genuinely
 * needs it.
 */
export function routeToHash(route: Route): string {
  switch (route.name) {
    case 'home':
      return '#/';
    case 'shorts':
      return route.videoId ? `#/shorts/${route.videoId}` : '#/shorts';
    case 'search': {
      const params = new URLSearchParams({ q: route.query });
      if (route.kind && route.kind !== 'all') params.set('kind', route.kind);
      return `#/search?${params.toString()}`;
    }
    case 'watch': {
      const params = new URLSearchParams();
      if (route.startAtMs !== undefined)
        params.set('t', String(Math.floor(route.startAtMs / 1000)));
      if (route.playlistId !== undefined) params.set('list', String(route.playlistId));
      const query = params.toString();
      return `#/watch/${route.videoId}${query ? `?${query}` : ''}`;
    }
    case 'channel':
      return `#/channel/${route.channelId}/${route.tab ?? 'videos'}`;
    case 'playlist':
      return `#/playlist/${route.playlistId}`;
    case 'localPlaylist':
      return `#/library/playlist/${route.id}`;
    case 'history':
      return '#/history';
    case 'library':
      return '#/library';
    case 'bookmarks':
      return '#/bookmarks';
    case 'settings':
      return route.section ? `#/settings/${route.section}` : '#/settings';
    case 'diagnostics':
      return '#/diagnostics';
    case 'notFound':
      return `#${route.path}`;
  }
}

function isOneOf<T extends string>(
  candidates: readonly T[],
  value: string | undefined,
): T | undefined {
  return value !== undefined && (candidates as readonly string[]).includes(value)
    ? (value as T)
    : undefined;
}

/**
 * Percent-decodes a path segment, returning `null` for an undecodable one.
 *
 * `decodeURIComponent` throws `URIError` on a lone or truncated escape such as `%` or `%E0%A4`.
 * A hash fragment is user-editable and arrives from deep links, so an exception here would take
 * down the router on input a user can type by hand.
 */
function decodeSegment(segment: string): string | null {
  try {
    return decodeURIComponent(segment);
  } catch {
    return null;
  }
}

/**
 * Parses a hash fragment back into a route.
 *
 * Never throws and never returns a partially-valid route: an identifier that fails validation
 * produces `notFound` rather than a route carrying a malformed id, so a hand-edited or hostile
 * fragment cannot reach a data-fetching call.
 */
export function hashToRoute(hash: string): Route {
  const raw = hash.startsWith('#') ? hash.slice(1) : hash;
  const [pathPart = '', queryPart = ''] = raw.split('?', 2);
  const params = new URLSearchParams(queryPart);
  const notFound = (): Route => ({ name: 'notFound', path: pathPart || '/' });

  const rawSegments = pathPart.split('/').filter((segment) => segment.length > 0);
  const segments: string[] = [];
  for (const raw of rawSegments) {
    const decoded = decodeSegment(raw);
    if (decoded === null) return notFound();
    segments.push(decoded);
  }

  if (segments.length === 0) return HOME;

  switch (segments[0]) {
    case 'shorts': {
      if (segments.length === 1) return { name: 'shorts' };
      const videoId = toVideoId(segments[1] ?? '');
      return videoId ? { name: 'shorts', videoId } : notFound();
    }

    case 'search': {
      const query = params.get('q');
      if (query === null) return notFound();
      const kind = isOneOf(SEARCH_KINDS, params.get('kind') ?? undefined);
      return kind ? { name: 'search', query, kind } : { name: 'search', query };
    }

    case 'watch': {
      const videoId = toVideoId(segments[1] ?? '');
      if (!videoId) return notFound();
      const route: Route = { name: 'watch', videoId };
      const seconds = Number(params.get('t'));
      if (Number.isFinite(seconds) && seconds > 0) route.startAtMs = Math.floor(seconds) * 1000;
      const list = Number(params.get('list'));
      if (Number.isInteger(list) && list > 0) route.playlistId = toLocalPlaylistId(list);
      return route;
    }

    case 'channel': {
      const channelId = toChannelId(segments[1] ?? '');
      if (!channelId) return notFound();
      const tab = isOneOf(CHANNEL_TABS, segments[2]);
      return tab ? { name: 'channel', channelId, tab } : { name: 'channel', channelId };
    }

    case 'playlist': {
      const playlistId = toPlaylistId(segments[1] ?? '');
      return playlistId ? { name: 'playlist', playlistId } : notFound();
    }

    case 'library': {
      if (segments.length === 1) return { name: 'library' };
      if (segments[1] === 'playlist') {
        const id = Number(segments[2]);
        return Number.isInteger(id) && id > 0
          ? { name: 'localPlaylist', id: toLocalPlaylistId(id) }
          : notFound();
      }
      return notFound();
    }

    case 'history':
      return segments.length === 1 ? { name: 'history' } : notFound();

    case 'bookmarks':
      return segments.length === 1 ? { name: 'bookmarks' } : notFound();

    case 'settings': {
      if (segments.length === 1) return { name: 'settings' };
      const section = isOneOf(SETTINGS_SECTIONS, segments[1]);
      return section ? { name: 'settings', section } : notFound();
    }

    case 'diagnostics':
      return segments.length === 1 ? { name: 'diagnostics' } : notFound();

    default:
      return notFound();
  }
}

/**
 * Whether two routes address the same destination.
 *
 * Used to suppress a redundant history entry when the user re-activates the current nav item —
 * without it, clicking "Home" five times means five back presses to leave.
 */
export function isSameRoute(a: Route, b: Route): boolean {
  return routeToHash(a) === routeToHash(b);
}

/**
 * Which sidebar entry should appear active for a route.
 *
 * Watching a video reached from history should keep History highlighted rather than clearing the
 * selection, so the user can see where they are in the application.
 */
export function sidebarSectionFor(route: Route): RouteName | null {
  switch (route.name) {
    case 'home':
    case 'shorts':
    case 'search':
    case 'history':
    case 'bookmarks':
    case 'settings':
    case 'diagnostics':
      return route.name;
    case 'library':
    case 'localPlaylist':
      return 'library';
    case 'watch':
    case 'channel':
    case 'playlist':
    case 'notFound':
      return null;
  }
}
