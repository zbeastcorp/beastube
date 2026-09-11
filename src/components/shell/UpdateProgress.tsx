/**
 * The card an automatic update shows while it works.
 *
 * The application is about to close and reopen on its own. A restart nobody was told about reads as
 * a crash, and a one-line toast saying "updating" leaves the viewer watching a frozen-looking
 * application with no idea how long it will be — which is the same complaint in a quieter voice.
 * So this shows the version, how far the download has got, and what will happen at the end.
 *
 * No overlay, no dialog, no focus trap. An update is maintenance, not an event the viewer has to
 * attend to: the card sits in a corner and everything carries on behind it. It can be dismissed,
 * which hides the card and not the update — there is no cancel here, because the download is
 * already paid for by the time it is visible and a half-applied installer is worse than a finished
 * one.
 *
 * `percent` is `null` when the server reports no content length, and the bar then animates as an
 * indeterminate sweep rather than inventing a position. A progress bar that guesses is worse
 * than one that admits it cannot measure.
 */

import { Download, RotateCw, X } from 'lucide-react';
import { type ReactNode } from 'react';

import { useTranslation } from '@/i18n/context';
import { useUpdateStore } from '@/stores/updates';

export function UpdateProgress(): ReactNode {
  const t = useTranslation();
  const stage = useUpdateStore((state) => state.stage);
  const dismissed = useUpdateStore((state) => state.dismissed);
  const dismiss = useUpdateStore((state) => state.dismiss);

  if (stage.kind === 'idle' || dismissed) return null;

  const percent = stage.kind === 'downloading' ? stage.percent : null;
  const determinate = percent !== null;
  // Installing and restarting are both "nearly there": the bar sits full rather than resetting,
  // because a bar that goes back to zero at the last step reads as the work being redone.
  const complete = stage.kind === 'installing' || stage.kind === 'restarting';

  const message = (): string => {
    switch (stage.kind) {
      case 'downloading':
        return percent === null
          ? t.t('update.downloading', { version: stage.version })
          : t.t('update.downloadingPercent', {
              version: stage.version,
              percent: String(percent),
            });
      case 'installing':
        return t.t('update.installing', { version: stage.version });
      case 'restarting':
        return t.t('update.restarting');
      case 'failed':
        return t.t('update.failed', { version: stage.version });
      default:
        return '';
    }
  };

  const failed = stage.kind === 'failed';

  return (
    <div
      // Polite, not assertive: this is progress, and a screen reader should finish its sentence
      // before hearing about it.
      role="status"
      aria-live="polite"
      className="pointer-events-none fixed right-4 bottom-4 z-50 flex justify-end"
    >
      <div className="bg-surface-raised border-border pointer-events-auto w-80 max-w-[calc(100vw-2rem)] rounded-xl border p-3 shadow-lg">
        <div className="flex items-start gap-3">
          <div className={`mt-0.5 shrink-0 ${failed ? 'text-danger' : 'text-accent'}`}>
            {stage.kind === 'restarting' ? (
              <RotateCw size={18} aria-hidden="true" className="motion-safe:animate-spin" />
            ) : (
              <Download size={18} aria-hidden="true" />
            )}
          </div>

          <div className="min-w-0 flex-1">
            <p className="text-text text-sm font-medium">{message()}</p>
            <p className="text-text-muted mt-0.5 text-xs">
              {failed ? t.t('update.failedHint') : t.t('update.willRestart')}
            </p>

            {!failed && (
              <div
                className="bg-surface-active mt-2 h-1 overflow-hidden rounded-full"
                role="progressbar"
                aria-valuemin={0}
                aria-valuemax={100}
                {...(determinate ? { 'aria-valuenow': percent } : {})}
                aria-label={t.t('update.progressLabel')}
              >
                <div
                  className={`bg-accent h-full rounded-full ${
                    // Without a length there is no position to show, so the bar sweeps instead of
                    // sitting at a number nobody measured.
                    determinate || complete
                      ? 'transition-[width] duration-300 ease-out'
                      : 'w-1/3 motion-safe:animate-[update-sweep_1.4s_ease-in-out_infinite]'
                  }`}
                  style={
                    determinate || complete
                      ? { width: `${String(complete ? 100 : percent)}%` }
                      : undefined
                  }
                />
              </div>
            )}
          </div>

          <button
            type="button"
            onClick={dismiss}
            aria-label={t.t('update.dismiss')}
            className="text-text-muted hover:text-text hover:bg-surface-hover -mt-1 -mr-1 shrink-0 rounded p-1"
          >
            <X size={14} aria-hidden="true" />
          </button>
        </div>
      </div>
    </div>
  );
}
