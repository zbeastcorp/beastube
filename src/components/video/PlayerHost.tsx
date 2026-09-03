/**
 * The single player, mounted once and never unmounted.
 *
 * Lives inside the scrolling region but outside the route-keyed subtree, so navigating between
 * screens cannot destroy it. A view that wants playback puts an empty box in its layout and
 * registers it as the slot; this positions the player over that box.
 *
 * ## Absolutely positioned inside the scroller, not fixed to the window
 *
 * The same arrangement `ShortsFeed` uses. Because the host is a child of the element that scrolls,
 * the browser moves it with the content for free — there is no scroll listener, and nothing to
 * fall behind during a fast scroll. Only a *layout* change needs re-measuring, which is rare.
 *
 * ## The embed's own chrome is cropped away, and replaced
 *
 * The embed paints its title and channel across the top of the player and a "More videos" strip
 * with a watermark across the bottom. `controls=0` removes the control bar but not the title, and
 * no parameter removes the title at all — measured against the running app rather than assumed.
 *
 * So the player is given extra height at the top *and* the bottom and shifted up by exactly one of
 * those amounts. The embed fits a 16:9 video to the box's width, so the extra height becomes an
 * equal letterbox bar above and below the picture, and the visible window lands precisely on the
 * picture. The embed's bands sit in those bars and are cropped with them. No frame is lost — that
 * symmetry is the whole point, and it is why the height is doubled rather than added to one side.
 *
 * Cropping the bottom takes the embed's controls with it, so the controls below are ours. Each one
 * drives the player for real. There is no quality menu: `setPlaybackQuality` has been a documented
 * no-op since 2025, and a control that cannot do its job does not belong on screen (§131).
 *
 * ## Leaving a screen pauses rather than tears down
 *
 * When no view wants the player, it is paused and parked out of sight with its browsing context
 * intact. That is the whole point: the next video costs one `loadVideoById` rather than a fresh
 * embed bootstrap.
 */

import { Captions, CaptionsOff, Maximize2, Pause, Play, Volume2, VolumeX } from 'lucide-react';
import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';

import { YouTubePlayer, type PlayerHandle } from '@/components/video/YouTubePlayer';
import { useTranslation } from '@/i18n/context';
import { playerHandlers, usePlayerStore } from '@/stores/player';

/** Where the player waits when nothing wants it: off-screen, alive, and out of the way. */
const PARKED = { top: -100_000, left: 0, width: 640, height: 360 } as const;

/**
 * How much of the embed's top and bottom edge is cropped away, in CSS pixels.
 *
 * Comfortably taller than either band. See the note above for why it applies to both edges and why
 * that costs no picture.
 */
const EMBED_CHROME_CROP_PX = 64;

/** How long the pointer must rest before the controls fade, as YouTube's do. */
const CHROME_IDLE_MS = 2600;

interface Box {
  top: number;
  left: number;
  width: number;
  height: number;
}

