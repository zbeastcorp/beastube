/**
 * The watch page.
 *
 * Owns one playback session: it resolves the video, decides where to resume from, drives the
 * player, and checkpoints the position back to the local library.
 *
 * ## Checkpointing
 *
 * Position is written on a timer, on pause, on state changes and on unmount — never on every
 * sample (§47). The player reports roughly four times a second; writing each one would mean four
 * SQLite transactions per second for the whole length of a video, for information that is only ever
 * read once, when the video is reopened.
 *
 * The position itself is held in a ref rather than in state. It changes several times a second, and
 * putting it in React state would re-render the page — including the description and the related
 * list — on every tick (§89).
 */

import { useCallback, useEffect, useRef, useState } from 'react';

import { ErrorState } from '@/components/common/ErrorState';
import { ShortsCard } from '@/components/video/ShortsCard';
import { VideoActions } from '@/components/video/VideoActions';
import { VideoCard, VideoGrid } from '@/components/video/VideoCard';
import { setPlayerHandlers, usePlayerStore } from '@/stores/player';
import { useAsyncResource } from '@/hooks/useAsyncResource';
import { useTranslation } from '@/i18n/context';
import { invoke } from '@/services/ipc';
import { videoDetails } from '@/services/videoCache';
import { useSettingsStore } from '@/stores/settings';
import { isPortraitVideo, type PlaybackState, type VideoId } from '@/types/domain';

/** Interval between position checkpoints while playing. */
const CHECKPOINT_INTERVAL_MS = 10_000;

/** Below this, a stored position is not worth resuming from. Mirrors the Rust rule. */
const MIN_RESUME_MS = 15_000;

export interface WatchViewProps {
  videoId: VideoId;
  /** An explicit start position from the route, overriding the stored one. */
  startAtMs?: number;
}

