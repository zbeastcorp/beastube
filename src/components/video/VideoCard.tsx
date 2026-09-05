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

import { BadgeCheck } from 'lucide-react';
import { memo, useRef, useState, type CSSProperties, type ReactNode } from 'react';

import { Link } from '@/app/router';
import { CardMenu } from '@/components/video/CardMenu';
import { LazyImage } from '@/components/common/LazyImage';
import { HoverPreview } from '@/components/video/HoverPreview';
import { useTranslation } from '@/i18n/context';
import { cachedDominantColor, sampleDominantColor } from '@/services/dominantColor';
import { prefetchVideoDetails } from '@/services/videoCache';
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
  /**
   * Called when the viewer removes this video from their history.
   *
   * Passed only by the surfaces where the card *is* a history entry — Continue watching, and the
   * History screen. Elsewhere the menu omits the item rather than offering to remove something
   * from a list it is not in (§131).
   */
  onRemoveFromHistory?: (() => void) | undefined;
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
  onRemoveFromHistory,
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

  // The smallest rendition offered: it is drawn at 36 CSS pixels, and the provider's largest is
  // 800 wide.
  const avatar = (video.channel_avatar ?? [])[0];

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
      className="group card-glow feed-card flex flex-col gap-3"
      data-glow={hovered && glow !== null ? 'on' : undefined}
      style={glow !== null ? ({ '--card-glow-color': glow } as CSSProperties) : undefined}
      // Hover is the best signal but not the only one, and it is unavailable to half the ways a
      // card gets opened. Focus covers keyboard navigation; pointer-down covers touch, where there
      // is no hover at all, and a mouse click quick enough that the enter and the click arrive
      // together. The cache de-duplicates, so the extra calls cost nothing when hover got there
      // first — and without them those paths waited the full round trip: measured at 530ms to a
      // title, against 41ms when the fetch had already been started.
      onFocus={() => {
        prefetchVideoDetails(video.id);
      }}
      onPointerDown={() => {
        prefetchVideoDetails(video.id);
      }}
      onPointerEnter={(event) => {
        // Pointer rather than mouse events, and coarse pointers are excluded: on a touch screen
        // every tap would fire an enter and start a preview the user never asked for.
        if (event.pointerType !== 'mouse') return;
        setHovered(true);
        // The watch page's first request, issued now instead of on the click. Costs one request
        // that is discarded when the guess is wrong, and saves the whole round trip when it is not.
        prefetchVideoDetails(video.id);

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
          <LazyImage
            imageRef={imageRef}
            src={thumbnail.url}
            // Measured, not assumed: i.ytimg.com answers with `Access-Control-Allow-Origin: *`, so
            // the card's own decoded pixels can be read back for the glow at no extra bytes. If
            // that ever stopped being true the image would fail to load outright rather than
            // silently tainting, and the existing onError below already renders the placeholder.
            crossOrigin="anonymous"
            alt={t.t('a11y.videoThumbnail', { title: video.title })}
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
            placeholder={<div className="bg-surface size-full" aria-hidden="true" />}
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

      {/* The avatar sits beside the text block rather than above it, which is the arrangement
          YouTube uses and the reason its cards read as one unit: the picture anchors the left edge
          and the three lines of text hang off it. */}
      <div className="flex min-w-0 gap-3">
        {avatar && (
          <Link
            to={
              video.channel_id
                ? { name: 'channel', channelId: video.channel_id, tab: 'videos' }
                : { name: 'watch', videoId: video.id }
            }
            className="mt-0.5 shrink-0"
            // The channel name beside it is the accessible label for this destination; a second
            // one here would have a screen reader announce the channel twice per card.
            aria-hidden="true"
            tabIndex={-1}
          >
            <img
              src={avatar.url}
              alt=""
              loading="lazy"
              decoding="async"
              className="bg-surface size-9 rounded-full object-cover"
            />
          </Link>
        )}

        <div className="flex min-w-0 flex-1 flex-col gap-1">
          <div className="flex min-w-0 items-start gap-1">
            <h3 className="text-text line-clamp-2 min-w-0 flex-1 text-base leading-snug font-medium">
              {video.title}
            </h3>
            {/* Beside the title, as YouTube places it. Hidden until the card is hovered, so a grid
                is not forty dots. */}
            <CardMenu video={video} {...(onRemoveFromHistory ? { onRemoveFromHistory } : {})} />
          </div>

          {video.channel_name !== undefined && (
            <span className="text-text-muted flex min-w-0 items-center gap-1 text-xs">
              {video.channel_id ? (
                <Link
                  to={{ name: 'channel', channelId: video.channel_id, tab: 'videos' }}
                  className="transition-surface hover:text-text truncate"
                >
                  {video.channel_name}
                </Link>
              ) : (
                <span className="truncate">{video.channel_name}</span>
              )}
              {video.channel_verified === true && (
                <BadgeCheck
                  size={13}
                  className="shrink-0"
                  // Labelled rather than decorative: "verified" is a claim about the channel, and
                  // a sighted viewer gets it from the badge. Hiding it would drop that fact.
                  aria-label={t.t('channel.verified')}
                />
              )}
            </span>
          )}

          {metadata.length > 0 && (
            <span className="text-text-muted text-xs">{metadata.join(' • ')}</span>
          )}
        </div>
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
    // `min(280px,100%)` rather than a bare 280px. `auto-fill` cannot make a track narrower than
    // its minimum, so in a column under 280px the grid overflowed its own box and the cards were
    // clipped by the shell. Identical wherever there is 280px to give, which is everywhere above
    // a very small window.
    <div className="grid grid-cols-[repeat(auto-fill,minmax(min(280px,100%),1fr))] gap-x-4 gap-y-8">
      {children}
    </div>
  );
}
