/**
 * Route-to-view mapping.
 *
 * A plain switch rather than a route table with lazy imports: the whole UI is a few hundred
 * kilobytes served from the local filesystem, so code-splitting the views would trade real
 * complexity for a saving that does not exist in a desktop shell.
 *
 * Every view follows the same shape — cached or previous data stays on screen, a skeleton fills the
 * first load, and a failure renders an explanation with a retry where one is meaningful. Navigation
 * is never gated on a request (§87).
 */

import { Clapperboard } from 'lucide-react';
import { Fragment, useCallback, useRef, useState, type ReactNode } from 'react';

import { EmptyState } from '@/components/common/EmptyState';
import { ErrorState } from '@/components/common/ErrorState';
import { DiagnosticsView } from '@/components/settings/DiagnosticsView';
import { SettingsView } from '@/components/settings/SettingsView';
import { ShortsFeed } from '@/components/video/ShortsFeed';
import {
  ShortsCard,
  ShortsCardSkeleton,
  ShortsGrid,
  ShortsShelf,
} from '@/components/video/ShortsCard';
import { VideoCard, VideoCardSkeleton, VideoGrid } from '@/components/video/VideoCard';
import { WatchView } from '@/components/video/WatchView';
import { useAsyncResource } from '@/hooks/useAsyncResource';
import type { TranslationKey } from '@/i18n';
import { useTranslation } from '@/i18n/context';
import { sharedShortsFeed } from '@/services/feedCache';
import { invoke } from '@/services/ipc';
import {
  isPortraitVideo,
  type ChannelTab,
  type SearchItem,
  type SearchResultKind,
  type VideoId,
  type VideoSummary,
} from '@/types/domain';

import { Link } from './router';
import type { Route } from './routes';

/** Placeholder cards for a first load. Roughly one screenful at 1080p. */
const SKELETON_COUNT = 12;

/** How many history rows a library page fetches at once. */
const HISTORY_PAGE_SIZE = 60;

/** How many recommendations the home feed asks for. Roughly three screenfuls at 1080p. */
const RECOMMENDED_COUNT = 36;

/** How many Shorts the tab loads at once. */
const SHORTS_COUNT = 40;

/** How many more it fetches each time the viewer nears the end. */
const SHORTS_PAGE_SIZE = 20;

/** How many Shorts the home shelf peeks at. Smaller than the tab: it is a row, not a screen. */
const HOME_SHORTS_COUNT = 16;

/** Videos shown before the first Shorts shelf breaks the grid. */
const VIDEOS_BEFORE_SHELF = 6;

/** Videos between one shelf and the next. */
const VIDEOS_BETWEEN_SHELVES = 9;

/** Shorts in each shelf. */
const SHORTS_PER_SHELF = 8;

function PageHeading({ children }: { children: ReactNode }): ReactNode {
  return <h1 className="text-text mb-4 text-xl font-medium">{children}</h1>;
}

function FeedSkeleton(): ReactNode {
  return (
    <VideoGrid>
      {Array.from({ length: SKELETON_COUNT }, (_, index) => (
        <VideoCardSkeleton key={index} />
      ))}
    </VideoGrid>
  );
}

/**
 * Renders one video with the card its shape calls for.
 *
 * Every grid and rail goes through here, so "a portrait video never appears in a landscape box" is
 * one rule in one place rather than a judgement repeated at each of a dozen call sites.
 */
function FeedCard({ video, width }: { video: VideoSummary; width?: number }): ReactNode {
  return isPortraitVideo(video) ? (
    <ShortsCard video={video} {...(width !== undefined ? { width } : {})} />
  ) : (
    <VideoCard video={video} {...(width !== undefined ? { width } : {})} />
  );
}

