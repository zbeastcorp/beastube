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
 * ## Navigation is the browser's, not ours
 *
 * The feed is a real scrolling element with mandatory snap points. Wheel, trackpad, touch drag and
 * the scrollbar therefore all work without a line of code, and they work *smoothly*, because native
 * scrolling runs on the compositor at the display's refresh rate. An earlier version intercepted
 * those gestures and stepped an index instead, complete with a wheel cooldown to stop one trackpad
 * flick skipping five videos — every bit of which was reimplementing, worse, something the browser
 * already does.
 *
 * The arrow keys and the on-screen buttons scroll the container rather than setting state, so every
 * route to the next short goes through the same mechanism.
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

import { EmptyState } from '@/components/common/EmptyState';
import { ErrorState } from '@/components/common/ErrorState';
import { VideoActions } from '@/components/video/VideoActions';
import { YouTubePlayer, type PlayerHandle } from '@/components/video/YouTubePlayer';
import { useTranslation } from '@/i18n/context';
import { useAsyncResource, type AsyncResource } from '@/hooks/useAsyncResource';
import { cachedShortChannel, prefetchShortChannel, shortChannel } from '@/services/channelInfo';
import { invoke } from '@/services/ipc';
import { useSessionStore } from '@/stores/session';
import { bestThumbnailFor, type VideoId, type VideoSummary } from '@/types/domain';

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

/**
 * How much of the embed's top and bottom edge is cropped away, in CSS pixels.
 *
 * The embed paints its own title, channel and hashtags across the top of the player and keeps them
 * there while the video plays, plus a "Shorts" watermark and a link button along the bottom.
 * Nothing turns any of it off: `showinfo` was withdrawn years ago, `controls=0` does not cover it,
 * and the privacy host behaves identically — all three measured against the running app rather
 * than assumed.
 *
 * So the player is given this much extra height at the top *and* the bottom, then shifted up by
 * exactly one of those amounts. The embed fits a 9:16 video to the box's width, so the extra height
 * becomes an equal letterbox bar above and below the picture, and the visible window lands
 * precisely on the picture. The embed's bands sit in those bars and are cropped away with them.
 *
 * No picture is lost. That symmetry is the entire point, and it is why the extra height is doubled
 * rather than simply added to the top — adding it to one side only would scale the video down and
 * cost real frame.
 */
const EMBED_CHROME_CROP_PX = 64;

/** How long the pointer must rest before the overlaid chrome fades away. */
const CHROME_IDLE_MS = 2600;

