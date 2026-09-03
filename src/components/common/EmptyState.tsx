/**
 * The empty state.
 *
 * Every list surface has one (§61). An empty screen with no explanation reads as a bug, so each
 * state says what is missing *and* what to do about it, and history additionally says where the
 * data lives — a user who opens History for the first time should learn that it is local, not
 * wonder what was uploaded.
 */

import { Bookmark, History, ListVideo, Search, TriangleAlert, type LucideIcon } from 'lucide-react';
import type { ReactNode } from 'react';

import type { TranslationKey, TranslationParams } from '@/i18n';
import { useTranslation } from '@/i18n/context';

/** Which glyph to show. Named by meaning rather than by icon so the mapping can change freely. */
export type EmptyStateIcon = 'history' | 'bookmark' | 'library' | 'search' | 'error';

const ICONS: Record<EmptyStateIcon, LucideIcon> = {
  history: History,
  bookmark: Bookmark,
  library: ListVideo,
  search: Search,
  error: TriangleAlert,
};

interface EmptyStateProps {
  titleKey: TranslationKey;
  bodyKey?: TranslationKey;
  /** Interpolation values for the title and body, e.g. the search query. */
  params?: TranslationParams;
  icon?: EmptyStateIcon;
  /**
   * Engineer-facing detail, shown in a muted monospace line.
   *
   * Never localized: it is a path, an identifier or a code, and translating it would make it
   * useless for diagnosis.
   */
  detail?: string;
  /** An optional call to action. */
  action?: { labelKey: TranslationKey; onClick: () => void };
}

/** A centred explanation for a surface with nothing to show. */
export function EmptyState({
  titleKey,
  bodyKey,
  params,
  icon = 'search',
  detail,
  action,
}: EmptyStateProps): ReactNode {
  const t = useTranslation();
  const Icon = ICONS[icon];

  return (
    <div className="flex flex-col items-center justify-center gap-4 px-6 py-24 text-center">
      <div className="bg-surface text-text-muted grid size-24 place-items-center rounded-full">
        <Icon size={40} strokeWidth={1.5} />
      </div>

      <div className="flex max-w-md flex-col gap-2">
        <h2 className="text-text text-lg font-medium">{t.t(titleKey, params)}</h2>
        {bodyKey && <p className="text-text-muted text-base">{t.t(bodyKey, params)}</p>}
        {detail !== undefined && (
          <code className="text-text-subtle selectable font-mono text-xs break-all">{detail}</code>
        )}
      </div>

      {action && (
        <button
          type="button"
          onClick={action.onClick}
          className="transition-surface bg-primary text-primary-contrast hover:bg-primary-hover rounded-full px-4 py-2 text-sm font-medium"
        >
          {t.t(action.labelKey)}
        </button>
      )}
    </div>
  );
}
