import { describe, expect, it } from 'vitest';

import { toChannelId, toLocalPlaylistId, toVideoId } from '@/types/domain';

import { hashToRoute, isSameRoute, routeToHash, sidebarSectionFor, type Route } from './routes';

const VIDEO = toVideoId('dQw4w9WgXcQ');
const CHANNEL = toChannelId('UCuAXFkgsw1L7xaCfnd5JJOw');

if (!VIDEO || !CHANNEL) throw new Error('fixture identifiers must be valid');

describe('route serialization', () => {
  const cases: Route[] = [
    { name: 'home' },
    { name: 'shorts' },
    { name: 'shorts', videoId: VIDEO },
    { name: 'search', query: 'lo-fi beats' },
    { name: 'search', query: 'rust', kind: 'channels' },
    { name: 'watch', videoId: VIDEO },
    { name: 'watch', videoId: VIDEO, startAtMs: 90_000 },
    { name: 'watch', videoId: VIDEO, playlistId: toLocalPlaylistId(3) },
    { name: 'channel', channelId: CHANNEL, tab: 'videos' },
    { name: 'channel', channelId: CHANNEL, tab: 'shorts' },
    { name: 'localPlaylist', id: toLocalPlaylistId(7) },
    { name: 'history' },
    { name: 'library' },
    { name: 'bookmarks' },
    { name: 'settings' },
    { name: 'settings', section: 'privacy' },
    { name: 'diagnostics' },
  ];

  it.each(cases)('round-trips $name', (route) => {
    expect(hashToRoute(routeToHash(route))).toEqual(route);
  });

  it('encodes a query containing characters that would otherwise break the fragment', () => {
    const route: Route = { name: 'search', query: 'a&b=c #tag?x' };
    expect(hashToRoute(routeToHash(route))).toEqual(route);
  });

  it('omits the default search kind from the URL', () => {
    expect(routeToHash({ name: 'search', query: 'x', kind: 'all' })).not.toContain('kind');
  });

  it('always writes a channel tab so a bare channel URL still resolves', () => {
    expect(routeToHash({ name: 'channel', channelId: CHANNEL })).toContain('/videos');
  });

  it('converts the watch start time to whole seconds', () => {
    expect(routeToHash({ name: 'watch', videoId: VIDEO, startAtMs: 90_500 })).toContain('t=90');
  });
});

describe('route parsing is defensive', () => {
  it('treats an empty or root hash as home', () => {
    expect(hashToRoute('')).toEqual({ name: 'home' });
    expect(hashToRoute('#')).toEqual({ name: 'home' });
    expect(hashToRoute('#/')).toEqual({ name: 'home' });
  });

  it('rejects a malformed identifier rather than routing with it', () => {
    // A hand-edited or hostile fragment must never yield a route carrying a bad id, because that
    // id would flow straight into a data-fetching call.
    for (const hostile of [
      '#/watch/../../etc/passwd',
      '#/watch/a b',
      '#/watch/',
      '#/channel/a%2Fb/videos',
      '#/playlist/<script>',
    ]) {
      expect(hashToRoute(hostile).name, hostile).toBe('notFound');
    }
  });

  it('rejects an unknown settings section', () => {
    expect(hashToRoute('#/settings/nonsense').name).toBe('notFound');
  });

  it('falls back to the untabbed channel route for an unknown tab', () => {
    // 404ing the whole channel because of a bad tab would be a worse outcome than defaulting.
    expect(hashToRoute(`#/channel/${CHANNEL}/nonsense`)).toEqual({
      name: 'channel',
      channelId: CHANNEL,
    });
  });

  it('rejects non-positive and non-integer local playlist ids', () => {
    expect(hashToRoute('#/library/playlist/0').name).toBe('notFound');
    expect(hashToRoute('#/library/playlist/-1').name).toBe('notFound');
    expect(hashToRoute('#/library/playlist/abc').name).toBe('notFound');
    expect(hashToRoute('#/library/playlist/1.5').name).toBe('notFound');
  });

  it('ignores a nonsensical start time instead of seeking to it', () => {
    expect(hashToRoute(`#/watch/${VIDEO}?t=-5`)).toEqual({ name: 'watch', videoId: VIDEO });
    expect(hashToRoute(`#/watch/${VIDEO}?t=abc`)).toEqual({ name: 'watch', videoId: VIDEO });
  });

  it('requires a query for the search route', () => {
    expect(hashToRoute('#/search').name).toBe('notFound');
  });

  it('routes an unknown path to notFound with the path preserved', () => {
    expect(hashToRoute('#/nope/deeper')).toEqual({ name: 'notFound', path: '/nope/deeper' });
  });

  it('never throws on adversarial input', () => {
    for (const hostile of ['#//////', '#/?', '#/watch?t=1', '#/%', '#/'.repeat(200)]) {
      expect(() => hashToRoute(hostile), hostile).not.toThrow();
    }
  });
});

describe('navigation helpers', () => {
  it('recognises the same destination regardless of object identity', () => {
    expect(isSameRoute({ name: 'home' }, { name: 'home' })).toBe(true);
    expect(isSameRoute({ name: 'watch', videoId: VIDEO }, { name: 'watch', videoId: VIDEO })).toBe(
      true,
    );
    expect(isSameRoute({ name: 'home' }, { name: 'history' })).toBe(false);
  });

  it('clears the sidebar selection on detail routes and keeps it on section routes', () => {
    expect(sidebarSectionFor({ name: 'watch', videoId: VIDEO })).toBeNull();
    expect(sidebarSectionFor({ name: 'localPlaylist', id: toLocalPlaylistId(1) })).toBe('library');
    expect(sidebarSectionFor({ name: 'history' })).toBe('history');
  });
});
