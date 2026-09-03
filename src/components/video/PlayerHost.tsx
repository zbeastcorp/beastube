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
 * ## The embed's chrome is left alone, and that is a deliberate trade
 *
 * The embed paints a band across the top of the player carrying the title and channel on the left
 * — and the volume, subtitles and **settings** controls on the right. They are one band. Nothing
 * removes the title alone: `showinfo` was withdrawn, and `controls=0` covers the bottom bar only.
 *
 * Two earlier attempts here are worth recording, because both were wrong in instructive ways.
 * The first cropped the band away by making the frame taller at both edges and shifting it up. It
 * removed the title cleanly and cost no picture — and it also removed the settings menu, because
 * the gear lives in that same band. The second replaced the cropped controls with hand-built ones,
 * which looked right and could not change quality, because nothing built on the IFrame JS API can:
 * Google documents `setPlaybackQuality`, `getPlaybackQuality` and `getAvailableQualityLevels` as no
 * longer supported, with `setPlaybackQuality` an explicit no-op.
 *
 * The embed's *own* settings menu is a different thing — YouTube's UI inside the frame, unaffected
 * by that deprecation — and its quality selector genuinely works. It is the only one on this
 * playback path that does. So the band stays: the title showing is the price of a quality menu
 * that is real, and that is the trade the user asked for explicitly.
 *
 * ## Leaving a screen pauses rather than tears down
 *
 * When no view wants the player, it is paused and parked out of sight with its browsing context
 * intact. That is the whole point: the next video costs one `loadVideoById` rather than a fresh
 * embed bootstrap.
 */

import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';

import { YouTubePlayer, type PlayerHandle } from '@/components/video/YouTubePlayer';
import { playerHandlers, usePlayerStore } from '@/stores/player';

/** Where the player waits when nothing wants it: off-screen, alive, and out of the way. */
const PARKED = { top: -100_000, left: 0, width: 640, height: 360 } as const;

interface Box {
  top: number;
  left: number;
  width: number;
  height: number;
}

/** The player, positioned over whichever slot is currently registered. */
export function PlayerHost({ scroller }: { scroller: HTMLElement | null }): ReactNode {
  const session = usePlayerStore((state) => state.session);
  const slot = usePlayerStore((state) => state.slot);
  const videoId = usePlayerStore((state) => state.lastVideoId);

  const playerRef = useRef<PlayerHandle>(null);
  const [box, setBox] = useState<Box>(PARKED);
  const [started, setStarted] = useState(false);

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

  // The store retains the last video, so the parked player stays pointed at something without this
  // component needing state and an effect to remember it.
  if (videoId === null) return null;

  const hidden = session === null || slot === null;

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
    >
      <div
        // 12px, measured on youtube.com. Promoting the box to its own layer is what makes an
        // `<iframe>` actually respect the radius — without it the embed's square corners show
        // through.
        className="relative size-full overflow-hidden rounded-xl bg-black"
        style={{ transform: 'translateZ(0)', isolation: 'isolate' }}
      >
        <div className="absolute inset-0">
          <YouTubePlayer
            ref={playerRef}
            videoId={videoId}
            fill
            // The embed's own controls, deliberately. Their quality menu is YouTube's UI rather
            // than the IFrame JS API, and it is the only quality control on this path that works.
            controls
            autoplay={session?.autoplay ?? false}
            {...(session?.startAtMs !== undefined ? { startAtMs: session.startAtMs } : {})}
            onStateChange={(state, forId) => {
              if (state === 'playing') setStarted(true);
              playerHandlers().onStateChange?.(state, forId);
            }}
            onPosition={(positionMs, durationMs) => {
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
      </div>
    </div>
  );
}
