/**
 * The search suggestion dropdown.
 *
 * Two sources, one list. The user's own previous queries come from the local database and are
 * marked with a clock icon and a remove control; the rest are completions from the provider. The
 * native side merges and de-duplicates them (`get_suggestions`), so the ordering rule — your own
 * queries first — lives in one place rather than being re-derived here.
 *
 * ## The keyboard contract
 *
 * This is a combobox, and the accessible pattern is not optional decoration: the input keeps focus
 * at all times and `aria-activedescendant` moves the *virtual* selection, so a screen reader
 * announces each option while typing still works. Arrow keys move, Enter accepts the highlighted
 * option (or submits what was typed when nothing is highlighted), and Escape closes the list
 * without clearing the field — closing and clearing on one key is the behaviour people complain
 * about in every search box that does it.
 *
 * ## Why the highlight does not rewrite the input
 *
 * Arrowing through options leaves the typed text alone. Rewriting the field on every arrow press
 * makes it impossible to see what you typed, and means backing out of the list loses your query.
 * The highlighted option is submitted on Enter regardless.
 */

import { Clock, Search } from 'lucide-react';
import { useEffect, useEffectEvent, useState, type ReactNode } from 'react';

import { useAsyncResource, useDebounced } from '@/hooks/useAsyncResource';
import { useTranslation } from '@/i18n/context';
import { invoke } from '@/services/ipc';
import type { Suggestion } from '@/types/domain';

/** How long the field must be still before a lookup is issued. */
const DEBOUNCE_MS = 140;

/** How many past queries the empty field offers. */
const RECENT_LIMIT = 8;

/** Shortest prefix worth asking the provider about. */
const MIN_PREFIX_LENGTH = 1;

/** A shared empty list, so the "nothing removed" case is referentially stable. */
const EMPTY: readonly string[] = [];

export interface SearchSuggestionsProps {
  /** The current field text. */
  query: string;
  /** Whether the list should be shown at all. */
  open: boolean;
  /** Index of the highlighted option, or `-1` for none. */
  highlighted: number;
  /**
   * Reports the option texts, in order.
   *
   * The field needs the text, not just a count: Enter on a highlighted option searches for that
   * option, and reading it back out of the DOM would be both fragile and wrong the moment an option
   * grows a second line.
   */
  onItemsChange: (items: readonly string[]) => void;
  /** Moves the highlight, used by pointer hover. */
  onHighlight: (index: number) => void;
  /** Accepts an option. */
  onAccept: (text: string) => void;
  /** Id of the listbox, referenced by the input's `aria-controls`. */
  listboxId: string;
  /** Builds the id of one option, so `aria-activedescendant` can name it. */
  optionId: (index: number) => string;
}

/**
 * Fetches suggestions for `query`.
 *
 * An empty field asks for recent searches instead of completions — that is the list YouTube shows
 * on focus, and it is the one list here that never leaves the device.
 */
function useSuggestions(query: string, open: boolean): Suggestion[] {
  const trimmed = query.trim();
  const debounced = useDebounced(trimmed, DEBOUNCE_MS);

  // The key encodes everything the request depends on, including whether the list is open — closing
  // it abandons an in-flight lookup rather than letting it land on a hidden list.
  const key = open ? `suggest:${debounced}` : null;

  const resource = useAsyncResource(key, async (signal) => {
    if (debounced.length < MIN_PREFIX_LENGTH) {
      return invoke('get_recent_searches', { limit: RECENT_LIMIT }, { signal });
    }
    return invoke('get_suggestions', { prefix: debounced }, { signal });
  });

  return resource.data ?? [];
}

/**
 * Renders a suggestion with the part you have not typed in bold.
 *
 * YouTube's own weighting, and it earns its place: the bold run is exactly the new information, so
 * a list of near-identical completions can be scanned by the differences rather than re-read in
 * full. Falls back to plain text when the suggestion does not start with what was typed — which
 * happens for spelling corrections and for entries recalled from history.
 */
