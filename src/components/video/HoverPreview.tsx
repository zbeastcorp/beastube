/**
 * The inline preview that plays when the pointer rests on a card.
 *
 * ## What YouTube does, and what this can do
 *
 * YouTube's preview is a `<video>` element fed from a MediaSource blob — verified by inspecting
 * their page: `ytd-video-preview` holds a real `<video>` with a `blob:` source. That needs stream
 * URLs, which SABR no longer hands out (ADR-0001), so the same mechanism is not available here. The
 * sanctioned embed is, and it produces the same result on screen.
 *
 * ## What makes it feel smooth
 *
 * Three things, and all of them are about *when* the preview appears rather than what it is:
 *
 * 1. **Nothing loads until the pointer has been still.** Sweeping across a grid must not start a
 *    dozen video loads and cancel them.
 * 2. **The thumbnail stays put underneath.** The preview is layered over it, so there is never a
 *    frame where the card is empty.
 * 3. **The fade starts when the picture does, not when the player mounts.** A player that is
 *    mounted is not yet a player that is showing anything — fading in on mount shows a black box
 *    and then a picture, which is exactly the stutter this avoids. The embed reports `playing`, and
 *    that is the moment the opacity moves.
 *
 * Leaving the card unmounts the player outright rather than pausing it: a paused hidden player
 * keeps a decoder and a socket alive for a card the pointer has already left.
 */

import { useEffect, useState, type ReactNode } from 'react';

import { YouTubePlayer } from '@/components/video/YouTubePlayer';
import type { PlaybackState, VideoId } from '@/types/domain';

/** How long the pointer must rest before anything loads. */
const HOVER_DELAY_MS = 500;

/** How long the preview fades in over. */
const FADE_MS = 260;

/**
 * Whether the user has asked for reduced motion.
 *
 * Reads the resolved decision from the document root, which the settings store writes on every
 * change — an explicit setting overrides the OS in both directions, and its absence means "follow
 * the OS", which is what the media query answers.
 */
function prefersReducedMotion(): boolean {
  if (typeof document === 'undefined') return false;
  const explicit = document.documentElement.dataset['reducedMotion'];
  if (explicit === 'true') return true;
  if (explicit === 'false') return false;
  return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

export interface HoverPreviewProps {
  videoId: VideoId;
  /** Whether the pointer is currently on the card. */
  active: boolean;
}

/**
 * A muted preview that covers the thumbnail while the pointer rests on it.
 *
 * Renders nothing until the delay has elapsed, so a pointer sweeping across a grid costs nothing.
 */
export function HoverPreview({ videoId, active }: HoverPreviewProps): ReactNode {
  const [armed, setArmed] = useState(false);
  const [playing, setPlaying] = useState(false);

  useEffect(() => {
    if (!active || prefersReducedMotion()) return undefined;

    const timer = setTimeout(() => {
      setArmed(true);
    }, HOVER_DELAY_MS);

    // Torn down in the cleanup rather than reset in the effect body: leaving the card is exactly
    // when this effect is torn down, so the unmount and the disarm are one event rather than two
    // renders chasing each other.
    return () => {
      clearTimeout(timer);
      setArmed(false);
      setPlaying(false);
    };
  }, [active, videoId]);

  if (!armed) return null;

  return (
    <div
      // `pointer-events-none` is what keeps the card clickable: without it the player swallows the
      // click and opening a video from a hovered card stops working.
      className="pointer-events-none absolute inset-0"
      style={{
        opacity: playing ? 1 : 0,
        transition: `opacity ${FADE_MS}ms var(--ease-yt, ease)`,
      }}
      aria-hidden="true"
    >
      <YouTubePlayer
        videoId={videoId}
        fill
        muted
        loop
        autoplay
        controls={false}
        onStateChange={(state: PlaybackState) => {
          // The first frame is on screen at `playing`; anything earlier would fade in a black box.
          setPlaying(state === 'playing');
        }}
      />
    </div>
  );
}
