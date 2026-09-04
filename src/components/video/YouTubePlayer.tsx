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
 *   reporting, therefore creator-marked segment skipping — and selecting a quality tier, which
 *   takes some explaining; see below.
 * * **Cannot**: read buffer level or dropped-frame counts. The embed exposes neither, so the
 *   readouts for them are absent rather than inert.
 *
 * ## Quality is selectable, by size rather than by command
 *
 * `setPlaybackQuality` really is a no-op — measured, not assumed: calling it with `hd1080` on a
 * player showing `hd720` leaves it on `hd720`. What is *not* a no-op is the size of the frame. The
 * embed measures its own viewport and picks the rendition to match, and it keeps doing so while
 * playing: a frame relaid from 640×360 to 3840×2160 moves from `medium` to `hd2160` within a few
 * seconds, with no reload and no rebuffer, and back down again just as readily.
 *
 * So a tier is requested by laying the frame out at the width that produces it and scaling the
 * result down to the box the design wants. `getPlaybackQuality` and `getAvailableQualityLevels`
 * both work and both report honestly, which is what makes the result checkable rather than hoped
 * for. See `renderSize` and ADR-0004.
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
import type { PlaybackState, Quality, VideoId } from '@/types/domain';

/** How often the playhead is sampled while playing. */
const POSITION_POLL_MS = 250;

/** Where the player API is loaded from. The only remote script the application loads. */
const IFRAME_API_SRC = 'https://www.youtube.com/iframe_api';

/** How long to wait for the API script before reporting failure. */
const API_LOAD_TIMEOUT_MS = 15_000;

/**
 * The frame width, in pixels, that makes the embed choose a given tier.
 *
 * The embed selects its rendition from the size of its own viewport, which it measures rather than
 * being told — so these widths *are* the quality control. Each is the width of a 16:9 frame whose
 * picture is that many lines tall, because the embed fits the picture to the width and letterboxes
 * whatever height is left over.
 *
 * `2160p` tops the ladder deliberately. The embed also names a `highres` level above it, which is
 * not offered: honouring it would mean laying the frame out at 7680 pixels, and a tier the player
 * would answer at 4K is the kind of control that lies about what it did (§131).
 */
const QUALITY_WIDTH: Record<Exclude<Quality, 'auto'>, number> = {
  '2160p': 3840,
  '1440p': 2560,
  '1080p': 1920,
  '720p': 1280,
  '480p': 854,
  '360p': 640,
  '240p': 426,
  '144p': 256,
};

/** The embed's own names for the tiers, mapped onto the vocabulary the rest of the app uses. */
const EMBED_LEVEL: Record<string, Exclude<Quality, 'auto'>> = {
  hd2160: '2160p',
  hd1440: '1440p',
  hd1080: '1080p',
  hd720: '720p',
  large: '480p',
  medium: '360p',
  small: '240p',
  tiny: '144p',
};

/**
 * The tiers YouTube encodes at 60fps, when a video has 60fps at all.
 *
 * Not a guess about this video — a fact about the ladder: YouTube produces high-frame-rate
 * renditions from 720p upward and nothing below it. Paired with an observation that *this* video is
 * playing at high frame rate, it is what lets the menu label `1080p60` rather than `1080p`.
 */
const HIGH_FRAME_RATE_TIERS: readonly Quality[] = ['2160p', '1440p', '1080p', '720p'];

/**
 * How a tier reads in a menu.
 *
 * The `60` suffix is appended only once the player has actually reported `hfr` for the video in
 * hand, so it states something observed rather than something assumed (§131).
 */
export function qualityLabel(tier: Quality, highFrameRate: boolean): string {
  if (tier === 'auto') return tier;
  return highFrameRate && HIGH_FRAME_RATE_TIERS.includes(tier) ? `${tier}60` : tier;
}

/**
 * The tiers a menu may offer, best first.
 *
 * It stops at 360p, and that is a measured limit rather than a preference. Quality is requested by
 * shrinking the frame, and the embed simply refuses to go below 360p however small the frame gets:
 * a player laid out at 120 pixels still serves `medium`. The two setters that could ask for less —
 * `setPlaybackQuality` and the internal `setPlaybackQualityRange` that YouTube's own menu uses —
 * are both refused over the parent's command channel, and the `vq` load parameter is ignored.
 *
 * `240p` and `1440p`-style entries below the floor are therefore absent rather than present and
 * inert: offering `144p` would serve 360p and call it 144p, which is exactly the fake feature the
 * specification forbids (§131). `EMBED_LEVEL` still names them, so a rendition the embed reports
 * from below the floor is still reported honestly if it ever appears.
 */
