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

import { BadgeCheck, Clapperboard, ListVideo } from 'lucide-react';
import { Fragment, useCallback, useRef, useState, type ReactNode, useEffect } from 'react';

import { EmptyState } from '@/components/common/EmptyState';
import { LazyImage } from '@/components/common/LazyImage';
import { playlistName } from '@/components/library/playlistName';
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
import {
  lastFeed,
  recommendedKey,
  sharedRecommended,
  sharedShortsFeed,
  shortsKey,
} from '@/services/feedCache';
import { capabilities, loadCapabilities } from '@/services/capabilities';
import { invoke, normalizeError } from '@/services/ipc';
import type { RecommendedFeed } from '@/services/ipc';
import { useFeedStore } from '@/stores/feed';
import { useUiStore } from '@/stores/ui';
import {
  bestThumbnailFor,
  isPortraitVideo,
  type ChannelDetails,
  type ChannelLink,
  type ChannelTab,
  type ExploreCategory,
  type LocalPlaylist,
  type LocalPlaylistId,
  type SearchItem,
  type SearchResultKind,
  type VideoId,
  type VideoSummary,
} from '@/types/domain';

import { Link, useNavigate } from './router';
import { HOME, type Route } from './routes';

/** Placeholder cards for a first load. Roughly one screenful at 1080p. */
const SKELETON_COUNT = 12;

/** How many history rows a library page fetches at once. */
const HISTORY_PAGE_SIZE = 60;

/** How many recommendations the home feed asks for. Roughly three screenfuls at 1080p. */
const RECOMMENDED_COUNT = 36;

/** How many playlist items one screen loads. */
const PLAYLIST_PAGE_SIZE = 200;

/** How many Shorts the tab loads at once. */
const SHORTS_COUNT = 40;

/** How many more it fetches each time the viewer nears the end. */
const SHORTS_PAGE_SIZE = 20;

/** How many Shorts the home shelf peeks at. Smaller than the tab: it is a row, not a screen. */
const HOME_SHORTS_COUNT = 16;

/**
 * The channel banner's box.
 *
 * The provider serves the widest rendition at 2560x424, and reserving that ratio up front is what
 * stops the whole page jumping down when the image lands.
 */