/** Renders a heterogeneous search result. */
function SearchResultCard({ item }: { item: SearchItem }): ReactNode {
  const t = useTranslation();

  if (item.type === 'video') {
    return <FeedCard video={item} />;
  }

  if (item.type === 'channel') {
    const avatar = item.avatar?.at(-1);
    return (
      <Link
        to={{ name: 'channel', channelId: item.id, tab: 'videos' }}
        className="transition-surface hover:bg-surface-hover flex flex-col items-center gap-3 rounded-lg p-4 text-center"
      >
        {avatar ? (
          <img
            src={avatar.url}
            alt=""
            loading="lazy"
            className="size-24 rounded-full object-cover"
          />
        ) : (
          <div className="bg-surface size-24 rounded-full" />
        )}
        <div className="flex flex-col gap-1">
          <span className="text-text line-clamp-1 text-base font-medium">{item.name}</span>
          {item.subscriber_count !== undefined && (
            <span className="text-text-muted text-xs">
              {t.plural('video.subscribers', item.subscriber_count, {
                count: t.compact(item.subscriber_count),
              })}
            </span>
          )}
        </div>
      </Link>
    );
  }

  const cover = item.thumbnails?.at(-1);
  return (
    <Link to={{ name: 'playlist', playlistId: item.id }} className="flex flex-col gap-3">
      <div
        className="bg-surface relative overflow-hidden rounded-md"
        style={{ aspectRatio: '16 / 9' }}
      >
        {cover && <img src={cover.url} alt="" loading="lazy" className="size-full object-cover" />}
        {item.video_count !== undefined && (
          <span className="absolute right-1 bottom-1 rounded bg-black/80 px-1.5 py-0.5 text-2xs font-medium text-white">
            {t.plural('library.itemCount', item.video_count)}
          </span>
        )}
      </div>
      <span className="text-text line-clamp-2 text-base font-medium">{item.title}</span>
    </Link>
  );
}

function SearchView({ query, kind }: { query: string; kind: SearchResultKind }): ReactNode {
  const t = useTranslation();
  const filters = { kind };

  const results = useAsyncResource(`search:${kind}:${query}`, (signal) =>
    invoke('search', { query, filters }, { signal }),
  );

  const items = results.data?.page.items ?? [];

  // Shorts are lifted out of the flat list into their own shelf, which is what YouTube does and is
  // also what keeps the grid usable: CSS grid rows size to their tallest item, so one 9:16 card in
  // a column sized for 16:9 cards would give its whole row a ~500px height with landscape cards
  // stranded at the top of it.
  const shorts = items.filter(
    (item): item is Extract<SearchItem, { type: 'video' }> =>
      item.type === 'video' && isPortraitVideo(item),
  );
  const isShort = (item: SearchItem) => item.type === 'video' && isPortraitVideo(item);
  const rest = kind === 'shorts' ? [] : items.filter((item) => !isShort(item));

  return (
    <>
      <PageHeading>{t.t('search.resultsFor', { query })}</PageHeading>

      {results.error && !results.data ? (
        <ErrorState error={results.error} onRetry={results.reload} />
      ) : results.loading && items.length === 0 ? (
        <FeedSkeleton />
      ) : items.length === 0 ? (
        <EmptyState
          titleKey="search.noResults"
          bodyKey="search.noResultsHint"
          icon="search"
          params={{ query }}
        />
      ) : (
        <>
          <p className="text-text-muted mb-4 text-xs">
            {t.plural('search.resultCount', items.length)}
          </p>

          {shorts.length > 0 &&
            (kind === 'shorts' ? (
              // A screen that is entirely shorts gets the tighter portrait column, not a landscape
              // grid with portrait cards floating in it.
              <ShortsGrid>
                {shorts.map((item) => (
                  <ShortsCard key={item.id} video={item} />
                ))}
              </ShortsGrid>
            ) : (
              <section className="mb-8">
                <h2 className="text-text mb-3 flex items-center gap-2 text-lg font-medium">
                  <Clapperboard size={20} strokeWidth={2} />
                  {t.t('shorts.title')}
                </h2>
                <ShortsShelf>
                  {shorts.map((item) => (
                    <div key={item.id} className="shrink-0 snap-start">
                      <ShortsCard video={item} />
                    </div>
                  ))}
                </ShortsShelf>
              </section>
            ))}

          {rest.length > 0 && (
            <VideoGrid>
              {rest.map((item) => (
                <SearchResultCard key={`${item.type}-${itemKey(item)}`} item={item} />
              ))}
            </VideoGrid>
          )}
        </>
      )}
    </>
  );
}

/** A stable list key for a heterogeneous result. */
function itemKey(item: SearchItem): string {
  return item.type === 'channel' ? item.id : item.type === 'playlist' ? item.id : item.id;
}

