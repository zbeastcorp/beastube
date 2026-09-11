/**
 * An image that has no `src` at all until it comes near the viewport.
 *
 * This is what YouTube actually does, and it is not `loading="lazy"`. Measured on their home feed:
 * 50 tiles in the DOM, 5 image elements with a `src`, and every one of those `loading="auto"`.
 * The other 45 tiles hold no image element at all until you scroll toward them.
 *
 * The difference matters. `loading="lazy"` still creates the request eventually, still hands the
 * decoder work, and — critically — leaves the browser deciding *when*, which in a freshly painted
 * grid means "more or less immediately for everything above the fold and a bit below it". Fifty
 * thumbnails decoding during first paint is precisely the "loading and settling" the shell does on
 * startup: the layout is correct the whole time, but the main thread is too busy to paint it
 * smoothly.
 *
 * Withholding the `src` makes the cost proportional to what is on screen. Nothing off-screen
 * requests, decodes, or occupies texture memory.
 *
 * The wrapper carries the aspect ratio, so a tile has its final size before it has its picture and
 * nothing reflows when one arrives. Lazy loading without a reserved box would trade a slow first
 * paint for a jumping one, which is worse.
 *
 * Fifty `IntersectionObserver` instances is fifty sets of bookkeeping for a single question. The
 * module keeps one and dispatches to per-element callbacks.
 */

import { useCallback, useRef, useState, type ReactNode, type SyntheticEvent } from 'react';

/**
 * How far outside the viewport counts as "near".
 *
 * Roughly a screen and a half of vertical runway at typical window heights, so an image is
 * requested and decoded well before it is scrolled to and appears already there rather than
 * fading in late.
 *
 * The horizontal margin is not incidental. The shorts shelves scroll sideways, and with no
 * horizontal runway a card just off the right edge held no image at all until it was dragged into
 * view — so every sideways scroll popped in blank tiles that then filled. Six hundred pixels is
 * about three shelf cards ahead.
 */
const ROOT_MARGIN = '900px 600px';

const callbacks = new WeakMap<Element, () => void>();
let observer: IntersectionObserver | null = null;

function sharedObserver(): IntersectionObserver | null {
  // Absent under jsdom and in any non-browser environment. Callers fall back to loading eagerly,
  // which is the safe direction: a test that renders a card should see its image.
  if (typeof IntersectionObserver === 'undefined') return null;
  observer ??= new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        const run = callbacks.get(entry.target);
        if (!run) continue;
        // Fired once. The element is unobserved before the callback so a re-entrant layout read
        // cannot deliver it twice.
        observer?.unobserve(entry.target);
        callbacks.delete(entry.target);
        run();
      }
    },
    { rootMargin: ROOT_MARGIN },
  );
  return observer;
}

export interface LazyImageProps {
  src: string;
  alt: string;
  /** Applied to the `<img>` itself, not the wrapper. */
  className?: string;
  /** Reserves the box before the picture exists, e.g. `'16 / 9'` or `'9 / 16'`. */
  aspectRatio?: string;
  /** Set for images whose pixels are read back, such as the card glow's colour sampling. */
  crossOrigin?: 'anonymous' | 'use-credentials';
  onLoad?: (event: SyntheticEvent<HTMLImageElement>) => void;
  onError?: () => void;
  /** Handed the `<img>` node, for callers that measure or sample it. */
  imageRef?: React.Ref<HTMLImageElement>;
  /** Rendered in the reserved box until the picture is there. */
  placeholder?: ReactNode;
}

/** An image that costs nothing until it is nearly on screen. */
export function LazyImage({
  src,
  alt,
  className,
  aspectRatio,
  crossOrigin,
  onLoad,
  onError,
  imageRef,
  placeholder,
}: LazyImageProps): ReactNode {
  const [near, setNear] = useState(false);
  const nearRef = useRef(false);

  const attach = useCallback((node: HTMLDivElement | null) => {
    if (!node || nearRef.current) return undefined;
    const active = sharedObserver();
    if (!active) {
      // No observer available: load immediately rather than never.
      nearRef.current = true;
      setNear(true);
      return undefined;
    }
    callbacks.set(node, () => {
      nearRef.current = true;
      setNear(true);
    });
    active.observe(node);
    return () => {
      active.unobserve(node);
      callbacks.delete(node);
    };
  }, []);

  return (
    <div
      ref={attach}
      className="relative size-full"
      style={aspectRatio !== undefined ? { aspectRatio } : undefined}
    >
      {near ? (
        <img
          ref={imageRef}
          src={src}
          alt={alt}
          decoding="async"
          {...(crossOrigin !== undefined ? { crossOrigin } : {})}
          {...(onLoad ? { onLoad } : {})}
          {...(onError ? { onError } : {})}
          className={className}
        />
      ) : (
        placeholder
      )}
    </div>
  );
}
