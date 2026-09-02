/**
 * The masthead.
 *
 * Mirrors YouTube's 56px bar: hamburger and wordmark on the left, a centred pill search field, and
 * actions on the right. The search field is a real `<form>` so Enter submits and the browser's own
 * accessibility affordances apply.
 *
 * The incognito indicator is deliberately prominent (§50): a mode that silently changes whether
 * activity is recorded must be impossible to be in by accident.
 */

import { EyeOff, Menu, Search, X } from 'lucide-react';
import { useEffect, useRef, useState, type ReactNode, type SyntheticEvent } from 'react';

import { useNavigate, useRoute } from '@/app/router';
import { useTranslation } from '@/i18n/context';
import { useSessionStore } from '@/stores/session';
import { useUiStore } from '@/stores/ui';

/** The product wordmark: the play glyph plus the name, as a link home. */
function Wordmark(): ReactNode {
  const navigate = useNavigate();
  const t = useTranslation();
  return (
    <button
      type="button"
      onClick={() => {
        navigate({ name: 'home' });
      }}
      aria-label={t.t('nav.home')}
      className="no-drag flex items-center gap-1.5 rounded-sm px-1"
    >
      <svg viewBox="0 0 28 20" width="30" height="21" aria-hidden="true" focusable="false">
        <path
          d="M27.4 3.1a3.5 3.5 0 0 0-2.46-2.48C22.77 0 14 0 14 0S5.23 0 3.06.62A3.5 3.5 0 0 0 .6 3.1 36.5 36.5 0 0 0 0 10a36.5 36.5 0 0 0 .6 6.9 3.5 3.5 0 0 0 2.46 2.48C5.23 20 14 20 14 20s8.77 0 10.94-.62a3.5 3.5 0 0 0 2.46-2.48A36.5 36.5 0 0 0 28 10a36.5 36.5 0 0 0-.6-6.9Z"
          fill="var(--color-brand)"
        />
        <path d="M11.2 14.29 18.49 10 11.2 5.71v8.58Z" fill="#fff" />
      </svg>
      <span className="text-md font-semibold tracking-tight">BEASTUBE</span>
    </button>
  );
}

/** The pill search field with its attached submit button. */
function SearchField(): ReactNode {
  const t = useTranslation();
  const navigate = useNavigate();
  const route = useRoute();
  const inputRef = useRef<HTMLInputElement>(null);
  const routeQuery = route.name === 'search' ? route.query : '';
  const [value, setValue] = useState(routeQuery);

  // Reset the field whenever the route's query changes, by remounting rather than by syncing in an
  // effect. An effect would render once with the stale value and again with the fresh one; keying
  // the component makes "a new query is a new field" structural, and React does it in one pass.

  // Ctrl+K and "/" focus search from anywhere, matching the shortcut contract (§54).
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target;
      const typingElsewhere =
        target instanceof HTMLElement &&
        (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable);

      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'l') {
        event.preventDefault();
        inputRef.current?.focus();
        inputRef.current?.select();
      } else if (event.key === '/' && !typingElsewhere) {
        event.preventDefault();
        inputRef.current?.focus();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
    };
  }, []);

  const submit = (event: SyntheticEvent) => {
    event.preventDefault();
    const query = value.trim();
    if (query.length > 0) {
      navigate({ name: 'search', query });
    }
  };

  return (
    <form
      onSubmit={submit}
      role="search"
      className="no-drag flex max-w-[640px] flex-1 items-center"
    >
      <div className="border-border bg-bg focus-within:border-border-focus flex h-10 flex-1 items-center rounded-l-full border py-0 pr-1 pl-4 transition-colors">
        <input
          ref={inputRef}
          type="search"
          value={value}
          onChange={(event) => {
            setValue(event.target.value);
          }}
          placeholder={t.t('search.placeholder')}
          aria-label={t.t('app.search')}
          // The native clear affordance is suppressed so the custom one below can match the theme.
          className="text-text placeholder:text-text-subtle h-full w-full bg-transparent text-md outline-none [&::-webkit-search-cancel-button]:appearance-none"
        />
        {value.length > 0 && (
          <button
            type="button"
            onClick={() => {
              setValue('');
              inputRef.current?.focus();
            }}
            aria-label={t.t('app.clear')}
            className="transition-surface text-text-muted hover:bg-surface-hover hover:text-text grid size-8 shrink-0 place-items-center rounded-full"
          >
            <X size={18} />
          </button>
        )}
      </div>
      <button
        type="submit"
        aria-label={t.t('app.search')}
        className="transition-surface border-border bg-surface hover:bg-surface-hover grid h-10 w-16 shrink-0 place-items-center rounded-r-full border border-l-0"
      >
        <Search size={20} strokeWidth={1.8} />
      </button>
    </form>
  );
}

/** The visible marker that this session records nothing. */
function IncognitoBadge(): ReactNode {
  const t = useTranslation();
  const incognito = useSessionStore((state) => state.incognito);
  const setIncognito = useSessionStore((state) => state.setIncognito);

  if (!incognito) return null;

  return (
    <button
      type="button"
      onClick={() => {
        void setIncognito(false);
      }}
      title={t.t('incognito.description')}
      className="transition-surface bg-surface-translucent hover:bg-surface-translucent-hover text-text no-drag flex h-8 items-center gap-2 rounded-full px-3 text-sm"
    >
      <EyeOff size={16} />
      <span>{t.t('incognito.title')}</span>
    </button>
  );
}

/** The application masthead. */
export function TopBar(): ReactNode {
  const t = useTranslation();
  const route = useRoute();
  const toggleSidebar = useUiStore((state) => state.toggleSidebar);
  const collapsed = useUiStore((state) => state.sidebarCollapsed);

  return (
    <header className="drag-region bg-bg flex h-[var(--layout-topbar-height)] shrink-0 items-center gap-4 px-4">
      <div className="flex shrink-0 items-center gap-1">
        <button
          type="button"
          onClick={toggleSidebar}
          aria-label={t.t(collapsed ? 'nav.expandSidebar' : 'nav.collapseSidebar')}
          aria-expanded={!collapsed}
          className="transition-surface hover:bg-surface-hover no-drag grid size-10 place-items-center rounded-full"
        >
          <Menu size={24} strokeWidth={1.8} />
        </button>
        <Wordmark />
      </div>

      <div className="flex flex-1 justify-center px-4">
        <SearchField key={route.name === 'search' ? route.query : ''} />
      </div>

      <div className="flex shrink-0 items-center gap-2">
        <IncognitoBadge />
      </div>
    </header>
  );
}