function HomeView(): ReactNode {
  const t = useTranslation();

  // Three sources, in the order they matter to someone opening the application: what they were part
  // way through, what the local ranker suggests, and what they watched recently. Each is
  // independent, so a slow or failing recommendation pass never delays the rest (§87).
  const resumable = useAsyncResource('home:resumable', () =>
    invoke('get_resumable', { limit: 12 }),
  );
  const recent = useAsyncResource('home:recent', () =>
    invoke('get_history', { limit: 12, offset: 0 }),
  );
  const recommended = useAsyncResource('home:recommended', (signal) =>
    invoke('get_recommended', { limit: RECOMMENDED_COUNT }, { signal }),
  );
  // Its own resource key rather than the tab's: the shelf asks for far fewer, and sharing a key
  // would make the two fight over one cache entry every time the user moved between them.
  const homeShorts = useAsyncResource('home:shorts', (signal) =>
    sharedShortsFeed(HOME_SHORTS_COUNT, signal),
  );

  const continueWatching = resumable.data ?? [];
  const recentlyWatched = recent.data ?? [];
  // The grid stays landscape and the portrait ones move to the shelf, which is how YouTube's home
  // is arranged and is also what keeps grid rows from being sized by a card twice their height.
  const recommendedAll = recommended.data?.videos ?? [];
  const suggestions = recommendedAll.filter((video) => !isPortraitVideo(video));

  // The shelf carries both what the shorts query returned and any portrait items lifted out of the
  // recommendations, deduplicated: the same short can legitimately arrive from both.
  const recentSummaries = recentlyWatched.map(historyToSummary);
  const recentlyWatchedVideos = recentSummaries.filter((video) => !isPortraitVideo(video));
  const recentlyWatchedShorts = recentSummaries.filter(isPortraitVideo);

  const shelfSeen = new Set<string>();
  const shortsShelf = [
    ...(homeShorts.data ?? []),
    ...recommendedAll.filter(isPortraitVideo),
  ].filter((video) => {
    if (shelfSeen.has(video.id)) return false;
    shelfSeen.add(video.id);
    return true;
  });

  // Alternating blocks: a shelf of shorts, then a row of videos, repeated. Sliced here rather than
  // in the markup so the two lists are consumed in step and neither repeats an item.
  const shelfBreaks: { shorts: VideoSummary[]; videos: VideoSummary[] }[] = [];
  for (let cursor = VIDEOS_BEFORE_SHELF, shelfCursor = 0; cursor < recommendedAll.length;) {
    shelfBreaks.push({
      shorts: shortsShelf.slice(shelfCursor, shelfCursor + SHORTS_PER_SHELF),
      videos: suggestions.slice(cursor, cursor + VIDEOS_BETWEEN_SHELVES),
    });
    cursor += VIDEOS_BETWEEN_SHELVES;
    shelfCursor += SHORTS_PER_SHELF;
    if (shelfCursor >= shortsShelf.length && cursor >= suggestions.length) break;
  }

  // Named from what the videos were actually derived from, so a feed of broad topics is not
  // presented as personalization that did not happen (§131).
  const feedHeading: TranslationKey =
    recommended.data?.source === 'discover' ? 'home.discover' : 'home.recommended';

  const empty =
    continueWatching.length === 0 && recentlyWatched.length === 0 && suggestions.length === 0;

  if (empty && (recommended.loading || recent.loading)) {
    return <FeedSkeleton />;
  }

  if (empty) {
    return (
      <>
        <PageHeading>{t.t('home.title')}</PageHeading>
        {recommended.error ? (
          <ErrorState error={recommended.error} onRetry={recommended.reload} />
        ) : (
          <EmptyState titleKey="home.empty" bodyKey="home.emptyHint" icon="search" />
        )}
      </>
    );
  }

  return (
    <>
      {continueWatching.length > 0 && (
        <FeedSection heading={t.t('home.continueWatching')}>
          {continueWatching.map((entry) => (
            <VideoCard
              key={entry.video_id}
              video={historyToSummary(entry)}
              progress={progressOf(entry)}
            />
          ))}
        </FeedSection>
      )}

      {suggestions.length > 0 && (
        <FeedSection heading={t.t(feedHeading)}>
          {suggestions.slice(0, VIDEOS_BEFORE_SHELF).map((video) => (
            <FeedCard key={video.id} video={video} />
          ))}
        </FeedSection>
      )}

      {/* Skeletons occupy the recommended section's final position while it loads.
          Rendering nothing there instead meant the sections below sat high on the page and were
          shoved down a second later when the feed arrived — content moving under the pointer is
          the single most jarring thing a feed can do. */}
      {suggestions.length === 0 && recommended.loading && (
        <FeedSection heading={t.t('home.recommended')}>
          {Array.from({ length: VIDEOS_BEFORE_SHELF }, (_, index) => (
            <VideoCardSkeleton key={index} />
          ))}
        </FeedSection>
      )}

      {/* The shelf reserves its row while it loads, for the same reason the grid above does: a
          section that appears from nothing shoves everything below it down, and the page the user
          started reading moves under them. */}
      {shortsShelf.length === 0 && homeShorts.loading && (
        <section className="mb-10">
          <h2 className="text-text mb-4 flex items-center gap-2 text-lg font-medium">
            <Clapperboard size={22} strokeWidth={2} />
            {t.t('shorts.title')}
          </h2>
          <ShortsShelf>
            {Array.from({ length: SHORTS_PER_SHELF }, (_, index) => (
              <div key={index} className="shrink-0">
                <ShortsCardSkeleton />
              </div>
            ))}
          </ShortsShelf>
        </section>
      )}

      {/* Shelves interleaved between rows of videos rather than one at the end, which is how
          YouTube's home is arranged: a row or two of videos, a shelf of shorts, more videos. The
          shelf is split across the breaks so each one holds different shorts. */}
      {shelfBreaks.map((chunk, breakIndex) => (
        <Fragment key={chunk.shorts[0]?.id ?? `break-${breakIndex}`}>
          {chunk.shorts.length > 0 && (
            <section className="mb-10">
              <h2 className="text-text mb-4 flex items-center gap-2 text-lg font-medium">
                <Clapperboard size={22} strokeWidth={2} />
                {t.t('shorts.title')}
              </h2>
              <ShortsShelf>
                {chunk.shorts.map((video) => (
                  <div key={video.id} className="shrink-0 snap-start">
                    <ShortsCard video={video} />
                  </div>
                ))}
              </ShortsShelf>
            </section>
          )}

          {chunk.videos.length > 0 && (
            <FeedSection heading="">
              {chunk.videos.map((video) => (
                <FeedCard key={video.id} video={video} />
              ))}
            </FeedSection>
          )}
        </Fragment>
      ))}

      {recentlyWatchedVideos.length > 0 && (
        <FeedSection heading={t.t('home.recentlyWatched')}>
          {recentlyWatchedVideos.map((video) => (
            <VideoCard key={video.id} video={video} />
          ))}
        </FeedSection>
      )}

      {/* Watched shorts get a shelf of their own. Mixed into the landscape grid they were half the
          width of their column with a gap beside them, and twice the height, so every row they
          touched was sized by them. */}
      {recentlyWatchedShorts.length > 0 && (
        <section className="mb-10 last:mb-0">
          <h2 className="text-text mb-4 flex items-center gap-2 text-lg font-medium">
            <Clapperboard size={22} strokeWidth={2} />
            {t.t('home.recentlyWatched')}
          </h2>
          <ShortsShelf>
            {recentlyWatchedShorts.map((video) => (
              <div key={video.id} className="shrink-0 snap-start">
                <ShortsCard video={video} />
              </div>
            ))}
          </ShortsShelf>
        </section>
      )}
    </>
  );
}

