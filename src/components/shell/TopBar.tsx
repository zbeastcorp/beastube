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
import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
  type SyntheticEvent,
} from 'react';

import { useNavigate, useRoute } from '@/app/router';
import { SearchSuggestions } from '@/components/shell/SearchSuggestions';
import { useTranslation } from '@/i18n/context';
import { clearFeedCache } from '@/services/feedCache';
import { clearVideoCache } from '@/services/videoCache';
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
      {/*
        BEASTUBE's own mark, and it has to be its own.

        What was here was YouTube's play-button logo — the 28x20 rounded screen with the inset white
        triangle, filled `--color-brand`, which is #ff0000. Not a generic play glyph: the specific
        registered mark, used unaltered as this application's identity beside its own name, in the
        top-left of every screen. That is a trademark claim, it is the opposite of the affiliation
        disclaimer this project prints in its README, and on a public repository it survives in every
        fork after any correction.

        This is the same artwork as the application and installer icon (`src-tauri/icons/source.svg`)
        — a real mark that already existed — scaled from its 1024 grid onto a 28 one. The gradient id
        is namespaced because this is inlined into a document that may hold other gradients.
      */}
      <svg viewBox="0 0 28 28" width="24" height="24" aria-hidden="true" focusable="false">
        <defs>
          <linearGradient id="beastube-mark-beam" x1="0" y1="0" x2="1" y2="1">
            <stop offset="0" stopColor="#ff5c5c" />
            <stop offset="1" stopColor="#ff9a3c" />
          </linearGradient>
          <linearGradient id="beastube-mark-ground" x1="0" y1="0" x2="1" y2="1">
            <stop offset="0" stopColor="#1b1030" />
            <stop offset="1" stopColor="#0b0716" />
          </linearGradient>
        </defs>
        <rect
          x="1.75"
          y="1.75"
          width="24.5"
          height="24.5"
          rx="6.125"
          fill="url(#beastube-mark-ground)"
          stroke="#3b2a5c"
          strokeWidth="0.9"
        />
        <path d="M10.72 8.2 10.72 19.8 20.34 14Z" fill="url(#beastube-mark-beam)" />
      </svg>
      {/* The logo carries the identity on its own below 640px, where those 90px are the difference
          between a search field that shows its placeholder and one that cuts it off mid-word. */}
      <span className="text-md font-semibold tracking-tight max-sm:hidden">BEASTUBE</span>
    </button>
  );
}