const QUALITY_ORDER: readonly Exclude<Quality, 'auto'>[] = [
  '2160p',
  '1440p',
  '1080p',
  '720p',
  '480p',
  '360p',
];

/**
 * The narrowest frame that loads into YouTube's 60fps track family.
 *
 * The embed settles on a 30fps or 60fps family when a video *loads* and keeps it for that load,
 * while resolution goes on following the frame size for as long as the video plays. YouTube encodes
 * 60fps from 720p up, so a video loaded into a frame whose picture is shorter than 720 lines lands
 * in the 30fps family — and then climbs to 2160p at 30fps and stays there. Measured in the
 * application: a 1050-wide player carries a 590-line picture, loaded at 30fps, and reached `hd2160`
 * with no `hfr` however large the frame was afterwards.
 *
 * So every load is bootstrapped at this width and the frame settles to its real size once playback
 * has started. 1280 is the 16:9 width whose picture is exactly 720 lines.
 */
const HIGH_FRAME_RATE_MIN_WIDTH = 1280;

/**
 * How long the bootstrap frame is held after playback starts, in milliseconds.
 *
 * Long enough for the embed to report the rendition it chose. The frame rate of a video is only
 * knowable by watching a rendition that carries it, and the bootstrap window is the one moment a
 * 60fps rendition is guaranteed to be in flight — so settling the instant `playing` fires means a
 * player whose real box is small never observes it, and a 60fps video is indistinguishable from a
 * 30fps one. Held briefly, the observation is reliable and the menu can say `1080p60` because it
 * has seen 60, rather than because 1080p usually is.
 */
const SETTLE_DELAY_MS = 4000;

/** Bounds on the laid-out frame. The floor is what an unmeasured element reports; the ceiling is 4K. */
const MIN_RENDER_WIDTH = 320;
const MAX_RENDER_WIDTH = 3840;
const MIN_RENDER_HEIGHT = 180;

/** How far the wanted width may drift before the frame is actually re-laid-out. */
const RESIZE_THRESHOLD_PX = 24;

/**
 * The frame width `auto` should ask for, given the pixels the picture is painted across.
 *
 * Rounds *up* to the next tier rather than passing the raw measurement through, because a tier is
 * a ceiling and the embed picks the largest one that fits. A 1050-pixel-wide player carries a
 * 590-line picture; asked for 1050 the embed serves 480p and the browser upscales it by a quarter,
 * which is precisely the "the player is blurry" complaint. Rounding up asks for 720p, which is
 * what YouTube's own player does at that size and what the ladder exists for.
 *
 * Never above `ceiling`, which is the viewer's `max_quality` setting.
 */
function autoWidth(measured: number, ceiling: number): number {
  const fits = [...QUALITY_ORDER].reverse().find((tier) => QUALITY_WIDTH[tier] >= measured);
  const rounded = fits === undefined ? MAX_RENDER_WIDTH : QUALITY_WIDTH[fits];
  return Math.min(rounded, ceiling);
}

/**
 * How large to lay the embed's frame out, in pixels, for a given quality.
 *
 * This is the whole quality mechanism, and it is worth being precise about why it is a *layout*
 * size rather than the `width`/`height` the API accepts. Those are attributes, and the iframe the
 * API builds inherits the mount node's class — so a stylesheet rule sized it and the attributes
 * never applied. Measured: an iframe constructed at 1800 and styled `width: 100%` inside a 900px
 * box reports `offsetWidth` of 900, and the embed serves the rendition for 900.
 *
 * What the embed actually reads is its own viewport, which is this element's layout box. So asking
 * for 2160p means genuinely laying the frame out 3840 pixels wide and scaling the result down to
 * the box the design wants — see the `scaler` element in the markup below.
 *
 * Width drives and height follows the frame's own proportions. Driving from height would aim the
 * tier at the *box* rather than at the picture, and land a tier low wherever the box is taller
 * than 16:9 — which is the normal case here, since the host over-sizes the frame to crop the
 * embed's chrome away.
 *
 * `auto` asks for the box's size in device pixels, because that is what the picture is finally
 * resampled to: on a 150% display a player 800 CSS pixels wide is painted across 1200 real ones,
 * and a rendition chosen for 800 is visibly soft there.
 */
