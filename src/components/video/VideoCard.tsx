/**
 * A video card, in YouTube's grid geometry.
 *
 * Three details here are performance decisions rather than cosmetic ones, and each matters when a
 * virtualized grid is scrolling:
 *
 * 1. **The thumbnail box reserves its aspect ratio before the image loads.** Without it every image
 *    that arrives shifts everything below it, producing the cumulative layout shift that makes a
 *    grid feel unstable (§90).
 * 2. **The rendition is chosen for the rendered width**, not the largest available, so a 210px card
 *    does not pull a 1280px JPEG (§35).
 * 3. **A failed thumbnail is detected by size, not by `onerror`.** The provider answers a missing
 *    thumbnail with HTTP 404 *and* valid JPEG bytes for a grey 120×90 placeholder, so `onerror`
 *    never fires; the load handler checks `naturalWidth` instead.
 */

import { memo, useRef, useState, type CSSProperties, type ReactNode } from 'react';

import { Link } from '@/app/router';
import { HoverPreview } from '@/components/video/HoverPreview';
import { useTranslation } from '@/i18n/context';
import { cachedDominantColor, sampleDominantColor } from '@/services/dominantColor';
import { bestThumbnailFor, type VideoSummary } from '@/types/domain';

/** Below this width the provider's grey "no thumbnail" placeholder is what came back. */
const PLACEHOLDER_WIDTH_THRESHOLD = 160;

/** Duration badge, bottom-right of the thumbnail. */
function DurationBadge({ video }: { video: VideoSummary }): ReactNode {
  const t = useTranslation();

  if (video.live_status === 'live') {
    return (
      <span className="bg-live absolute right-1 bottom-1 flex items-center gap-1 rounded px-1.5 py-0.5 text-2xs font-medium text-white">
        <span className="animate-live-pulse size-1.5 rounded-full bg-white" />
        {t.t('video.live')}
      </span>
    );
  }
  if (video.duration_ms === undefined) return null;
  return (
    <span className="absolute right-1 bottom-1 rounded bg-black/80 px-1 py-0.5 text-2xs font-medium text-white">
      {t.duration(video.duration_ms)}
    </span>
  );
}

/** The watched-progress bar YouTube draws along the bottom of a partly-watched thumbnail. */
function ProgressBar({ fraction }: { fraction: number }): ReactNode {
  return (
    <div className="absolute inset-x-0 bottom-0 h-1 bg-white/30">
      <div
        className="bg-brand h-full"
        style={{ width: `${Math.min(100, Math.max(0, fraction * 100)).toFixed(2)}%` }}
      />
    </div>
  );
}

interface VideoCardProps {
  video: VideoSummary;
  /** Rendered width in CSS pixels, used to pick the thumbnail rendition. */
  width?: number | undefined;
  /**
   * Fraction watched in `0..1`, when the local library knows.
   *
   * Explicitly accepts `undefined` so callers can spread a computed value under
   * `exactOptionalPropertyTypes` without building a conditional object at every call site.
   */
  progress?: number | undefined;
}

/**
 * One card in a grid.
 *
 * Memoized because a grid re-renders on every scroll frame while cards themselves rarely change;
 * this is one of the few places where memoization is justified by measurement rather than habit
 * (§88).
 */