/** One titled row of the home feed. */
function FeedSection({ heading, children }: { heading: string; children: ReactNode }): ReactNode {
  return (
    <section className="mb-10 last:mb-0">
      {heading !== '' && <h2 className="text-text mb-4 text-lg font-medium">{heading}</h2>}
      <VideoGrid>{children}</VideoGrid>
    </section>
  );
}

/**
 * The Shorts tab.
 *
 * A real feed of short-form videos, assembled natively from searches the provider marks as
 * short-form — the tab used to run a text search for the word "shorts", which looked like a feature
 * and was not one (§131).
 */
function ShortsView({ videoId }: { videoId?: VideoId }): ReactNode {
  // Shared with the launch preload and with Home's shelf, so arriving here reads memory rather
  // than starting the most expensive request in the application and watching it.
  const shorts = useAsyncResource('shorts:feed', (signal) =>
    sharedShortsFeed(SHORTS_COUNT, signal),
  );

  // Everything fetched after the first batch. The view owns the accumulation because the resource
  // hook models one request, not a growing list.
  const [more, setMore] = useState<readonly VideoSummary[]>([]);
  const loadingMore = useRef(false);

  // The same test the native side applies, restated here because the vertical player renders
  // whatever it is handed and the invariant otherwise lives only in another crate. Deliberately not
  // the bare `is_short` marker: that marker is absent for most short-form video this extractor
  // returns, and filtering on it emptied the tab.
  const seen = new Set<string>();
  const videos = [...(shorts.data ?? []), ...more].filter((video) => {
    if (!isPortraitVideo(video) || seen.has(video.id)) return false;
    seen.add(video.id);
    return true;
  });

  const loadMore = useCallback((recent: readonly VideoId[], all: readonly VideoId[]) => {
    // One request in flight at a time. Without this, three quick swipes near the end fire three
    // overlapping fetches that mostly return the same videos.
    if (loadingMore.current) return;
    loadingMore.current = true;
    void invoke('get_more_shorts', {
      seeds: [...recent],
      exclude: [...all],
      limit: SHORTS_PAGE_SIZE,
    })
      .then((batch) => {
        setMore((current) => [...current, ...batch]);
      })
      .catch(() => {
        // The feed simply stops growing; the videos already loaded still play.
      })
      .finally(() => {
        loadingMore.current = false;
      });
  }, []);

  return (
    <ShortsFeed
      videos={videos}
      state={shorts}
      onNearEnd={loadMore}
      {...(videoId !== undefined ? { initialVideoId: videoId } : {})}
    />
  );
}

