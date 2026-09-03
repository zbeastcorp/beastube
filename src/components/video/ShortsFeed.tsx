/**
 * The Shorts feed.
 *
 * YouTube's shape: one portrait video filling the column, the next one a scroll or an arrow key
 * away, with the title, channel and controls overlaid. The tab previously ran a text search for the
 * word "shorts" and rendered the results as ordinary landscape cards — which looked like a Shorts
 * tab and was not one (§131).
 *
 * ## Only one player exists
 *
 * The feed holds many videos and exactly one player. Mounting a player per short would start a
 * network fetch and a decode for every item in the list; instead the single player is pointed at
 * whichever short is current. That is also why moving between shorts is instant rather than a
 * mount/unmount cycle.
 *
 * ## Navigation
 *
 * Wheel, arrow keys, the on-screen buttons and touch drag all move by exactly one short. Wheel
 * events are rate-limited: a trackpad emits a burst of small deltas for one physical flick, and
 * without a cooldown a single gesture would skip four or five videos.
 */

import { ChevronDown, ChevronUp } from 'lucide-react';
import {
  useCallback,
  useEffect,
  useEffectEvent,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from 'react';

import { Link } from '@/app/router';
import { EmptyState } from '@/components/common/EmptyState';
import { ErrorState } from '@/components/common/ErrorState';
import { VideoActions } from '@/components/video/VideoActions';
import { YouTubePlayer } from '@/components/video/YouTubePlayer';
import { useTranslation } from '@/i18n/context';
import type { AsyncResource } from '@/hooks/useAsyncResource';
import { invoke } from '@/services/ipc';
import { useSessionStore } from '@/stores/session';
import {
  bestThumbnailFor,
  videoAspectRatio,
  type VideoId,
  type VideoSummary,
} from '@/types/domain';

/** Minimum gap between two accepted wheel gestures. */
const WHEEL_COOLDOWN_MS = 450;

/** Wheel delta below which an event is treated as inertial noise rather than a gesture. */
const WHEEL_THRESHOLD = 12;

/** Vertical drag distance that counts as a swipe. */
const SWIPE_THRESHOLD_PX = 60;

interface ShortsFeedProps {
  videos: readonly VideoSummary[];
  /** The resource backing `videos`, for the loading and error states. */
  state: Pick<AsyncResource<unknown>, 'loading' | 'error' | 'reload'>;
  /**
   * Open on this video rather than at the top.
   *
   * Set when the feed was reached by clicking a specific short. If that video is not in the batch
   * the feed happened to load, the request is dropped rather than failing — the user still gets a
   * feed, which is better than an error about a video they can see the thumbnail of.
   */
  initialVideoId?: VideoId;
}

/** The Shorts tab. */
export function ShortsFeed({ videos, state, initialVideoId }: ShortsFeedProps): ReactNode {
  const t = useTranslation();
  /**
   * Where the user has navigated to, tagged with the deep link it was relative to.
   *
   * Derived rather than seeded by an effect. The requested video arrives asynchronously — the feed
   * renders before the batch containing it lands — and an effect that reached back to correct the
   * index would run a render with the wrong short on screen first. Tagging the stored position with
   * the id it belongs to lets a stale value simply not apply.
   */
  const [navigated, setNavigated] = useState<{ forId: VideoId | null; index: number } | null>(null);
  const lastWheelAt = useRef(0);
  const touchStartY = useRef<number | null>(null);
  const incognito = useSessionStore((session) => session.incognito);

  const requested = initialVideoId ?? null;
  const requestedIndex =
    initialVideoId === undefined ? -1 : videos.findIndex((video) => video.id === initialVideoId);

  // A stored position applies only while it belongs to the current request; otherwise the deep
  // link decides, and failing that the top of the feed does.
  const index =
    navigated !== null && navigated.forId === requested
      ? navigated.index
      : requestedIndex >= 0
        ? requestedIndex
        : 0;

  const current = videos[Math.min(index, Math.max(0, videos.length - 1))];
  // Portrait-first, and never wider than portrait. Sizing purely from the thumbnail was tried and
  // produced a landscape stage with the video pillar-boxed inside it, because some renditions of a
  // short are padded to 16:9 — the very thing this feed exists to not do. A measured ratio is only
  // trusted when it is itself portrait, which is where it can still help: a 3:4 short then gets a
  // 3:4 stage instead of black bars.
  const measured = current ? videoAspectRatio(current) : 9 / 16;
  const stageRatio = measured < 1 ? measured : 9 / 16;
  // A small rendition on purpose: it is about to be blurred to mush, and it is swapped on every
  // navigation.
  const backdrop = current?.thumbnails ? bestThumbnailFor(current.thumbnails, 160)?.url : undefined;

  // Bounds are clamped rather than wrapped: arriving back at the first short after the last one
  // reads as a bug, not as a loop.
  const move = useCallback(
    (delta: number) => {
      setNavigated({
        forId: requested,
        index: Math.min(Math.max(index + delta, 0), Math.max(0, videos.length - 1)),
      });
    },
    [index, requested, videos.length],
  );

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target;
      if (
        target instanceof HTMLElement &&
        (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable)
      ) {
        return;
      }
      if (event.key === 'ArrowDown' || event.key === 'PageDown' || event.key === 'j') {
        event.preventDefault();
        move(1);
      } else if (event.key === 'ArrowUp' || event.key === 'PageUp' || event.key === 'k') {
        event.preventDefault();
        move(-1);
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
    };
  }, [move]);

  // Watching a short is watching a video, so it is recorded on the same terms as anything else —
  // the native side is the authority on whether the write actually happens.
  const record = useEffectEvent(() => {
    if (!current || incognito) return;
    void invoke('record_watch', { video: current }).catch(() => {
      // Best effort: a failed history write must not interrupt playback.
    });
  });
  useEffect(() => {
    record();
  }, [current?.id]);

  if (videos.length === 0) {
    if (state.error) {
      return <ErrorState error={state.error} onRetry={state.reload} />;
    }
    if (state.loading) {
      return (
        <div className="mx-auto w-full max-w-[420px]">
          <div className="skeleton rounded-xl" style={{ aspectRatio: '9 / 16' }} />
        </div>
      );
    }
    return <EmptyState titleKey="shorts.empty" bodyKey="home.emptyHint" icon="search" />;
  }

  if (!current) return null;

  return (
    <div
      className="flex justify-center"
      onWheel={(event) => {
        if (Math.abs(event.deltaY) < WHEEL_THRESHOLD) return;
        const now = Date.now();
        if (now - lastWheelAt.current < WHEEL_COOLDOWN_MS) return;
        lastWheelAt.current = now;
        move(event.deltaY > 0 ? 1 : -1);
      }}
      onTouchStart={(event) => {
        touchStartY.current = event.touches[0]?.clientY ?? null;
      }}
      onTouchEnd={(event) => {
        const start = touchStartY.current;
        const end = event.changedTouches[0]?.clientY;
        touchStartY.current = null;
        if (start === null || end === undefined) return;
        const travelled = start - end;
        if (Math.abs(travelled) < SWIPE_THRESHOLD_PX) return;
        move(travelled > 0 ? 1 : -1);
      }}
    >
      <div className="flex items-center gap-4">
        <div
          className="bg-bg relative overflow-hidden rounded-xl"
          /*
           * Sized to the video, not to a fixed 9:16 box. Shorts are not all 9:16 — real YouTube
           * measures its stage against the video and this does the same, from the thumbnail, which
           * is the only aspect signal a cross-origin embed leaves reachable.
           *
           * Height is the definite dimension and the ratio derives the width. The third `min()`
           * term is what stops a wide-tagged item from blowing the row out sideways, and the height
           * budget subtracts the shell chrome rather than guessing at a viewport fraction — 82vh
           * ignored 128px of top bar and padding, so on a short window the tab scrolled.
           */
          style={
            {
              '--ar': stageRatio,
              aspectRatio: 'var(--ar)',
              height:
                'min(calc(100dvh - var(--layout-topbar-height) - 5rem), 900px, calc(520px / var(--ar)))',
            } as CSSProperties
          }
        >
          {/*
           * The blurred backdrop that fills whatever the video does not. YouTube does exactly this,
           * and it is also the safety net for the cases the thumbnail ratio gets wrong: the strips
           * either side stop being flat black and start being the video's own colours.
           *
           * Kept mounted across navigations rather than remounted, so a short change does not flash
           * the empty stage while the next thumbnail decodes.
           */}
          {backdrop !== undefined && (
            <img
              src={backdrop}
              alt=""
              aria-hidden="true"
              className="absolute inset-0 size-full scale-110 object-cover opacity-60 blur-2xl"
            />
          )}

          {/* Deliberately unkeyed. A key here would unmount and rebuild the player — and with it the
              whole embed iframe — on every navigation, which is exactly the stutter this feed is
              supposed to not have. One player persists and swaps videos in place. */}
          <YouTubePlayer
            videoId={current.id}
            fill
            transparent
            autoplay
            onStateChange={(playbackState) => {
              // Advancing on end is what makes the feed a feed. At the last short it stops, rather
              // than looping back to the top.
              if (playbackState === 'ended') {
                move(1);
              }
            }}
          />

          {/*
           * Lifted clear of the embed's own control strip. The embed is one opaque iframe, so an
           * absolutely-positioned sibling paints OVER its chrome however it is ordered — the
           * gradient was covering the seek bar, and the channel link (the one clickable thing in an
           * otherwise pointer-transparent band) sat directly on top of the play button.
           */}
          <div className="pointer-events-none absolute inset-x-0 bottom-[52px] bg-gradient-to-t from-black/85 to-transparent p-4 pt-16">
            <h2 className="line-clamp-2 text-base leading-snug font-medium text-white">
              {current.title}
            </h2>
            {current.channel_name !== undefined && (
              <span className="mt-1 block text-xs text-white/75">
                {current.channel_id ? (
                  <Link
                    to={{ name: 'channel', channelId: current.channel_id, tab: 'videos' }}
                    className="pointer-events-auto hover:text-white"
                  >
                    {current.channel_name}
                  </Link>
                ) : (
                  current.channel_name
                )}
              </span>
            )}
          </div>
        </div>

        {/* The action rail sits against the video, the way YouTube's does; navigation is a separate
            column further out, so a mis-aimed click on "next" cannot land on "save". */}
        {/* Keyed on the video so each short gets its own action state, rather than carrying the
            previous short's saved marker across. */}
        <VideoActions key={current.id} video={current} />

        <div className="flex flex-col gap-3">
          <NavButton
            label={t.t('shorts.previous')}
            disabled={index === 0}
            onClick={() => {
              move(-1);
            }}
          >
            <ChevronUp size={22} />
          </NavButton>
          <NavButton
            label={t.t('shorts.next')}
            disabled={index >= videos.length - 1}
            onClick={() => {
              move(1);
            }}
          >
            <ChevronDown size={22} />
          </NavButton>
          <span className="text-text-muted text-center font-mono text-2xs">
            {index + 1}/{videos.length}
          </span>
        </div>
      </div>
    </div>
  );
}

function NavButton({
  label,
  onClick,
  disabled,
  children,
}: {
  label: string;
  onClick: () => void;
  disabled: boolean;
  children: ReactNode;
}): ReactNode {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      aria-label={label}
      title={label}
      className="transition-surface bg-surface-translucent hover:bg-surface-translucent-hover text-text grid size-11 place-items-center rounded-full disabled:opacity-30"
    >
      {children}
    </button>
  );
}
