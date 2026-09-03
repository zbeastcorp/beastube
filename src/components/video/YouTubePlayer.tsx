/**
 * The sanctioned embedded player.
 *
 * The production playback adapter chosen in ADR-0001. It drives YouTube's IFrame Player API, which
 * means the provider's own player handles the media transport — including the SABR transport that
 * has removed plain stream URLs, and which the direct-stream adapter therefore cannot reach.
 *
 * ## What this adapter can and cannot do
 *
 * Capability-gated, not silently broken (§131):
 *
 * * **Can**: play/pause, seek, playback rate, volume, fullscreen, captions toggle, position
 *   reporting, and therefore creator-marked segment skipping.
 * * **Cannot**: select a quality tier (`setPlaybackQuality` is a documented no-op), read buffer
 *   level, or read dropped-frame counts. The controls for those are absent rather than inert.
 *
 * ## The origin risk
 *
 * The embed rejects requests it cannot attribute with error 153. Under a custom `tauri://` scheme
 * there is no HTTP-compliant `Referer` and the embed refuses; on Windows the shell is served from
 * an `http(s)://tauri.localhost` origin, which does emit one. That is why the failure path here is
 * explicit and reports the code rather than showing a blank frame — if 153 ever appears, it names
 * itself.
 */

import { useEffect, useEffectEvent, useRef, useState } from 'react';

import { useTranslation } from '@/i18n/context';
import type { PlaybackState, VideoId } from '@/types/domain';

/** How often the playhead is sampled while playing. */
const POSITION_POLL_MS = 250;

/** Where the player API is loaded from. The only remote script the application loads. */
const IFRAME_API_SRC = 'https://www.youtube.com/iframe_api';

/** How long to wait for the API script before reporting failure. */
const API_LOAD_TIMEOUT_MS = 15_000;

/** The subset of the player API this component uses. */
interface YouTubePlayerInstance {
  playVideo: () => void;
  pauseVideo: () => void;
  seekTo: (seconds: number, allowSeekAhead: boolean) => void;
  getCurrentTime: () => number;
  getDuration: () => number;
  getPlayerState: () => number;
  setVolume: (volume: number) => void;
  mute: () => void;
  unMute: () => void;
  setPlaybackRate: (rate: number) => void;
  loadVideoById: (options: { videoId: string; startSeconds?: number }) => void;
  destroy: () => void;
}

interface PlayerEvent {
  target: YouTubePlayerInstance;
  data: number;
}

interface YouTubeApi {
  Player: new (
    element: HTMLElement,
    options: {
      videoId: string;
      playerVars: Record<string, string | number>;
      events: {
        onReady?: (event: PlayerEvent) => void;
        onStateChange?: (event: PlayerEvent) => void;
        onError?: (event: PlayerEvent) => void;
      };
    },
  ) => YouTubePlayerInstance;
}

declare global {
  interface Window {
    YT?: YouTubeApi;
    onYouTubeIframeAPIReady?: () => void;
  }
}

/**
 * Player states as reported by the embed.
 *
 * The numbers are the API's own; naming them here keeps the mapping in one place instead of
 * scattering magic constants through the state handler.
 */
const EMBED_STATE = {
  unstarted: -1,
  ended: 0,
  playing: 1,
  paused: 2,
  buffering: 3,
  cued: 5,
} as const;

/** Maps an embed state onto the application's own playback state machine. */
function toPlaybackState(embedState: number): PlaybackState {
  switch (embedState) {
    case EMBED_STATE.playing:
      return 'playing';
    case EMBED_STATE.paused:
      return 'paused';
    case EMBED_STATE.buffering:
      return 'buffering';
    case EMBED_STATE.ended:
      return 'ended';
    case EMBED_STATE.cued:
      return 'ready';
    default:
      return 'loading';
  }
}

/**
 * Error codes the embed reports, mapped to i18n keys.
 *
 * 153 is called out specifically: it means the embed could not attribute the request, which under a
 * desktop shell is an origin problem rather than anything about the video.
 */
