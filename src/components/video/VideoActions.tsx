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

import {
  Bookmark,
  BookmarkCheck,
  Check,
  Download,
  ExternalLink,
  FolderOpen,
  ListPlus,
  Share2,
  X,
} from 'lucide-react';
import { useEffect, useState, type ReactNode } from 'react';

import { useAsyncResource } from '@/hooks/useAsyncResource';
import { useTranslation } from '@/i18n/context';
import { invoke } from '@/services/ipc';
import { canDownload, useDownloadsStore } from '@/stores/downloads';
import { useUiStore } from '@/stores/ui';
import type { DownloadProgress, VideoSummary } from '@/types/domain';
import { isTerminalDownload } from '@/types/domain';

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
  showLabel = false,
  children,
}: {
  label: string;
  onClick: () => void;
  active?: boolean;
  horizontal?: boolean;
  /**
   * Whether the label is printed beside the icon in the horizontal row.
   *
   * Off for the actions whose icon says it on its own — save, add to a playlist, open, copy. The
   * label is still the accessible name and the tooltip, so nothing is lost to a screen reader or
   * to a pointer that hovers; what goes is four words of chrome under every video.
   *
   * On for the download, because its label is not a name but a *state* — "Downloading 42%" — and
   * an icon cannot carry that.
   */
  showLabel?: boolean;
  children: ReactNode;
}): ReactNode {
  if (horizontal) {
    const surface = active
      ? 'bg-accent text-accent-contrast'
      : 'bg-surface-translucent hover:bg-surface-translucent-hover text-text';

    // A pill when it carries a label, a round icon button when it does not.
    return (
      <button
        type="button"
        onClick={onClick}
        aria-label={label}
        title={label}
        aria-pressed={active}
        className={[
          'transition-surface shrink-0 rounded-full',
          showLabel
            ? 'flex h-8 items-center gap-1.5 px-3 text-xs font-medium'
            : 'grid size-8 place-items-center',
          surface,
        ].join(' ')}
      >
        {children}
        {showLabel && label}
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

/**
 * The download control, in whichever of its four states applies.
 *
 * It is absent entirely when no downloader is installed. That is the §131 rule applied literally:
 * BEASTUBE drives `yt-dlp` and cannot produce a file without it, so on a computer that has none
 * the button would be a control that cannot do its job. The settings screen is where its absence
 * is explained and fixed, and picking a downloader there makes this appear without a restart.
 *
 * While a download runs the same button cancels it, which is where a person reaches when they want
 * it to stop — and the label carries the percentage so the state is readable without a separate
 * progress row. A percentage appears only when the size is known; an unknown total shows the
 * running state with no number rather than a fabricated one.
 */
function DownloadButton({
  video,
  horizontal,
  size,
}: {
  video: VideoSummary;
  horizontal: boolean;
  size: number;
}): ReactNode {
  const t = useTranslation();
  const available = useDownloadsStore(canDownload);
  const download = useDownloadsStore((state) => state.byVideo[video.id]);
  const start = useDownloadsStore((state) => state.start);
  const cancel = useDownloadsStore((state) => state.cancel);
  const reveal = useDownloadsStore((state) => state.reveal);

  if (!available) return null;

  const running = download !== undefined && !isTerminalDownload(download.status);

  if (running) {
    return (
      <RailButton
        label={runningLabel(t, download)}
        onClick={() => {
          void cancel(video.id);
        }}
        active
        horizontal={horizontal}
        showLabel={horizontal}
      >
        <X size={size} />
      </RailButton>
    );
  }

  if (download?.status === 'finished') {
    return (
      <RailButton
        label={t.t('download.showInFolder')}
        onClick={() => {
          void reveal(video.id);
        }}
        horizontal={horizontal}
        showLabel={horizontal}
      >
        <FolderOpen size={size} />
      </RailButton>
    );
  }

  // Failed and cancelled both come back to "download", because pressing it again is exactly what
  // either state calls for. The reason it failed was already said, once, in a toast.
  return (
    <RailButton
      label={t.t('download.start')}
      onClick={() => {
        void start(video);
      }}
      horizontal={horizontal}
      showLabel={horizontal}
    >
      <Download size={size} />
    </RailButton>
  );
}

/** The label for a download in flight: its stage, plus a percentage when the size is known. */
function runningLabel(t: ReturnType<typeof useTranslation>, download: DownloadProgress): string {
  if (download.status === 'merging') return t.t('download.merging');
  if (download.status === 'queued') return t.t('download.queued');
  if (download.fraction === undefined) return t.t('download.inProgress');
  return t.t('download.percent', { percent: Math.round(download.fraction * 100) });
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
  const openOverlay = useUiStore((state) => state.openOverlay);
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
  const size = horizontal ? 16 : 22;

  return (
    <div
      className={
        horizontal
          ? // Wraps rather than scrolls. A scrollbar under four buttons hides the last one behind
            // a gesture nobody expects on a desktop panel; a second line shows all of them. The
            // pills are short enough that one can always fit, so nothing is ever clipped.
            'ml-auto flex min-w-0 max-w-full flex-wrap items-center justify-end gap-2'
          : 'flex flex-col gap-4'
      }
    >
      <RailButton
        label={saved ? t.t('video.removeBookmark') : t.t('video.bookmark')}
        onClick={toggleSave}
        active={saved}
        horizontal={horizontal}
      >
        {saved ? <BookmarkCheck size={size} /> : <Bookmark size={size} />}
      </RailButton>

      {/* Opens the dialog rather than acting: which playlist is the question, and a button that
          picked one for you would be answering it on your behalf. */}
      <RailButton
        label={t.t('library.addToPlaylist')}
        onClick={() => {
          openOverlay({ kind: 'addToPlaylist', video });
        }}
        horizontal={horizontal}
      >
        <ListPlus size={size} />
      </RailButton>

      {/* Saves the video as a file, by driving `yt-dlp` (ADR-0003). The cryptography this needs —
          the signature cipher, the `n`-parameter transform, the player challenges — is not
          implemented here and never will be; the tool that already solves it is run as a separate
          program the user installs. That keeps ADR-0001's invariant intact: no such machinery, and
          no JavaScript engine, enters this application's dependency graph. Absent when no
          downloader is installed. */}
      <DownloadButton video={video} horizontal={horizontal} size={size} />

      {/* Hands the video to the browser rather than pretending to be one. Kept alongside the
          download: opening YouTube proper is what you want when the answer is their player, not a
          file — their own offline download included. */}
      <RailButton
        label={t.t('video.openExternally')}
        onClick={() => {
          void invoke('open_external', { url: shareUrl(video) }).catch(() => {
            // The link simply does not open; nothing here is recoverable in the UI.
          });
        }}
        horizontal={horizontal}
      >
        <ExternalLink size={size} />
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
