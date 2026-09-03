/**
 * The error surface.
 *
 * Renders an {@link ErrorPayload} the way the payload itself asks to be rendered: the message comes
 * from `message_key` through the localization layer, and whether a retry is offered comes from
 * `recovery` rather than from the component guessing.
 *
 * That indirection is the point. A component that decided for itself when to show a retry button
 * would drift from the native side's actual retry policy, and users would be offered a retry for
 * something that can never succeed — a geo-blocked video, a deleted one — which is worse than no
 * button at all (§74, §75).
 */

import { RotateCw, TriangleAlert, WifiOff } from 'lucide-react';
import { useState, type ReactNode } from 'react';

import { useTranslation } from '@/i18n/context';
import type { TranslationKey } from '@/i18n';
import { offersRetry, type ErrorPayload } from '@/types/domain';

interface ErrorStateProps {
  error: ErrorPayload;
  /** Called when the user asks to retry. Omit when retrying is not possible. */
  onRetry?: () => void;
  /** Renders inline within a section rather than as a full-height surface. */
  compact?: boolean;
}

/** A failure explanation, with a retry only where one is meaningful. */
export function ErrorState({ error, onRetry, compact = false }: ErrorStateProps): ReactNode {
  const t = useTranslation();
  const [showDetail, setShowDetail] = useState(false);

  // A message key is always present, but a build can fall behind the native side; falling back to
  // the generic message beats rendering a raw key at the user.
  const message = t.t(error.message_key as TranslationKey, error.params);
  const resolved = message === error.message_key ? t.t('error.generic') : message;

  const isOffline = error.kind === 'network' && error.code.includes('offline');
  const Icon = isOffline ? WifiOff : TriangleAlert;
  const canRetry = onRetry !== undefined && offersRetry(error);

  return (
    <div
      role="alert"
      className={[
        'flex flex-col items-center justify-center gap-4 text-center',
        compact ? 'px-4 py-10' : 'px-6 py-24',
      ].join(' ')}
    >
      <div className="bg-surface text-text-muted grid size-16 place-items-center rounded-full">
        <Icon size={28} strokeWidth={1.6} />
      </div>

      <div className="flex max-w-md flex-col gap-2">
        <p className="text-text text-base font-medium">{resolved}</p>
        {error.kind === 'network' && (
          <p className="text-text-muted text-sm">{t.t('error.network.offlineHint')}</p>
        )}
      </div>

      <div className="flex items-center gap-2">
        {canRetry && (
          <button
            type="button"
            onClick={onRetry}
            className="transition-surface bg-primary text-primary-contrast hover:bg-primary-hover flex items-center gap-2 rounded-full px-4 py-2 text-sm font-medium"
          >
            <RotateCw size={16} />
            {t.t('app.retry')}
          </button>
        )}

        {error.diagnostic !== undefined && (
          <button
            type="button"
            onClick={() => {
              setShowDetail((shown) => !shown);
            }}
            aria-expanded={showDetail}
            className="transition-surface text-text-muted hover:bg-surface-hover hover:text-text rounded-full px-4 py-2 text-sm"
          >
            {t.t('app.more')}
          </button>
        )}
      </div>

      {showDetail && error.diagnostic !== undefined && (
        // Engineer-facing, never localized, and never transmitted anywhere.
        <code className="text-text-subtle selectable bg-surface max-w-xl rounded-md p-3 text-left font-mono text-xs break-all">
          {error.code}
          {'\n'}
          {error.diagnostic}
        </code>
      )}
    </div>
  );
}