function errorKeyFor(code: number): string {
  switch (code) {
    case 2:
      return 'error.playback.load_failed';
    case 5:
      return 'error.playback.decode';
    case 100:
      return 'error.provider.not_found';
    case 101:
    case 150:
      return 'error.playback.not_embeddable';
    case 153:
      return 'error.playback.referer_rejected';
    default:
      return 'error.playback.load_failed';
  }
}

/** Loads the IFrame API once per process, resolving when `window.YT` is usable. */
let apiPromise: Promise<YouTubeApi> | null = null;

function loadPlayerApi(): Promise<YouTubeApi> {
  if (apiPromise) return apiPromise;

  apiPromise = new Promise<YouTubeApi>((resolve, reject) => {
    if (window.YT?.Player) {
      resolve(window.YT);
      return;
    }

    const timeout = setTimeout(() => {
      reject(new Error('the player API did not load'));
    }, API_LOAD_TIMEOUT_MS);

    // The API calls this global when it is ready; it is the only supported readiness signal.
    window.onYouTubeIframeAPIReady = () => {
      clearTimeout(timeout);
      if (window.YT?.Player) {
        resolve(window.YT);
      } else {
        reject(new Error('the player API loaded without a Player constructor'));
      }
    };

    const script = document.createElement('script');
    script.src = IFRAME_API_SRC;
    script.async = true;
    script.onerror = () => {
      clearTimeout(timeout);
      reject(new Error('the player API script could not be fetched'));
    };
    document.head.append(script);
  }).catch((cause: unknown) => {
    // Allow a later mount to retry rather than caching the failure for the session.
    apiPromise = null;
    throw cause;
  });

  return apiPromise;
}

export interface YouTubePlayerProps {
  videoId: VideoId;
  /** Where to resume from, in milliseconds. */
  startAtMs?: number;
  /** Begin playing as soon as the player is ready. */
  autoplay?: boolean;
  /** Called on every state transition. */
  onStateChange?: (state: PlaybackState) => void;
  /**
   * Called with the playhead position while playing.
   *
   * Sampled rather than pushed into global state: position changes several times a second, and
   * routing it through a store would re-render the application on every tick (§89).
   */
  onPosition?: (positionMs: number, durationMs: number) => void;
  /** Called when the embed reports a failure, with an i18n key. */
  onError?: (messageKey: string, code: number) => void;
  /**
   * Aspect ratio of the player box, as a CSS `aspect-ratio` value.
   *
   * The default is the landscape frame every ordinary video wants. Shorts pass `9 / 16` so the
   * portrait video fills the column instead of sitting letterboxed inside a landscape box.
   */
  aspectRatio?: string;
  /** Fill the parent's height instead of its width. Used where the parent is height-bounded. */
  fill?: boolean;
  /**
   * Start muted.
   *
   * Required for anything that plays without being asked for — a browser refuses unmuted autoplay,
   * and a grid that starts making noise as the pointer crosses it would be hostile anyway.
   */
  muted?: boolean;
  /** Show the player's own controls. Off for previews, where the card underneath is the control. */
  controls?: boolean;
  /** Loop the video. Used by previews, which are shorter than what they preview. */
  loop?: boolean;
}