function renderSize(
  rect: { width: number; height: number },
  quality: Quality,
  maxAuto: Quality,
): { width: number; height: number } {
  const cssWidth = rect.width || 640;
  const cssHeight = rect.height || 360;
  const ratio = typeof window === 'undefined' ? 1 : Math.min(window.devicePixelRatio || 1, 2);
  // The ceiling bounds the *frame*, which is the only thing the embed reads — so a capped `auto`
  // is capped in fact and not merely in the menu.
  const ceiling = maxAuto === 'auto' ? MAX_RENDER_WIDTH : QUALITY_WIDTH[maxAuto];
  const wanted = quality === 'auto' ? autoWidth(cssWidth * ratio, ceiling) : QUALITY_WIDTH[quality];
  const width = Math.min(MAX_RENDER_WIDTH, Math.max(MIN_RENDER_WIDTH, Math.round(wanted)));
  return {
    width,
    height: Math.max(MIN_RENDER_HEIGHT, Math.round(width * (cssHeight / cssWidth))),
  };
}

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
  getPlaybackRate: () => number;
  /** The rates this player will actually accept. Asked rather than assumed. */
  getAvailablePlaybackRates: () => number[];
  loadVideoById: (options: { videoId: string; startSeconds?: number }) => void;
  /**
   * Writes the `<iframe>`'s `width` and `height` attributes.
   *
   * Bookkeeping, not the quality lever. The iframe inherits the mount node's class, so a
   * stylesheet rule wins over these attributes and the embed measures the styled box instead —
   * which is what `renderSize` sets. This is called anyway so the attributes never disagree with
   * the layout for anyone reading the DOM.
   */
  setSize: (width: number, height: number) => void;
  /** The tier the embed is currently serving, in its own vocabulary. */
  getPlaybackQuality: () => string;
  /**
   * Metadata about what is playing.
   *
   * Read for `video_quality_features`, which carries `hfr` when the rendition in flight is a
   * high-frame-rate one. It is the only signal the embed gives about frame rate.
   */
  getVideoData: () => { video_quality_features?: string[] };
  /**
   * The tiers this video actually has, in the embed's vocabulary.
   *
   * Genuinely per-video rather than a fixed ladder — measured: a 240p-era upload answers with
   * `["small", "auto"]` and nothing else. That is what lets a quality menu offer only tiers that
   * exist for the video in front of the viewer (§131).
   */
  getAvailableQualityLevels: () => string[];
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
      width?: number;
      height?: number;
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

/**
 * Fetches the IFrame API before anything needs it.
 *
 * Called once at launch. The API is a cross-origin script that has to be fetched, parsed and
 * executed before the first player can be constructed, and doing that on the first click means the
 * viewer waits for it with nothing on screen. Started at launch, it is almost always resolved by
 * the time a video is opened, and `loadPlayerApi` then returns an already-settled promise.
 *
 * Failure is swallowed: this is speculative, and a real mount reports its own failure through the
 * same shared promise.
 */
