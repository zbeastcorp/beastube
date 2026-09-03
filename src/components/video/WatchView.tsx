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
import { YouTubePlayer } from '@/components/video/YouTubePlayer';
import { useAsyncResource } from '@/hooks/useAsyncResource';
import { useTranslation } from '@/i18n/context';
import { invoke } from '@/services/ipc';
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

  const video = useAsyncResource(
    `video:${videoId}`,
    (signal) => invoke('get_video', { videoId }, { signal }),
    { navigation: true },
  );
  const related = useAsyncResource(`related:${videoId}`, (signal) =>
    invoke('get_related', { videoId }, { signal }),
  );
  const stored = useAsyncResource(`position:${videoId}`, (signal) =>
    invoke('get_position', { videoId }, { signal }),
  );

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

  const details = video.data;

  // Route wins over the stored position: an explicit timestamp is a deliberate request.
  //
  // `!stored.loading` is load-bearing. The resource hook deliberately retains the previous value
  // while a new request runs, so at the moment the video changes `stored.data` still holds the
  // PREVIOUS video's position — and handing that to the player would start the new video at the old
  // one's timestamp. Waiting for the fetch that belongs to this video is the only honest test.
  const resumeAt =
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

          {/* The player mounts as soon as the id is known — it does not wait for metadata, because
              the embed resolves the video itself and waiting would delay the first frame. */}
          <YouTubePlayer
            videoId={videoId}
            {...(resumeAt !== undefined ? { startAtMs: resumeAt } : {})}
            autoplay={settings.playback.autoplay_on_open}
            onStateChange={setPlaybackState}
            onPosition={(positionMs, durationMs) => {
              positionRef.current = { positionMs, durationMs };
            }}
          />
        </div>

        <div className="mt-4 flex flex-col gap-3">
          {details ? (
            <h1 className="text-text text-md leading-snug font-medium">{details.title}</h1>
          ) : (
            <div className="skeleton h-6 w-3/4 rounded" />
          )}

          <div className="border-border flex items-center gap-3 border-b pb-4">
            {details?.channel_avatar?.at(-1) && (
              <img
                src={details.channel_avatar.at(-1)?.url}
                alt=""
                className="size-10 shrink-0 rounded-full object-cover"
              />
            )}
            <div className="flex min-w-0 flex-1 flex-col">
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
            {/* Save and share, on the row with the channel — where YouTube puts them. There is no
                download button: this application cannot produce a file, and a control that cannot
                do its job is worse than its absence (§131). */}
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