/** The embedded player. */
export function YouTubePlayer({
  videoId,
  startAtMs,
  autoplay = true,
  onStateChange,
  onPosition,
  onError,
  aspectRatio = '16 / 9',
  fill = false,
  muted = false,
  controls = true,
  loop = false,
}: YouTubePlayerProps): React.ReactNode {
  const t = useTranslation();
  const containerRef = useRef<HTMLDivElement>(null);
  const playerRef = useRef<YouTubePlayerInstance | null>(null);
  const [failed, setFailed] = useState<string | null>(null);

  // Callbacks are wrapped as effect events so changing one does not tear down and rebuild the
  // player — which would restart playback from the beginning every time a parent re-rendered.
  const reportState = useEffectEvent((state: PlaybackState) => {
    onStateChange?.(state);
  });
  const reportPosition = useEffectEvent((positionMs: number, durationMs: number) => {
    onPosition?.(positionMs, durationMs);
  });
  const reportError = useEffectEvent((messageKey: string, code: number) => {
    onError?.(messageKey, code);
  });

  useEffect(() => {
    let cancelled = false;
    let poll: ReturnType<typeof setInterval> | undefined;

    // Defined inside the effect because it calls an effect event, which React only permits from
    // effects and other effect events.
    const sample = () => {
      const player = playerRef.current;
      if (!player) return;
      try {
        const positionMs = Math.floor(player.getCurrentTime() * 1000);
        const durationMs = Math.floor(player.getDuration() * 1000);
        reportPosition(positionMs, durationMs);
      } catch {
        // The player throws if queried during teardown; a missed sample is not worth reporting.
      }
    };

    void loadPlayerApi()
      .then((api) => {
        if (cancelled || !containerRef.current) return;

        playerRef.current = new api.Player(containerRef.current, {
          videoId,
          playerVars: {
            autoplay: autoplay ? 1 : 0,
            // Required for the API to accept commands from this page.
            enablejsapi: 1,
            // Attributes the embed to this origin. Without it the embed cannot identify the caller
            // and answers with error 153.
            origin: window.location.origin,
            playsinline: 1,
            // Related videos are restricted to the same channel; the API no longer allows
            // suppressing them entirely, so this is the least intrusive setting available.
            rel: 0,
            mute: muted ? 1 : 0,
            controls: controls ? 1 : 0,
            // Keyboard handling belongs to the application, not to a preview embedded in a card.
            disablekb: controls ? 0 : 1,
            // `loop` needs the playlist to name the video itself; without it the parameter is
            // silently ignored, which is a documented quirk of the embed rather than a guess.
            ...(loop ? { loop: 1, playlist: videoId } : {}),
            ...(startAtMs !== undefined ? { start: Math.floor(startAtMs / 1000) } : {}),
          },
          events: {
            onReady: () => {
              if (cancelled) return;
              reportState('ready');
              poll = setInterval(sample, POSITION_POLL_MS);
            },
            onStateChange: (event) => {
              if (cancelled) return;
              reportState(toPlaybackState(event.data));
              // Sample immediately on transition so a pause records its exact position rather than
              // waiting up to a poll interval.
              sample();
            },
            onError: (event) => {
              if (cancelled) return;
              const key = errorKeyFor(event.data);
              setFailed(key);
              reportState('error');
              reportError(key, event.data);
            },
          },
        });
      })
      .catch(() => {
        if (cancelled) return;
        setFailed('error.playback.load_failed');
        reportState('error');
      });

    return () => {
      cancelled = true;
      if (poll !== undefined) clearInterval(poll);
      try {
        playerRef.current?.destroy();
      } catch {
        // Destroying an already-torn-down player throws; nothing to recover.
      }
      playerRef.current = null;
    };
  }, [videoId, autoplay, startAtMs, muted, controls, loop]);

  if (failed !== null) {
    return (
      <div
        className={`bg-surface flex flex-col items-center justify-center gap-3 rounded-lg text-center ${
          fill ? 'size-full' : ''
        }`}
        style={fill ? undefined : { aspectRatio }}
        role="alert"
      >
        <p className="text-text text-base font-medium">
          {t.t(failed as Parameters<typeof t.t>[0])}
        </p>
      </div>
    );
  }

  return (
    <div
      className={`relative overflow-hidden rounded-lg bg-black ${fill ? 'size-full' : 'w-full'}`}
      style={fill ? undefined : { aspectRatio }}
      aria-label={t.t('a11y.playerRegion')}
    >
      {/* The API replaces this element with its iframe. */}
      <div ref={containerRef} className="size-full" />
    </div>
  );
}
