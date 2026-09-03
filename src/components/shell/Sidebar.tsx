/**
 * The navigation rail.
 *
 * Two forms, matching YouTube's own: a 240px expanded sidebar with grouped, labelled rows, and a
 * 72px mini rail with the icon above a small label. The mini rail shows only the top-level
 * destinations, because a rail of seven identical-looking icons is unusable — that is why YouTube
 * truncates it too.
 *
 * Collapse state lives in the UI store rather than local state so the top bar's hamburger and the
 * keyboard shortcut act on the same value.
 */

import {
  Bookmark,
  Clapperboard,
  History,
  House,
  ListVideo,
  Settings,
  SquarePlay,
  Wrench,
} from 'lucide-react';
import type { ComponentType, ReactNode } from 'react';

import { Link, useRoute } from '@/app/router';
import { useFeedStore } from '@/stores/feed';
import { sidebarSectionFor, type Route, type RouteName } from '@/app/routes';
import { useTranslation } from '@/i18n/context';
import type { TranslationKey } from '@/i18n';

interface NavItem {
  route: Route;
  labelKey: TranslationKey;
  icon: ComponentType<{ size?: number; strokeWidth?: number; className?: string }>;
  /** Shown in the collapsed rail. Only the primary destinations are. */
  inMiniRail: boolean;
}

interface NavGroup {
  /** `null` renders the group without a heading, as YouTube does for the first group. */
  headingKey: TranslationKey | null;
  items: NavItem[];
}

const GROUPS: NavGroup[] = [
  {
    headingKey: null,
    items: [
      { route: { name: 'home' }, labelKey: 'nav.home', icon: House, inMiniRail: true },
      { route: { name: 'shorts' }, labelKey: 'nav.shorts', icon: Clapperboard, inMiniRail: true },
      {
        route: { name: 'library' },
        labelKey: 'nav.library',
        icon: SquarePlay,
        inMiniRail: true,
      },
    ],
  },
  {
    headingKey: 'library.title',
    items: [
      { route: { name: 'history' }, labelKey: 'nav.history', icon: History, inMiniRail: true },
      {
        route: { name: 'library' },
        labelKey: 'nav.playlists',
        icon: ListVideo,
        inMiniRail: false,
      },
      {
        route: { name: 'bookmarks' },
        labelKey: 'nav.bookmarks',
        icon: Bookmark,
        inMiniRail: false,
      },
    ],
  },
  {
    headingKey: null,
    items: [
      { route: { name: 'settings' }, labelKey: 'nav.settings', icon: Settings, inMiniRail: false },
      {
        route: { name: 'diagnostics' },
        labelKey: 'nav.diagnostics',
        icon: Wrench,
        inMiniRail: false,
      },
    ],
  },
];

/** Whether `item` is the destination the user is currently in. */
function isActive(item: NavItem, active: RouteName | null): boolean {
  return active !== null && sidebarSectionFor(item.route) === active;
}

/**
 * Clicking Home or Shorts asks for a *new* feed, as YouTube's does.
 *
 * Only those two. Bumping the revision on History or Bookmarks would mean nothing — those show
 * stored rows, not a generated feed — and the click would pay for a refetch of something that
 * cannot have changed.
 *
 * Note this fires whether or not you are already on the destination. Clicking Home while on Home is
 * how you ask for something new to watch, and returning to Home from a video should not hand back
 * the identical grid you left.
 */
function useFeedRefresh(item: NavItem): (() => void) | undefined {
  const refresh = useFeedStore((state) => state.refresh);
  const name = item.route.name;
  return name === 'home' || name === 'shorts' ? refresh : undefined;
}

function ExpandedRow({ item, active }: { item: NavItem; active: RouteName | null }): ReactNode {
  const t = useTranslation();
  const Icon = item.icon;
  const selected = isActive(item, active);
  const onNavigate = useFeedRefresh(item);

  return (
    <Link
      to={item.route}
      onClick={onNavigate}
      aria-current={selected ? 'page' : undefined}
      className={[
        'transition-surface flex h-10 items-center gap-6 rounded-[10px] px-3',
        selected ? 'bg-surface-hover text-text font-medium' : 'text-text hover:bg-surface-hover',
      ].join(' ')}
    >
      {/* Filled weight for the active row is how YouTube distinguishes it beyond the background. */}
      <Icon size={24} strokeWidth={selected ? 2.4 : 1.8} className="shrink-0" />
      <span className="truncate text-sm">{t.t(item.labelKey)}</span>
    </Link>
  );
}

function MiniRow({ item, active }: { item: NavItem; active: RouteName | null }): ReactNode {
  const t = useTranslation();
  const Icon = item.icon;
  const selected = isActive(item, active);
  const label = t.t(item.labelKey);
  const onNavigate = useFeedRefresh(item);

  return (
    <Link
      to={item.route}
      onClick={onNavigate}
      title={label}
      aria-current={selected ? 'page' : undefined}
      className={[
        'transition-surface flex w-16 flex-col items-center gap-1.5 rounded-[10px] py-4',
        selected ? 'bg-surface-hover text-text' : 'text-text hover:bg-surface-hover',
      ].join(' ')}
    >
      <Icon size={24} strokeWidth={selected ? 2.4 : 1.8} />
      <span className="text-2xs leading-none">{label}</span>
    </Link>
  );
}

/** The navigation rail, expanded or collapsed. */
export function Sidebar({ collapsed }: { collapsed: boolean }): ReactNode {
  const t = useTranslation();
  const activeSection = sidebarSectionFor(useRoute());

  if (collapsed) {
    const miniItems = GROUPS.flatMap((group) => group.items).filter((item) => item.inMiniRail);
    return (
      <nav
        aria-label={t.t('a11y.mainNavigation')}
        className="scroll-region scrollbar-none flex w-[var(--layout-sidebar-collapsed-width)] shrink-0 flex-col items-center gap-1 py-1"
      >
        {miniItems.map((item) => (
          <MiniRow key={t.t(item.labelKey)} item={item} active={activeSection} />
        ))}
      </nav>
    );
  }

  return (
    <nav
      aria-label={t.t('a11y.mainNavigation')}
      className="scroll-region w-[var(--layout-sidebar-width)] shrink-0 px-3 pb-4"
    >
      {GROUPS.map((group, index) => (
        <div key={group.headingKey ?? `group-${index}`}>
          {index > 0 && <hr className="border-border my-3" />}
          {group.headingKey && (
            <h2 className="text-text px-3 pt-1 pb-1 text-md font-medium">
              {t.t(group.headingKey)}
            </h2>
          )}
          <div className="flex flex-col gap-0.5">
            {group.items.map((item) => (
              <ExpandedRow
                key={`${item.route.name}-${item.labelKey}`}
                item={item}
                active={activeSection}
              />
            ))}
          </div>
        </div>
      ))}

      <hr className="border-border my-3" />
      <p className="text-text-subtle px-3 text-xs leading-relaxed">
        {t.t('settings.privacy.subtitle')}
      </p>
    </nav>
  );
}