/** The Shorts tab. */
export function ShortsFeed({
  videos,
  state,
  initialVideoId,
  onNearEnd,
}: ShortsFeedProps): ReactNode {
  const t = useTranslation();
  const scrollRef = useRef<HTMLDivElement>(null);
  const playerRef = useRef<PlayerHandle>(null);

  /**
   * Which short is on screen.
   *
   * Read from the scroll position rather than driving it. The container is a real scrolling element
   * with snap points, so the wheel, a trackpad, a touch drag and the scrollbar all move it natively
   * — and native scrolling is the only kind that runs on the compositor at the display's refresh
   * rate. Anything that intercepted those gestures to animate a transition itself would be slower
   * and worse than what the browser already does.
   */
  const [index, setIndex] = useState(0);

  /** Height of one snap section, measured rather than assumed so the geometry survives a resize. */
  const [stageHeight, setStageHeight] = useState(0);

  const [playing, setPlaying] = useState(true);
  /**
   * Starts muted, and says so.
   *
   * Not a preference: a browser refuses to autoplay audio, so an unmuted feed does not start at all
   * — it shows the embed's poster and a play button, which is what "the feed isn't coming"
   * actually looked like. Muted autoplay always starts, and the volume control sits in the corner
   * showing exactly one click to sound.
   */
  const [muted, setMuted] = useState(true);
  const [volume, setVolume] = useState(100);
  const [volumeOpen, setVolumeOpen] = useState(false);
  /** Whether the overlaid chrome is showing. It hides while the pointer is still, as YouTube's does. */
  const [chromeVisible, setChromeVisible] = useState(true);
  const idleTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [captions, setCaptions] = useState(false);
  const [captionsAvailable, setCaptionsAvailable] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const seekedToDeepLink = useRef(false);
  const scrollFrame = useRef(0);
  const incognito = useSessionStore((session) => session.incognito);

  const current = videos[Math.min(index, Math.max(0, videos.length - 1))];

  /**
   * Who made the short on screen.
   *
   * The feed's own entries carry no channel — see `channelInfo` — so this is one request against
   * the video, cached, and warmed one short ahead so it is there before the viewer is.
   *
   * The result carries the video id it belongs to and is only rendered when that still matches.
   * The resource hook deliberately retains the previous value across a key change so a refetch
   * does not blank the screen, which here would mean showing the last short's channel under the
   * current one's picture.
   */
  const channel = useAsyncResource(current ? `short-channel:${current.id}` : null, (signal) =>
    shortChannel(current?.id ?? ('' as VideoId), signal),
  );
  const channelNow =
    channel.data?.videoId === current?.id ? channel.data : cachedShortChannel(current?.id);

  // The feed's own entry sometimes already carries a channel — a short that arrived through the
  // search path rather than as a lockup — so that is preferred and the row renders on the first
  // frame, with the fetched value filling in for the rest.
  const channelName = current?.channel_name ?? channelNow?.name ?? null;

  const nextId = videos[index + 1]?.id;
  const warmNext = useEffectEvent(() => {
    prefetchShortChannel(nextId);
  });
  useEffect(() => {
    warmNext();
  }, [nextId]);

  /**
   * Toggles playback and flips the icon immediately.
   *
   * The player lives in a cross-origin iframe, so every command is a postMessage round trip and the
   * state change comes back a beat later. Waiting for it made the button feel like it had missed
   * the press. The optimistic flip is corrected by `onStateChange` if the player disagrees.
   */
  const togglePlayback = useCallback(() => {
    setPlaying((wasPlaying) => !wasPlaying);
    playerRef.current?.toggle();
  }, []);

  /** Shows the chrome and restarts the idle countdown. Called on any pointer activity. */
  const wakeChrome = useCallback(() => {
    setChromeVisible(true);
    if (idleTimer.current !== null) clearTimeout(idleTimer.current);
    idleTimer.current = setTimeout(() => {
      setChromeVisible(false);
    }, CHROME_IDLE_MS);
  }, []);

  useEffect(
    () => () => {
      if (idleTimer.current !== null) clearTimeout(idleTimer.current);
    },
    [],
  );

  /**
   * Exactly 9:16, for every short.
   *
   * Measured on youtube.com/shorts: their player is 460 by 818, which is 0.5625 to the pixel, and
   * it is that shape whatever the video inside it happens to be. Deriving the ratio from the
   * thumbnail was tried twice and abandoned — some renditions of a short are padded to 16:9, so the
   * derivation produced a landscape stage with the video pillar-boxed inside it, which is the exact
   * defect this surface exists to avoid.
   */
  const ratioOf = (): number => 9 / 16;
  const stageWidth = current ? stageHeight * ratioOf() : 0;

  /**
   * Attaches the size observer as the scroll element mounts.
   *
   * A ref callback rather than an effect: the feed renders a loading state first, so an effect with
   * empty dependencies runs while `scrollRef.current` is still null and never runs again — leaving
   * the height at zero and every section falling back to full width, which is exactly the landscape
   * stage this feed exists to avoid.
   */
  const observerRef = useRef<ResizeObserver | null>(null);
  const attachScroll = useCallback((node: HTMLDivElement | null) => {
    scrollRef.current = node;
    observerRef.current?.disconnect();
    observerRef.current = null;
    if (!node) return;

    const observer = new ResizeObserver((entries) => {
      const height = entries[0]?.contentRect.height ?? 0;
      if (height > 0) setStageHeight(height);
    });
    observer.observe(node);
    observerRef.current = observer;
    // Seeded immediately, because the observer's first callback lands a frame later and one frame
    // of full-width sections is one frame of visibly wrong layout.
    if (node.clientHeight > 0) setStageHeight(node.clientHeight);
    // And measured again on the next frame. At ref-attach time the parent's height is still
    // resolving — it is a `min()` over `dvh` — so the first reading can be short, which leaves every
    // section shorter than the viewport and the next short peeking in below the current one.
    requestAnimationFrame(() => {
      if (node.isConnected && node.clientHeight > 0) setStageHeight(node.clientHeight);
    });
  }, []);

  /** Scrolls to a short. Smooth for a deliberate move, instant for the initial deep link. */
  const scrollToIndex = useCallback((target: number, smooth: boolean) => {
    const element = scrollRef.current;
    if (!element || element.clientHeight === 0) return;
    element.scrollTo({
      top: target * element.clientHeight,
      behavior: smooth ? 'smooth' : 'auto',
    });
  }, []);

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
      scrollToIndex(next, true);
      // Pressing next at the boundary asks for more rather than doing nothing at all, so a viewer
      // who outruns the prefetch gets the feed to catch up instead of a dead button.
      if (delta > 0 && next === index) requestMoreNow();
    },
    [index, videos.length, requestMoreNow, scrollToIndex],
  );

  // Open on the deep-linked short once it is in the batch. Instant rather than animated: this is
  // where the feed starts, not somewhere it travelled to.
  const seekToDeepLink = useEffectEvent(() => {
    if (seekedToDeepLink.current || initialVideoId === undefined || stageHeight === 0) return;
    const found = videos.findIndex((video) => video.id === initialVideoId);
    if (found < 0) return;
    seekedToDeepLink.current = true;
    scrollToIndex(found, false);
  });
  useEffect(() => {
    seekToDeepLink();
  }, [initialVideoId, videos.length, stageHeight]);

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
      if (event.key === ' ') {
        // The embed's own keyboard handling is off with its chrome, so these are ours to provide.
        event.preventDefault();
        togglePlayback();
        return;
      }
      if (event.key === 'm') {
        event.preventDefault();
        setMuted((currentlyMuted) => {
          playerRef.current?.setMuted(!currentlyMuted);
          return !currentlyMuted;
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
  }, [move, togglePlayback]);

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
      className="relative mx-auto"
      onPointerMove={wakeChrome}
      onPointerLeave={() => {
        setChromeVisible(false);
      }}
      style={
        {
          // The height budget subtracts the shell chrome rather than guessing at a viewport
          // fraction: 82vh ignored 128px of top bar and padding, so on a short window the tab
          // scrolled behind the layout instead of inside it.
          height: 'min(calc(100dvh - var(--layout-topbar-height) - 5rem), 900px)',
        } satisfies CSSProperties
      }
    >
      <div
        ref={attachScroll}
        // The scroll surface. `snap-mandatory` is what makes a flick land on exactly one short
        // instead of between two, and `overscroll-contain` stops a scroll that reaches either end
        // from chaining to the page behind it.
        className="scrollbar-none relative h-full snap-y snap-mandatory overflow-y-auto overscroll-contain"
        onScroll={() => {
          // Coalesced to one read per frame. A scroll event can fire many times between paints, and
          // `scrollTop` forces layout, so reading it per event is the classic way to make a
          // smooth-scrolling list stutter.
          if (scrollFrame.current !== 0) return;
          scrollFrame.current = requestAnimationFrame(() => {
            scrollFrame.current = 0;
            const element = scrollRef.current;
            if (!element || element.clientHeight === 0) return;
            const next = Math.round(element.scrollTop / element.clientHeight);
            setIndex((shown) => (shown === next ? shown : next));
          });
        }}
      >
        {videos.map((video) => {
          const poster = video.thumbnails
            ? bestThumbnailFor(video.thumbnails, 480)?.url
            : undefined;
          return (
            <section
              key={video.id}
              className="relative flex snap-center snap-always items-center justify-center"
              style={{ height: stageHeight > 0 ? stageHeight : '100%' }}
            >
              <div
                className="relative h-full"
                style={{ width: stageHeight > 0 ? stageHeight * ratioOf() : '100%' }}
              >
                <div
                  className="relative h-full overflow-hidden rounded-xl bg-black"
                  style={{
                    width: '100%',
                    // Forces the box onto its own compositing layer. An `<iframe>` inside an
                    // `overflow: hidden` parent is not reliably clipped by the parent's radius — the
                    // embed's square corners poke through — and promoting the clipper is what makes
                    // the rounding actually apply to it.
                    transform: 'translateZ(0)',
                    isolation: 'isolate',
                  }}
                >
                  {/* The poster stands in for the video on every short except the one playing. It is
                    what makes scrolling look continuous: there is always a picture under the
                    gesture, rather than an empty box waiting for a player that will never mount
                    here. */}
                  {poster !== undefined && (
                    <img
                      src={poster}
                      alt=""
                      aria-hidden="true"
                      loading="lazy"
                      decoding="async"
                      className="absolute inset-0 size-full object-cover"
                    />
                  )}
                </div>
              </div>
            </section>
          );
        })}

        {/*
         * The player, pinned over whichever section is current.
         *
         * Absolutely positioned inside the scrolling content, so it travels with the scroll like
         * any other child — but it never moves in the React tree, which is the whole point. Moving
         * an iframe in the DOM destroys its browsing context and reloads it, so a player rendered
         * inside each section would pay a full embed bootstrap on every single scroll.
         */}
        {stageHeight > 0 && (
          <div
            // Centred with `inset-x-0` and auto margins rather than a half-width translate. The
            // translate had to share the `transform` property with the `translateZ` that forces the
            // iframe to be clipped, and one inline value overwrote the other — which put the player
            // beside its own section instead of over it.
            className="absolute inset-x-0 mx-auto"
            style={{ top: index * stageHeight, height: stageHeight, width: stageWidth }}
          >
            {/* Only the video and its overlays are clipped. The clip used to sit on the whole
                positioned box, which cut off the action rail hanging beside it. */}
            <div
              className="absolute inset-0 overflow-hidden rounded-xl"
              style={{
                // Forces the box onto its own compositing layer. An `<iframe>` inside an
                // `overflow: hidden` parent is not reliably clipped by the parent's radius — the
                // embed's square corners poke through — and promoting the clipper is what makes
                // the rounding apply to it.
                transform: 'translateZ(0)',
                isolation: 'isolate',
              }}
            >
              {/* Taller than the clip on both sides and shifted up by half the difference, so the
                  embed's own title band and watermark land outside the visible window. See
                  `EMBED_CHROME_CROP_PX`; the picture itself is untouched. */}
              <div
                className="absolute inset-x-0"
                style={{
                  top: -EMBED_CHROME_CROP_PX,
                  height: `calc(100% + ${String(EMBED_CHROME_CROP_PX * 2)}px)`,
                }}
              >
                <YouTubePlayer
                  ref={playerRef}
                  videoId={current.id}
                  fill
                  transparent
                  autoplay
                  muted={muted}
                  // The embed's own chrome is hidden and replaced below, which is what YouTube does
                  // on its Shorts surface. Every control drawn in its place drives the player for
                  // real.
                  controls={false}
                  onStateChange={(playbackState) => {
                    setPlaying(playbackState === 'playing' || playbackState === 'buffering');
                    // Caption availability is a property of the video, and the embed only knows once
                    // it has loaded one. Asked here so the control is absent for a short with none.
                    setCaptionsAvailable(playerRef.current?.hasCaptions() ?? false);
                    if (playbackState === 'ended') move(1);
                  }}
                />
              </div>

              {/* Channel and title along the bottom edge, where YouTube's sit. Ours replaces the
                  embed's band rather than sitting under it — that band is cropped away above, and
                  this is drawn at a size and weight measured off youtube.com/shorts.

                  Legibility comes from a text shadow, not a scrim. A gradient wash over the bottom
                  third is what made the picture look dull and dirty next to YouTube's, and the
                  shadow reads just as well over a bright frame.

                  No Subscribe button, deliberately. Subscribing is an account action, and this
                  application has no account — a button that cannot do its job does not belong on
                  the screen (§131). The channel row is itself absent for a short whose channel the
                  extractor did not report, for the same reason. */}
              <div
                className="pointer-events-none absolute inset-x-0 bottom-0 z-20 flex flex-col gap-1.5 p-4 pr-14"
                style={{
                  textShadow: '0 1px 3px rgba(0,0,0,0.75), 0 0 12px rgba(0,0,0,0.45)',
                  opacity: chromeVisible || volumeOpen || menuOpen ? 1 : 0,
                  transition: 'opacity var(--duration-chrome) var(--ease-player-out)',
                }}
              >
                {channelName !== null && (
                  <div className="flex items-center gap-2">
                    {channelNow?.avatarUrl != null ? (
                      <img
                        src={channelNow.avatarUrl}
                        alt=""
                        aria-hidden="true"
                        width={32}
                        height={32}
                        className="size-8 shrink-0 rounded-full object-cover"
                      />
                    ) : (
                      // The initial stands in only until the avatar arrives, so the row does not
                      // change height when it does.
                      <span
                        className="grid size-8 shrink-0 place-items-center rounded-full bg-white/20 text-xs font-semibold text-white"
                        aria-hidden="true"
                      >
                        {channelName.slice(0, 1).toUpperCase()}
                      </span>
                    )}
                    <span className="truncate text-sm font-medium text-white">{channelName}</span>
                  </div>
                )}
                <p className="line-clamp-2 text-sm leading-snug font-medium text-white">
                  {current.title}
                </p>
                {current.view_count !== undefined && (
                  <span className="text-xs text-white/80">
                    {t.plural('video.views', current.view_count, {
                      count: t.compact(current.view_count),
                    })}
                  </span>
                )}
              </div>

              {/* The whole frame is the play/pause target, as it is on YouTube. A button rather than
                a div so it is keyboard reachable and announced; it carries no chrome of its own. */}
              <button
                type="button"
                onClick={togglePlayback}
                aria-label={t.t(playing ? 'player.pause' : 'player.play')}
                className="absolute inset-0 z-10 cursor-default"
              />

              {/* Chrome belongs to the short it controls, so it lives inside the overlay and travels
                with it. Pinned to the feed instead, it hung in mid-air over whatever was sliding
                past during a scroll. */}
              <div
                className="pointer-events-none absolute inset-x-0 top-0 z-30 flex items-start justify-between p-3"
                style={{
                  opacity: chromeVisible || volumeOpen || menuOpen ? 1 : 0,
                  transition: 'opacity var(--duration-chrome) var(--ease-player-out)',
                }}
              >
                <div className="pointer-events-auto flex items-center gap-1">
                  <StageButton
                    label={t.t(playing ? 'player.pause' : 'player.play')}
                    onClick={() => {
                      playerRef.current?.toggle();
                    }}
                  >
                    {playing ? <Pause size={18} /> : <Play size={18} />}
                  </StageButton>

                  {/* The slider grows out of the icon on hover, the way YouTube's does. It stays open
                while the pointer is anywhere over the pair, so travelling from the icon to the
                slider does not close the thing being travelled to. */}
                  <div
                    className="flex items-center"
                    onPointerEnter={(event) => {
                      if (event.pointerType === 'mouse') setVolumeOpen(true);
                    }}
                    onPointerLeave={() => {
                      setVolumeOpen(false);
                    }}
                  >
                    <StageButton
                      label={t.t(muted ? 'player.unmute' : 'player.mute')}
                      onClick={() => {
                        const next = !muted;
                        setMuted(next);
                        playerRef.current?.setMuted(next);
                      }}
                    >
                      {muted || volume === 0 ? <VolumeX size={18} /> : <Volume2 size={18} />}
                    </StageButton>

                    <div
                      className="overflow-hidden"
                      style={{
                        width: volumeOpen ? 88 : 0,
                        opacity: volumeOpen ? 1 : 0,
                        transition:
                          'width var(--duration-volume) var(--ease-player-in), opacity var(--duration-volume) var(--ease-player-in)',
                      }}
                    >
                      <input
                        type="range"
                        min={0}
                        max={100}
                        value={muted ? 0 : volume}
                        aria-label={t.t('player.volume')}
                        tabIndex={volumeOpen ? 0 : -1}
                        onChange={(event) => {
                          const next = Number(event.target.value);
                          setVolume(next);
                          playerRef.current?.setVolume(next);
                          // Moving the slider off zero is an unmute; nobody drags a slider expecting
                          // silence to continue.
                          const shouldMute = next === 0;
                          if (shouldMute !== muted) {
                            setMuted(shouldMute);
                            playerRef.current?.setMuted(shouldMute);
                          }
                        }}
                        className="accent-brand ml-1 w-20 align-middle"
                      />
                    </div>
                  </div>
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
                      className="bg-surface border-border absolute top-11 right-0 z-40 min-w-48 overflow-hidden rounded-lg border py-1 shadow-lg"
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
            </div>

            {/* Save and share, against the video's right edge — inside the overlay, so they move
                with the short rather than hanging over the one scrolling past.
                Keyed on the video so each short gets its own action state. */}
            <div className="pointer-events-auto absolute bottom-2 left-full z-30 ml-4">
              <VideoActions key={current.id} video={current} />
            </div>
          </div>
        )}
      </div>

      {/* Navigation in the corner, well clear of everything else: a mis-aimed press on "next"
          should never be able to land on "save". */}
      <div className="pointer-events-auto absolute right-4 bottom-4 z-30 flex flex-col gap-3">
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
          // Never disabled while the feed can still grow. Greying it out at the boundary tells the
          // viewer they have reached the end when they have only reached the end of what has
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
        // A filled translucent disc, not a bare glyph. Measured on youtube.com/shorts: their
        // controls carry their own dark disc so they stay readable over a bright frame — ours were
        // white-on-transparent and vanished against anything pale.
        'grid size-9 place-items-center rounded-full text-white',
        'transition-[background-color] duration-[var(--duration-chrome-button)] ease-[var(--ease-player-out)]',
        active ? 'bg-black/70' : 'bg-black/45 hover:bg-black/70',
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