/** The watch page. */
export function WatchView({ videoId, startAtMs }: WatchViewProps): React.ReactNode {
  const t = useTranslation();
  const settings = useSettingsStore((state) => state.settings);

  // Through the shared cache, so a card the pointer rested on has already answered this by the
  // time it is clicked and the page paints in one go rather than filling in around the player.
  const video = useAsyncResource(`video:${videoId}`, (signal) => videoDetails(videoId, signal), {
    navigation: true,
  });
  const related = useAsyncResource(`related:${videoId}`, (signal) =>
    invoke('get_related', { videoId }, { signal }),
  );
  const stored = useAsyncResource(`position:${videoId}`, (signal) =>
    invoke('get_position', { videoId }, { signal }),
  );

  const autoplay = settings.playback.autoplay_on_open;
  const setSession = usePlayerStore((state) => state.setSession);
  const setSlot = usePlayerStore((state) => state.setSlot);

  const [descriptionExpanded, setDescriptionExpanded] = useState(false);
  const [playbackState, setPlaybackState] = useState<PlaybackState>('loading');

  // High-frequency values live in refs: they change several times a second and nothing renders
  // from them directly.
  const positionRef = useRef({ positionMs: 0, durationMs: 0 });

  const checkpoint = useCallback(() => {
    const { positionMs, durationMs } = positionRef.current;
    if (positionMs <= 0) return;
    void invoke('checkpoint_playback', {
      videoId,
      positionMs,
      durationMs: durationMs > 0 ? durationMs : null,
    }).catch(() => {
      // A failed checkpoint costs the resume position for this video, not the session.
    });
  }, [videoId]);

  // Record the watch once the details arrive, so history shows a title rather than a bare id.
  useEffect(() => {
    if (!video.data) return;
    void invoke('record_watch', { video: video.data }).catch(() => {
      // History is best-effort; a failure must not interrupt playback.
    });
  }, [video.data]);

  // Periodic checkpoint plus one on unmount, which covers navigating away mid-video.
  useEffect(() => {
    const timer = setInterval(checkpoint, CHECKPOINT_INTERVAL_MS);
    return () => {
      clearInterval(timer);
      checkpoint();
    };
  }, [checkpoint]);

  // A pause or an end is the moment a position is most worth keeping.
  useEffect(() => {
    if (playbackState === 'paused' || playbackState === 'ended') {
      checkpoint();
    }
  }, [playbackState, checkpoint]);

  // Handlers are pushed rather than passed, because the player is not this component's child.
  //
  // Deliberately in an effect with no dependency list: it runs after every render, so the host
  // always calls this render's closures. Registering during render instead would be a side effect
  // in a render, which React is entitled to run twice or throw away.
  useEffect(() => {
    setPlayerHandlers({
      onStateChange: (state) => {
        // The video the change belongs to is ignored here: this page holds one video for its whole
        // life, so there is no other it could be about.
        setPlaybackState(state);
      },
      onPosition: (positionMs, durationMs) => {
        positionRef.current = { positionMs, durationMs };
      },
    });
  });

  useEffect(
    () => () => {
      // Leaving the page parks the player: paused, off-screen, and still alive for the next video.
      setSession(null);
      setSlot(null);
      setPlayerHandlers({});
    },
    [setSession, setSlot],
  );

  /** The video whose stored position has already been handed to the player. */
  const resumedFor = useRef<string | null>(null);

  const details = video.data;
  // Only from *this* video's metadata. The resource retains the previous value across a key change,
  // so the guard is what stops the last video's thumbnail being shown over the new one.
  const poster = video.data?.id === videoId ? details?.thumbnails?.at(-1)?.url : undefined;

  // Route wins over the stored position: an explicit timestamp is a deliberate request.
  //
  // `!stored.loading` is load-bearing. The resource hook deliberately retains the previous value
  // while a new request runs, so at the moment the video changes `stored.data` still holds the
  // PREVIOUS video's position — and handing that to the player would start the new video at the old
  // one's timestamp. Waiting for the fetch that belongs to this video is the only honest test.
  const computedResume =
    startAtMs ??
    (settings.playback.resume_playback &&
    !stored.loading &&
    stored.data &&
    stored.data.position_ms >= MIN_RESUME_MS &&
    // Do not resume into the last moments; that shows a frozen final frame rather than the video.
    (stored.data.duration_ms === undefined ||
      stored.data.duration_ms - stored.data.position_ms > 20_000)
      ? stored.data.position_ms
      : undefined);

  /**
   * The resume position, decided once per video and then held.
   *
   * Latched because `resumeAt` feeds the session, and the session is what tells the player where to
   * start. Refetching the stored position for the *same* video — which the Refresh button in the
   * top bar does — produced a new checkpoint written by the video that is currently playing, so the
   * session changed under a running player and threw it back to that checkpoint. Refresh reloaded
   * the page's data and rewound the video with it.
   *
   * Deciding once per video is also simply what the value means: where to *begin*. After that the
   * playhead belongs to the player.
   */
  const resumeAt = computedResume;

  // What to play. Set after `resumeAt` is known so a stored position is honoured on the first
  // attempt rather than by seeking a moment after playback has already started somewhere else.
  useEffect(() => {
    // A resume position is where to *begin*, so it is handed to the player once per video and
    // never again. The position is also a moving target: the video currently playing writes its
    // own checkpoints, so refetching it — which the Refresh button in the top bar does — produced
    // a newer timestamp for the same video, changed the session under a running player, and threw
    // playback back to that checkpoint. Refresh reloaded the page's data and rewound the video.
    //
    // The ref is written from inside the effect rather than during render, which is what keeps
    // this safe under concurrent rendering.
    const applying = resumedFor.current !== videoId && resumeAt !== undefined;
    if (applying) resumedFor.current = videoId;

    setSession({
      videoId,
      ...(applying ? { startAtMs: resumeAt } : {}),
      autoplay,
      ...(poster !== undefined ? { posterUrl: poster } : {}),
    });
  }, [videoId, resumeAt, autoplay, poster, setSession]);

  if (video.error && !details) {
    return <ErrorState error={video.error} onRetry={video.reload} />;
  }

  const relatedItems = related.data?.items ?? [];

  // The largest rendition, because this image is scaled up and blurred: a small one would band.
  const ambientSource = details?.thumbnails?.at(-1)?.url;

  return (
    <div className="flex flex-col gap-6 lg:flex-row">
      <div className="min-w-0 flex-1">
        <div className="relative">
          {/* Ambient glow. A scaled, heavily blurred copy of the poster frame behind the player,
              which is what YouTube's ambient mode amounts to visually: the video's own colours
              spilling past its edges. It cannot be sampled from the video itself — the embed is
              cross-origin, so its pixels are not readable — and the poster frame is the same
              image the player shows before playback anyway. Purely decorative, so it is hidden
              from assistive technology and never intercepts a click. */}
          {settings.appearance.ambient_mode && ambientSource !== undefined && (
            <img
              src={ambientSource}
              alt=""
              aria-hidden="true"
              // Reaches well past the player's own box so the colour spills into the space either
              // side of it, which is where the effect actually reads — inside the frame it is
              // hidden by the video the moment the embed paints.
              className="pointer-events-none absolute -inset-x-24 -inset-y-10 -z-10 size-auto scale-105 object-cover opacity-50 blur-[64px] saturate-150"
            />
          )}

          {/* The slot, not the player.
              The player itself is mounted once by `PlayerHost`, above the router, and positioned
              over this box — so arriving here from Home costs one `loadVideoById` rather than a
              fresh `<iframe>` and a full embed bootstrap. This element is only ever an empty box
              of the right shape; it reserves the layout so nothing shifts when the picture
              appears. */}
          <div ref={setSlot} className="w-full" style={{ aspectRatio: '16 / 9' }} />
        </div>

        <div className="mt-4 flex flex-col gap-3">
          {details ? (
            <h1 className="text-text text-md leading-snug font-medium">{details.title}</h1>
          ) : (
            <div className="skeleton h-6 w-3/4 rounded" />
          )}

          {/* Wraps rather than crushes. The action pills have a fixed width and the channel
              block does not, so on a narrow window the old row squeezed the channel name to
              nothing and *still* pushed the last button off the right edge. Wrapping puts the
              actions on their own line instead, which is what YouTube does at the same width. */}
          <div className="border-border flex flex-wrap items-center gap-x-3 gap-y-3 border-b pb-4">
            {details?.channel_avatar?.at(-1) && (
              <img
                src={details.channel_avatar.at(-1)?.url}
                alt=""
                className="size-10 shrink-0 rounded-full object-cover"
              />
            )}
            {/* `basis-48` is the width below which the channel block stops sharing the line and
                the actions wrap under it, rather than both getting too little to read. */}
            <div className="flex min-w-0 flex-[1_1_12rem] flex-col">
              <span className="text-text truncate text-sm font-medium">
                {details?.channel_name ?? ''}
              </span>
              {details?.channel_subscriber_count !== undefined && (
                <span className="text-text-muted text-xs">
                  {t.plural('video.subscribers', details.channel_subscriber_count, {
                    count: t.compact(details.channel_subscriber_count),
                  })}
                </span>
              )}
            </div>
            {/* Save, download and share, on the row with the channel — where YouTube puts them.
                The download control is present only on a computer that has `yt-dlp` installed,
                because that is the program which actually produces the file (ADR-0003); without
                it the button would be a control that cannot do its job (§131). */}
            {details && <VideoActions video={details} orientation="horizontal" />}
          </div>

          {details && (
            <div className="bg-surface rounded-lg p-3">
              <div className="text-text-muted mb-2 flex gap-2 text-xs font-medium">
                {details.view_count !== undefined && (
                  <span>
                    {t.plural('video.views', details.view_count, {
                      count: t.compact(details.view_count),
                    })}
                  </span>
                )}
                {details.published_at !== undefined && (
                  <span>{t.relative(details.published_at)}</span>
                )}
                {details.published_at === undefined && details.published_text !== undefined && (
                  <span>{details.published_text}</span>
                )}
              </div>

              {details.description !== undefined && details.description.length > 0 && (
                <>
                  <p
                    className={[
                      'text-text selectable text-sm whitespace-pre-wrap',
                      descriptionExpanded ? '' : 'line-clamp-3',
                    ].join(' ')}
                  >
                    {details.description}
                  </p>
                  <button
                    type="button"
                    onClick={() => {
                      setDescriptionExpanded((expanded) => !expanded);
                    }}
                    className="text-text-muted hover:text-text mt-2 text-xs font-medium"
                  >
                    {t.t(descriptionExpanded ? 'video.showLess' : 'video.showMore')}
                  </button>
                </>
              )}
            </div>
          )}
        </div>
      </div>

      <aside className="w-full shrink-0 lg:w-[402px]">
        <h2 className="text-text mb-3 text-base font-medium">{t.t('video.related')}</h2>
        {relatedItems.length > 0 ? (
          <VideoGrid>
            {/* The rail is a fixed 402px column, so a portrait card is told its width explicitly:
                left to the grid it would be more than twice as tall as the landscape ones. */}
            {relatedItems.map((item) =>
              isPortraitVideo(item) ? (
                <ShortsCard key={item.id} video={item} width={168} />
              ) : (
                <VideoCard key={item.id} video={item} width={168} />
              ),
            )}
          </VideoGrid>
        ) : (
          <div className="flex flex-col gap-3">
            {Array.from({ length: 4 }, (_, index) => (
              <div key={index} className="skeleton h-20 rounded-md" />
            ))}
          </div>
        )}
      </aside>
    </div>
  );
}