export function preloadPlayerApi(): void {
  void loadPlayerApi().catch(() => {
    // Speculative; a real player mount surfaces the failure.
  });
}

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
  /**
   * Called when playback state changes, with the video the change belongs to.
   *
   * The id is not decoration. One player serves a whole feed, and an event queued for the outgoing
   * video can be delivered after the caller has already re-rendered around the incoming one — so a
   * handler that assumed "the current one" would record the *previous* short as having started.
   * `null` only before any video has been loaded.
   */
  onStateChange?: (state: PlaybackState, videoId: VideoId | null) => void;
  /**
   * Called with the playhead position while playing.
   *
   * Sampled rather than pushed into global state: position changes several times a second, and
   * routing it through a store would re-render the application on every tick (§89).
   */
  onPosition?: (positionMs: number, durationMs: number) => void;
  /**
   * Called when the embed reports a failure, with an i18n key and the video it applies to.
   *
   * The video id is not decoration. One player serves a whole feed, so by the time an error is
   * delivered the caller may already have moved on — a handler that assumed "the current one"
   * would blame a perfectly good video for its predecessor's failure.
   */
  onError?: (messageKey: string, code: number, videoId: VideoId) => void;
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
  /**
   * The tier to ask the embed for, or `auto` to let it choose for the size it is displayed at.
   *
   * Applied by relaying the frame out, not by a command — see `renderSize`. A tier the video does
   * not have is simply not offered by the menu that sets this; if one arrives anyway the embed
   * serves the closest it has, which is the same thing it does for `auto`.
   */
  quality?: Quality;
  /**
   * A ceiling on what `auto` may climb to.
   *
   * The viewer's `max_quality` setting, whose stated purpose is exactly this: stopping automatic
   * selection reaching 4K on a metered connection. It bounds `auto` only — an explicit `quality`
   * is a deliberate choice and is honoured as given.
   */
  maxAutoQuality?: Quality;
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
  /** Moves the playhead, in milliseconds from the start. */
  seek: (positionMs: number) => void;
  /**
   * Playback speed, and the speeds on offer.
   *
   * These are asked of the player rather than hardcoded, so the menu can never list a rate the
   * player would refuse. Unlike quality — which the IFrame API documents as a no-op — speed is
   * genuinely supported, which is why it is the one thing the settings menu can actually change.
   */
  rate: () => number;
  availableRates: () => number[];
  setRate: (rate: number) => void;
  /**
   * The tiers this video actually offers, best first, and the one being served right now.
   *
   * Both are asked of the player rather than assumed, so a menu can list exactly what exists and
   * report what the request actually achieved — which matters because a tier is requested by
   * resizing rather than commanded, and the embed takes a few seconds to move.
   */
  availableQualities: () => Quality[];
  currentQuality: () => Quality | null;
  /**
   * Whether the rendition in flight is a high-frame-rate one.
   *
   * A property of what is playing, not of the video: a 60fps upload reports `false` while its 360p
   * rendition is on screen, because that rendition genuinely is 30fps.
   */
  isHighFrameRate: () => boolean;
  setMuted: (muted: boolean) => void;
  /** Sets the volume, `0..100`, as the embed expresses it. */
  setVolume: (volume: number) => void;
  /** Puts the player's frame into fullscreen, when the browser allows it. */
  requestFullscreen: () => void;
  /**
   * Leaves fullscreen.
   *
   * Its own command rather than a toggle, because the caller already tracks the state to draw the
   * right icon — and a toggle that disagreed with that icon would be worse than two commands.
   */
  exitFullscreen: () => void;
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
  quality = 'auto',
  maxAutoQuality = 'auto',
  ref,
}: YouTubePlayerProps): React.ReactNode {
  const t = useTranslation();
  const frameRef = useRef<HTMLDivElement>(null);
  const scalerRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const playerRef = useRef<YouTubePlayerInstance | null>(null);

  /**
   * The size the frame is currently laid out at, and the quality that chose it.
   *
   * The width is sticky: for a fixed tier it never moves, and for `auto` it only moves once the
   * box has drifted far enough to change the rendition. Relaying the frame reaches into the
   * embed's document, so a resize drag must not do it on every pointer move. The *height* and the
   * scale are recomputed every time regardless, because those are what keep the picture filling
   * its box — a stale one would show as a gap.
   */
  const layoutRef = useRef<{ width: number; height: number; quality: Quality }>({
    width: 0,
    height: 0,
    quality: 'auto',
  });

  /** Which video the live player currently holds, so a redundant swap is skipped. */
  const loadedIdRef = useRef<VideoId | null>(null);

  /** A resume position that arrived before the player was ready, drained by `onReady`. */
  const pendingResumeRef = useRef<number | null>(null);

  /** The pending drop from the load-time frame back to the viewer's own size. */
  const settleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  /** The last tier acted on, so the reload fires on a change rather than on mount. */
  const pickedRef = useRef<Quality>(quality);

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
    // `loadedIdRef` is set synchronously by `swapVideo`, before `loadVideoById` is issued, so it is
    // the player's own view of which video an event belongs to — and during the window between a
    // re-render and the swap effect that follows it, that is the *old* video, which is exactly the
    // case this exists to distinguish.
    onStateChange?.(state, loadedIdRef.current);
  });
  const reportPosition = useEffectEvent((positionMs: number, durationMs: number) => {
    onPosition?.(positionMs, durationMs);
  });
  const reportError = useEffectEvent((messageKey: string, code: number, id: VideoId) => {
    onError?.(messageKey, code, id);
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

  /** Whether the caller wants playback to begin by itself. Read live, not captured. */
  const autoplayNow = useEffectEvent(() => autoplay);

  /**
   * Lays the frame out at the size the current quality asks for, and scales it back into its box.
   *
   * The two halves are the whole trick. The `scaler` is given a real pixel size — 3840 wide for
   * 2160p — so the embed inside it has a 4K viewport and serves a 4K rendition. It is then scaled
   * by `box width / render width`, which puts it back exactly where the design wanted it. A
   * transform does not touch the transformed element's own layout, so the embed keeps measuring
   * the large box while the viewer sees the small one.
   *
   * Because the scale is derived from the same width the layout used, the picture lands on the box
   * to the pixel at any tier — including the host's deliberately over-tall frame, whose letterbox
   * bars scale down to exactly the chrome crop they are there to hide.
   */
  const applyLayout = useEffectEvent((minWidth = 0) => {
    const frame = frameRef.current;
    const scaler = scalerRef.current;
    if (!frame || !scaler) return;

    const rect = frame.getBoundingClientRect();
    // Nothing useful to measure yet; the observer fires again once the box has a size.
    if (rect.width <= 0 || rect.height <= 0) return;

    const wanted = renderSize(rect, quality, maxAutoQuality);
    const settled = layoutRef.current;
    const stable =
      settled.width === 0 ||
      settled.quality !== quality ||
      Math.abs(wanted.width - settled.width) >= RESIZE_THRESHOLD_PX
        ? wanted.width
        : settled.width;
    // `minWidth` is the load-time bootstrap, never a permanent floor: the next call settles back.
    const width = Math.min(MAX_RENDER_WIDTH, Math.max(stable, minWidth));
    // Follows the box's proportions against whichever width survived, so the scale below fits both
    // axes with one factor.
    const height = Math.max(MIN_RENDER_HEIGHT, Math.round(width * (rect.height / rect.width)));

    // The bootstrap width is deliberately not recorded, so the settle that follows sees the size
    // the viewer actually asked for rather than the one the load needed.
    if (minWidth === 0) layoutRef.current = { width, height, quality };
    scaler.style.width = `${String(width)}px`;
    scaler.style.height = `${String(height)}px`;
    scaler.style.transform = `scale(${String(rect.width / width)})`;

    try {
      playerRef.current?.setSize(width, height);
    } catch {
      // Attribute bookkeeping only, and the player is mid-teardown. The layout above is what counts.
    }
  });

  /**
   * Drops the frame from its load-time width back to the one the viewer asked for.
   *
   * Deferred rather than immediate, and re-armed on every call so a burst of state changes settles
   * once. See `SETTLE_DELAY_MS` for why the delay exists at all.
   */
  const scheduleSettle = useEffectEvent(() => {
    if (settleTimerRef.current !== null) clearTimeout(settleTimerRef.current);
    settleTimerRef.current = setTimeout(() => {
      settleTimerRef.current = null;
      applyLayout();
    }, SETTLE_DELAY_MS);
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

      // Already settled by the `applyLayout` call the construction effect makes before this runs,
      // so the frame the API builds into is the right size from its very first measurement.
      const size = layoutRef.current;

      return new api.Player(mount, {
        // The privacy-preserving host, which the API accepts as a first-class option. Nothing is
        // stored against the viewer until they actually play something — and the hover preview,
        // which has always used this host through a plain iframe, never shows the title bar the
        // default host paints over the top of the picture.
        host: 'https://www.youtube-nocookie.com',
        videoId: id,
        // The attributes, matching the layout `applyLayout` has already set. The layout is what
        // the embed measures — see `renderSize` — but starting the attributes anywhere else would
        // leave the DOM describing a player that does not exist.
        width: size.width,
        height: size.height,
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

            // Asked explicitly rather than trusting the `autoplay` player var. The embed honours it
            // inconsistently once the player is constructed programmatically, and a short that does
            // not start shows the embed's poster chrome — its title bar and a play button — which
            // is the single most visible defect on the Shorts surface.
            if (autoplayNow()) {
              try {
                playerRef.current?.playVideo();
              } catch {
                // Nothing to play yet; the state handler will not report `playing` either.
              }
            }

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
            // Playing means the format selection for this load is committed, so the frame can drop
            // back to the size the viewer actually asked for without changing the frame rate — but
            // not instantly; see `SETTLE_DELAY_MS`.
            if (event.data === EMBED_STATE.playing) scheduleSettle();
            reportState(toPlaybackState(event.data));
            // Sample immediately on transition so a pause records its exact position rather than
            // waiting up to a poll interval.
            samplePosition();
          },
          onError: (event) => {
            if (isCancelled()) return;
            const key = errorKeyFor(event.data);
            const failed = loadedIdRef.current ?? id;
            setFailure({ id: failed, key });
            reportState('error');
            reportError(key, event.data, failed);
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
    // A swap is a load, and the frame-rate family is chosen per load — so the frame is widened for
    // it exactly as it is at construction, and settles again once the new video is playing.
    applyLayout(HIGH_FRAME_RATE_MIN_WIDTH);
    try {
      // Deliberately no `startSeconds`. At the instant `videoId` changes, a resume position fetched
      // for the *previous* video is still the newest settled value the parent holds — the resource
      // hook retains data across a key change on purpose — so honouring it here would start the new
      // video at the old one's timestamp. The resume effect applies the right position once it
      // actually belongs to this video.
      player.loadVideoById({ videoId: id });
      // `loadVideoById` is documented to start playback, but does not always do so for a player
      // that was paused before the swap — and a short that sits on its poster is exactly what the
      // Shorts feed must never show.
      if (autoplay) player.playVideo();
      else player.pauseVideo();
    } catch {
      // A swap during teardown throws; the player is going away regardless.
    }
  });

  /**
   * Reloads the current video where it stands.
   *
   * This is what makes a quality change actually change the picture. Resizing the frame moves the
   * embed's *selection* immediately, and going up is served immediately too — but going down it
   * keeps playing the high-quality segments it has already buffered, so the tier reads `hd720`
   * while 2160p is still on screen. Measured: seventeen seconds after picking 720p, the video
   * element was still decoding 3840x2160.
   *
   * Reloading at the same position discards that buffer and refills it at the new size. It costs a
   * short rebuffer, which is exactly what picking a quality on YouTube itself costs, and it fixes
   * the frame-rate family at the same time — that is chosen per load, so a tier picked without a
   * reload can arrive at the right resolution and the wrong frame rate.
   */
  const reloadAtPosition = useEffectEvent(() => {
    const player = playerRef.current;
    if (!player) return;
    try {
      const resumeAt = player.getCurrentTime();
      const wasPlaying = player.getPlayerState() === EMBED_STATE.playing;
      player.loadVideoById({ videoId, startSeconds: resumeAt });
      // `loadVideoById` always starts playing; someone who had paused did not ask to resume.
      if (!wasPlaying) player.pauseVideo();
    } catch {
      // Mid-teardown, or nothing cued yet. The frame is the right size either way.
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

    // Before the API is even asked for. The embed reads its viewport as it boots, so a frame still
    // at its placeholder size would have the first rendition chosen against the wrong number — and
    // the frame rate with it, which no later resize can undo.
    applyLayout(HIGH_FRAME_RATE_MIN_WIDTH);
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
      if (settleTimerRef.current !== null) {
        clearTimeout(settleTimerRef.current);
        settleTimerRef.current = null;
      }
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

  /**
   * Effect 5 — size. Keeps the frame's layout in step with the box it has to fill.
   *
   * Without this the size is only correct at construction, and the player is constructed once per
   * session: entering theatre mode, resizing the window or collapsing the sidebar would all leave
   * the embed serving a rendition chosen for a box it no longer occupies, and — now that the frame
   * is scaled — leave the picture visibly the wrong size inside it.
   *
   * The frame is observed rather than the container, because the container is now the *result* of
   * the layout rather than an input to it; observing it would feed the effect its own output.
   *
   * Coalesced to a frame, because a resize drag would otherwise relay the embed's document on
   * every pointer move.
   */
  useEffect(() => {
    const frame = frameRef.current;
    if (!frame || typeof ResizeObserver === 'undefined') return undefined;

    let scheduled = 0;
    const observer = new ResizeObserver(() => {
      if (scheduled !== 0) return;
      scheduled = requestAnimationFrame(() => {
        scheduled = 0;
        applyLayout();
      });
    });
    observer.observe(frame);

    return () => {
      observer.disconnect();
      if (scheduled !== 0) cancelAnimationFrame(scheduled);
    };
  }, []);

  /**
   * Effect 6 — quality. Relays the frame, which is how a tier is actually requested.
   *
   * A live change, not a rebuild: the embed notices its new viewport and moves to the matching
   * rendition within a few seconds while playback continues — in either direction, and without a
   * rebuffer. That is why `auto` can track the window continuously, and why choosing a tier by hand
   * costs nothing either.
   *
   * Resolution follows the frame; *frame rate* does not. The embed settles on a 30fps or 60fps
   * track family when a video loads and keeps it for that load, which is why the player is parked
   * at a 720p-shaped box rather than a 360p-shaped one — see `PARKED` in `PlayerHost`.
   */
  useEffect(() => {
    applyLayout();
  }, [quality, maxAutoQuality]);

  /**
   * Effect 7 — an explicitly chosen tier is re-picked at load.
   *
   * Only for a deliberate choice, and only when it changes: never on mount, and never for `auto`,
   * whose whole value is that it follows the window without ever interrupting playback. See
   * `reloadAtPosition` for why the resize in Effect 6 is not enough on its own.
   */
  useEffect(() => {
    const previous = pickedRef.current;
    pickedRef.current = quality;
    if (quality === 'auto' || previous === quality) return;
    reloadAtPosition();
  }, [quality]);

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
      rate: () => {
        try {
          return playerRef.current?.getPlaybackRate() ?? 1;
        } catch {
          return 1;
        }
      },
      availableRates: () => {
        try {
          return playerRef.current?.getAvailablePlaybackRates() ?? [];
        } catch {
          // Nothing to offer is the honest answer; the menu omits the section entirely.
          return [];
        }
      },
      setRate: (rate: number) => {
        try {
          playerRef.current?.setPlaybackRate(rate);
        } catch {
          // The player throws once torn down; a command with nothing to command is a no-op.
        }
      },
      availableQualities: () => {
        try {
          const levels = playerRef.current?.getAvailableQualityLevels() ?? [];
          // Intersected with the ladder this application knows how to ask for, in menu order. The
          // embed's `auto` entry is dropped: it is a mode rather than a tier, and the menu offers
          // it separately.
          return QUALITY_ORDER.filter((tier) =>
            levels.some((level) => EMBED_LEVEL[level] === tier),
          );
        } catch {
          // Nothing to offer is the honest answer; the menu omits the section entirely.
          return [];
        }
      },
      currentQuality: () => {
        try {
          const level = playerRef.current?.getPlaybackQuality();
          return level === undefined ? null : (EMBED_LEVEL[level] ?? null);
        } catch {
          return null;
        }
      },
      isHighFrameRate: () => {
        try {
          return playerRef.current?.getVideoData().video_quality_features?.includes('hfr') ?? false;
        } catch {
          return false;
        }
      },
      seek: (positionMs: number) => {
        try {
          // `true` allows the request to be served before the buffer catches up, which is what
          // makes a scrub feel like scrubbing rather than like waiting.
          playerRef.current?.seekTo(Math.max(0, positionMs) / 1000, true);
        } catch {
          // Seeking before the media is cued throws; there is nothing to move yet.
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
      exitFullscreen: () => {
        // Guarded: calling this with nothing fullscreen rejects, and there is nothing to report.
        if (document.fullscreenElement === null) return;
        void document.exitFullscreen().catch(() => {
          // Already left, or the document refused. Either way there is nothing left to do.
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
      {/*
        The frame, laid out at the size the chosen quality needs and scaled back into the box.

        Sized imperatively rather than from React state: it changes on every resize frame, and
        routing that through a render would re-render the player — and everything under it — at
        pointer rate. `size-full` is only the starting value, until `applyLayout` writes pixels.
      */}
      <div ref={scalerRef} className="absolute top-0 left-0 size-full origin-top-left">
        {/* React owns this element. The API replaces a throwaway child appended inside it. */}
        <div ref={containerRef} className="size-full" />
      </div>

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
