/**
 * Renders the toast queue.
 *
 * The UI store has had a toast queue, an auto-dismiss duration and an optional action since the
 * beginning, and nothing ever rendered it — so `toast()` appended to an array no pixel reflected.
 * Three real notices were being raised into that void: the offline banner, the filter-rollback
 * warning, and now the result of every download. This is the missing half, and the same gap
 * `OverlayHost` was written to close.
 *
 * ## Dismissal is owned here, not by the caller
 *
 * A caller that scheduled its own timer would have to cancel it when the user dismisses early, and
 * would keep running after the component that raised it unmounted. One timer per toast, held by
 * the host, is the only version where "six seconds" means six seconds on screen.
 *
 * ## Keys, not sentences
 *
 * A toast carries an i18n key and its parameters, never rendered text, so a message raised in Rust
 * and one raised in the UI are translated by the same layer (§109). The key is typed as a plain
 * string because error payloads bring theirs across the IPC boundary; an unknown key renders as
 * itself rather than throwing, which is the localization layer's documented fallback.
 */

import { X } from 'lucide-react';
import { useEffect, type ReactNode } from 'react';

import type { TranslationKey } from '@/i18n';
import { useTranslation } from '@/i18n/context';
import { useUiStore, type Toast } from '@/stores/ui';

/** Tone-specific accent down the leading edge of the card. */
const TONE_ACCENT: Record<Toast['tone'], string> = {
  info: 'bg-accent',
  success: 'bg-success',
  warning: 'bg-warning',
  danger: 'bg-danger',
};

/** The live toast stack, bottom-left, newest at the bottom. */
export function ToastHost(): ReactNode {
  const toasts = useUiStore((state) => state.toasts);

  if (toasts.length === 0) return null;

  return (
    // `pointer-events-none` on the stack and `auto` on each card, so the empty space beside a
    // toast does not swallow clicks meant for the page under it.
    <div
      // A live region rather than an alert: these announce completed work, and `assertive` would
      // interrupt a screen reader mid-sentence to say a download finished.
      role="status"
      aria-live="polite"
      className="pointer-events-none fixed bottom-4 left-4 z-50 flex w-[min(24rem,calc(100vw-2rem))] flex-col gap-2"
    >
      {toasts.map((toast) => (
        <ToastCard key={toast.id} toast={toast} />
      ))}
    </div>
  );
}

function ToastCard({ toast }: { toast: Toast }): ReactNode {
  const t = useTranslation();
  const dismiss = useUiStore((state) => state.dismissToast);

  useEffect(() => {
    // `null` means the toast stays until something dismisses it — the offline notice, which is
    // removed when the connection returns rather than after a countdown.
    if (toast.durationMs === null) return undefined;
    const timer = setTimeout(() => {
      dismiss(toast.id);
    }, toast.durationMs);
    return () => {
      clearTimeout(timer);
    };
  }, [toast.id, toast.durationMs, dismiss]);

  // The catalogue's own fallback chain resolves an unknown key to the key itself, so a message key
  // that arrived from Rust and has no translation yet degrades to something legible rather than
  // throwing inside a notification.
  const translate = (key: string): string => t.t(key as TranslationKey, toast.params);

  return (
    <div
      className={[
        'animate-toast-in bg-surface-raised text-text pointer-events-auto relative flex',
        'items-start gap-3 overflow-hidden rounded-lg py-3 pr-2 pl-4 shadow-lg',
      ].join(' ')}
    >
      <span className={`absolute inset-y-0 left-0 w-1 ${TONE_ACCENT[toast.tone]}`} aria-hidden />

      <p className="min-w-0 flex-1 text-sm leading-snug break-words">
        {translate(toast.messageKey)}
      </p>

      {toast.action && (
        <button
          type="button"
          onClick={() => {
            // Run first, then dismiss: the action may read the toast's own data, and dismissing
            // first would drop the record it reads from.
            toast.action?.run();
            dismiss(toast.id);
          }}
          className="text-accent hover:bg-surface-hover shrink-0 rounded-full px-3 py-1 text-sm font-medium"
        >
          {translate(toast.action.labelKey)}
        </button>
      )}

      <button
        type="button"
        onClick={() => {
          dismiss(toast.id);
        }}
        aria-label={t.t('app.close')}
        title={t.t('app.close')}
        className="text-text-muted hover:bg-surface-hover hover:text-text grid size-7 shrink-0 place-items-center rounded-full"
      >
        <X size={16} />
      </button>
    </div>
  );
}
