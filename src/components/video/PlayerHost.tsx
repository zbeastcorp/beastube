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
 * ## The embed's chrome is cropped away, and the controls are ours
 *
 * The embed paints one band across the top of the player — title and channel on the left, volume,
 * subtitles and settings on the right — and a "More videos" strip with a watermark across the
 * bottom. No parameter removes them: `showinfo` was withdrawn and `controls=0` covers the bottom
 * bar only. So the frame is given {@link EMBED_CHROME_CROP_PX} of extra height at the top *and*
 * the bottom and shifted up by that amount. The embed fits a 16:9 video to the box's width, so the
 * extra height becomes an equal letterbox bar above and below the picture and the visible window
 * lands exactly on the picture. The bands sit in those bars and are cropped with them. Doubling
 * the height rather than adding it to one side is what costs no frame.
 *
 * ### Cropping the gear away used to cost the quality selector. It no longer does.
 *
 * The first attempt at this crop was reverted because the band it removes carries YouTube's own
 * settings gear, and that gear held the only working quality control on this path.
 *
 * It is no longer the only one. `setPlaybackQuality` is genuinely inert, but the embed picks its
 * rendition from the size of its own viewport and keeps doing so while playing — so laying the
 * frame out at 3840 pixels and scaling it back down asks for 2160p, and gets it, without a reload
 * (`YouTubePlayer`, ADR-0004). `getAvailableQualityLevels` reports what each video actually has,
 * so the menu below offers real tiers and nothing else.
 *
 * Which means there is no longer a trade to state. The whole player is the application's own dark
 * UI — YouTube's embed ignores `prefers-color-scheme` entirely, verified with the operating system
 * in dark mode, where its panel stayed white — and quality is selectable inside it.
 *
 * ## Leaving a screen pauses rather than tears down
 *
 * When no view wants the player, it is paused and parked out of sight with its browsing context
 * intact. That is the whole point: the next video costs one `loadVideoById` rather than a fresh
 * embed bootstrap.
 */

import {
  ArrowLeft,
  Captions,
  CaptionsOff,
  ChevronRight,
  Maximize2,
  Minimize2,
  Pause,
  Play,
  Settings,
  Volume2,
  VolumeX,
} from 'lucide-react';
import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';

import { qualityLabel, YouTubePlayer, type PlayerHandle } from '@/components/video/YouTubePlayer';
import { useAsyncResource } from '@/hooks/useAsyncResource';
import { useTranslation } from '@/i18n/context';
import { invoke } from '@/services/ipc';
import { playerHandlers, usePlayerStore } from '@/stores/player';
import { useSettingsStore } from '@/stores/settings';
import type { CaptionTrack, Quality } from '@/types/domain';

/**
 * Where the player waits when nothing wants it: off-screen, alive, and out of the way.
 *
 * 720p-shaped rather than 360p-shaped, and that is not cosmetic. The embed picks a *frame rate*
 * family when a video loads and then keeps it, while resolution follows the frame size for as long
 * as the video plays. A player first constructed in a 640-wide box loads into the 30fps family —
 * because YouTube encodes 60fps only from 720p up — and then climbs to 2160p at 30fps and stays
 * there. Parking at 1280×720 means the first load is already in the 60fps family, and every later
 * tier inherits it. Measured; see ADR-0004.
 */
const PARKED = { top: -100_000, left: 0, width: 1280, height: 720 } as const;

/**
 * Where the player waits under YouTube's own controls.
 *
 * Deliberately modest. The 1280-wide park above exists so a size-driven quality choice loads into
 * the 60fps family, and nothing under YouTube's controls needs that — their gear selects the track
 * directly. Parking wide anyway would mean a player constructed before its slot is measured boots
 * believing it is 1280 across, then shrinks to the real box, and the embed spends a second or two
 * visibly re-fitting its picture. Parking at the size it will actually occupy avoids that entirely.
 */
const PARKED_SMALL = { top: -100_000, left: 0, width: 640, height: 360 } as const;

/**
 * How much of the embed's top and bottom edge is cropped away, in the embed's *own* pixels.
 *
 * Comfortably taller than either band. See the note above for why it applies to both edges and why
 * that costs no picture.
 *
 * In the embed's pixels, not the screen's, and that distinction is the whole of a bug that was
 * visible at every quality but one. The bands are drawn inside the embed's viewport, and the player
 * scales that viewport to fit this box — so their height on screen is this number multiplied by
 * that scale. Cut as a fixed 64 screen pixels, the crop was only ever correct while the scale
 * happened to be 1.
 *
 * Measured in fullscreen on a 1920-wide screen, changing quality:
 *
 * ```text
 * 2160p   frame 3840 wide, scale 0.5   bands ~32px   crop 64px   over-cropped
 * 1080p   frame 1920 wide, scale 1     bands ~64px   crop 64px   correct
 *  360p   frame  640 wide, scale 3     bands ~192px  crop 64px   YouTube's title,
 *                                                                pause overlay, "More
 *                                                                videos" bar and logo
 *                                                                all visible over the video
 * ```
 *
 * Multiplied by the live scale below, it is correct at every tier.
 */
const EMBED_CHROME_CROP_PX = 64;

/** How long the pointer must rest before the controls fade, as YouTube's do. */
const CHROME_IDLE_MS = 2600;

