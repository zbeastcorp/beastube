/**
 * A short, as a card.
 *
 * No "New" badge, deliberately. YouTube's means "new since you last looked", which is a fact about
 * the viewer that this application does not collect — rendering one from an upload date would be a
 * different claim wearing the same badge.
 *
 * A short is a different *kind* of thing from a video, and YouTube renders it as one: portrait,
 * no duration badge, no channel line, and a tap that opens the vertical player rather than the
 * watch page. Rendering one through the landscape `VideoCard` produced a wide thumbnail with the
 * middle of a portrait frame cropped out of it — which is the complaint this component answers.
 *
 * ## A sibling of VideoCard, not a variant of it
 *
 * The two disagree about almost everything below the thumbnail: what metadata is shown, whether a
 * duration belongs, and where the link goes. Folding that into one memoised component as a set of
 * conditionals would make both harder to read and would change the memo comparison surface for the
 * eight existing `VideoCard` call sites. The shared parts — rendition selection, the placeholder
 * detection, the hover preview, the colour glow — are shared as functions and components, which is
 * the part that actually matters.
 *
 * ## Width is the caller's business
 *
 * A 9:16 box is nearly twice as tall as it is wide, so an unbounded portrait card in a grid column
 * sized for landscape cards would be about 500px tall and would set the height of every row it sits
 * in. Callers pass the width they intend, and the card never exceeds it.
 */

import { memo, useRef, useState, type CSSProperties, type ReactNode } from 'react';

import { Link } from '@/app/router';
import { HoverPreview } from '@/components/video/HoverPreview';
import { useTranslation } from '@/i18n/context';
import { cachedDominantColor, sampleDominantColor } from '@/services/dominantColor';
import { bestThumbnailFor, type VideoSummary } from '@/types/domain';

/** Below this width the provider's grey "no thumbnail" placeholder is what came back. */
const PLACEHOLDER_WIDTH_THRESHOLD = 160;

/** Default rendered width. Matches YouTube's shorts shelf, which is narrower than a video card. */
const DEFAULT_WIDTH = 180;

export interface ShortsCardProps {
  video: VideoSummary;
  /** Rendered width in CSS pixels. Also the cap: the card never grows past it. */
  width?: number | undefined;
}

/** One short in a shelf or grid. */
export const ShortsCard = memo(function ShortsCard({
  video,
  width = DEFAULT_WIDTH,
}: ShortsCardProps): ReactNode {
  const t = useTranslation();
  const [thumbnailFailed, setThumbnailFailed] = useState(false);
  const [hovered, setHovered] = useState(false);
  const imageRef = useRef<HTMLImageElement>(null);

  const thumbnail = video.thumbnails ? bestThumbnailFor(video.thumbnails, width * 2) : undefined;
  const showImage = thumbnail !== undefined && !thumbnailFailed;
  const [glow, setGlow] = useState<string | null>(() => cachedDominantColor(thumbnail?.url));

  return (
    <article
      className="group card-glow feed-card flex flex-col gap-2"
      style={
        {
          width,
          maxWidth: '100%',
          ...(glow !== null ? { '--card-glow-color': glow } : {}),
        } satisfies CSSProperties & Record<string, unknown>
      }
      data-glow={hovered && glow !== null ? 'on' : undefined}
      onPointerEnter={(event) => {
        if (event.pointerType !== 'mouse') return;
        setHovered(true);
        if (glow === null && imageRef.current !== null) {
          setGlow(sampleDominantColor(imageRef.current));
        }
      }}
      onPointerLeave={() => {
        setHovered(false);
      }}
    >
      <Link
        // The vertical player, not the watch page. A short opened in a landscape frame is the same
        // bug as a short rendered on a landscape card.
        to={{ name: 'shorts', videoId: video.id }}
        className="bg-surface relative block overflow-hidden rounded-xl"
        style={{ aspectRatio: '9 / 16' }}
      >
        {showImage ? (
          <img
            ref={imageRef}
            src={thumbnail.url}
            alt={t.t('a11y.videoThumbnail', { title: video.title })}
            loading="lazy"
            decoding="async"
            crossOrigin="anonymous"
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
      </Link>

      <div className="flex min-w-0 flex-col gap-0.5">
        <h3 className="text-text line-clamp-2 text-sm leading-snug font-medium">{video.title}</h3>
        {video.view_count !== undefined && (
          <span className="text-text-muted text-xs">
            {t.plural('video.views', video.view_count, { count: t.compact(video.view_count) })}
          </span>
        )}
      </div>
    </article>
  );
});

/** A placeholder short, matching the loaded card's geometry so nothing shifts. */
export function ShortsCardSkeleton({ width = DEFAULT_WIDTH }: { width?: number }): ReactNode {
  return (
    <div className="flex flex-col gap-2" style={{ width, maxWidth: '100%' }} aria-hidden="true">
      <div className="skeleton rounded-xl" style={{ aspectRatio: '9 / 16' }} />
      <div className="skeleton h-3.5 w-11/12 rounded" />
      <div className="skeleton h-3 w-1/2 rounded" />
    </div>
  );
}

/**
 * A horizontal row of shorts, as YouTube's home and search shelves are.
 *
 * A row rather than a grid because a shelf is a peek into a larger set, not the set itself — and
 * because a full grid of portrait cards would push everything below it off the screen.
 */
export function ShortsShelf({ children }: { children: ReactNode }): ReactNode {
  return (
    <div className="scrollbar-none flex snap-x snap-mandatory gap-4 overflow-x-auto pb-1">
      {children}
    </div>
  );
}

/** A full grid of shorts, for a screen that is entirely shorts. */
export function ShortsGrid({ children }: { children: ReactNode }): ReactNode {
  return (
    <div className="grid grid-cols-[repeat(auto-fill,minmax(160px,1fr))] gap-x-4 gap-y-6">
      {children}
    </div>
  );
}