/** Projects a history row onto the card shape. */
function historyToSummary(entry: {
  video_id: VideoSummary['id'];
  title: string;
  channel_id?: VideoSummary['channel_id'];
  channel_name?: string;
  thumbnails?: VideoSummary['thumbnails'];
  position: { duration_ms?: number };
}): VideoSummary {
  return {
    id: entry.video_id,
    title: entry.title,
    ...(entry.channel_id !== undefined ? { channel_id: entry.channel_id } : {}),
    ...(entry.channel_name !== undefined ? { channel_name: entry.channel_name } : {}),
    ...(entry.thumbnails !== undefined ? { thumbnails: entry.thumbnails } : {}),
    ...(entry.position.duration_ms !== undefined
      ? { duration_ms: entry.position.duration_ms }
      : {}),
  };
}

/** Watched fraction, or undefined when the duration is unknown. */
function progressOf(entry: {
  position: { position_ms: number; duration_ms?: number };
}): number | undefined {
  const { position_ms, duration_ms } = entry.position;
  if (duration_ms === undefined || duration_ms <= 0) return undefined;
  return Math.min(1, position_ms / duration_ms);
}

function HistoryView(): ReactNode {
  const t = useTranslation();
  const history = useAsyncResource('history', () =>
    invoke('get_history', { limit: HISTORY_PAGE_SIZE, offset: 0 }),
  );

  const entries = history.data ?? [];

  return (
    <>
      <PageHeading>{t.t('library.history')}</PageHeading>
      {history.error && entries.length === 0 ? (
        <ErrorState error={history.error} onRetry={history.reload} />
      ) : history.loading && entries.length === 0 ? (
        <FeedSkeleton />
      ) : entries.length === 0 ? (
        <EmptyState
          titleKey="library.empty.history"
          bodyKey="library.empty.historyHint"
          icon="history"
        />
      ) : (
        <VideoGrid>
          {entries.map((entry) => (
            <VideoCard
              key={entry.video_id}
              video={historyToSummary(entry)}
              progress={progressOf(entry)}
            />
          ))}
        </VideoGrid>
      )}
    </>
  );
}

function BookmarksView(): ReactNode {
  const t = useTranslation();
  const bookmarks = useAsyncResource('bookmarks', () =>
    invoke('get_bookmarks', { limit: HISTORY_PAGE_SIZE, offset: 0 }),
  );

  const entries = bookmarks.data ?? [];

  return (
    <>
      <PageHeading>{t.t('nav.bookmarks')}</PageHeading>
      {bookmarks.error && entries.length === 0 ? (
        <ErrorState error={bookmarks.error} onRetry={bookmarks.reload} />
      ) : bookmarks.loading && entries.length === 0 ? (
        <FeedSkeleton />
      ) : entries.length === 0 ? (
        <EmptyState
          titleKey="library.empty.bookmarks"
          bodyKey="library.empty.bookmarksHint"
          icon="bookmark"
        />
      ) : (
        <VideoGrid>
          {entries.map((bookmark) => (
            <VideoCard
              key={bookmark.video_id}
              video={{
                id: bookmark.video_id,
                title: bookmark.title,
                ...(bookmark.channel_id !== undefined ? { channel_id: bookmark.channel_id } : {}),
                ...(bookmark.channel_name !== undefined
                  ? { channel_name: bookmark.channel_name }
                  : {}),
                ...(bookmark.thumbnails !== undefined ? { thumbnails: bookmark.thumbnails } : {}),
              }}
            />
          ))}
        </VideoGrid>
      )}
    </>
  );
}