const BANNER_ASPECT = '2560 / 424';

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

  const results = useAsyncResource(
    `search:${kind}:${query}`,
    (signal) => invoke('search', { query, filters }, { signal }),
    { navigation: true },
  );

  // Read through a resource rather than the module directly, so the view re-renders once the
  // provider has answered. Resolves from memory on every mount after the first.
  const reported = useAsyncResource('capabilities', () => loadCapabilities());
  const supported = reported.data ?? capabilities();

  // A playlist card that leads nowhere is worse than no card. The extractor's remote-playlist
  // parser no longer matches YouTube's response, the provider reports `playlists: false` because of
  // it, and these results were still being rendered as links into a Not Found page (§131).
  const items = (results.data?.page.items ?? []).filter(
    (item) => item.type !== 'playlist' || supported.playlists,
  );

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
  // Folded into the key so that clicking Home in the sidebar fetches a new feed rather than
  // re-serving the one already on screen. See `useFeedStore`.
  const revision = useFeedStore((state) => state.revision);
  const recommended = useAsyncResource(
    `home:recommended:${String(revision)}`,
    (signal) => sharedRecommended(RECOMMENDED_COUNT, revision, signal),
    { navigation: true },
  );
  // Its own resource key rather than the tab's: the shelf asks for far fewer, and sharing a key
  // would make the two fight over one cache entry every time the user moved between them.
  const homeShorts = useAsyncResource(
    `home:shorts:${String(revision)}`,
    (signal) => sharedShortsFeed(HOME_SHORTS_COUNT, revision, signal),
    { navigation: true },
  );

  const continueWatching = resumable.data ?? [];
  const recentlyWatched = recent.data ?? [];
  // The grid stays landscape and the portrait ones move to the shelf, which is how YouTube's home
  // is arranged and is also what keeps grid rows from being sized by a card twice their height.
  // Falls back to whatever was last fetched, which is what makes arriving here instant. The shell
  // keys the routed view on the route name, so coming back from a video remounts this component
  // with no state at all — without the cache behind it, every return to Home is a skeleton.
  const recommendedAll =
    (recommended.data ?? lastFeed<RecommendedFeed>(recommendedKey(RECOMMENDED_COUNT)))?.videos ??
    [];
  const suggestions = recommendedAll.filter((video) => !isPortraitVideo(video));

  // The shelf carries both what the shorts query returned and any portrait items lifted out of the
  // recommendations, deduplicated: the same short can legitimately arrive from both.
  const recentSummaries = recentlyWatched.map(historyToSummary);
  const recentlyWatchedVideos = recentSummaries.filter((video) => !isPortraitVideo(video));
  const recentlyWatchedShorts = recentSummaries.filter(isPortraitVideo);

  const shelfSeen = new Set<string>();
  const shortsShelf = [
    ...(homeShorts.data ?? lastFeed<VideoSummary[]>(shortsKey(HOME_SHORTS_COUNT)) ?? []),
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
              onRemoveFromHistory={() => {
                void invoke('delete_history_entry', { videoId: entry.video_id })
                  .then(() => {
                    // Both shelves are drawn from history, so both are re-read.
                    resumable.reload();
                    recent.reload();
                  })
                  .catch((cause: unknown) => {
                    useUiStore.getState().toast({
                      messageKey: normalizeError(cause).message_key,
                      tone: 'danger',
                      durationMs: 6000,
                    });
                  });
              }}
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
  const revision = useFeedStore((state) => state.revision);
  const shorts = useAsyncResource(
    `shorts:feed:${String(revision)}`,
    (signal) => sharedShortsFeed(SHORTS_COUNT, revision, signal),
    { navigation: true },
  );

  // Everything fetched after the first batch. The view owns the accumulation because the resource
  // hook models one request, not a growing list.
  //
  // Tagged with the revision it was fetched for, and read back only when that still matches. A
  // refresh replaces the base feed, and pages fetched against the *previous* feed have no business
  // being appended to the new one — deriving that here rather than resetting in an effect, which
  // would be a render-cascade for something already knowable.
  const [more, setMore] = useState<{ revision: number; items: readonly VideoSummary[] }>({
    revision,
    items: [],
  });
  const carried = more.revision === revision ? more.items : [];
  const loadingMore = useRef(false);

  // The same test the native side applies, restated here because the vertical player renders
  // whatever it is handed and the invariant otherwise lives only in another crate. Deliberately not
  // the bare `is_short` marker: that marker is absent for most short-form video this extractor
  // returns, and filtering on it emptied the tab.
  const seen = new Set<string>();
  const base = shorts.data ?? lastFeed<VideoSummary[]>(shortsKey(SHORTS_COUNT)) ?? [];
  const videos = [...base, ...carried].filter((video) => {
    if (!isPortraitVideo(video) || seen.has(video.id)) return false;
    seen.add(video.id);
    return true;
  });

  const loadMore = useCallback(
    (recent: readonly VideoId[], all: readonly VideoId[]) => {
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
          setMore((current) =>
            current.revision === revision
              ? { revision, items: [...current.items, ...batch] }
              : { revision, items: batch },
          );
        })
        .catch(() => {
          // The feed simply stops growing; the videos already loaded still play.
        })
        .finally(() => {
          loadingMore.current = false;
        });
    },
    [revision],
  );

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
  view_count?: number;
  published_at?: number;
  channel_avatar?: VideoSummary['channel_avatar'];
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
    // Stored when the video was watched. Without these a "Continue watching" card was visibly
    // poorer than every card beneath it — no view count, no date.
    ...(entry.view_count !== undefined ? { view_count: entry.view_count } : {}),
    ...(entry.published_at !== undefined ? { published_at: entry.published_at } : {}),
    ...(entry.channel_avatar !== undefined ? { channel_avatar: entry.channel_avatar } : {}),
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
              onRemoveFromHistory={() => {
                void invoke('delete_history_entry', { videoId: entry.video_id })
                  .then(() => {
                    history.reload();
                  })
                  .catch((cause: unknown) => {
                    useUiStore.getState().toast({
                      messageKey: normalizeError(cause).message_key,
                      tone: 'danger',
                      durationMs: 6000,
                    });
                  });
              }}
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