/** The player, positioned over whichever slot is currently registered. */
export function PlayerHost({ scroller }: { scroller: HTMLElement | null }): ReactNode {
  const t = useTranslation();
  const session = usePlayerStore((state) => state.session);
  const slot = usePlayerStore((state) => state.slot);
  const videoId = usePlayerStore((state) => state.lastVideoId);

  const playerRef = useRef<PlayerHandle>(null);
  const [box, setBox] = useState<Box>(PARKED);
  const [started, setStarted] = useState(false);
  const [playing, setPlaying] = useState(false);
  const [at, setAt] = useState({ positionMs: 0, durationMs: 0 });
  const [muted, setMuted] = useState(false);
  const [volume, setVolume] = useState(100);
  const [captionsOn, setCaptionsOn] = useState(false);
  const [captionsAvailable, setCaptionsAvailable] = useState(false);
  const [chromeVisible, setChromeVisible] = useState(true);
  const idleTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const measure = useCallback(() => {
    // Only ever called while a slot exists. When one does not, the player is hidden, so whatever
    // box it last held is never on screen.
    if (!slot || !scroller) return;
    const slotRect = slot.getBoundingClientRect();
    const scrollerRect = scroller.getBoundingClientRect();
    setBox({
      // Relative to the scroller's content, so the browser scrolls the player with everything else.
      top: slotRect.top - scrollerRect.top + scroller.scrollTop,
      left: slotRect.left - scrollerRect.left + scroller.scrollLeft,
      width: slotRect.width,
      height: slotRect.height,
    });
  }, [slot, scroller]);

  useEffect(() => {
    if (!slot) return undefined;

    // No measurement in the effect body: that would be a synchronous setState inside an effect,
    // which cascades a second render every time. `ResizeObserver` invokes its callback once as
    // soon as it observes, so the first measurement arrives from the observer like every other —
    // as an update from an external system, which is what effects are for.
    const observer = new ResizeObserver(measure);
    observer.observe(slot);
    if (scroller) observer.observe(scroller);
    window.addEventListener('resize', measure);
    return () => {
      observer.disconnect();
      window.removeEventListener('resize', measure);
    };
  }, [measure, slot, scroller]);

  // Nothing wants the player: stop it, but keep it. Pausing rather than unmounting is what makes
  // the next video instant.
  useEffect(() => {
    if (session === null) playerRef.current?.pause();
  }, [session]);

  useEffect(
    () => () => {
      if (idleTimer.current !== null) clearTimeout(idleTimer.current);
    },
    [],
  );

  const wake = useCallback(() => {
    setChromeVisible(true);
    if (idleTimer.current !== null) clearTimeout(idleTimer.current);
    idleTimer.current = setTimeout(() => {
      setChromeVisible(false);
    }, CHROME_IDLE_MS);
  }, []);

  const toggle = useCallback(() => {
    // Flipped optimistically: every command is a postMessage round trip, and waiting for the state
    // to come back makes the button feel like it missed the press.
    setPlaying((was) => !was);
    playerRef.current?.toggle();
  }, []);

  // The store retains the last video, so the parked player stays pointed at something without this
  // component needing state and an effect to remember it.
  if (videoId === null) return null;

  const hidden = session === null || slot === null;
  const fraction = at.durationMs > 0 ? Math.min(1, at.positionMs / at.durationMs) : 0;

  return (
    <div
      // `aria-hidden` while parked: it is off-screen and paused, and announcing it would put a
      // player in the reading order of a screen that has nothing to do with one.
      aria-hidden={hidden}
      className="absolute"
      style={{
        top: box.top,
        left: box.left,
        width: box.width,
        height: box.height,
        // Kept out of the way rather than removed, so the browsing context survives.
        visibility: hidden ? 'hidden' : 'visible',
        pointerEvents: hidden ? 'none' : 'auto',
      }}
      onPointerMove={wake}
      onPointerLeave={() => {
        setChromeVisible(false);
      }}
    >
      <div
        // 12px, measured on youtube.com. Promoting the box to its own layer is what makes an
        // `<iframe>` actually respect the radius — without it the embed's square corners show
        // through.
        className="relative size-full overflow-hidden rounded-xl bg-black"
        style={{ transform: 'translateZ(0)', isolation: 'isolate' }}
      >
        {/* Taller than the clip on both sides and shifted up by half the difference, so the embed's
            own title band and bottom strip land outside the visible window. See the note above. */}
        <div
          className="absolute inset-x-0"
          style={{
            top: -EMBED_CHROME_CROP_PX,
            height: `calc(100% + ${String(EMBED_CHROME_CROP_PX * 2)}px)`,
          }}
        >
          <YouTubePlayer
            ref={playerRef}
            videoId={videoId}
            fill
            // The embed's own controls would be cropped away with the bottom band, so they are
            // turned off and replaced rather than left half-visible.
            controls={false}
            autoplay={session?.autoplay ?? false}
            {...(session?.startAtMs !== undefined ? { startAtMs: session.startAtMs } : {})}
            onStateChange={(state, forId) => {
              setPlaying(state === 'playing' || state === 'buffering');
              if (state === 'playing') setStarted(true);
              // Caption availability is a property of the video, and the embed only knows once it
              // has loaded one. Asked here so the control is absent for a video that has none.
              setCaptionsAvailable(playerRef.current?.hasCaptions() ?? false);
              playerHandlers().onStateChange?.(state, forId);
            }}
            onPosition={(positionMs, durationMs) => {
              setAt({ positionMs, durationMs });
              playerHandlers().onPosition?.(positionMs, durationMs);
            }}
          />
        </div>

        {/* The video's own thumbnail, over the player until the first frame lands. The embed paints
            black while it buffers, and a black rectangle reads as broken rather than as loading. */}
        {session?.posterUrl !== undefined && (
          <img
            src={session.posterUrl}
            alt=""
            aria-hidden="true"
            className="pointer-events-none absolute inset-0 z-10 size-full object-cover"
            style={{
              opacity: started ? 0 : 1,
              transition: 'opacity 220ms var(--ease-player-out)',
            }}
          />
        )}

        {/* The whole frame toggles playback, as it does on YouTube. A button so it is reachable
            from the keyboard and announced; it carries no chrome of its own. */}
        <button
          type="button"
          onClick={toggle}
          aria-label={t.t(playing ? 'player.pause' : 'player.play')}
          className="absolute inset-0 z-20 cursor-default"
        />

        <div
          className="pointer-events-none absolute inset-x-0 bottom-0 z-30 px-3 pt-8 pb-2"
          style={{
            // A short gradient under the bar only, which is how YouTube keeps white controls
            // legible over a bright frame. It stops well short of the picture.
            background: 'linear-gradient(to top, rgba(0,0,0,0.7), transparent)',
            // Held open while paused: a paused video with no visible controls looks stuck.
            opacity: chromeVisible || !playing ? 1 : 0,
            transition: 'opacity var(--duration-chrome) var(--ease-player-out)',
          }}
        >
          <Scrubber
            fraction={fraction}
            label={t.t('player.seek')}
            onSeek={(next) => {
              const target = next * at.durationMs;
              setAt((current) => ({ ...current, positionMs: target }));
              playerRef.current?.seek(target);
            }}
          />

          <div className="pointer-events-auto mt-1 flex items-center gap-1">
            <ControlButton label={t.t(playing ? 'player.pause' : 'player.play')} onClick={toggle}>
              {playing ? <Pause size={20} /> : <Play size={20} />}
            </ControlButton>

            <ControlButton
              label={t.t(muted ? 'player.unmute' : 'player.mute')}
              onClick={() => {
                const next = !muted;
                setMuted(next);
                playerRef.current?.setMuted(next);
              }}
            >
              {muted || volume === 0 ? <VolumeX size={20} /> : <Volume2 size={20} />}
            </ControlButton>

            <input
              type="range"
              min={0}
              max={100}
              value={muted ? 0 : volume}
              aria-label={t.t('player.volume')}
              onChange={(event) => {
                const next = Number(event.target.value);
                setVolume(next);
                playerRef.current?.setVolume(next);
                // Moving the slider off zero is an unmute; nobody drags a slider expecting silence.
                const shouldMute = next === 0;
                if (shouldMute !== muted) {
                  setMuted(shouldMute);
                  playerRef.current?.setMuted(shouldMute);
                }
              }}
              className="accent-brand w-20"
            />

            <span className="ml-1 font-mono text-xs tabular-nums text-white/90">
              {t.duration(at.positionMs)} / {t.duration(at.durationMs)}
            </span>

            <span className="flex-1" />

            {/* Only for a video that actually has captions — the embed is asked, not assumed. */}
            {captionsAvailable && (
              <ControlButton
                label={t.t('player.captions')}
                active={captionsOn}
                onClick={() => {
                  const next = !captionsOn;
                  setCaptionsOn(next);
                  playerRef.current?.setCaptions(next);
                }}
              >
                {captionsOn ? <Captions size={20} /> : <CaptionsOff size={20} />}
              </ControlButton>
            )}

            <ControlButton
              label={t.t('player.fullscreen')}
              onClick={() => {
                playerRef.current?.requestFullscreen();
              }}
            >
              <Maximize2 size={20} />
            </ControlButton>
          </div>
        </div>
      </div>
    </div>
  );
}

