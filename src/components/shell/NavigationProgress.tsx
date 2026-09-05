/**
 * The red bar across the top of the window while a screen is fetching.
 *
 * Every value here was measured off youtube.com rather than guessed, by sampling their
 * `yt-page-navigation-progress` element every 30ms across a real navigation:
 *
 * - 2px tall, fixed to the top edge, full width, above everything.
 * - Track `rgba(255,255,255,0.2)`; fill `linear-gradient(90deg, #f03 80%, #ff2791)` — their bar is
 *   not flat red, it warms to pink at the leading edge.
 * - Driven by `transform: scaleX()` from a `0% 50%` origin, with `transition: transform 0.08s`.
 * - The value ramps linearly to 80% over roughly 600ms and then *holds* there, however long the
 *   request takes. On completion it jumps to 100% and the element is hidden about 90ms later.
 *
 * That hold at 80% is the whole trick, and it is worth naming: a bar that animated to 100% on a
 * guess would either finish before the data did (and then sit full while nothing happened) or crawl
 * visibly. Holding short of the end means the bar is never wrong, and the jump to full reads as the
 * page arriving.
 *
 * ## Why the DOM is written directly
 *
 * The ramp updates about 60 times a second. Putting it in React state would re-render this
 * component — and everything it is nested in — on every frame of every navigation, which is a lot
 * of work to animate two pixels. The rAF loop writes `transform` on a ref'd node instead, which is
 * a compositor-only change and costs nothing.
 */

import { useEffect, useRef, type ReactNode } from 'react';

import { useProgressStore } from '@/stores/progress';

/** Where the measured ramp ends and the trickle begins. */
const HOLD_AT = 0.8;

/**
 * Where the trickle tends to, and never reaches.
 *
 * YouTube's bar parks dead at 80%. On a request that takes three seconds that reads as *frozen*:
 * the one thing the bar exists to say is "still working", and a bar that has stopped moving says
 * the opposite. So past the ramp it keeps creeping, closing a fixed fraction of the remaining gap
 * each frame — visibly moving for as long as the request runs, visibly slowing so it can never
 * arrive early. The jump to full on completion is preserved, and the space left for it is what
 * keeps that jump legible.
 */
const TRICKLE_TO = 0.94;

/**
 * Fraction of the remaining gap to {@link TRICKLE_TO} closed per second of trickle.
 *
 * At this rate the bar is at ~86% after one second past the ramp and ~91% after three — always
 * moving, never done.
 */
const TRICKLE_RATE_PER_S = 0.45;

/** How long the ramp takes to travel from nothing to {@link HOLD_AT}. */
const RAMP_MS = 600;

/** How long the full bar stays on screen after the work completes. */
const FINISH_MS = 90;

/**
 * How long work must run before the bar appears at all.
 *
 * Most navigations are now answered out of the feed cache and settle within a microtask. Showing
 * the bar for those would mean a red flicker across the top on every click that was *fast* — the
 * exact opposite of what it is for. Below this threshold nothing is drawn, so the bar means "this
 * is taking a moment", which is the only thing worth telling someone.
 */
const SHOW_DELAY_MS = 140;