function Completion({ text, typed }: { text: string; typed: string }): ReactNode {
  const prefix = typed.trim();
  const matches = prefix.length > 0 && text.toLowerCase().startsWith(prefix.toLowerCase());
  if (!matches) return <span className="font-medium">{text}</span>;

  // Sliced from the suggestion rather than rendering what was typed: the stored casing is the one
  // to show, so typing "nasa" against a remembered "NASA launch" does not render two casings.
  return (
    <>
      {text.slice(0, prefix.length)}
      <span className="font-medium">{text.slice(prefix.length)}</span>
    </>
  );
}

/** The dropdown. Renders nothing when there is nothing to offer. */
export function SearchSuggestions({
  query,
  open,
  highlighted,
  onItemsChange,
  onHighlight,
  onAccept,
  listboxId,
  optionId,
}: SearchSuggestionsProps): ReactNode {
  const t = useTranslation();
  const suggestions = useSuggestions(query, open);
  // Removals are scoped to the query they were made against, rather than reset by an effect when
  // the query changes. Deriving it keeps a removal from silently hiding an unrelated later
  // suggestion that happens to have the same text, in one render rather than two.
  const [removed, setRemoved] = useState<{ query: string; texts: readonly string[] }>({
    query,
    texts: [],
  });
  const hidden = removed.query === query ? removed.texts : EMPTY;

  const visible = suggestions.filter((suggestion) => !hidden.includes(suggestion.text));
  const items = visible.map((suggestion) => suggestion.text);
  // Serialized for the dependency comparison only. The array's identity changes every render while
  // its contents usually do not, and depending on the identity would report on every render for as
  // long as the list is open.
  const itemsKey = JSON.stringify(items);

  // Reported through an effect rather than during render: the parent stores it, and writing another
  // component's state while rendering is the one thing React will not tolerate. The callback reads
  // `items` out of the latest render rather than taking it as an argument, which is what keeps the
  // effect's dependencies down to "did the contents change".
  const report = useEffectEvent(() => {
    onItemsChange(open ? items : EMPTY);
  });
  useEffect(() => {
    report();
  }, [open, itemsKey]);

  if (!open || visible.length === 0) return null;

  const forget = (text: string) => {
    // Hidden immediately and deleted in the background: the row is the user's own and the delete
    // cannot meaningfully fail, so making them wait for a round trip would only add latency.
    setRemoved({ query, texts: [...hidden, text] });
    void invoke('delete_search', { query: text });
  };

  return (
    <div
      className="border-border bg-surface absolute top-full right-0 left-0 z-50 mt-1 overflow-hidden rounded-xl border py-2 shadow-lg"
      // The list is scrolled into view by the browser; capping the height keeps a long list from
      // covering the whole window on a short display.
      style={{ maxHeight: 'min(60vh, 30rem)', overflowY: 'auto' }}
    >
      <ul id={listboxId} role="listbox" aria-label={t.t('search.suggestions')}>
        {visible.map((suggestion, index) => {
          const selected = index === highlighted;
          return (
            <li
              key={suggestion.text}
              id={optionId(index)}
              role="option"
              aria-selected={selected}
              onMouseEnter={() => {
                onHighlight(index);
              }}
              className={[
                'flex cursor-default items-center gap-4 px-4 py-1.5',
                selected ? 'bg-surface-hover' : '',
              ].join(' ')}
            >
              {/* `onMouseDown` rather than `onClick`: the input's blur handler closes the list, and
                  blur fires before click would. */}
              <button
                type="button"
                tabIndex={-1}
                onMouseDown={(event) => {
                  event.preventDefault();
                  onAccept(suggestion.text);
                }}
                className="flex min-w-0 flex-1 items-center gap-4 text-left"
              >
                {suggestion.from_history === true ? (
                  <Clock size={18} className="text-text-muted shrink-0" />
                ) : (
                  <Search size={18} className="text-text-muted shrink-0" />
                )}
                <span className="text-text truncate text-md">
                  <Completion text={suggestion.text} typed={query} />
                </span>
              </button>

              {suggestion.from_history === true && (
                <button
                  type="button"
                  tabIndex={-1}
                  onMouseDown={(event) => {
                    event.preventDefault();
                    forget(suggestion.text);
                  }}
                  aria-label={t.t('search.removeSuggestion')}
                  className="text-text-muted hover:text-text shrink-0 text-xs"
                >
                  {t.t('app.remove')}
                </button>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