/**
 * The seek bar.
 *
 * Pointer capture rather than window listeners: the drag belongs to this element, so releasing
 * outside the window still ends it and no listener can outlive the component.
 */
function Scrubber({
  fraction,
  label,
  onSeek,
}: {
  fraction: number;
  label: string;
  onSeek: (fraction: number) => void;
}): ReactNode {
  const seekTo = (element: HTMLElement, clientX: number) => {
    const rect = element.getBoundingClientRect();
    if (rect.width <= 0) return;
    onSeek(Math.min(1, Math.max(0, (clientX - rect.left) / rect.width)));
  };

  return (
    <div
      role="slider"
      tabIndex={0}
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(fraction * 100)}
      // A generous hit area around a thin bar, which is how a scrubber stays grabbable without
      // looking heavy.
      className="group pointer-events-auto -mx-1 cursor-pointer px-1 py-2"
      onPointerDown={(event) => {
        event.currentTarget.setPointerCapture(event.pointerId);
        seekTo(event.currentTarget, event.clientX);
      }}
      onPointerMove={(event) => {
        if (!event.currentTarget.hasPointerCapture(event.pointerId)) return;
        seekTo(event.currentTarget, event.clientX);
      }}
      onKeyDown={(event) => {
        // Five percent a press, roughly what YouTube's arrow keys move.
        if (event.key === 'ArrowRight') onSeek(Math.min(1, fraction + 0.05));
        if (event.key === 'ArrowLeft') onSeek(Math.max(0, fraction - 0.05));
      }}
    >
      <div className="h-[3px] w-full rounded-full bg-white/30 transition-[height] group-hover:h-[5px]">
        <div
          className="bg-brand h-full rounded-full"
          // Width rather than a transform: the fill is a child of a rounded track, and a scaled
          // child would round its own right edge in the wrong place.
          style={{ width: `${String(fraction * 100)}%` }}
        />
      </div>
    </div>
  );
}

/** One control on the bar: legible over any frame, no chrome until hovered. */
function ControlButton({
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
        'grid size-9 shrink-0 place-items-center rounded-full text-white',
        'transition-[background-color] duration-[var(--duration-chrome-button)] ease-[var(--ease-player-out)] hover:bg-white/15',
        active ? 'bg-white/20' : '',
      ].join(' ')}
    >
      {children}
    </button>
  );
}
