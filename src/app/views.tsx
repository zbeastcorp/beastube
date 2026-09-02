/**
 * Route-to-view mapping.
 *
 * A plain switch rather than a route table with lazy imports: the whole UI is a few hundred
 * kilobytes served from the local filesystem, so code-splitting the views would trade a real
 * complexity cost for a saving that does not exist in a desktop shell. `shaka-player` is the one
 * genuinely large dependency, and it is split out in the Vite config where it belongs.
 */

import type { ReactNode } from 'react';

import { EmptyState } from '@/components/common/EmptyState';
import { VideoCardSkeleton, VideoGrid } from '@/components/video/VideoCard';
import { useTranslation } from '@/i18n/context';

import type { Route } from './routes';

/** How many placeholder cards to draw while a feed loads. Roughly one screenful at 1080p. */
const SKELETON_COUNT = 12;

function PageHeading({ children }: { children: ReactNode }): ReactNode {
  return <h1 className="text-text mb-4 text-xl font-medium">{children}</h1>;
}

/**
 * A feed placeholder.
 *
 * Used until the provider layer lands. It renders the real grid geometry, so replacing it with
 * live data changes no layout.
 */
function FeedSkeleton(): ReactNode {
  return (
    <VideoGrid>
      {Array.from({ length: SKELETON_COUNT }, (_, index) => (
        <VideoCardSkeleton key={index} />
      ))}
    </VideoGrid>
  );
}

function HomeView(): ReactNode {
  const t = useTranslation();
  return (
    <>
      <PageHeading>{t.t('home.title')}</PageHeading>
      <FeedSkeleton />
    </>
  );
}

function ShortsView(): ReactNode {
  const t = useTranslation();
  return (
    <>
      <PageHeading>{t.t('shorts.title')}</PageHeading>
      <FeedSkeleton />
    </>
  );
}

function SearchView({ query }: { query: string }): ReactNode {
  const t = useTranslation();
  return (
    <>
      <PageHeading>{t.t('search.resultsFor', { query })}</PageHeading>
      <FeedSkeleton />
    </>
  );
}

function HistoryView(): ReactNode {
  const t = useTranslation();
  return (
    <>
      <PageHeading>{t.t('library.history')}</PageHeading>
      <EmptyState
        titleKey="library.empty.history"
        bodyKey="library.empty.historyHint"
        icon="history"
      />
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

function BookmarksView(): ReactNode {
  const t = useTranslation();
  return (
    <>
      <PageHeading>{t.t('nav.bookmarks')}</PageHeading>
      <EmptyState
        titleKey="library.empty.bookmarks"
        bodyKey="library.empty.bookmarksHint"
        icon="bookmark"
      />
    </>
  );
}

function SettingsView(): ReactNode {
  const t = useTranslation();
  return (
    <>
      <PageHeading>{t.t('settings.title')}</PageHeading>
      <p className="text-text-muted max-w-prose text-base">{t.t('settings.privacy.subtitle')}</p>
    </>
  );
}

function DiagnosticsView(): ReactNode {
  const t = useTranslation();
  return (
    <>
      <PageHeading>{t.t('diagnostics.title')}</PageHeading>
      <p className="text-text-muted max-w-prose text-base">{t.t('diagnostics.subtitle')}</p>
    </>
  );
}

function NotFoundView({ path }: { path: string }): ReactNode {
  return (
    <EmptyState titleKey="error.generic" bodyKey="error.genericHint" icon="error" detail={path} />
  );
}

function WatchView({ videoId }: { videoId: string }): ReactNode {
  const t = useTranslation();
  return (
    <>
      <div
        className="bg-surface mb-4 w-full overflow-hidden rounded-lg"
        style={{ aspectRatio: '16 / 9' }}
        aria-label={t.t('a11y.playerRegion')}
      />
      <h1 className="text-text text-md font-medium">{videoId}</h1>
    </>
  );
}

/** Renders the view for `route`. */
export function renderRoute(route: Route): ReactNode {
  switch (route.name) {
    case 'home':
      return <HomeView />;
    case 'shorts':
      return <ShortsView />;
    case 'search':
      return <SearchView query={route.query} />;
    case 'watch':
      return <WatchView videoId={route.videoId} />;
    case 'channel':
      return <NotFoundView path={route.channelId} />;
    case 'playlist':
      return <NotFoundView path={route.playlistId} />;
    case 'localPlaylist':
      return <LibraryView />;
    case 'history':
      return <HistoryView />;
    case 'library':
      return <LibraryView />;
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
