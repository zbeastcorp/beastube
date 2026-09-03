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
import { useCallback, useEffect, useEffectEvent, useRef, useState, type ReactNode } from 'react';

import { Link } from '@/app/router';
import { EmptyState } from '@/components/common/EmptyState';
import { ErrorState } from '@/components/common/ErrorState';
import { VideoActions } from '@/components/video/VideoActions';
import { YouTubePlayer } from '@/components/video/YouTubePlayer';
import { useTranslation } from '@/i18n/context';
import type { AsyncResource } from '@/hooks/useAsyncResource';
import { invoke } from '@/services/ipc';
import { useSessionStore } from '@/stores/session';
import type { VideoSummary } from '@/types/domain';

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
}

/** The Shorts tab. */
export function ShortsFeed({ videos, state }: ShortsFeedProps): ReactNode {
  const t = useTranslation();
  const [index, setIndex] = useState(0);
  const lastWheelAt = useRef(0);
  const touchStartY = useRef<number | null>(null);
  const incognito = useSessionStore((session) => session.incognito);

  const current = videos[Math.min(index, Math.max(0, videos.length - 1))];

  // Bounds are clamped rather than wrapped: arriving back at the first short after the last one
  // reads as a bug, not as a loop.
  const move = useCallback(
    (delta: number) => {
      setIndex((currentIndex) =>
        Math.min(Math.max(currentIndex + delta, 0), Math.max(0, videos.length - 1)),
      );
    },
    [videos.length],
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
          className="bg-surface relative overflow-hidden rounded-xl"
          // Portrait, and bounded by the viewport height so the whole short is visible without
          // scrolling — a Shorts player you have to scroll to see is not one.
          style={{ aspectRatio: '9 / 16', height: 'min(82vh, 900px)' }}
        >
          <YouTubePlayer
            key={current.id}
            videoId={current.id}
            fill
            autoplay
            onStateChange={(playbackState) => {
              // Advancing on end is what makes the feed a feed. At the last short it stops, rather
              // than looping back to the top.
              if (playbackState === 'ended') {
                move(1);
              }
            }}
          />

          {/* The overlay sits below the player's own controls but above the frame, matching where
              YouTube puts a short's title and channel. */}
          <div className="pointer-events-none absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/85 to-transparent p-4 pt-16">
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