/** How long the poster frame may cover the player before it lifts regardless. */
const POSTER_MAX_MS = 6000;

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
  // Bounds `auto` only. A tier chosen by hand in the menu below is honoured as given.
  const maxAutoQuality = useSettingsStore((state) => state.settings.playback.max_quality);
  // Preferences the settings screen has always persisted. Until now nothing read them, so each of
  // those rows moved a value into the database and changed nothing a viewer could see (§131).
  const preferredRate = useSettingsStore((state) => state.settings.playback.speed);
  const defaultQuality = useSettingsStore((state) => state.settings.playback.default_quality);
  const seekStepSeconds = useSettingsStore((state) => state.settings.playback.seek_step_seconds);
  const seekStepLargeSeconds = useSettingsStore(
    (state) => state.settings.playback.seek_step_large_seconds,
  );
  const captionsByDefault = useSettingsStore((state) => state.settings.playback.captions_enabled);
  // Which language to ask the embed for, when the viewer has stated one.
  const captionLanguage = useSettingsStore((state) => state.settings.playback.caption_language);
  const captionOffset = useSettingsStore((state) => state.settings.playback.caption_offset_percent);
  const captionScale = useSettingsStore((state) => state.settings.playback.caption_scale_percent);
  const captionBackground = useSettingsStore(
    (state) => state.settings.playback.caption_background_percent,
  );
  /**
   * Whether the viewer has asked for YouTube's own control bar instead of ours.
   *
   * When they have, the crop is lifted, the embed's controls are turned on, and everything this
   * component draws over the picture is withheld — including the click-to-play surface, which
   * would otherwise sit on top of YouTube's bar and swallow every press.
   */
  const nativeControls =
    useSettingsStore((state) => state.settings.playback.player_controls) === 'youtube';

  const playerRef = useRef<PlayerHandle>(null);
  /** The element fullscreen is requested on: the whole player, controls included. */
  const boxRef = useRef<HTMLDivElement>(null);
  /** The video the viewer's preferred playback speed has already been applied to. */
  const ratedFor = useRef<string | null>(null);
  // Parked to match the mode, so a player constructed before its slot has been measured starts at
  // a size close to the one it will end up at rather than shrinking into place afterwards.
  const parked = nativeControls ? PARKED_SMALL : PARKED;
  const [box, setBox] = useState<Box>(parked);
  /**
   * The video that has actually painted a frame, or `null`.
   *
   * Keyed on the video rather than a plain flag. As a flag it latched true on the first video ever
   * played and never went back, so the poster covered the black buffering frame exactly once per
   * session and every video after it opened on a black rectangle.
   */
  const [startedId, setStartedId] = useState<string | null>(null);
  /**
   * The video whose playback failed, or `null`.
   *
   * The poster sits at `z-10` inside a stacking context, above the player's own failure panel — so
   * a video that never reaches `playing` (embed-blocked, private, removed) kept an opaque
   * thumbnail over the message explaining why. Keyed on the video for the same reason the poster
   * is: a failure belongs to one video, not to the session.
   */
  const [erroredId, setErroredId] = useState<string | null>(null);
  const [playing, setPlaying] = useState(false);
  const [at, setAt] = useState({ positionMs: 0, durationMs: 0 });
  const [muted, setMuted] = useState(false);
  const [volume, setVolume] = useState(100);
  // Seeded from the setting so the caption button reflects what the embed is actually doing
  // rather than always starting "off" over a player that has captions showing. Applied again once
  // the settings document has actually loaded — see below.
  const [captionsOn, setCaptionsOn] = useState(captionsByDefault);
  /**
   * The video the embed has confirmed captions for, or `null`.
   *
   * Held as an id rather than a flag so it resets itself when the video changes — no effect, no
   * stale "has captions" carried onto the next video. It only ever *adds* to what the provider
   * said: the embed reports "none" for any video whose caption module is not loaded, which is
   * every video with captions switched off, so it is not allowed to deny availability.
   */
  const [embedCaptionsFor, setEmbedCaptionsFor] = useState<string | null>(null);
  const [chromeVisible, setChromeVisible] = useState(true);
  /**
   * Whether anything inside the player currently holds keyboard focus.
   *
   * The control bar fades to `opacity: 0` while it stays in the tab ring, which is right — the
   * controls have to be reachable without a pointer. What was missing was the other half: waking
   * was bound to `onPointerMove` alone, so tabbing to play, mute, captions, settings or fullscreen
   * focused a control that could not be seen, with the focus ring drawn on nothing. Anyone
   * navigating by keyboard was operating the player blind.
   *
   * A separate condition rather than another call to `wake`, because focus is not a timeout: the
   * bar stays up for as long as focus is inside it, and the idle timer resumes when focus leaves.
   */
  const [chromeFocused, setChromeFocused] = useState(false);
  /**
   * What the player is currently scaling its frame by, reported by the player itself.
   *
   * Drives the chrome crop, which has to grow and shrink with it — see
   * {@link EMBED_CHROME_CROP_PX}. Starts at 1, the value it holds whenever the frame is laid out at
   * the size it is displayed at.
   */
  const [frameScale, setFrameScale] = useState(1);
  /**
   * Which video the settings menu was opened for, or `null` for closed.
   *
   * Derived rather than reset in an effect: an open menu belongs to the video it was opened over,
   * so a new video closes it by simply no longer matching. Resetting a boolean in an effect keyed
   * on `videoId` would be a synchronous setState inside an effect, which cascades a render on
   * every video change.
   */
  const [menuFor, setMenuFor] = useState<string | null>(null);
  const [rate, setRate] = useState(1);
  /**
   * The speeds the player will accept, read when it reports a state change.
   *
   * Held in state rather than asked of the ref during render: a ref read during render is not a
   * value React knows changed, so the menu could render a stale list.
   */
  const [rates, setRates] = useState<number[]>([]);
  /**
   * The audio tracks this video offers.
   *
   * Named and counted by the provider, which read them from the video's own player response. The
   * embed can switch tracks but the shape of its list is undocumented, so deriving the menu from
   * it meant a field name changing upstream would make the row quietly disappear. The embed is
   * still what performs the switch; it is simply not what decides whether there is one to offer.
   *
   * Most videos have a single track and get no row at all: one entry is not a choice (§131).
   */
  /**
   * What the embed reports, held against the video it was read for.
   *
   * Only a fallback. YouTube omits track information entirely from a video that has one audio
   * track, so the provider has nothing to name for those — and the row would disappear on exactly
   * the videos most people watch. The embed still names the one track it is playing.
   */
  const [embedAudio, setEmbedAudio] = useState<{
    videoId: string;
    tracks: { id: string; label: string; isDefault: boolean }[];
  } | null>(null);

  const providerAudio = (session?.audioTracks ?? []).map((track) => ({
    id: track.id,
    label: track.language_name,
    isDefault: track.is_original === true || track.is_default === true,
  }));
  const audioTracks =
    providerAudio.length > 0
      ? providerAudio
      : embedAudio?.videoId === videoId
        ? embedAudio.tracks
        : [];
  /**
   * The track chosen by hand, held against the video it was chosen for.
   *
   * An id rather than a flag so it resets itself when the video changes — Spanish chosen on one
   * video must not read as chosen on the next, which offers a different set entirely.
   */
  const [chosenAudio, setChosenAudio] = useState<{ videoId: string; id: string } | null>(null);
  const audioTrack =
    chosenAudio?.videoId === videoId
      ? chosenAudio.id
      : (audioTracks.find((track) => track.isDefault)?.id ?? null);
  /**
   * The tier the viewer asked for, and the tiers this video has.
   *
   * `auto` until someone chooses otherwise, which is the right default: left alone the embed picks
   * the best rendition for the size it is displayed at.
   */
  // Seeded from the viewer's default rather than hardcoded to `auto`, which is what left
  // `playback.default_quality` a setting with no consumer.
  const [quality, setQuality] = useState<Quality>(defaultQuality);

  /**
   * Re-seeds from the settings document once it has actually arrived.
   *
   * The shell paints before settings load, so the initialisers above read the *defaults* rather
   * than the viewer's own values: someone with captions on saw a CC button reporting off, over a
   * player showing subtitles. Applied once, and only while the state still holds the default, so
   * it can never overwrite a choice made in the moments before the document landed.
   */
  /**
   * Whether to offer the caption control at all.
   *
   * Derived rather than stored, so it follows the video with nothing to keep in step. The provider
   * read the track list from the video's own player response and is the answer that matters; the
   * embed's own report can confirm it late but never contradict it.
   */
  const captionsAvailable = (session?.hasCaptions ?? false) || embedCaptionsFor === videoId;

  const settingsLoaded = useSettingsStore((state) => state.loaded);
  const seeded = useRef(false);
  useEffect(() => {
    if (!settingsLoaded || seeded.current) return;
    seeded.current = true;
    setCaptionsOn((was) => (was === captionsByDefault ? was : captionsByDefault));
    setQuality((was) => (was === 'auto' ? defaultQuality : was));
  }, [settingsLoaded, captionsByDefault, defaultQuality]);
  const [qualities, setQualities] = useState<Quality[]>([]);
  /**
   * The tier actually being served, which is not the same thing as the one requested.
   *
   * A tier is asked for by resizing the frame, and the embed moves to it over the next few seconds
   * rather than at once. Showing what is being served is what stops the menu claiming a 4K switch
   * the moment it is clicked, while the picture is still 1080p — and it is what puts a real number
   * beside `Auto`.
   */
  const [serving, setServing] = useState<Quality | null>(null);
  /**
   * The video observed playing at 60fps, or `null`.
   *
   * Held as an id rather than a flag so it resets itself when the video changes — no effect, no
   * stale `60` on the next video's menu. Sticky within a video on purpose: frame rate is a property
   * of the upload, so once it has been seen at a high tier the labels stay right even while a low
   * tier, which genuinely is 30fps, is on screen.
   */
  const [highFrameRateFor, setHighFrameRateFor] = useState<string | null>(null);
  /**
   * Whether the player is currently filling the screen.
   *
   * Read from the document rather than remembered from our own button, because fullscreen can be
   * left by pressing Escape or by the browser deciding to — and a button that had only counted its
   * own presses would then offer to leave a fullscreen that had already ended. Escape was, until
   * now, the *only* way out: the control offered no way back, which is the bug this fixes.
   */
  const [fullscreen, setFullscreen] = useState(false);
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
    if (session === null) {
      playerRef.current?.pause();
      // And drop out of fullscreen. The player is about to be parked off-screen, so staying
      // fullscreen would leave the whole window filled by an element that is no longer shown.
      if (document.fullscreenElement !== null) {
        void document.exitFullscreen().catch(() => {
          // Nothing to recover; the player is being put away either way.
        });
      }
    }
  }, [session]);

  useEffect(
    () => () => {
      if (idleTimer.current !== null) clearTimeout(idleTimer.current);
    },
    [],
  );

  /**
   * A hard cap on how long the poster may cover the player.
   *
   * Belt and braces for the case the state machine misses entirely — an embed that reports nothing
   * at all. A thumbnail that never lifts is indistinguishable from a frozen application, and after
   * a few seconds it is better to show whatever the embed is showing, even a black frame, than to
   * keep insisting on a picture that is not the video.
   */
  useEffect(() => {
    const timer = setTimeout(() => {
      setStartedId((was) => (was === videoId ? was : videoId));
    }, POSTER_MAX_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [videoId]);

  // The document is the authority on fullscreen, so it is the thing we listen to.
  useEffect(() => {
    const sync = () => {
      setFullscreen(document.fullscreenElement !== null);
    };
    document.addEventListener('fullscreenchange', sync);
    return () => {
      document.removeEventListener('fullscreenchange', sync);
    };
  }, []);

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
  // No slot means nothing on screen wants the player, so the parked size applies rather than
  // whichever box it last occupied.
  const shown = slot === null ? parked : box;
  const fraction = at.durationMs > 0 ? Math.min(1, at.positionMs / at.durationMs) : 0;
  // Held open while the menu is: controls that faded out from under an open panel would leave it
  // floating over the picture attached to nothing.
  const settingsOpen = menuFor === videoId;
  // The crop is cut in screen pixels; what it hides is drawn in the embed's. Multiplying by the
  // live scale is what keeps the two in step at every tier.
  const crop = EMBED_CHROME_CROP_PX * frameScale;

  const chromeUp = chromeVisible || !playing || settingsOpen || chromeFocused;

  return (
    <div
      // `aria-hidden` while parked: it is off-screen and paused, and announcing it would put a
      // player in the reading order of a screen that has nothing to do with one.
      aria-hidden={hidden}
      className="absolute"
      style={{
        top: shown.top,
        left: shown.left,
        width: shown.width,
        height: shown.height,
        // Kept out of the way rather than removed, so the browsing context survives.
        visibility: hidden ? 'hidden' : 'visible',
        pointerEvents: hidden ? 'none' : 'auto',
      }}
      onPointerMove={wake}
      onFocusCapture={() => {
        setChromeFocused(true);
      }}
      onBlurCapture={(event) => {
        // Only when focus has actually left the player, not on every hop between its buttons:
        // `relatedTarget` is where focus is going, and `null` means it left the document.
        if (!event.currentTarget.contains(event.relatedTarget)) setChromeFocused(false);
      }}
      onPointerLeave={() => {
        setChromeVisible(false);
        setMenuFor(null);
      }}
    >
      <div
        // The element that goes fullscreen, and it has to be this one rather than the player's own
        // frame inside it. A fullscreen element is promoted to the browser's top layer with an
        // opaque backdrop behind it, and *only its descendants* come with it. The frame is a child
        // of this box but every control below is a sibling of the frame — so fullscreening the
        // frame left the scrubber, the volume, the settings menu and the exit-fullscreen button
        // itself outside the top layer: invisible and unclickable, with Escape the only way back.
        // Fullscreening the box takes the whole player, chrome crop included.
        ref={boxRef}
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
          style={
            nativeControls
              ? { top: 0, height: '100%' }
              : {
                  top: -crop,
                  height: `calc(100% + ${String(crop * 2)}px)`,
                }
          }
        >
          <YouTubePlayer
            ref={playerRef}
            videoId={videoId}
            fill
            // Off when we crop the chrome away — a half-visible bar is worse than none — and on
            // when the viewer has asked for YouTube's, which is the whole point of that setting.
            controls={nativeControls}
            // Only ever under YouTube's own control bar. With ours, captions are drawn by
            // `CaptionOverlay` below so they can be positioned — and two sets on screen at once is
            // the one outcome worse than none.
            captionsByDefault={nativeControls && captionsByDefault}
            {...(captionLanguage !== null ? { captionLanguage } : {})}
            // Left on `auto` under YouTube's controls: its gear sets the embed's own preference,
            // which overrides frame size entirely, so asking by size as well would be two hands on
            // the same lever.
            onScale={setFrameScale}
            quality={nativeControls ? 'auto' : quality}
            maxAutoQuality={maxAutoQuality}
            autoplay={session?.autoplay ?? false}
            {...(session?.startAtMs !== undefined ? { startAtMs: session.startAtMs } : {})}
            onStateChange={(state, forId) => {
              setPlaying(state === 'playing' || state === 'buffering');
              // Any state that means the embed has something on screen clears the poster, not
              // `playing` alone. A video that is cued and waiting — autoplay refused, or simply
              // paused — never reaches `playing`, so the thumbnail stayed opaque over a player
              // that was ready and willing, and the whole page read as hung. The poster is there
              // to hide the black frame *while the embed has nothing*, and that is over the
              // moment it reports anything else.
              if (state === 'playing' || state === 'paused' || state === 'ready') {
                setStartedId((was) => (was === videoId ? was : videoId));
              }
              // Tracked so the poster can get out of the way of the message.
              setErroredId((was) => {
                if (state === 'error') return was === videoId ? was : videoId;
                return was === videoId ? null : was;
              });
              // The provider already answered this from the video's own track list; the embed can
              // only ever add to that. It must not be allowed to *remove* availability, because it
              // reports "none" for any video whose caption module has not been loaded — which is
              // every video with captions switched off.
              if (playerRef.current?.hasCaptions() === true) setEmbedCaptionsFor(videoId);
              // Likewise the rates on offer: a new video resets them, and the menu must show what
              // is true rather than what the last video allowed.
              setRates(playerRef.current?.availableRates() ?? []);
              // Only used when the provider named nothing; see `embedAudio`.
              const embedTracks = playerRef.current?.audioTracks() ?? [];
              if (embedTracks.length > 0) setEmbedAudio({ videoId, tracks: embedTracks });
              // The viewer's chosen speed is applied rather than read back. A new video resets the
              // embed to 1x, so without this the preference was overwritten on every video and the
              // settings slider changed nothing that could be observed.
              if (state === 'playing' && ratedFor.current !== videoId) {
                ratedFor.current = videoId;
                playerRef.current?.setRate(preferredRate);
                setRate(preferredRate);
              } else {
                setRate(playerRef.current?.rate() ?? 1);
              }
              // And the tiers: they are a property of the video, so a 240p-era upload must offer
              // 240p and nothing above it.
              // Only ever the list. The viewer's choice is never revised from here.
              //
              // It was, once, so that a tier carried over from a previous video could fall back to
              // `auto` when the new one could not honour it. That reverted the choice the instant
              // it was made: the embed narrows the ladder it reports while a load is in flight, so
              // the very next state change said "2160p is not available" and undid the click. The
              // menu looked inert, which is exactly the complaint it was meant to prevent.
              //
              // Nothing is lost by keeping the choice. A tier the video does not have simply gets
              // the closest the embed can serve, and the row reports what is actually playing — so
              // the menu stays honest without ever overruling the viewer (§131).
              setQualities(playerRef.current?.availableQualities() ?? []);
              playerHandlers().onStateChange?.(state, forId);
            }}
            onPosition={(positionMs, durationMs) => {
              setAt({ positionMs, durationMs });
              // Sampled here rather than on state changes, because the rendition moves *during*
              // playback — a quality switch is the embed noticing its new viewport, which produces
              // no state change at all. Written only when it differs, so the common case is a
              // bail-out rather than a render.
              const now = playerRef.current?.currentQuality() ?? null;
              setServing((was) => (was === now ? was : now));
              // Also polled, because the embed learns about its caption module a moment after
              // playback starts. One-directional for the reason above: this can confirm captions
              // exist, never deny it. Written only when it changes, so the common case is a
              // bail-out rather than a render.
              if (playerRef.current?.hasCaptions() === true) {
                setEmbedCaptionsFor((was) => (was === videoId ? was : videoId));
              }
              if (playerRef.current?.isHighFrameRate() === true) {
                setHighFrameRateFor((was) => (was === videoId ? was : videoId));
              }
              playerHandlers().onPosition?.(positionMs, durationMs);
            }}
          />
        </div>

        {/* Captions, drawn here rather than by the embed so they can be placed. Above the poster
            and below the control bar, so a caption never covers a control or sits under artwork. */}
        {!nativeControls && captionsOn && (
          <CaptionOverlay
            tracks={session?.captionTracks ?? []}
            preferredLanguage={captionLanguage}
            positionMs={at.positionMs}
            offsetPercent={captionOffset}
            scalePercent={captionScale}
            backgroundPercent={captionBackground}
            raised={chromeVisible}
            menuOpen={settingsOpen}
          />
        )}

        {/* The video's own thumbnail, over the player until the first frame lands. The embed paints
            black while it buffers, and a black rectangle reads as broken rather than as loading. */}
        {session?.posterUrl !== undefined && erroredId !== videoId && (
          <img
            src={session.posterUrl}
            alt=""
            aria-hidden="true"
            className="pointer-events-none absolute inset-0 z-10 size-full object-cover"
            style={{
              opacity: startedId === videoId ? 0 : 1,
              transition: 'opacity 220ms var(--ease-player-out)',
            }}
          />
        )}

        {/* Everything this component draws over the picture, withheld when the viewer has
            asked for YouTube's own bar. The click surface especially: it spans the whole
            frame, so left in place it would sit on top of their controls and swallow every
            press. */}
        {!nativeControls && (
          <>
            {/* The whole frame toggles playback, as it does on YouTube. A button so it is reachable
              from the keyboard and announced; it carries no chrome of its own. */}
            <button
              type="button"
              onClick={() => {
                if (settingsOpen) {
                  setMenuFor(null);
                  return;
                }
                toggle();
              }}
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
                opacity: chromeUp ? 1 : 0,
                transition: 'opacity var(--duration-chrome) var(--ease-player-out)',
              }}
            >
              <Scrubber
                fraction={fraction}
                durationMs={at.durationMs}
                stepSeconds={seekStepSeconds}
                largeStepSeconds={seekStepLargeSeconds}
                label={t.t('player.seek')}
                onSeek={(next) => {
                  const target = next * at.durationMs;
                  setAt((current) => ({ ...current, positionMs: target }));
                  playerRef.current?.seek(target);
                }}
              />

              <div className="pointer-events-auto mt-1 flex items-center gap-1">
                <ControlButton
                  label={t.t(playing ? 'player.pause' : 'player.play')}
                  onClick={toggle}
                >
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

                <SettingsMenu
                  open={settingsOpen}
                  onOpenChange={(next) => {
                    setMenuFor(next ? videoId : null);
                    if (next) wake();
                  }}
                  rate={rate}
                  audioTracks={audioTracks}
                  audioTrack={audioTrack}
                  onAudioTrack={(next) => {
                    setChosenAudio({ videoId, id: next });
                    const language = (session?.audioTracks ?? []).find(
                      (track) => track.id === next,
                    )?.language_code;
                    playerRef.current?.setAudioTrack(next, language);
                  }}
                  rates={rates}
                  onRate={(next) => {
                    setRate(next);
                    playerRef.current?.setRate(next);
                  }}
                  quality={quality}
                  qualities={qualities}
                  serving={serving}
                  highFrameRate={highFrameRateFor === videoId}
                  onQuality={setQuality}
                  captionsAvailable={captionsAvailable}
                  captionsOn={captionsOn}
                  onCaptions={(next) => {
                    // This menu exists only under BEASTUBE's own control bar, where captions are
                    // drawn by the overlay. The embed is not asked: under YouTube's bar its own
                    // gear owns captions, and this menu is not on screen at all.
                    setCaptionsOn(next);
                  }}
                />

                <ControlButton
                  label={t.t(fullscreen ? 'player.exitFullscreen' : 'player.fullscreen')}
                  onClick={() => {
                    if (document.fullscreenElement !== null) {
                      void document.exitFullscreen().catch(() => {
                        // Already left, or refused. Nothing to recover.
                      });
                    } else {
                      void boxRef.current?.requestFullscreen().catch(() => {
                        // Refused when the gesture is not trusted, or unavailable here.
                      });
                    }
                  }}
                >
                  {fullscreen ? <Minimize2 size={20} /> : <Maximize2 size={20} />}
                </ControlButton>
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

/**
 * The settings menu, in the application's own colours.
 *
 * Two levels, the way YouTube's current player menu is built: a list of settings with their values
 * on the right, and a panel per setting behind a back button. That shape is not decoration — with
 * eight quality tiers a flat menu would be a column of radio buttons taller than the player.
 *
 * ## Everything here does something
 *
 * Speed is set through the API, which genuinely supports it, and the rates are asked of the player
 * rather than hardcoded. Subtitles are offered only for a video that has them. Quality is asked
 * for by relaying the frame — see `YouTubePlayer` — and the tiers listed are the ones the embed
 * reports for this particular video, so the menu can never offer a rendition that does not exist
 * (§131).
 *
 * The quality row shows what is being *served*, not what was clicked. Requesting a tier is a
 * resize, and the embed takes a few seconds to act on it; a row that flipped to `2160p` the
 * instant it was pressed would be describing the click rather than the picture.
 */
function SettingsMenu({
  open,
  onOpenChange,
  audioTracks,
  audioTrack,
  onAudioTrack,
  rate,
  rates,
  onRate,
  quality,
  qualities,
  serving,
  highFrameRate,
  onQuality,
  captionsAvailable,
  captionsOn,
  onCaptions,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  audioTracks: { id: string; label: string; isDefault: boolean }[];
  audioTrack: string | null;
  onAudioTrack: (id: string) => void;
  rate: number;
  rates: number[];
  onRate: (rate: number) => void;
  quality: Quality;
  qualities: Quality[];
  serving: Quality | null;
  highFrameRate: boolean;
  onQuality: (quality: Quality) => void;
  captionsAvailable: boolean;
  captionsOn: boolean;
  onCaptions: (enabled: boolean) => void;
}): ReactNode {
  const t = useTranslation();

  useEffect(() => {
    if (!open) return undefined;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onOpenChange(false);
    };
    window.addEventListener('keydown', onKeyDown);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
    };
  }, [open, onOpenChange]);

  // A player that reports neither rates nor tiers has nothing to put in a menu, so there is no
  // menu — rather than a gear that opens onto an empty panel.
  if (rates.length === 0 && qualities.length === 0) return null;

  return (
    <div className="relative">
      <ControlButton
        label={t.t('player.settings')}
        active={open}
        onClick={() => {
          onOpenChange(!open);
        }}
      >
        <Settings size={20} />
      </ControlButton>

      {/* Mounted only while open, which is what returns it to the top level. Holding the current
          panel in state up here instead would leave the menu reopening onto whichever sub-panel it
          was last closed from. */}
      {open && (
        <SettingsPanel
          close={() => {
            onOpenChange(false);
          }}
          audioTracks={audioTracks}
          audioTrack={audioTrack}
          onAudioTrack={onAudioTrack}
          rate={rate}
          rates={rates}
          onRate={onRate}
          quality={quality}
          qualities={qualities}
          serving={serving}
          highFrameRate={highFrameRate}
          onQuality={onQuality}
          captionsAvailable={captionsAvailable}
          captionsOn={captionsOn}
          onCaptions={onCaptions}
        />
      )}
    </div>
  );
}

/** Which panel of the menu is showing. */
/**
 * Captions, drawn by the application rather than by the embed.
 *
 * ## Why this exists
 *
 * The embed renders its captions inside a cross-origin iframe. Nothing outside that frame can move
 * them, resize them or restyle them — the player API exposes a font size and nothing else. Drawing
 * them here is what makes a position setting possible at all.
 *
 * The cue file is fetched through the native side: it is provider data at a provider address, and
 * the check that it *is* one lives with the provider rather than here.
 */
function CaptionOverlay({
  tracks,
  preferredLanguage,
  positionMs,
  offsetPercent,
  scalePercent,
  backgroundPercent,
  raised,
  menuOpen,
}: {
  tracks: CaptionTrack[];
  preferredLanguage: string | null;
  positionMs: number;
  offsetPercent: number;
  scalePercent: number;
  backgroundPercent: number;
  /** Whether the control bar is showing, so captions can step over it rather than behind it. */
  raised: boolean;
  /** Whether the settings menu is open, which covers the right of the frame. */
  menuOpen: boolean;
}): ReactNode {
  // The viewer's language if the video has it, otherwise whatever the provider listed first, which
  // is the video's own language. A human-written track beats a machine-written one at equal rank.
  const track =
    tracks.find((one) => preferredLanguage !== null && one.language_code === preferredLanguage) ??
    tracks.find((one) => one.is_auto_generated !== true) ??
    tracks[0];

  const cues = useAsyncResource(track === undefined ? null : `cues:${track.url}`, (signal) =>
    invoke('get_caption_cues', { url: track?.url ?? '' }, { signal }),
  );

  // Linear rather than binary: a caption file is a few hundred lines and this runs on the position
  // poll, not per frame.
  const active = (cues.data ?? []).find(
    (cue) => positionMs >= cue.start_ms && positionMs < cue.end_ms,
  );
  if (active === undefined) return null;

  return (
    <div
      aria-live="off"
      className="pointer-events-none absolute inset-x-0 z-20 flex justify-center"
      style={{
        // The control bar is about a tenth of the frame, so captions lift clear of it while it is
        // on screen and settle back when it fades.
        bottom: `${String(offsetPercent + (raised ? 8 : 0))}%`,
        paddingLeft: '8%',
        // The settings menu is opaque and sits above this, so a caption running under it was simply
        // cut in half — the text ended at the menu's edge and read as broken rather than covered.
        // Yielding the width it occupies makes the line wrap into what is left instead.
        paddingRight: menuOpen ? 'min(18rem, 42%)' : '8%',
      }}
    >
      <span
        className="rounded px-2 py-1 text-center leading-snug font-medium text-white"
        style={{
          // Scaled to the window rather than fixed, so captions stay proportionate from a small
          // window up to fullscreen, and clamped at both ends so neither becomes unreadable.
          // Deliberately not a container query: nothing above sets a container context, and the
          // unit would silently resolve to zero.
          fontSize: `clamp(13px, calc(1.7vw * ${String(scalePercent)} / 100), 44px)`,
          backgroundColor: `rgb(0 0 0 / ${String(backgroundPercent)}%)`,
          textShadow: backgroundPercent < 20 ? '0 1px 3px rgb(0 0 0 / 90%)' : undefined,
        }}
      >
        {active.text}
      </span>
    </div>
  );
}

type Panel = 'root' | 'quality' | 'speed' | 'captions' | 'audio';

/** The panel itself, mounted only while the menu is open. */
function SettingsPanel({
  close,
  audioTracks,
  audioTrack,
  onAudioTrack,
  rate,
  rates,
  onRate,
  quality,
  qualities,
  serving,
  highFrameRate,
  onQuality,
  captionsAvailable,
  captionsOn,
  onCaptions,
}: {
  close: () => void;
  audioTracks: { id: string; label: string; isDefault: boolean }[];
  audioTrack: string | null;
  onAudioTrack: (id: string) => void;
  rate: number;
  rates: number[];
  onRate: (rate: number) => void;
  quality: Quality;
  qualities: Quality[];
  serving: Quality | null;
  highFrameRate: boolean;
  onQuality: (quality: Quality) => void;
  captionsAvailable: boolean;
  captionsOn: boolean;
  onCaptions: (enabled: boolean) => void;
}): ReactNode {
  const t = useTranslation();
  const [panel, setPanel] = useState<Panel>('root');

  // What is on screen, not what was asked for. `Auto` carries the tier it resolved to, which is
  // the number a viewer checking whether the player is doing its job actually wants.
  // `1080p60` rather than `1080p`, for a video that genuinely has it.
  //
  // This is only honest because the observation behind it is reliable: every load is held at a
  // 720p-wide frame for a moment precisely so a 60fps rendition is actually seen before the frame
  // settles (`SETTLE_DELAY_MS`). Without that hold the flag stayed false at small window sizes and
  // the menu renamed itself as playback moved — `2160p60` one moment, `2160p` the next, for the
  // same entry. With it, a 60fps upload reads 60 on every tier that carries it and a 30fps upload
  // never does.
  const served = serving === null ? null : qualityLabel(serving, highFrameRate);
  // `Auto` names the tier it resolved to, but only while it is the mode in force. Reusing the
  // row's value for the option itself made the list read `144p … 144p ✓`, with no `Auto` in it.
  const autoLabel =
    quality === 'auto' && served !== null
      ? t.t('player.qualityAutoAt', { quality: served })
      : t.t('player.qualityAuto');
  const rowValue = quality === 'auto' ? autoLabel : qualityLabel(quality, highFrameRate);

  return (
    <div
      role="menu"
      aria-label={t.t('player.settings')}
      className="bg-surface-raised text-text absolute right-0 bottom-11 z-40 flex max-h-[min(22rem,60vh)] min-w-56 flex-col overflow-hidden rounded-xl py-1 shadow-lg"
    >
      {panel === 'root' && (
        <>
          {qualities.length > 0 && (
            <RootRow
              label={t.t('player.quality')}
              value={rowValue}
              onSelect={() => {
                setPanel('quality');
              }}
            />
          )}
          {captionsAvailable && (
            <RootRow
              label={t.t('player.captions')}
              value={t.t(captionsOn ? 'player.captionsOn' : 'player.captionsOff')}
              onSelect={() => {
                setPanel('captions');
              }}
            />
          )}
          {audioTracks.length > 0 && (
            <RootRow
              label={t.t('player.audio')}
              value={
                audioTracks.find((track) => track.id === audioTrack)?.label ??
                audioTracks.find((track) => track.isDefault)?.label ??
                t.t('player.audioDefault')
              }
              onSelect={() => {
                setPanel('audio');
              }}
            />
          )}
          {rates.length > 0 && (
            <RootRow
              label={t.t('player.speed')}
              value={rate === 1 ? t.t('player.speedNormal') : `${String(rate)}×`}
              onSelect={() => {
                setPanel('speed');
              }}
            />
          )}
        </>
      )}

      {panel === 'quality' && (
        <SubPanel
          title={t.t('player.quality')}
          onBack={() => {
            setPanel('root');
          }}
        >
          {/* `auto` first, as the default and the recommendation: it tracks the window, so a
              viewer who resizes gets the right rendition without coming back here. */}
          {(['auto', ...qualities] as Quality[]).map((tier) => (
            <OptionRow
              key={tier}
              label={tier === 'auto' ? autoLabel : qualityLabel(tier, highFrameRate)}
              checked={tier === quality}
              onSelect={() => {
                onQuality(tier);
                close();
              }}
            />
          ))}
        </SubPanel>
      )}

      {panel === 'audio' && (
        <SubPanel
          title={t.t('player.audio')}
          onBack={() => {
            setPanel('root');
          }}
        >
          {audioTracks.map((track) => (
            <OptionRow
              key={track.id}
              // Not decorated: the provider's own name already says "original" where it is one,
              // and adding it again read as "English (US) original (original)".
              label={track.label}
              checked={track.id === (audioTrack ?? audioTracks.find((one) => one.isDefault)?.id)}
              onSelect={() => {
                onAudioTrack(track.id);
                close();
              }}
            />
          ))}
        </SubPanel>
      )}

      {panel === 'captions' && (
        <SubPanel
          title={t.t('player.captions')}
          onBack={() => {
            setPanel('root');
          }}
        >
          {[false, true].map((on) => (
            <OptionRow
              key={String(on)}
              label={t.t(on ? 'player.captionsOn' : 'player.captionsOff')}
              checked={on === captionsOn}
              onSelect={() => {
                onCaptions(on);
                close();
              }}
            />
          ))}
        </SubPanel>
      )}

      {panel === 'speed' && (
        <SubPanel
          title={t.t('player.speed')}
          onBack={() => {
            setPanel('root');
          }}
        >
          {rates.map((option) => (
            <OptionRow
              key={option}
              label={option === 1 ? t.t('player.speedNormal') : `${String(option)}×`}
              checked={option === rate}
              onSelect={() => {
                onRate(option);
                close();
              }}
            />
          ))}
        </SubPanel>
      )}
    </div>
  );
}

/** One setting on the top level: its name, its current value, and the way into its panel. */
function RootRow({
  label,
  value,
  onSelect,
}: {
  label: string;
  value: string;
  onSelect: () => void;
}): ReactNode {
  return (
    <button
      type="button"
      role="menuitem"
      onClick={onSelect}
      className="hover:bg-surface-hover text-text flex w-full items-center gap-6 px-4 py-2 text-left text-sm"
    >
      <span className="flex-1">{label}</span>
      <span className="text-text-muted flex items-center gap-1">
        {value}
        <ChevronRight size={16} aria-hidden="true" />
      </span>
    </button>
  );
}

/** A setting's own panel, headed by the way back. */
function SubPanel({
  title,
  onBack,
  children,
}: {
  title: string;
  onBack: () => void;
  children: ReactNode;
}): ReactNode {
  const t = useTranslation();
  return (
    <>
      <button
        type="button"
        onClick={onBack}
        aria-label={t.t('app.back')}
        className="hover:bg-surface-hover text-text flex w-full items-center gap-3 px-3 py-2 text-left text-sm font-medium"
      >
        <ArrowLeft size={16} aria-hidden="true" />
        {title}
      </button>
      <div className="bg-border my-1 h-px" />
      {/* A video dubbed into twenty languages produced a list taller than the player, with the
          entries past the bottom edge unreachable. The options scroll on their own so the heading
          and its back control stay put. */}
      <div className="max-h-[min(18rem,45vh)] overflow-y-auto overscroll-contain">{children}</div>
    </>
  );
}

/** One choice inside a panel. */
function OptionRow({
  label,
  checked,
  onSelect,
}: {
  label: string;
  checked: boolean;
  onSelect: () => void;
}): ReactNode {
  return (
    <button
      type="button"
      role="menuitemradio"
      aria-checked={checked}
      onClick={onSelect}
      className={[
        'hover:bg-surface-hover flex w-full items-center justify-between gap-6 px-4 py-2 text-left text-sm',
        checked ? 'text-accent' : 'text-text',
      ].join(' ')}
    >
      <span>{label}</span>
      {checked && <span aria-hidden="true">✓</span>}
    </button>
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
  durationMs,
  stepSeconds,
  largeStepSeconds,
  label,
  onSeek,
}: {
  fraction: number;
  durationMs: number;
  stepSeconds: number;
  largeStepSeconds: number;
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
        if (event.key !== 'ArrowRight' && event.key !== 'ArrowLeft') return;
        // The viewer's own step, in seconds, converted to a fraction of this video. It used to be
        // a hardcoded five percent, which made both "skip amount" settings inert — and meant one
        // press moved eight seconds of a three-minute song and three minutes of an hour-long talk.
        // Shift takes the larger step, which is what the second setting is for.
        const seconds = event.shiftKey ? largeStepSeconds : stepSeconds;
        const step = durationMs > 0 ? (seconds * 1000) / durationMs : 0.05;
        if (event.key === 'ArrowRight') onSeek(Math.min(1, fraction + step));
        else onSeek(Math.max(0, fraction - step));
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
