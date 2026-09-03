/**
 * The action rail beside a short.
 *
 * YouTube puts like, comment, share and remix here. Two of those need an account and one needs a
 * creation pipeline, and rendering a control that cannot do its job is the failure mode the
 * specification calls out by name (§131) — so this rail carries the two actions that are real
 * without one:
 *
 * * **Save** writes a bookmark to the local database. It is the honest local equivalent of a like:
 *   it means "I want to find this again", it is stored on this device, and it is visible in the
 *   Bookmarks screen.
 * * **Share** copies the canonical link. Nothing is sent anywhere by the application; the clipboard
 *   is the user's, and where the link goes next is their decision.
 *
 * Both report their result on the button itself. A control that silently succeeded would leave the
 * user pressing it again to find out whether it worked.
 */

import { Bookmark, BookmarkCheck, Check, Share2 } from 'lucide-react';
import { useEffect, useState, type ReactNode } from 'react';

import { useAsyncResource } from '@/hooks/useAsyncResource';
import { useTranslation } from '@/i18n/context';
import { invoke } from '@/services/ipc';
import type { VideoSummary } from '@/types/domain';

/** How long a confirmation stays on the button before it returns to its resting label. */
const CONFIRMATION_MS = 1800;

/** The canonical short link for a video. */
function shareUrl(video: VideoSummary): string {
  return `https://youtu.be/${video.id}`;
}

function RailButton({
  label,
  onClick,
  active = false,
  horizontal = false,
  children,
}: {
  label: string;
  onClick: () => void;
  active?: boolean;
  horizontal?: boolean;
  children: ReactNode;
}): ReactNode {
  if (horizontal) {
    // A pill with the label beside the icon, matching the row under a video.
    return (
      <button
        type="button"
        onClick={onClick}
        aria-label={label}
        title={label}
        aria-pressed={active}
        className={[
          'transition-surface flex h-9 shrink-0 items-center gap-2 rounded-full px-4 text-sm font-medium',
          active
            ? 'bg-accent text-accent-contrast'
            : 'bg-surface-translucent hover:bg-surface-translucent-hover text-text',
        ].join(' ')}
      >
        {children}
        {label}
      </button>
    );
  }

  return (
    <div className="flex flex-col items-center gap-1">
      <button
        type="button"
        onClick={onClick}
        aria-label={label}
        title={label}
        aria-pressed={active}
        className={[
          'transition-surface grid size-12 place-items-center rounded-full',
          active
            ? 'bg-accent text-accent-contrast'
            : 'bg-surface-translucent hover:bg-surface-translucent-hover text-text',
        ].join(' ')}
      >
        {children}
      </button>
      <span className="text-text-muted max-w-16 truncate text-center text-2xs">{label}</span>
    </div>
  );
}

export interface VideoActionsProps {
  video: VideoSummary;
  /**
   * How the buttons are stacked.
   *
   * Vertical is the Shorts rail; horizontal is the row under a video on the watch page, where the
   * label sits beside the icon instead of under it.
   */
  orientation?: 'vertical' | 'horizontal';
}

/** Save and share, for the video currently on screen. */
export function VideoActions({ video, orientation = 'vertical' }: VideoActionsProps): ReactNode {
  const t = useTranslation();
  const [copied, setCopied] = useState(false);

  // The stored answer, so the control shows the library's real state rather than assuming "not
  // saved" and telling the user something untrue about their own library.
  const stored = useAsyncResource(`bookmarked:${video.id}`, (signal) =>
    invoke('is_bookmarked', { videoId: video.id }, { signal }),
  );

  // `null` means "no local decision yet", which is what lets the stored answer show through until
  // the user presses the button. Deriving it rather than syncing in an effect keeps the two from
  // fighting over one render.
  const [pending, setPending] = useState<boolean | null>(null);
  const saved = pending ?? stored.data ?? false;

  useEffect(() => {
    if (!copied) return undefined;
    const timer = setTimeout(() => {
      setCopied(false);
    }, CONFIRMATION_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [copied]);

  const toggleSave = () => {
    // Flipped optimistically: the write is local and effectively cannot fail, so waiting for a
    // round trip would only add latency to a button whose whole job is to feel instant.
    const next = !saved;
    setPending(next);
    const call = next
      ? invoke('set_bookmark', { video })
      : invoke('remove_bookmark', { videoId: video.id });
    void call.catch(() => {
      setPending(!next);
    });
  };

  const share = () => {
    void navigator.clipboard.writeText(shareUrl(video)).then(
      () => {
        setCopied(true);
      },
      () => {
        // Clipboard access can be refused. Saying nothing would look like the button did nothing.
        setCopied(false);
      },
    );
  };

  const horizontal = orientation === 'horizontal';
  const size = horizontal ? 18 : 22;

  return (
    <div className={horizontal ? 'flex shrink-0 items-center gap-2' : 'flex flex-col gap-4'}>
      <RailButton
        label={saved ? t.t('video.removeBookmark') : t.t('video.bookmark')}
        onClick={toggleSave}
        active={saved}
        horizontal={horizontal}
      >
        {saved ? <BookmarkCheck size={size} /> : <Bookmark size={size} />}
      </RailButton>

      <RailButton
        label={copied ? t.t('app.copied') : t.t('video.copyLink')}
        onClick={share}
        horizontal={horizontal}
      >
        {copied ? <Check size={size} /> : <Share2 size={size} />}
      </RailButton>
    </div>
  );
}
