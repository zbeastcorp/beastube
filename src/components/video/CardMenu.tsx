/**
 * The overflow menu on a card.
 *
 * YouTube puts a `⋮` beside every title, and what sits behind it is the difference between a grid
 * you browse and a grid you use: saving something without opening it, copying a link, starting a
 * download. Those actions already exist on the watch page here — this brings them to the card,
 * which is where the decision is actually made.
 *
 * ## Only what works
 *
 * Every entry is an action this application genuinely performs. Download appears only where a
 * downloader is installed, exactly as the watch-page control does (ADR-0003, §131). There is no
 * "Not interested" or "Don't recommend channel", because nothing here would act on either — a
 * menu item that quietly does nothing is worse than a shorter menu.
 *
 * ## Why it is not a Radix dropdown
 *
 * The card is a link and the grid is virtualized. A portal-based menu anchored to a row that can
 * unmount underneath it leaves an orphaned popup, and the library's focus management fights the
 * card's own hover state. This is a small absolutely-positioned panel that closes on blur, on
 * Escape, and on any scroll — which is the entire behaviour needed.
 */

import {
  Bookmark,
  BookmarkCheck,
  Check,
  Download,
  Link2,
  ListPlus,
  MoreVertical,
} from 'lucide-react';
import { useEffect, useId, useRef, useState, type ReactNode } from 'react';

import { useTranslation } from '@/i18n/context';
import { invoke } from '@/services/ipc';
import { canDownload, useDownloadsStore } from '@/stores/downloads';
import { useUiStore } from '@/stores/ui';
import { isTerminalDownload, type VideoSummary } from '@/types/domain';

/** How long a confirmation replaces an item's label before it returns. */
const CONFIRMATION_MS = 1600;

/** The canonical short link for a video. */
function shareUrl(video: VideoSummary): string {
  return `https://youtu.be/${video.id}`;
}

function MenuItem({
  icon,
  label,
  onSelect,
}: {
  icon: ReactNode;
  label: string;
  onSelect: () => void;
}): ReactNode {
  return (
    <button
      type="button"
      role="menuitem"
      // `onPointerDown` rather than `onClick`: the panel closes on blur, and a click that lands
      // after the blur has already unmounted it never fires.
      onPointerDown={(event) => {
        event.preventDefault();
        event.stopPropagation();
        onSelect();
      }}
      className="hover:bg-surface-hover text-text flex w-full items-center gap-3 px-4 py-2 text-left text-sm"
    >
      <span className="text-text-muted shrink-0">{icon}</span>
      <span className="truncate">{label}</span>
    </button>
  );
}

/** The `⋮` button and its panel. */
export function CardMenu({ video }: { video: VideoSummary }): ReactNode {
  const t = useTranslation();
  const [open, setOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const [saved, setSaved] = useState<boolean | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const menuId = useId();

  const openOverlay = useUiStore((state) => state.openOverlay);
  const downloadable = useDownloadsStore(canDownload);
  const download = useDownloadsStore((state) => state.byVideo[video.id]);
  const startDownload = useDownloadsStore((state) => state.start);

  // Asked only once the menu is opened. Reading it for every card in a grid would be one IPC call
  // per tile for a state almost none of them ever show.
  useEffect(() => {
    if (!open || saved !== null) return;
    void invoke('is_bookmarked', { videoId: video.id })
      .then(setSaved)
      .catch(() => {
        // Unknown stays unknown; the item offers to save, which is the harmless direction.
        setSaved(false);
      });
  }, [open, saved, video.id]);

  // Closing on scroll as well as blur: the panel is positioned against a card that moves, and a
  // menu that follows the page while its owner scrolls away is worse than one that closes.
  useEffect(() => {
    if (!open) return undefined;
    const close = () => {
      setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') close();
    };
    window.addEventListener('keydown', onKeyDown);
    window.addEventListener('scroll', close, true);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
      window.removeEventListener('scroll', close, true);
    };
  }, [open]);

  useEffect(() => {
    if (!copied) return undefined;
    const timer = setTimeout(() => {
      setCopied(false);
    }, CONFIRMATION_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [copied]);

  const running = download !== undefined && !isTerminalDownload(download.status);

  return (
    <div
      ref={containerRef}
      className="relative shrink-0"
      onBlur={(event) => {
        // Only when focus has left the whole menu, not when it moves between its own items.
        if (!event.currentTarget.contains(event.relatedTarget)) setOpen(false);
      }}
    >
      <button
        type="button"
        aria-label={t.t('app.more')}
        title={t.t('app.more')}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        onPointerDown={(event) => {
          // The card is a link; without this the press navigates instead of opening the menu.
          event.preventDefault();
          event.stopPropagation();
        }}
        onClick={(event) => {
          event.preventDefault();
          event.stopPropagation();
          setOpen((was) => !was);
        }}
        // Hidden until the card is hovered or the button itself is focused, which is how YouTube
        // does it — forty always-visible dots on a grid is noise. `group-hover` comes from the
        // card, and `focus-visible` is what keeps it reachable by keyboard.
        className={[
          'text-text-muted hover:bg-surface-hover hover:text-text grid size-8 place-items-center rounded-full',
          'opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100',
          open ? 'bg-surface-hover text-text opacity-100' : '',
        ].join(' ')}
      >
        <MoreVertical size={18} />
      </button>

      {open && (
        <div
          id={menuId}
          role="menu"
          aria-label={t.t('app.more')}
          className="bg-surface-raised absolute right-0 z-40 mt-1 w-56 overflow-hidden rounded-xl py-1 shadow-lg"
        >
          <MenuItem
            icon={saved === true ? <BookmarkCheck size={18} /> : <Bookmark size={18} />}
            label={saved === true ? t.t('video.removeBookmark') : t.t('video.bookmark')}
            onSelect={() => {
              const next = saved !== true;
              setSaved(next);
              const call = next
                ? invoke('set_bookmark', { video })
                : invoke('remove_bookmark', { videoId: video.id });
              void call.catch(() => {
                setSaved(!next);
              });
              setOpen(false);
            }}
          />

          <MenuItem
            icon={<ListPlus size={18} />}
            label={t.t('library.addToPlaylist')}
            onSelect={() => {
              setOpen(false);
              openOverlay({ kind: 'addToPlaylist', video });
            }}
          />

          {downloadable && !running && (
            <MenuItem
              icon={<Download size={18} />}
              label={t.t('download.start')}
              onSelect={() => {
                void startDownload(video);
                setOpen(false);
              }}
            />
          )}

          <MenuItem
            icon={copied ? <Check size={18} /> : <Link2 size={18} />}
            label={copied ? t.t('app.copied') : t.t('video.copyLink')}
            onSelect={() => {
              void navigator.clipboard.writeText(shareUrl(video)).then(
                () => {
                  setCopied(true);
                },
                () => {
                  // Clipboard access can be refused; saying nothing would look like it worked.
                  setCopied(false);
                },
              );
            }}
          />
        </div>
      )}
    </div>
  );
}
