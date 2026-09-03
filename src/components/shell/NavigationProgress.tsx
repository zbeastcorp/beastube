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

/** Where the ramp stops and waits for the request to actually finish. */
const HOLD_AT = 0.8;

/** How long the ramp takes to travel from nothing to {@link HOLD_AT}. */
const RAMP_MS = 600;

/** How long the full bar stays on screen after the work completes. */
const FINISH_MS = 90;

/** The navigation progress bar. Renders nothing at all while idle. */
export function NavigationProgress(): ReactNode {
  // The *boolean*, not the count. Home starts two fetches, so the count goes 0 → 1 → 2, and an
  // effect keyed on the number would re-run on that second increment and restart the ramp from the
  // left mid-navigation. Zustand compares the selected value, so selecting the boolean means the
  // second fetch does not even re-render this component.
  const active = useProgressStore((state) => state.pending > 0);
  const trackRef = useRef<HTMLDivElement>(null);
  const fillRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const track = trackRef.current;
    const fill = fillRef.current;
    if (!track || !fill) return undefined;

    if (!active) {
      // Nothing in flight. If the bar was up, run it to the end and take it away; if it was never
      // up (the common case — most renders are not navigations) this is a no-op.
      if (track.style.opacity === '0') return undefined;
      fill.style.transform = 'scaleX(1)';
      const timer = setTimeout(() => {
        track.style.opacity = '0';
        // Reset only once hidden, so the bar does not visibly rewind to the left.
        fill.style.transition = 'none';
        fill.style.transform = 'scaleX(0)';
        // Read back to flush the transition removal before the next navigation re-enables it,
        // otherwise the reset itself animates. The value is discarded; the read is the point.
        fill.getBoundingClientRect();
        fill.style.transition = '';
      }, FINISH_MS);
      return () => {
        clearTimeout(timer);
      };
    }

    // Work started. Show the bar and ramp toward the hold point.
    track.style.opacity = '1';
    let frame = 0;
    let start = 0;
    const step = (now: number) => {
      if (start === 0) start = now;
      const progress = Math.min(1, (now - start) / RAMP_MS);
      fill.style.transform = `scaleX(${String(progress * HOLD_AT)})`;
      if (progress < 1) frame = requestAnimationFrame(step);
    };
    frame = requestAnimationFrame(step);
    return () => {
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