/** The navigation progress bar. Renders nothing at all while idle. */
export function NavigationProgress(): ReactNode {
  // The *boolean*, not the count. Home starts two fetches, so the count goes 0 → 1 → 2, and an
  // effect keyed on the number would re-run on that second increment and restart the ramp from the
  // left mid-navigation. Zustand compares the selected value, so selecting the boolean means the
  // second fetch does not even re-render this component.
  const active = useProgressStore((state) => state.pending > 0);
  const trackRef = useRef<HTMLDivElement>(null);
  const fillRef = useRef<HTMLDivElement>(null);

  /**
   * How far along the bar currently is, kept across runs.
   *
   * Clicking Home twice in quick succession finishes one run and starts another. Without this the
   * second run began its ramp at zero while the bar was still sitting at full from the first, so
   * the bar visibly snapped backwards — the one motion a progress indicator must never make. The
   * next run picks up from where the last one got to instead.
   */
  const reached = useRef(0);

  useEffect(() => {
    const track = trackRef.current;
    const fill = fillRef.current;
    if (!track || !fill) return undefined;

    if (!active) {
      // Nothing in flight. If the bar was up, run it to the end and take it away; if it was never
      // up (the common case — most renders are not navigations) this is a no-op.
      if (track.style.opacity === '0') return undefined;
      fill.style.transform = 'scaleX(1)';
      reached.current = 1;
      const timer = setTimeout(() => {
        track.style.opacity = '0';
        // Reset only once hidden, so the bar does not visibly rewind to the left.
        fill.style.transition = 'none';
        fill.style.transform = 'scaleX(0)';
        reached.current = 0;
        // Read back to flush the transition removal before the next navigation re-enables it,
        // otherwise the reset itself animates. The value is discarded; the read is the point.
        fill.getBoundingClientRect();
        fill.style.transition = '';
      }, FINISH_MS);
      return () => {
        clearTimeout(timer);
      };
    }

    // Work started — but not shown yet. If it finishes inside the grace period the viewer never
    // sees anything, which is correct: nothing was slow enough to be worth reporting.
    //
    // A bar already on screen means a second navigation overtook the first. It keeps going from
    // wherever it had reached rather than restarting, and it is not re-delayed: it is visible, so
    // there is nothing left to decide about whether to show it.
    const visible = track.style.opacity === '1';
    const from = visible ? reached.current : 0;

    // The remaining distance is travelled in the remaining share of the ramp, so a run that picks
    // up near the hold point does not crawl the last few percent over the full duration.
    const distance = Math.max(0, HOLD_AT - from);
    const duration = RAMP_MS * (distance / HOLD_AT);

    let frame = 0;
    let start = 0;
    let previous = 0;
    const step = (now: number) => {
      if (start === 0) {
        start = now;
        previous = now;
      }
      const dt = Math.min(0.1, (now - previous) / 1000);
      previous = now;

      let scale: number;
      if (duration > 0 && now - start < duration) {
        // The measured ramp: linear to the hold point.
        scale = from + distance * ((now - start) / duration);
      } else {
        // Past it: close a fixed share of the remaining gap per second. Frame-rate independent —
        // the same motion at 60 Hz and 144 Hz — and asymptotic, so it slows but never stops and
        // never arrives before the data does.
        // A navigation beginning while the previous bar is still at full restarts it. The
        // monotonic clamp below is what stops a bar running backwards, but on its own it also
        // pinned an overtaking navigation at 100% for its whole duration — motionless, and
        // finished-looking, while work was still going on.
        if (reached.current >= 1) {
          fill.style.transition = 'none';
          fill.style.transform = 'scaleX(0)';
          reached.current = 0;
          // Flush the removal so the restart does not itself animate. The read is the point.
          fill.getBoundingClientRect();
          fill.style.transition = '';
        }
        const current = Math.max(reached.current, HOLD_AT);
        // Clamped to where the bar already is, and the gap floored at zero. A run started while a
        // previous one was still finishing found `reached` at 1 and pulled it back toward the 0.94
        // trickle target — a progress bar visibly running backwards. It can now only hold or move
        // forward, which is the one thing a progress bar must never get wrong.
        const target =
          current + Math.max(0, TRICKLE_TO - current) * (1 - Math.exp(-TRICKLE_RATE_PER_S * dt));
        scale = Math.max(reached.current, target);
      }

      reached.current = scale;
      fill.style.transform = `scaleX(${String(scale)})`;
      // Keeps going until the run is cancelled by completion. A stopped loop was the frozen bar.
      frame = requestAnimationFrame(step);
    };

    if (visible) {
      frame = requestAnimationFrame(step);
      return () => {
        cancelAnimationFrame(frame);
      };
    }

    const reveal = setTimeout(() => {
      track.style.opacity = '1';
      frame = requestAnimationFrame(step);
    }, SHOW_DELAY_MS);
    return () => {
      clearTimeout(reveal);
      cancelAnimationFrame(frame);
    };
  }, [active]);

  return (
    <div
      ref={trackRef}
      // Not `aria-busy` or a live region: this is decoration for a state the view itself already
      // announces through its skeleton and its loading text. A second announcement of the same
      // fact is noise to a screen reader.
      aria-hidden="true"
      className="pointer-events-none fixed inset-x-0 top-0 z-[2100] h-0.5 overflow-hidden"
      style={{ background: 'rgba(255,255,255,0.2)', opacity: 0 }}
    >
      <div
        ref={fillRef}
        className="h-full w-full"
        style={{
          background: 'linear-gradient(90deg, #f03 80%, #ff2791)',
          transformOrigin: '0% 50%',
          transform: 'scaleX(0)',
          transition: 'transform 80ms linear',
        }}
      />
    </div>
  );
}