/** The pill search field, its suggestion dropdown, and the attached submit button. */
function SearchField(): ReactNode {
  const t = useTranslation();
  const navigate = useNavigate();
  const route = useRoute();
  const inputRef = useRef<HTMLInputElement>(null);
  const routeQuery = route.name === 'search' ? route.query : '';
  const [value, setValue] = useState(routeQuery);

  // Combobox state. `highlighted` is -1 when nothing is chosen, which is the state Enter treats as
  // "search for what I typed" rather than "accept a suggestion".
  const [open, setOpen] = useState(false);
  const [highlighted, setHighlighted] = useState(-1);
  const [options, setOptions] = useState<readonly string[]>([]);
  const listboxId = useId();
  const optionId = useCallback((index: number) => `${listboxId}-option-${index}`, [listboxId]);

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

  const runSearch = useCallback(
    (raw: string) => {
      const query = raw.trim();
      if (query.length === 0) return;
      setValue(query);
      setOpen(false);
      setHighlighted(-1);
      inputRef.current?.blur();
      navigate({ name: 'search', query });
    },
    [navigate],
  );

  const submit = (event: SyntheticEvent) => {
    event.preventDefault();
    runSearch(value);
  };

  /**
   * Keyboard handling for the combobox.
   *
   * Enter is deliberately not handled here when nothing is highlighted: letting the form's own
   * submit fire keeps the field working exactly as a plain search box when the dropdown is closed
   * or empty.
   */
  const onKeyDown = (event: ReactKeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Escape') {
      // Closes the list, keeps the text. Doing both on one key is the behaviour people complain
      // about in every search box that does it.
      if (open) {
        event.preventDefault();
        setOpen(false);
        setHighlighted(-1);
      }
      return;
    }

    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      if (!open) {
        setOpen(true);
        return;
      }
      if (options.length === 0) return;
      event.preventDefault();
      const delta = event.key === 'ArrowDown' ? 1 : -1;
      // Wraps through -1, so arrowing past either end returns to the typed text rather than
      // sticking at the first or last option.
      setHighlighted((current) => {
        const next = current + delta;
        if (next >= options.length) return -1;
        if (next < -1) return options.length - 1;
        return next;
      });
      return;
    }

    if (event.key === 'Enter' && highlighted >= 0) {
      event.preventDefault();
      runSearch(options[highlighted] ?? value);
    }
  };

  const listOpen = open && options.length > 0;

  return (
    <form
      onSubmit={submit}
      role="search"
      className="no-drag relative flex max-w-[640px] flex-1 items-center"
    >
      <div className="border-border bg-bg focus-within:border-border-focus flex h-10 flex-1 items-center rounded-l-full border py-0 pr-1 pl-4 transition-colors">
        <input
          ref={inputRef}
          type="search"
          value={value}
          onChange={(event) => {
            setValue(event.target.value);
            // Typing reopens the list and abandons any highlight: the options are about to change,
            // and keeping an index into the old ones would accept the wrong suggestion.
            setOpen(true);
            setHighlighted(-1);
          }}
          onFocus={() => {
            setOpen(true);
          }}
          onBlur={() => {
            setOpen(false);
            setHighlighted(-1);
          }}
          onKeyDown={onKeyDown}
          placeholder={t.t('search.placeholder')}
          aria-label={t.t('app.search')}
          role="combobox"
          aria-expanded={listOpen}
          aria-controls={listboxId}
          aria-autocomplete="list"
          aria-activedescendant={listOpen && highlighted >= 0 ? optionId(highlighted) : undefined}
          // Both the browser's own autofill list and any password-manager overlay would cover the
          // suggestion dropdown with a second, unrelated list.
          autoComplete="off"
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

      <SearchSuggestions
        query={value}
        open={open}
        highlighted={highlighted}
        onItemsChange={setOptions}
        onHighlight={setHighlighted}
        onAccept={runSearch}
        listboxId={listboxId}
        optionId={optionId}
      />
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
/**
 * Re-fetches whatever is on screen, without leaving it.
 *
 * Deliberately plain text with no surface of its own: it sits beside the wordmark, and a filled
 * button there would compete with the search field for the eye. It is not a browser reload — the
 * route, the scroll position and the running player all survive, and only the requests the current
 * screen actually made are issued again.
 *
 * The in-memory caches are dropped first. Without that the refetch would be served from the same
 * data it is meant to replace, and the control would appear to do nothing.
 */
function RefreshButton(): ReactNode {
  const t = useTranslation();
  const refreshContent = useUiStore((state) => state.refreshContent);

  return (
    <button
      type="button"
      onClick={() => {
        clearFeedCache();
        clearVideoCache();
        refreshContent();
      }}
      className="no-drag text-text-muted hover:text-text focus-visible:text-text shrink-0 rounded-sm px-1 text-sm font-medium transition-colors"
    >
      {t.t('app.refresh')}
    </button>
  );
}

export function TopBar(): ReactNode {
  const t = useTranslation();
  const route = useRoute();
  const toggleSidebar = useUiStore((state) => state.toggleSidebar);
  const collapsed = useUiStore((state) => state.sidebarCollapsed);
  const narrow = useUiStore((state) => state.shellNarrow);
  const drawerOpen = useUiStore((state) => state.drawerOpen);

  // What this button does depends on the width, so what it announces has to as well. Narrow, it
  // opens the sidebar over the content and `drawerOpen` is the state that is expanded or not;
  // wide, it switches the sidebar between its rail and its column. Reporting `collapsed` in both
  // cases told a screen reader the sidebar was expanded while the drawer it had just opened was
  // shut, and the reverse.
  const sidebarShown = narrow ? drawerOpen : !collapsed;

  return (
    <header className="drag-region bg-bg flex h-[var(--layout-topbar-height)] shrink-0 items-center gap-4 px-4">
      <div className="flex shrink-0 items-center gap-1">
        <button
          type="button"
          onClick={toggleSidebar}
          aria-label={t.t(sidebarShown ? 'nav.collapseSidebar' : 'nav.expandSidebar')}
          aria-expanded={sidebarShown}
          className="transition-surface hover:bg-surface-hover no-drag grid size-10 place-items-center rounded-full"
        >
          <Menu size={24} strokeWidth={1.8} />
        </button>
        <Wordmark />
        <RefreshButton />
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
