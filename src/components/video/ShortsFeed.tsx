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

import {
  Captions,
  CaptionsOff,
  ChevronDown,
  ChevronUp,
  ExternalLink,
  Link2,
  Maximize2,
  MoreVertical,
  Pause,
  Play,
  Volume2,
  VolumeX,
} from 'lucide-react';
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
import { YouTubePlayer, type PlayerHandle } from '@/components/video/YouTubePlayer';
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
  /**
   * Asked for more when the viewer nears the end.
   *
   * Receives the ids just watched, to seed the next batch, and every id already shown, so the feed
   * does not circle back on itself.
   */
  onNearEnd?: (recent: readonly VideoId[], all: readonly VideoId[]) => void;
}

/** How close to the end the viewer gets before more is fetched. */
const PREFETCH_MARGIN = 6;

/** How many of the most recently shown shorts seed the next batch. */
const SEED_WINDOW = 3;

/** The Shorts tab. */
export function ShortsFeed({
  videos,
  state,
  initialVideoId,
  onNearEnd,
}: ShortsFeedProps): ReactNode {
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
  const playerRef = useRef<PlayerHandle>(null);
  const [playing, setPlaying] = useState(true);
  const [muted, setMuted] = useState(false);
  const [captions, setCaptions] = useState(false);
  const [captionsAvailable, setCaptionsAvailable] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
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

  // Asks for the next batch, seeded with what was just watched. Shared by the prefetch below and by
  // a press of "next" at the boundary.
  const requestMoreNow = useCallback(() => {
    if (!onNearEnd || videos.length === 0) return;
    const from = Math.min(index, videos.length - 1);
    const recent = videos
      .slice(Math.max(0, from - SEED_WINDOW + 1), from + 1)
      .map((video) => video.id);
    onNearEnd(
      recent,
      videos.map((video) => video.id),
    );
  }, [index, onNearEnd, videos]);

  // Bounds are clamped rather than wrapped: arriving back at the first short after the last one
  // reads as a bug, not as a loop.
  const move = useCallback(
    (delta: number) => {
      const next = Math.min(Math.max(index + delta, 0), Math.max(0, videos.length - 1));
      setNavigated({ forId: requested, index: next });
      // Pressing next at the boundary asks for more rather than doing nothing at all, so a viewer
      // who outruns the prefetch gets the feed to catch up instead of a dead button.
      if (delta > 0 && next === index) requestMoreNow();
    },
    [index, requested, videos.length, requestMoreNow],
  );

  // Fetched ahead of the end rather than at it, so the next short is already there when the viewer
  // arrives. An effect event so it can read the current list without re-firing on every change to
  // the array's identity.
  const requestMore = useEffectEvent(() => {
    if (index < videos.length - PREFETCH_MARGIN) return;
    requestMoreNow();
  });
  useEffect(() => {
    requestMore();
  }, [index, videos.length]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target;
      if (
        target instanceof HTMLElement &&
        (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable)
      ) {
        return;
      }
      if (event.key === ' ' || event.key === 'k') {
        // The embed's own keyboard handling is off with its chrome, so these are ours to provide.
        event.preventDefault();
        playerRef.current?.toggle();
        return;
      }
      if (event.key === 'm') {
        event.preventDefault();
        setMuted((current) => {
          playerRef.current?.setMuted(!current);
          return !current;
        });
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
           * The blurred poster frame, shown while the embed loads.
           *
           * It cannot fill the letterbox bars of a video that is not the stage's shape, which was
           * the original hope: the embed is one opaque iframe that fills the stage and paints its
           * own bars, so nothing behind it is ever visible once it has painted. What it does do is
           * replace a black rectangle with the video's own colours for the moment before that —
           * which is most of what makes a swap between shorts feel continuous.
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
            ref={playerRef}
            videoId={current.id}
            fill
            transparent
            autoplay
            // The embed's own chrome is hidden and replaced below, which is what YouTube does on its
            // Shorts surface. Every control drawn in its place drives the player for real.
            controls={false}
            onStateChange={(playbackState) => {
              setPlaying(playbackState === 'playing' || playbackState === 'buffering');
              // Caption availability is a property of the video, and the embed only knows once it
              // has loaded one. Asked here so the control is absent for a short that has none.
              setCaptionsAvailable(playerRef.current?.hasCaptions() ?? false);
              // Advancing on end is what makes the feed a feed. At the last short it stops, rather
              // than looping back to the top.
              if (playbackState === 'ended') {
                move(1);
              }
            }}
          />

          {/* The whole frame is the play/pause target, as it is on YouTube. A button rather than a
              div so it is keyboard reachable and announced; it carries no visible chrome of its
              own. */}
          <button
            type="button"
            onClick={() => {
              playerRef.current?.toggle();
            }}
            aria-label={t.t(playing ? 'player.pause' : 'player.play')}
            className="absolute inset-0 z-10 cursor-default"
          />

          <div className="pointer-events-none absolute inset-x-0 top-0 z-20 flex items-start justify-between p-3">
            <div className="pointer-events-auto flex items-center gap-1">
              <StageButton
                label={t.t(playing ? 'player.pause' : 'player.play')}
                onClick={() => {
                  playerRef.current?.toggle();
                }}
              >
                {playing ? <Pause size={18} /> : <Play size={18} />}
              </StageButton>
              <StageButton
                label={t.t(muted ? 'player.unmute' : 'player.mute')}
                onClick={() => {
                  const next = !muted;
                  setMuted(next);
                  playerRef.current?.setMuted(next);
                }}
              >
                {muted ? <VolumeX size={18} /> : <Volume2 size={18} />}
              </StageButton>
            </div>

            <div className="pointer-events-auto relative flex items-center gap-1">
              {/* Only for a short that actually has captions — the embed is asked, not assumed. */}
              {captionsAvailable && (
                <StageButton
                  label={t.t('player.captions')}
                  active={captions}
                  onClick={() => {
                    const next = !captions;
                    setCaptions(next);
                    playerRef.current?.setCaptions(next);
                  }}
                >
                  {captions ? <Captions size={18} /> : <CaptionsOff size={18} />}
                </StageButton>
              )}

              <StageButton
                label={t.t('app.more')}
                active={menuOpen}
                onClick={() => {
                  setMenuOpen((open) => !open);
                }}
              >
                <MoreVertical size={18} />
              </StageButton>

              <StageButton
                label={t.t('player.fullscreen')}
                onClick={() => {
                  playerRef.current?.requestFullscreen();
                }}
              >
                <Maximize2 size={18} />
              </StageButton>

              {menuOpen && (
                <div
                  className="bg-surface border-border absolute top-11 right-0 z-30 min-w-48 overflow-hidden rounded-lg border py-1 shadow-lg"
                  role="menu"
                >
                  <MenuItem
                    label={t.t('video.copyLink')}
                    icon={<Link2 size={16} />}
                    onClick={() => {
                      setMenuOpen(false);
                      void navigator.clipboard.writeText(`https://youtu.be/${current.id}`);
                    }}
                  />
                  <MenuItem
                    label={t.t('video.openExternally')}
                    icon={<ExternalLink size={16} />}
                    onClick={() => {
                      setMenuOpen(false);
                      void invoke('open_external', {
                        url: `https://www.youtube.com/shorts/${current.id}`,
                      }).catch(() => {
                        // The link simply does not open; nothing here is recoverable in the UI.
                      });
                    }}
                  />
                </div>
              )}
            </div>
          </div>

          {/* Back at the bottom edge: with the embed's own chrome hidden there is nothing left
              underneath for this band to cover. */}
          <div className="pointer-events-none absolute inset-x-0 bottom-0 z-20 bg-gradient-to-t from-black/90 to-transparent p-4 pt-16">
            {/* Channel first, then the title — YouTube's order, and the more useful one: the
                channel is what you act on, the title is what you read. */}
            {current.channel_name !== undefined && (
              <div className="mb-2 flex items-center gap-2">
                <span
                  aria-hidden="true"
                  className="grid size-7 shrink-0 place-items-center rounded-full bg-white/20 text-2xs font-semibold text-white"
                >
                  {current.channel_name.trim().charAt(0).toUpperCase()}
                </span>
                {current.channel_id ? (
                  <Link
                    to={{ name: 'channel', channelId: current.channel_id, tab: 'videos' }}
                    className="pointer-events-auto truncate text-sm font-medium text-white hover:underline"
                  >
                    {current.channel_name}
                  </Link>
                ) : (
                  <span className="truncate text-sm font-medium text-white">
                    {current.channel_name}
                  </span>
                )}
              </div>
            )}
            <h2 className="line-clamp-2 text-sm leading-snug text-white/90">{current.title}</h2>
          </div>
        </div>

        {/* Against the video's right edge, where YouTube puts it. */}
        {/* Keyed on the video so each short gets its own action state, rather than carrying the
            previous short's saved marker across. */}
        <VideoActions key={current.id} video={current} />

        {/* Navigation lives well clear of the action rail, matching YouTube: a large target out at
            the far right, so a mis-aimed press on "next" cannot land on "save". */}
        <div className="ml-8 flex flex-col items-center gap-3">
          <NavButton
            label={t.t('shorts.previous')}
            disabled={index === 0}
            onClick={() => {
              move(-1);
            }}
          >
            <ChevronUp size={24} />
          </NavButton>
          <NavButton
            // Never disabled while the feed can still grow. Greying it out at the boundary tells
            // the viewer they have reached the end when they have only reached the end of what has
            // loaded so far, which is the moment a feed feels finite.
            label={t.t('shorts.next')}
            disabled={index >= videos.length - 1 && onNearEnd === undefined}
            onClick={() => {
              move(1);
            }}
          >
            <ChevronDown size={24} />
          </NavButton>
        </div>
      </div>
    </div>
  );
}

/** One control on the video itself: circular, translucent, legible over any frame. */
function StageButton({
  label,
  onClick,
  children,
  active = false,
}: {
  label: string;
  onClick: () => void;
  children: ReactNode;
  active?: boolean;
}): ReactNode {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={label}
      aria-pressed={active}
      className={[
        'grid size-9 place-items-center rounded-full text-white/90',
        'transition-[background-color,color] duration-150 hover:bg-white/15 hover:text-white',
        active ? 'bg-white/20 text-white' : '',
      ].join(' ')}
    >
      {children}
    </button>
  );
}

/** One row of the overflow menu. */
function MenuItem({
  label,
  icon,
  onClick,
}: {
  label: string;
  icon: ReactNode;
  onClick: () => void;
}): ReactNode {
  return (
    <button
      type="button"
      role="menuitem"
      onClick={onClick}
      className="transition-surface text-text hover:bg-surface-hover flex w-full items-center gap-3 px-3 py-2 text-left text-sm"
    >
      <span className="text-text-muted shrink-0">{icon}</span>
      {label}
    </button>
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
      className="transition-surface bg-surface-translucent hover:bg-surface-translucent-hover text-text grid size-12 place-items-center rounded-full disabled:opacity-30"
    >
      {children}
    </button>
  );
}