/**
 * One browsable category.
 *
 * A plain grid, because that is what the provider's own category pages are: an editorial hub whose
 * front page collects videos from across the service. Nothing here is personalised and nothing
 * needs an account, which is the whole reason these can be offered at all (§43).
 *
 * The heading names the category rather than leaving the page unlabelled — the sidebar selection
 * says the same thing, but a page that states what it is survives being opened from a link.
 */
function ExploreView({ category }: { category: ExploreCategory }): ReactNode {
  const t = useTranslation();
  const content = useAsyncResource(
    `explore:${category}`,
    (signal) => invoke('get_explore', { category }, { signal }),
    { navigation: true },
  );

  const videos = content.data?.items ?? [];

  return (
    <>
      <h1 className="text-text mb-6 text-2xl font-medium">
        {t.t(`explore.${category}` as TranslationKey)}
      </h1>

      {content.error && videos.length === 0 ? (
        <ErrorState error={content.error} onRetry={content.reload} />
      ) : content.loading && videos.length === 0 ? (
        <FeedSkeleton />
      ) : videos.length === 0 ? (
        <EmptyState titleKey="explore.empty" icon="search" />
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

/**
 * One channel, laid out the way the provider's own site lays one out.
 *
 * ## What is here, and what is deliberately not
 *
 * The site's page is a banner, an avatar, a name, a metadata line, a description that expands, a
 * row of the owner's links and a row of tabs. All of that is drawn here, because the adapter
 * returns all of it — it was simply being discarded before it reached this file.
 *
 * Three things the site has are absent on purpose, each because this build cannot do them
 * truthfully (§131):
 *
 * - **Subscribe / Join.** There is no account in this application, so there is nothing to
 *   subscribe with.
 * - **A Playlists tab.** The provider's playlists endpoint returns zero items for every channel
 *   measured, so the tab would always open onto nothing.
 * - **Latest / Popular / Oldest chips.** The ordered-videos endpoint refuses every request, for
 *   every channel and every tab. A sort control that silently does not sort is worse than none.
 *
 * Tabs come from `available_tabs`, which the adapter fills from what each channel reports having,
 * so a channel with no Shorts never grows a Shorts tab.
 */
function ChannelView({
  channelId,
  tab,
}: {
  channelId: VideoSummary['channel_id'] & string;
  tab: ChannelTab;
}): ReactNode {
  const t = useTranslation();
  const channel = useAsyncResource(
    `channel:${channelId}`,
    (signal) => invoke('get_channel', { channelId }, { signal }),
    { navigation: true },
  );
  const content = useAsyncResource(`channel-content:${channelId}:${tab}`, (signal) =>
    invoke('get_channel_content', { channelId, tab }, { signal }),
  );

  const details = channel.data;
  const videos = content.data?.items ?? [];
  // Until the header arrives there is nothing to say about which tabs exist, and guessing would
  // mean drawing a tab bar that rearranges itself a moment later.
  const tabs = details?.available_tabs ?? [];

  return (
    <>
      {details ? <ChannelHeader details={details} /> : <ChannelHeaderSkeleton />}

      {tabs.length > 0 && (
        <nav className="border-border mb-6 flex gap-8 border-b" aria-label={t.t('channel.tabs')}>
          {tabs.map((name) => {
            const active = name === tab;
            return (
              <Link
                key={name}
                to={{ name: 'channel', channelId, tab: name }}
                aria-current={active ? 'page' : undefined}
                className={`-mb-px border-b-2 px-1 pb-3 text-sm font-medium transition-colors ${
                  active
                    ? 'border-text text-text'
                    : 'text-text-muted hover:text-text border-transparent'
                }`}
              >
                {t.t(`channel.${name}` as TranslationKey)}
              </Link>
            );
          })}
        </nav>
      )}

      {content.error && videos.length === 0 ? (
        <ErrorState error={content.error} onRetry={content.reload} />
      ) : content.loading && videos.length === 0 ? (
        <FeedSkeleton />
      ) : videos.length === 0 ? (
        <EmptyState titleKey="channel.empty" icon="search" />
      ) : tab === 'shorts' ? (
        // Every item on this tab is a short, so the grid is chosen once here rather than card by
        // card: a shelf of 9:16 cards laid out on the 16:9 grid leaves a row of gaps.
        <ShortsGrid>
          {videos.map((video) => (
            <ShortsCard key={video.id} video={video} />
          ))}
        </ShortsGrid>
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

/**
 * The banner, the avatar, and everything written beside them.
 *
 * The description starts clamped to one line, as the site's does, and expands in place rather than
 * into a dialog — the same information with one less thing to dismiss. Expanding also reveals the
 * owner's remaining links and the figures the site keeps behind its own *About* panel.
 */
function ChannelHeader({ details }: { details: ChannelDetails }): ReactNode {
  const t = useTranslation();
  const [expanded, setExpanded] = useState(false);

  const banner = details.banner?.at(-1);
  const avatar = details.avatar?.at(-1);
  const links = details.links ?? [];
  const description = details.description?.trim() ?? '';

  // Assembled as a list and joined, so a channel that hides its subscriber count does not leave a
  // stray separator behind.
  const metadata = [
    details.handle !== undefined ? `@${details.handle}` : undefined,
    details.subscriber_count !== undefined
      ? t.plural('video.subscribers', details.subscriber_count, {
          count: t.compact(details.subscriber_count),
        })
      : undefined,
    details.video_count !== undefined
      ? t.plural('channel.videoCount', details.video_count, {
          count: t.compact(details.video_count),
        })
      : undefined,
  ].filter((part): part is string => part !== undefined);

  const expandable = description.length > 0 || links.length > 0;

  return (
    <header className="mb-6">
      {banner && (
        <LazyImage
          src={banner.url}
          alt=""
          aspectRatio={BANNER_ASPECT}
          className="mb-4 w-full rounded-xl object-cover"
        />
      )}

      <div className="flex flex-col gap-4 sm:flex-row sm:items-center">
        {avatar ? (
          <img
            src={avatar.url}
            alt=""
            className="size-20 shrink-0 rounded-full object-cover sm:size-40"
          />
        ) : (
          <div className="bg-surface-hover size-20 shrink-0 rounded-full sm:size-40" />
        )}

        <div className="flex min-w-0 flex-col gap-1">
          <h1 className="text-text flex items-center gap-2 text-2xl font-bold sm:text-4xl">
            <span className="min-w-0 break-words">{details.name}</span>
            {details.is_verified === true && (
              <BadgeCheck
                size={20}
                className="text-text-muted shrink-0"
                aria-label={t.t('channel.verified')}
              />
            )}
          </h1>

          {metadata.length > 0 && <p className="text-text-muted text-sm">{metadata.join(' • ')}</p>}

          {/* Clamped even while the panel below is open. Letting it grow in place would drag the
              avatar down with it — the row centres on this column — so the full text lives in the
              panel instead, which is also where the site puts it. */}
          {description.length > 0 && (
            <p className="text-text-muted line-clamp-1 max-w-2xl text-sm">{description}</p>
          )}

          {/* Collapsed, this is the site's "YouTube and 6 more links" line; expanded, the panel
              below carries the whole list. Either way it is one control in one place, so nothing
              moves under the pointer. */}
          {expandable && !expanded && (
            <button
              type="button"
              onClick={() => setExpanded(true)}
              className="text-text hover:text-text-muted w-fit text-sm font-medium transition-colors"
            >
              {links.length > 0
                ? [
                    links[0]?.title,
                    links.length > 1
                      ? t.plural('channel.andMoreLinks', links.length - 1, {
                          count: t.number(links.length - 1),
                        })
                      : undefined,
                  ]
                    .filter((part): part is string => part !== undefined && part.length > 0)
                    .join(' ')
                : t.t('channel.more')}
            </button>
          )}
        </div>
      </div>

      {expanded && (
        <ChannelAbout
          details={details}
          description={description}
          links={links}
          onCollapse={() => setExpanded(false)}
        />
      )}
    </header>
  );
}

/** What the header leaves out: the rest of the description, the owner's links, and the figures. */
function ChannelAbout({
  details,
  description,
  links,
  onCollapse,
}: {
  details: ChannelDetails;
  description: string;
  links: ChannelLink[];
  onCollapse: () => void;
}): ReactNode {
  const t = useTranslation();

  // The adapter stores a country *code* rather than a country name, so each locale can name the
  // place itself. `DisplayNames` is not obliged to know every code, and one it does not know is
  // dropped rather than shown raw: a bare "US" in the middle of a sentence is not information
  // anybody asked for.
  const country = ((): string | undefined => {
    if (details.country === undefined) return undefined;
    try {
      return new Intl.DisplayNames([t.locale], { type: 'region' }).of(details.country);
    } catch {
      return undefined;
    }
  })();

  const facts = [
    details.joined_at !== undefined
      ? t.t('channel.joined', { date: t.date(details.joined_at, { dateStyle: 'long' }) })
      : undefined,
    details.view_count !== undefined
      ? t.plural('channel.totalViews', details.view_count, { count: t.compact(details.view_count) })
      : undefined,
    country,
  ].filter((fact): fact is string => fact !== undefined);

  return (
    <div className="mt-4 flex max-w-3xl flex-col gap-3">
      {description.length > 0 && (
        <p className="text-text text-sm whitespace-pre-line">{description}</p>
      )}

      {links.length > 0 && (
        <ul className="flex flex-wrap gap-x-5 gap-y-1">
          {links.map((link) => (
            <li key={link.url}>
              <ExternalLink url={link.url} label={link.title} />
            </li>
          ))}
        </ul>
      )}

      {facts.length > 0 && <p className="text-text-muted text-sm">{facts.join(' • ')}</p>}

      <div className="flex items-center gap-5">
        <button
          type="button"
          onClick={onCollapse}
          className="text-text hover:text-text-muted text-sm font-medium transition-colors"
        >
          {t.t('channel.showLess')}
        </button>
        {details.canonical_url !== undefined && (
          <ExternalLink url={details.canonical_url} label={t.t('channel.openOnYouTube')} />
        )}
      </div>
    </div>
  );
}

/**
 * A link out to the viewer's own browser.
 *
 * A button rather than an `<a>`: the webview must never navigate away from the application, and an
 * anchor carrying an external href is one middle-click from doing exactly that. The URL is checked
 * again on the far side of the command — `open_external` admits `https` and nothing else — so this
 * is not the only thing standing between a drifted provider response and the shell.
 */
function ExternalLink({ url, label }: { url: string; label: string }): ReactNode {
  return (
    <button
      type="button"
      onClick={() => {
        void invoke('open_external', { url }).catch(() => {
          // Nothing to recover here: the link either opens or it is refused, and being refused is
          // the outcome the validation exists to produce.
        });
      }}
      className="text-accent hover:text-accent-hover text-sm underline-offset-2 hover:underline"
      title={url}
    >
      {label.length > 0 ? label : url}
    </button>
  );
}

/** The header's shape while it loads, so the page does not jump when the real one arrives. */
function ChannelHeaderSkeleton(): ReactNode {
  return (
    <header className="mb-6" aria-hidden="true">
      <div className="skeleton mb-4 w-full rounded-xl" style={{ aspectRatio: BANNER_ASPECT }} />
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center">
        <div className="skeleton size-20 shrink-0 rounded-full sm:size-40" />
        <div className="flex flex-col gap-2">
          <div className="skeleton h-9 w-64 rounded" />
          <div className="skeleton h-4 w-48 rounded" />
          <div className="skeleton h-4 w-80 rounded" />
        </div>
      </div>
    </header>
  );
}

/**
 * The user's playlists.
 *
 * Local only, and the screen says so by what it offers: rename and delete, but no share, no
 * collaborators, no sync. These lists exist on this machine and nowhere else (§42).
 *
 * Built-in lists come first and cannot be renamed or deleted. The storage layer enforces that; the
 * controls are also absent here rather than present-and-refusing, which is the rule the rest of the
 * application follows (§131).
 */
function PlaylistsView(): ReactNode {
  const t = useTranslation();
  const openOverlay = useUiStore((state) => state.openOverlay);
  const revision = useUiStore((state) => state.playlistRevision);

  // The revision forces a refetch, but it must not be part of the *identity*. A key change means
  // a different question, so the previous answer is discarded and the screen blanks to a skeleton —
  // which is what renaming or deleting a playlist started doing to the whole list. `reload()` goes
  // through the nonce instead: same question, asked again, with what is on screen left alone.
  const playlists = useAsyncResource('playlists', () => invoke('get_playlists', undefined), {
    navigation: true,
  });
  const reloadPlaylists = playlists.reload;
  useEffect(() => {
    reloadPlaylists();
  }, [revision, reloadPlaylists]);
  const lists = playlists.data ?? [];

  return (
    <>
      <div className="mb-4 flex items-center justify-between gap-4">
        <h1 className="text-text text-xl font-medium">{t.t('library.playlists')}</h1>
        <button
          type="button"
          onClick={() => {
            openOverlay({ kind: 'createPlaylist' });
          }}
          className="transition-surface bg-surface hover:bg-surface-hover text-text shrink-0 rounded-full px-4 py-2 text-sm font-medium"
        >
          {t.t('library.newPlaylist')}
        </button>
      </div>

      {playlists.error && lists.length === 0 ? (
        <ErrorState error={playlists.error} onRetry={playlists.reload} />
      ) : lists.length === 0 && playlists.loading ? (
        <FeedSkeleton />
      ) : (
        <VideoGrid>
          {lists.map((list) => (
            <PlaylistCard key={list.id} list={list} />
          ))}
        </VideoGrid>
      )}
    </>
  );
}

/** One playlist in the grid: its cover, its name, and how much is in it. */
function PlaylistCard({ list }: { list: LocalPlaylist }): ReactNode {
  const t = useTranslation();
  const cover = list.thumbnails ? bestThumbnailFor(list.thumbnails, 640) : undefined;

  return (
    <article className="feed-card flex flex-col gap-3">
      <Link
        to={{ name: 'localPlaylist', id: list.id }}
        className="bg-surface relative block overflow-hidden rounded-md"
        style={{ aspectRatio: '16 / 9' }}
      >
        {cover ? (
          <LazyImage
            src={cover.url}
            alt=""
            className="size-full object-cover"
            placeholder={<div className="bg-surface size-full" aria-hidden="true" />}
          />
        ) : (
          <div className="bg-surface text-text-muted grid size-full place-items-center">
            <ListVideo size={28} />
          </div>
        )}
        {/* The count sits on the cover, as YouTube's does, so the row below stays one line. */}
        <span className="absolute right-1 bottom-1 rounded bg-black/80 px-1.5 py-0.5 text-xs font-medium text-white">
          {t.plural('library.itemCount', list.item_count, { count: String(list.item_count) })}
        </span>
      </Link>
      <h3 className="text-text line-clamp-2 text-sm leading-snug font-medium">
        {playlistName(list, t.t)}
      </h3>
    </article>
  );
}

/**
 * One playlist, and what is in it.
 *
 * The route has carried an identifier from the beginning and rendered the generic library page, so
 * every playlist link landed on the same screen.
 */
function LocalPlaylistView({ id }: { id: LocalPlaylistId }): ReactNode {
  const t = useTranslation();
  const navigate = useNavigate();
  const openOverlay = useUiStore((state) => state.openOverlay);
  const closeOverlay = useUiStore((state) => state.closeOverlay);
  const bump = useUiStore((state) => state.notePlaylistsChanged);
  const revision = useUiStore((state) => state.playlistRevision);

  // As above: a local edit is a refetch of this playlist, not a different playlist.
  const loaded = useAsyncResource(
    `playlist:${String(id)}`,
    async () => {
      const [lists, items] = await Promise.all([
        invoke('get_playlists', undefined),
        invoke('get_playlist_items', { playlistId: id, limit: PLAYLIST_PAGE_SIZE, offset: 0 }),
      ]);
      return { list: lists.find((candidate) => candidate.id === id), items };
    },
    { navigation: true },
  );
  const reloadPlaylist = loaded.reload;
  useEffect(() => {
    reloadPlaylist();
  }, [revision, reloadPlaylist]);

  const list = loaded.data?.list;
  const items = loaded.data?.items ?? [];

  if (loaded.error && !loaded.data) {
    return <ErrorState error={loaded.error} onRetry={loaded.reload} />;
  }
  // Settled, and no such playlist: deleted in another window, or a stale link.
  if (!loaded.loading && !list) {
    return (
      <EmptyState
        titleKey="library.empty.playlists"
        bodyKey="library.empty.playlistsHint"
        icon="library"
      />
    );
  }

  return (
    <>
      <div className="mb-4 flex items-start justify-between gap-4">
        <div className="min-w-0">
          <h1 className="text-text truncate text-xl font-medium">
            {list ? playlistName(list, t.t) : ''}
          </h1>
          {list && (
            <p className="text-text-muted mt-1 text-sm">
              {t.plural('library.itemCount', list.item_count, {
                count: String(list.item_count),
              })}
            </p>
          )}
        </div>

        {/* Absent for a built-in list rather than disabled: it cannot be renamed or deleted, and a
            control that refuses is worse than one that is not there (§131). */}
        {list && list.is_system !== true && (
          <div className="flex shrink-0 gap-2">
            <button
              type="button"
              onClick={() => {
                openOverlay({ kind: 'renamePlaylist', id: list.id, currentName: list.name });
              }}
              className="transition-surface bg-surface hover:bg-surface-hover text-text rounded-full px-4 py-2 text-sm"
            >
              {t.t('app.rename')}
            </button>
            <button
              type="button"
              onClick={() => {
                openOverlay({
                  kind: 'confirm',
                  titleKey: 'library.deletePlaylist',
                  bodyKey: 'library.deletePlaylistHint',
                  confirmKey: 'app.delete',
                  onConfirm: () => {
                    void invoke('delete_playlist', { playlistId: list.id }).then(
                      () => {
                        bump();
                        closeOverlay();
                        // Back to the list: staying on a playlist that no longer exists would show
                        // the "no such playlist" state as though something had gone wrong.
                        navigate({ name: 'library' });
                      },
                      (cause: unknown) => {
                        // The failure branch, not a `.catch` in the middle of the chain: catching
                        // there reported the error and then ran the success path anyway, so a
                        // playlist that had not been deleted still closed the dialog and sent the
                        // viewer back to a library that still contained it.
                        useUiStore.getState().toast({
                          messageKey: normalizeError(cause).message_key,
                          tone: 'danger',
                          durationMs: 6000,
                        });
                      },
                    );
                  },
                });
              }}
              className="transition-surface border-danger text-danger hover:bg-danger rounded-full border px-4 py-2 text-sm hover:text-white"
            >
              {t.t('app.delete')}
            </button>
          </div>
        )}
      </div>

      {loaded.loading && items.length === 0 ? (
        <FeedSkeleton />
      ) : items.length === 0 ? (
        <EmptyState
          titleKey="library.empty.playlistItems"
          bodyKey="library.empty.playlistsHint"
          icon="library"
        />
      ) : (
        <VideoGrid>
          {items.map((item) => (
            <div key={item.video.id} className="flex flex-col gap-2">
              <VideoCard video={item.video} />
              <button
                type="button"
                onClick={() => {
                  void invoke('remove_from_playlist', {
                    playlistId: id,
                    videoId: item.video.id,
                  }).then(
                    () => {
                      bump();
                    },
                    (cause: unknown) => {
                      useUiStore.getState().toast({
                        messageKey: normalizeError(cause).message_key,
                        tone: 'danger',
                        durationMs: 6000,
                      });
                    },
                  );
                }}
                className="transition-surface text-text-muted hover:text-text self-start text-xs"
              >
                {t.t('library.removeFromPlaylist')}
              </button>
            </div>
          ))}
        </VideoGrid>
      )}
    </>
  );
}

/**
 * An address that matches no screen.
 *
 * Deliberately not the generic error. This is reached by a stale link, a mistyped hash, or a
 * bookmark from an older build — none of which is anything going wrong, and calling it that
 * invites a bug report about a typo. The raw path is gone with it: it was rendered in a monospace
 * block under the message, which is the shape of a diagnostic, and it told the viewer nothing they
 * did not already have in the address bar.
 *
 * The way out matters more than the explanation, so the button is the point of the screen.
 */
function NotFoundView(): ReactNode {
  const navigate = useNavigate();
  return (
    <EmptyState
      titleKey="error.notFound"
      bodyKey="error.notFoundHint"
      icon="error"
      action={{
        labelKey: 'nav.home',
        onClick: () => {
          navigate(HOME);
        },
      }}
    />
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
    case 'explore':
      return <ExploreView category={route.category} />;
    case 'playlist':
      return <NotFoundView />;
    case 'localPlaylist':
      return <LocalPlaylistView id={route.id} />;
    case 'library':
      return <PlaylistsView />;
    case 'history':
      return <HistoryView />;
    case 'bookmarks':
      return <BookmarksView />;
    case 'settings':
      return <SettingsView />;
    case 'diagnostics':
      return <DiagnosticsView />;
    case 'notFound':
      return <NotFoundView />;
  }
}
