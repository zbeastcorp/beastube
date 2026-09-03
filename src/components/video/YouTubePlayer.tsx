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

import { useEffect, useEffectEvent, useImperativeHandle, useRef, useState, type Ref } from 'react';

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
  /** Names of the option modules the player currently has, e.g. `captions`. */
  getOptions: () => string[];
  loadModule: (module: string) => void;
  unloadModule: (module: string) => void;
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
      host?: string;
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
  /**
   * Leave the frame's own background unpainted.
   *
   * The embed is opaque once it paints, so this only shows before that — which is exactly when a
   * caller that has something better to show there, like a blurred poster frame, wants it visible
   * instead of a black rectangle.
   */
  transparent?: boolean;
  /**
   * Handle for driving the player from outside.
   *
   * Exists so a surface that hides the embed's own chrome can offer real controls in its place. A
   * control that cannot act on the player would be exactly the kind of decoration the specification
   * forbids (§131), so the commands are the same ones the API actually supports and nothing more.
   */
  ref?: Ref<PlayerHandle>;
}

/** What a caller can ask the live player to do. */
export interface PlayerHandle {
  play: () => void;
  pause: () => void;
  /** Plays if paused, pauses if playing. Reads the live state rather than trusting a cached one. */
  toggle: () => void;
  setMuted: (muted: boolean) => void;
  /** Sets the volume, `0..100`, as the embed expresses it. */
  setVolume: (volume: number) => void;
  /** Puts the player's frame into fullscreen, when the browser allows it. */
  requestFullscreen: () => void;
  /**
   * Whether this video actually has captions.
   *
   * Asked of the player rather than assumed, so a caption control can be absent for a video that
   * has none instead of present and inert (§131).
   */
  hasCaptions: () => boolean;
  setCaptions: (enabled: boolean) => void;
}