export const VideoCard = memo(function VideoCard({
  video,
  width = 360,
  progress,
}: VideoCardProps): ReactNode {
  const t = useTranslation();
  const [thumbnailFailed, setThumbnailFailed] = useState(false);
  const [hovered, setHovered] = useState(false);
  const imageRef = useRef<HTMLImageElement>(null);

  // Request roughly two device pixels per CSS pixel so the image stays crisp on a HiDPI display.
  const thumbnail = video.thumbnails ? bestThumbnailFor(video.thumbnails, width * 2) : undefined;
  const showImage = thumbnail !== undefined && !thumbnailFailed;

  // Seeded from the cache so a card scrolled back into view paints its glow on the first render,
  // with no effect and no second pass. A miss stays null until the pointer arrives.
  const [glow, setGlow] = useState<string | null>(() => cachedDominantColor(thumbnail?.url));

  const metadata: string[] = [];
  if (video.view_count !== undefined) {
    metadata.push(
      t.plural('video.views', video.view_count, { count: t.compact(video.view_count) }),
    );
  }
  if (video.published_at !== undefined) {
    metadata.push(t.relative(video.published_at));
  } else if (video.published_text !== undefined) {
    metadata.push(video.published_text);
  }

  return (
    <article
      className="group card-glow flex flex-col gap-3"
      data-glow={hovered && glow !== null ? 'on' : undefined}
      style={glow !== null ? ({ '--card-glow-color': glow } as CSSProperties) : undefined}
      onPointerEnter={(event) => {
        // Pointer rather than mouse events, and coarse pointers are excluded: on a touch screen
        // every tap would fire an enter and start a preview the user never asked for.
        if (event.pointerType !== 'mouse') return;
        setHovered(true);

        // Sampled on hover rather than on load. A screen holds around forty cards and a viewer
        // hovers one or two; measuring every thumbnail as it decodes would do forty times the work
        // for a state almost none of them enter. The cost is well under a millisecond and it is
        // absorbed by the preview's own rest delay.
        if (glow === null && imageRef.current !== null) {
          setGlow(sampleDominantColor(imageRef.current));
        }
      }}
      onPointerLeave={() => {
        setHovered(false);
      }}
    >
      <Link
        to={{ name: 'watch', videoId: video.id }}
        className="relative block overflow-hidden rounded-md"
        // The box holds its shape before the image arrives, which is what prevents layout shift.
        // A hovered card lifts its corners the way YouTube's does, so the preview reads as the card
        // coming forward rather than as an unrelated frame appearing.
        style={{ aspectRatio: '16 / 9' }}
      >
        {showImage ? (
          <img
            ref={imageRef}
            src={thumbnail.url}
            // Measured, not assumed: i.ytimg.com answers with `Access-Control-Allow-Origin: *`, so
            // the card's own decoded pixels can be read back for the glow at no extra bytes. If
            // that ever stopped being true the image would fail to load outright rather than
            // silently tainting, and the existing onError below already renders the placeholder.
            crossOrigin="anonymous"
            alt={t.t('a11y.videoThumbnail', { title: video.title })}
            loading="lazy"
            decoding="async"
            onLoad={(event) => {
              // A 404 still returns a valid grey placeholder image, so `onerror` never fires.
              if (event.currentTarget.naturalWidth < PLACEHOLDER_WIDTH_THRESHOLD) {
                setThumbnailFailed(true);
              }
            }}
            onError={() => {
              setThumbnailFailed(true);
            }}
            className="size-full object-cover"
          />
        ) : (
          <div className="bg-surface size-full" aria-hidden="true" />
        )}

        <HoverPreview videoId={video.id} active={hovered} />

        {/* Both stay on top of the preview: the duration is still true while it plays, and losing
            the progress bar on hover would hide the one thing that says you have seen this. */}
        <DurationBadge video={video} />
        {progress !== undefined && progress > 0 && <ProgressBar fraction={progress} />}
      </Link>

      <div className="flex min-w-0 flex-col gap-1">
        <h3 className="text-text line-clamp-2 text-base leading-snug font-medium">{video.title}</h3>

        {video.channel_name !== undefined && (
          <span className="text-text-muted truncate text-xs">
            {video.channel_id ? (
              <Link
                to={{ name: 'channel', channelId: video.channel_id, tab: 'videos' }}
                className="transition-surface hover:text-text"
              >
                {video.channel_name}
              </Link>
            ) : (
              video.channel_name
            )}
          </span>
        )}

        {metadata.length > 0 && (
          <span className="text-text-muted text-xs">{metadata.join(' • ')}</span>
        )}
      </div>
    </article>
  );
});

/** A placeholder card, matching the loaded card's geometry so nothing shifts when data arrives. */
export function VideoCardSkeleton(): ReactNode {
  return (
    <div className="flex flex-col gap-3" aria-hidden="true">
      <div className="skeleton rounded-md" style={{ aspectRatio: '16 / 9' }} />
      <div className="flex flex-col gap-2">
        <div className="skeleton h-4 w-11/12 rounded" />
        <div className="skeleton h-3 w-2/3 rounded" />
        <div className="skeleton h-3 w-1/2 rounded" />
      </div>
    </div>
  );
}

/** The responsive grid YouTube uses for video cards. */
export function VideoGrid({ children }: { children: ReactNode }): ReactNode {
  return (
    <div className="grid grid-cols-[repeat(auto-fill,minmax(280px,1fr))] gap-x-4 gap-y-8">
      {children}
    </div>
  );
}