function ChannelView({
  channelId,
  tab,
}: {
  channelId: VideoSummary['channel_id'] & string;
  tab: ChannelTab;
}): ReactNode {
  const t = useTranslation();
  const channel = useAsyncResource(`channel:${channelId}`, (signal) =>
    invoke('get_channel', { channelId }, { signal }),
  );
  const content = useAsyncResource(`channel-content:${channelId}:${tab}`, (signal) =>
    invoke('get_channel_content', { channelId, tab }, { signal }),
  );

  const videos = content.data?.items ?? [];

  return (
    <>
      {channel.data ? (
        <div className="mb-6 flex items-center gap-4">
          {channel.data.avatar?.at(-1) && (
            <img
              src={channel.data.avatar.at(-1)?.url}
              alt=""
              className="size-20 rounded-full object-cover"
            />
          )}
          <div className="flex flex-col gap-1">
            <h1 className="text-text text-xl font-medium">{channel.data.name}</h1>
            {channel.data.subscriber_count !== undefined && (
              <span className="text-text-muted text-sm">
                {t.plural('video.subscribers', channel.data.subscriber_count, {
                  count: t.compact(channel.data.subscriber_count),
                })}
              </span>
            )}
          </div>
        </div>
      ) : (
        <div className="mb-6 flex items-center gap-4">
          <div className="skeleton size-20 rounded-full" />
          <div className="skeleton h-6 w-48 rounded" />
        </div>
      )}

      {content.error && videos.length === 0 ? (
        <ErrorState error={content.error} onRetry={content.reload} />
      ) : content.loading && videos.length === 0 ? (
        <FeedSkeleton />
      ) : videos.length === 0 ? (
        <EmptyState titleKey="channel.empty" icon="search" />
      ) : (
        <VideoGrid>
          {videos.map((video) => (
            <FeedCard key={video.id} video={video} />
          ))}
        </VideoGrid>
      )}
    </>
  );
}

function LibraryView(): ReactNode {
  const t = useTranslation();
  return (
    <>
      <PageHeading>{t.t('library.title')}</PageHeading>
      <EmptyState
        titleKey="library.empty.playlists"
        bodyKey="library.empty.playlistsHint"
        icon="library"
      />
    </>
  );
}

function NotFoundView({ path }: { path: string }): ReactNode {
  return (
    <EmptyState titleKey="error.generic" bodyKey="error.genericHint" icon="error" detail={path} />
  );
}

/** Renders the view for `route`. */
export function renderRoute(route: Route): ReactNode {
  switch (route.name) {
    case 'home':
      return <HomeView />;
    case 'shorts':
      // The route has always carried an optional video id and always round-tripped through the
      // hash; it was simply never read, so every shorts link landed on the top of the feed.
      return <ShortsView {...(route.videoId !== undefined ? { videoId: route.videoId } : {})} />;
    case 'search':
      return <SearchView query={route.query} kind={route.kind ?? 'all'} />;
    case 'watch':
      // The `?t=` timestamp is parsed by the router and was being dropped here, which made every
      // deep link into the middle of a video start at zero.
      return (
        <WatchView
          videoId={route.videoId}
          {...(route.startAtMs !== undefined ? { startAtMs: route.startAtMs } : {})}
        />
      );
    case 'channel':
      return <ChannelView channelId={route.channelId} tab={route.tab ?? 'videos'} />;
    case 'playlist':
      return <NotFoundView path={route.playlistId} />;
    case 'localPlaylist':
    case 'library':
      return <LibraryView />;
    case 'history':
      return <HistoryView />;
    case 'bookmarks':
      return <BookmarksView />;
    case 'settings':
      return <SettingsView />;
    case 'diagnostics':
      return <DiagnosticsView />;
    case 'notFound':
      return <NotFoundView path={route.path} />;
  }
}