/**
 * The embedded player.
 *
 * ## Built once, pointed at many videos
 *
 * The expensive thing here is not this component — it is the `<iframe>` the API creates, which is a
 * whole embed document with its own bootstrap and media pipeline. Rebuilding it is what a viewer
 * experiences as the stutter between two shorts, or as the pause before a hover preview appears.
 *
 * So construction and video selection are separate effects. Effect 1 builds the player once per
 * mount and tears it down once per unmount. Effect 2 watches `videoId` alone and calls
 * `loadVideoById`, which swaps the media inside the *existing* iframe. Mute and resume are the same
 * shape: a live command, not a rebuild.
 *
 * Everything reactive is read through `useEffectEvent`, which always sees the latest render's
 * values and is excluded from dependency arrays. That is what lets the construction effect declare
 * `[controls, loop]` — the two genuinely construction-time player vars — without dragging `videoId`
 * or `startAtMs` in with them.
 *
 * ## The sacrificial mount node
 *
 * The API *replaces* the element it is handed. Handing it the element React owns leaves React's ref
 * pointing at a detached node, so a second construction would build into a node outside the
 * document and React's unmount would try to remove a child the API had already deleted. The fix is
 * one line of ownership: React keeps its own `<div>`, and each construction appends a throwaway
 * child for the API to consume.
 */
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
  transparent = false,
  ref,
}: YouTubePlayerProps): React.ReactNode {
  const t = useTranslation();
  const containerRef = useRef<HTMLDivElement>(null);
  const playerRef = useRef<YouTubePlayerInstance | null>(null);

  /** Which video the live player currently holds, so a redundant swap is skipped. */
  const loadedIdRef = useRef<VideoId | null>(null);

  /** A resume position that arrived before the player was ready, drained by `onReady`. */
  const pendingResumeRef = useRef<number | null>(null);

  /**
   * The failure, tagged with the video it belongs to.
   *
   * Tagged rather than a bare key because the player now outlives a video: an unembeddable video
   * would otherwise leave its error on screen for every video swapped in after it.
   */
  const [failure, setFailure] = useState<{ id: VideoId; key: string } | null>(null);

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

  /**
   * Reads the playhead and reports it.
   *
   * An effect event rather than a closure inside the construction effect: that is what removes the
   * last reactive read from the effect body, which is what lets its dependency list shrink to the
   * two construction-time vars.
   */
  const samplePosition = useEffectEvent(() => {
    const player = playerRef.current;
    if (!player) return;
    try {
      reportPosition(
        Math.floor(player.getCurrentTime() * 1000),
        Math.floor(player.getDuration() * 1000),
      );
    } catch {
      // The player throws if queried during teardown; a missed sample is not worth reporting.
    }
  });

  /**
   * Constructs the player against the current render's props.
   *
   * `isCancelled` is passed in rather than read here: it belongs to one particular run of the
   * construction effect, and an effect event has no way to know which run is asking.
   */
  const createPlayer = useEffectEvent(
    (
      api: YouTubeApi,
      mount: HTMLElement,
      isCancelled: () => boolean,
      startPoll: (timer: ReturnType<typeof setInterval>) => void,
    ): YouTubePlayerInstance => {
      const id = videoId;
      loadedIdRef.current = id;

      return new api.Player(mount, {
        // The privacy-preserving host, which the API accepts as a first-class option. Nothing is
        // stored against the viewer until they actually play something — and the hover preview,
        // which has always used this host through a plain iframe, never shows the title bar the
        // default host paints over the top of the picture.
        host: 'https://www.youtube-nocookie.com',
        videoId: id,
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
          // Reduces the chrome the embed paints over the video. It does not remove the title bar
          // the embed fades in at the start of playback — no parameter does, `showinfo` was
          // withdrawn — but it is the most the supported surface offers.
          modestbranding: 1,
          // No annotation or card overlays on top of the picture.
          iv_load_policy: 3,
          mute: muted ? 1 : 0,
          controls: controls ? 1 : 0,
          // Keyboard handling belongs to the application, not to a preview embedded in a card.
          disablekb: controls ? 0 : 1,
          // `loop` needs the playlist to name the video itself; without it the parameter is
          // silently ignored, which is a documented quirk of the embed rather than a guess.
          ...(loop ? { loop: 1, playlist: id } : {}),
          ...(startAtMs !== undefined ? { start: Math.floor(startAtMs / 1000) } : {}),
        },
        events: {
          onReady: () => {
            if (isCancelled()) return;
            reportState('ready');

            // A resume that arrived while the API was still loading is applied now rather than
            // dropped — the position request and the script fetch race, and either can win.
            const pending = pendingResumeRef.current;
            pendingResumeRef.current = null;
            if (pending !== null) {
              try {
                playerRef.current?.seekTo(pending / 1000, true);
              } catch {
                // A seek before the media is cued throws; the start var already covers this case.
              }
            }

            startPoll(setInterval(samplePosition, POSITION_POLL_MS));
          },
          onStateChange: (event) => {
            if (isCancelled()) return;
            reportState(toPlaybackState(event.data));
            // Sample immediately on transition so a pause records its exact position rather than
            // waiting up to a poll interval.
            samplePosition();
          },
          onError: (event) => {
            if (isCancelled()) return;
            const key = errorKeyFor(event.data);
            setFailure({ id: loadedIdRef.current ?? id, key });
            reportState('error');
            reportError(key, event.data);
          },
        },
      });
    },
  );

  /**
   * Points the live player at a different video.
   *
   * The two guards carry the whole correctness argument. `loadedIdRef` makes the run immediately
   * after construction a no-op, because construction already loaded that video. A null player makes
   * a change during API load a no-op — which is safe precisely because `createPlayer` reads the
   * *current* `videoId`, so whichever of the two paths wins, the player lands on the latest one.
   */
  const swapVideo = useEffectEvent((id: VideoId) => {
    const player = playerRef.current;
    if (!player || loadedIdRef.current === id) return;
    loadedIdRef.current = id;
    pendingResumeRef.current = null;
    try {
      // Deliberately no `startSeconds`. At the instant `videoId` changes, a resume position fetched
      // for the *previous* video is still the newest settled value the parent holds — the resource
      // hook retains data across a key change on purpose — so honouring it here would start the new
      // video at the old one's timestamp. The resume effect applies the right position once it
      // actually belongs to this video.
      player.loadVideoById({ videoId: id });
      if (!autoplay) player.pauseVideo();
    } catch {
      // A swap during teardown throws; the player is going away regardless.
    }
  });

  /** Records a failure against whichever video the player is currently holding. */
  const reportLoadFailure = useEffectEvent(() => {
    setFailure({ id: loadedIdRef.current ?? videoId, key: 'error.playback.load_failed' });
    reportState('error');
  });

  const applyMuted = useEffectEvent((next: boolean) => {
    const player = playerRef.current;
    if (!player) return;
    try {
      if (next) player.mute();
      else player.unMute();
    } catch {
      // Same teardown race as every other live command.
    }
  });

  const applyResume = useEffectEvent((ms: number | undefined) => {
    if (ms === undefined) return;
    const player = playerRef.current;
    if (!player) {
      // Held for `onReady`, which is the first moment a seek can land.
      pendingResumeRef.current = ms;
      return;
    }
    try {
      player.seekTo(ms / 1000, true);
    } catch {
      pendingResumeRef.current = ms;
    }
  });

  // Effect 1 — construct and destroy. `controls` and `loop` are the only genuinely
  // construction-time vars: one changes the embed's chrome and the other needs the video named in a
  // playlist parameter, and neither has a live command. Everything else is applied by the effects
  // below, so in practice this runs exactly once per mount.
  useEffect(() => {
    let cancelled = false;
    let poll: ReturnType<typeof setInterval> | undefined;
    const isCancelled = () => cancelled;
    // Captured now rather than read in the cleanup: by teardown the ref may already point somewhere
    // else, and the node this run appended its player into is the one that must be emptied.
    const host = containerRef.current;

    void loadPlayerApi()
      .then((api) => {
        if (cancelled || !host) return;
        // The throwaway node the API is allowed to replace; React never sees it.
        const mount = document.createElement('div');
        mount.className = 'size-full';
        host.append(mount);
        playerRef.current = createPlayer(api, mount, isCancelled, (timer) => {
          poll = timer;
        });
      })
      .catch(() => {
        if (cancelled) return;
        reportLoadFailure();
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
      loadedIdRef.current = null;
      // `mount` is already detached by this point — the API replaced it — so removing it does
      // nothing. The live node is the iframe the API left inside our own container, and a
      // `destroy()` that threw leaves it there to be stacked under by the next construction.
      // Emptying the container is what actually guarantees a clean slate.
      host?.replaceChildren();
    };
  }, [controls, loop]);

  // Effect 2 — swap. This is the whole point: a new video costs one API call, not a new iframe.
  useEffect(() => {
    swapVideo(videoId);
  }, [videoId]);

  // Effect 3 — mute. No call site changes it live today; leaving it in the construction deps would
  // make the first mute control anyone adds rebuild the player.
  useEffect(() => {
    applyMuted(muted);
  }, [muted]);

  // Effect 4 — resume. WatchView resolves a stored position asynchronously, so `startAtMs` almost
  // always arrives *after* the first render; rebuilding on it restarted playback from zero on every
  // partly-watched video, which is the default configuration.
  useEffect(() => {
    applyResume(startAtMs);
  }, [startAtMs]);

  const frameRef = useRef<HTMLDivElement>(null);

  /**
   * The name the embed uses for its caption module, or `null` when this video has none.
   *
   * The name differs between the two player builds, so both are checked rather than one guessed.
   */
  const captionModule = (): string | null => {
    try {
      const modules = playerRef.current?.getOptions() ?? [];
      if (modules.includes('captions')) return 'captions';
      if (modules.includes('cc')) return 'cc';
      return null;
    } catch {
      return null;
    }
  };

  useImperativeHandle(
    ref,
    (): PlayerHandle => ({
      play: () => {
        try {
          playerRef.current?.playVideo();
        } catch {
          // The player throws once torn down; a command with nothing to command is a no-op.
        }
      },
      pause: () => {
        try {
          playerRef.current?.pauseVideo();
        } catch {
          // As above.
        }
      },
      toggle: () => {
        const player = playerRef.current;
        if (!player) return;
        try {
          // Asked of the player rather than derived from the last reported state: a state change
          // the parent has not re-rendered for yet would otherwise invert the button.
          if (player.getPlayerState() === EMBED_STATE.playing) player.pauseVideo();
          else player.playVideo();
        } catch {
          // As above.
        }
      },
      setMuted: (next: boolean) => {
        const player = playerRef.current;
        if (!player) return;
        try {
          if (next) player.mute();
          else player.unMute();
        } catch {
          // As above.
        }
      },
      setVolume: (volume: number) => {
        const player = playerRef.current;
        if (!player) return;
        try {
          player.setVolume(Math.max(0, Math.min(100, volume)));
        } catch {
          // As above.
        }
      },
      hasCaptions: () => captionModule() !== null,
      setCaptions: (enabled: boolean) => {
        const player = playerRef.current;
        const module = captionModule();
        if (!player || module === null) return;
        try {
          // Unloading is how the embed turns captions off; there is no `setEnabled`.
          if (enabled) player.loadModule(module);
          else player.unloadModule(module);
        } catch {
          // As above.
        }
      },
      requestFullscreen: () => {
        void frameRef.current?.requestFullscreen().catch(() => {
          // Refused when the gesture is not trusted, or unavailable in this context.
        });
      },
    }),
    [],
  );

  // A failure belongs to the video that produced it. Once a different video is loaded the frame is
  // live again, so the error must not outlive its subject.
  const failed = failure !== null && failure.id === videoId ? failure.key : null;

  return (
    <div
      ref={frameRef}
      // No rounding of its own when filling a parent: the parent is doing the clipping, and two
      // radii on top of each other leave a hairline seam at the corners.
      className={`relative overflow-hidden ${fill ? 'size-full' : 'w-full rounded-lg'} ${
        transparent ? '' : 'bg-black'
      }`}
      style={fill ? undefined : { aspectRatio }}
      aria-label={t.t('a11y.playerRegion')}
    >
      {/* React owns this element. The API replaces a throwaway child appended inside it. */}
      <div ref={containerRef} className="size-full" />

      {/* The failure is painted OVER the container rather than instead of it. Returning early here
          would unmount the container, and since the construction effect runs once per mount there
          would be nothing left to rebuild into — one unembeddable video would black out the player
          for every video after it. */}
      {failed !== null && (
        <div
          className="bg-surface absolute inset-0 flex flex-col items-center justify-center gap-3 rounded-lg text-center"
          role="alert"
        >
          <p className="text-text text-base font-medium">
            {t.t(failed as Parameters<typeof t.t>[0])}
          </p>
        </div>
      )}
    </div>
  );
}
