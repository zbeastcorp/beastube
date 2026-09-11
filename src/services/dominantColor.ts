/**
 * The colour a video's thumbnail is "about".
 *
 * Used to tint a card's hover glow, the way YouTube's cards pick up the colours of what they hold.
 * One number comes out of each thumbnail: a hue that is genuinely present in the image.
 *
 * **Not an average.** The arithmetic mean of a colourful image converges on grey-brown, because
 * complementary hues cancel component-wise. That is not a tuning problem to be fixed with a
 * multiplier — it is what the mean of a multimodal distribution *is*.
 *
 * **Not k-means.** Even k=3 over a thousand pixels is tens of thousands of distance computations
 * per card on the main thread, and without a fixed seed the same thumbnail yields a different
 * colour on a second visit, which reads as a bug. A glow needs one colour, not a palette.
 *
 * A bucket histogram is a single O(n) pass with integer-only inner work, it is deterministic, and
 * the mode of the distribution is by definition a colour that actually appears in the picture.
 *
 * A plain mode returns black for most thumbnails, because letterbox bars and shadow are the largest
 * flat region in a great many of them. So near-black and blown-white pixels are discarded outright,
 * and the remaining votes are weighted by saturation: a small vivid region beats a large grey one,
 * which is exactly the judgement a person makes when asked what colour a picture is.
 *
 * Saturation and lightness are clamped into a narrow band before the colour is returned. Without
 * that, a dark thumbnail yields an invisible glow and a neon one yields something that hurts to
 * look at. Clamping in JavaScript rather than with CSS `color-mix` keeps the result deterministic
 * and testable, and drops a browser-feature dependency for about twenty lines.
 *
 * Reading pixels back from a canvas that has drawn a cross-origin image throws unless the image was
 * fetched with CORS. `i.ytimg.com` answers with `Access-Control-Allow-Origin: *` — measured, not
 * assumed — so `crossOrigin="anonymous"` on the card's own `<img>` is enough, and sampling the
 * already-decoded element costs no extra bytes. Every failure path here returns `null`, and a
 * `null` glow is simply no glow.
 */

/** Edge length of the sampling canvas. 16x9 = 144 pixels, which is ample for a single hue. */
const SAMPLE_WIDTH = 16;
const SAMPLE_HEIGHT = 9;

/** Bits kept per channel when bucketing. 4 bits each gives 4096 buckets. */
const BUCKET_SHIFT = 4;
const BUCKET_COUNT = 4096;

/** Pixels darker or brighter than these are discarded: letterbox bars and blown highlights. */
const MIN_LUMA = 0.1;
const MAX_LUma = 0.92;

/** Below this saturation the whole image is treated as greyscale and gets no glow. */
const MIN_RESULT_SATURATION = 0.08;

/** The band every returned colour is squeezed into, so the glow is the app's, not the video's. */
const GLOW_SATURATION = { min: 0.35, max: 0.75 } as const;
const GLOW_LIGHTNESS = { min: 0.45, max: 0.62 } as const;

/** How many results to keep. Bounded so a long session cannot grow it without limit. */
const CACHE_LIMIT = 512;

/**
 * Results keyed by thumbnail URL, not by video id.
 *
 * A card picks its rendition by rendered width, so the same video can be sampled from a 168px
 * thumbnail in the related rail and a 360px one in the grid. Keying on the URL means each set of
 * pixels is measured once and neither result is attributed to the other.
 *
 * `null` is cached too: a thumbnail that could not be sampled must not be retried on every hover.
 */
const cache = new Map<string, string | null>();

/** Scratch canvas, allocated once. Per-card allocation is the cost this module exists to avoid. */
let scratch: { canvas: HTMLCanvasElement; ctx: CanvasRenderingContext2D } | null = null;

function scratchContext(): { canvas: HTMLCanvasElement; ctx: CanvasRenderingContext2D } | null {
  if (scratch) return scratch;
  if (typeof document === 'undefined') return null;

  const canvas = document.createElement('canvas');
  canvas.width = SAMPLE_WIDTH;
  canvas.height = SAMPLE_HEIGHT;
  // `willReadFrequently` is the highest-leverage flag here: without it the backing store stays on
  // the GPU and every read back is a pipeline stall rather than a memcpy.
  const ctx = canvas.getContext('2d', { willReadFrequently: true });
  if (!ctx) return null;

  scratch = { canvas, ctx };
  return scratch;
}

/** Vote accumulators, allocated once and cleared per sample. */
const counts = new Float64Array(BUCKET_COUNT);
const sumR = new Float64Array(BUCKET_COUNT);
const sumG = new Float64Array(BUCKET_COUNT);
const sumB = new Float64Array(BUCKET_COUNT);

function remember(url: string, colour: string | null): string | null {
  if (cache.size >= CACHE_LIMIT) {
    // Insertion-order eviction. Each entry is a few dozen bytes, so this is not about memory
    // pressure — it is about an unbounded Map in a long-lived desktop session.
    const oldest = cache.keys().next();
    if (!oldest.done) cache.delete(oldest.value);
  }
  cache.set(url, colour);
  return colour;
}

/** Converts `0..255` RGB to a hex string. */
function toHex(r: number, g: number, b: number): string {
  const channel = (value: number) =>
    Math.max(0, Math.min(255, Math.round(value)))
      .toString(16)
      .padStart(2, '0');
  return `#${channel(r)}${channel(g)}${channel(b)}`;
}

/** RGB in `0..255` to HSL with each component in `0..1`. */
function rgbToHsl(r: number, g: number, b: number): [number, number, number] {
  const rn = r / 255;
  const gn = g / 255;
  const bn = b / 255;
  const max = Math.max(rn, gn, bn);
  const min = Math.min(rn, gn, bn);
  const lightness = (max + min) / 2;
  const delta = max - min;

  if (delta === 0) return [0, 0, lightness];

  const saturation = delta / (1 - Math.abs(2 * lightness - 1));
  let hue: number;
  if (max === rn) hue = ((gn - bn) / delta) % 6;
  else if (max === gn) hue = (bn - rn) / delta + 2;
  else hue = (rn - gn) / delta + 4;

  hue *= 60;
  if (hue < 0) hue += 360;
  return [hue, saturation, lightness];
}

/** HSL with hue in degrees and the rest in `0..1`, back to `0..255` RGB. */
function hslToRgb(hue: number, saturation: number, lightness: number): [number, number, number] {
  const chroma = (1 - Math.abs(2 * lightness - 1)) * saturation;
  const secondary = chroma * (1 - Math.abs(((hue / 60) % 2) - 1));
  const match = lightness - chroma / 2;

  const [r, g, b] =
    hue < 60
      ? [chroma, secondary, 0]
      : hue < 120
        ? [secondary, chroma, 0]
        : hue < 180
          ? [0, chroma, secondary]
          : hue < 240
            ? [0, secondary, chroma]
            : hue < 300
              ? [secondary, 0, chroma]
              : [chroma, 0, secondary];

  return [(r + match) * 255, (g + match) * 255, (b + match) * 255];
}

const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value));

/**
 * Picks a glow colour out of raw RGBA pixels.
 *
 * Exported so the weighting can be tested against synthetic pixel data, with no DOM and no network.
 *
 * @param pixels RGBA bytes, four per pixel, as `getImageData` returns them.
 * @returns an `#rrggbb` string, or `null` when the image has no colour worth showing.
 */
export function dominantColorFromPixels(pixels: Uint8ClampedArray): string | null {
  counts.fill(0);
  sumR.fill(0);
  sumG.fill(0);
  sumB.fill(0);

  let voted = false;

  for (let index = 0; index + 3 < pixels.length; index += 4) {
    const r = pixels[index] ?? 0;
    const g = pixels[index + 1] ?? 0;
    const b = pixels[index + 2] ?? 0;
    const alpha = pixels[index + 3] ?? 0;
    if (alpha < 128) continue;

    const luma = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
    if (luma < MIN_LUMA || luma > MAX_LUma) continue;

    const max = Math.max(r, g, b);
    const min = Math.min(r, g, b);
    const saturation = max === 0 ? 0 : (max - min) / max;

    // Saturation carries the vote, damped for pixels far from mid-lightness. A large pale region
    // therefore loses to a small vivid one, which is the judgement a person makes.
    const weight = saturation * (1 - Math.abs(luma - 0.5) * 2 * 0.6);
    if (weight <= 0) continue;

    const bucket = ((r >> BUCKET_SHIFT) << 8) | ((g >> BUCKET_SHIFT) << 4) | (b >> BUCKET_SHIFT);

    counts[bucket] = (counts[bucket] ?? 0) + weight;
    sumR[bucket] = (sumR[bucket] ?? 0) + r * weight;
    sumG[bucket] = (sumG[bucket] ?? 0) + g * weight;
    sumB[bucket] = (sumB[bucket] ?? 0) + b * weight;
    voted = true;
  }

  if (!voted) return null;

  let best = 0;
  let bestWeight = 0;
  for (let bucket = 0; bucket < BUCKET_COUNT; bucket += 1) {
    const weight = counts[bucket] ?? 0;
    if (weight > bestWeight) {
      bestWeight = weight;
      best = bucket;
    }
  }
  if (bestWeight <= 0) return null;

  // The winning bucket's centroid rather than its midpoint: this recovers the precision the 4-bit
  // quantisation threw away, for the cost of three divisions.
  const r = (sumR[best] ?? 0) / bestWeight;
  const g = (sumG[best] ?? 0) / bestWeight;
  const b = (sumB[best] ?? 0) / bestWeight;

  const [hue, saturation] = rgbToHsl(r, g, b);
  if (saturation < MIN_RESULT_SATURATION) return null;

  const [outR, outG, outB] = hslToRgb(
    hue,
    clamp(saturation, GLOW_SATURATION.min, GLOW_SATURATION.max),
    clamp(0.55, GLOW_LIGHTNESS.min, GLOW_LIGHTNESS.max),
  );
  return toHex(outR, outG, outB);
}

/**
 * The cached colour for a thumbnail URL, if it has already been measured.
 *
 * Synchronous and side-effect free, so a card can use it as a lazy `useState` initialiser and paint
 * the right glow on its first render when scrolling back to something already seen.
 */
export function cachedDominantColor(url: string | undefined): string | null {
  if (url === undefined) return null;
  return cache.get(url) ?? null;
}

/**
 * Measures a decoded image element and caches the result.
 *
 * Takes the element the card is already showing rather than fetching its own copy: a second
 * `Image` with the same URL is a cache miss when the HTTP cache is partitioned by request mode, so
 * it would mean a second network fetch per card for pixels already in memory.
 *
 * Returns `null` — and caches `null` — when the image is not decoded, is not CORS-clean, or has no
 * colour worth showing.
 */
export function sampleDominantColor(image: HTMLImageElement): string | null {
  const url = image.currentSrc || image.src;
  if (!url) return null;

  const cached = cache.get(url);
  if (cached !== undefined) return cached;

  if (!image.complete || image.naturalWidth === 0) return null;

  const context = scratchContext();
  if (!context) return remember(url, null);

  try {
    context.ctx.drawImage(image, 0, 0, SAMPLE_WIDTH, SAMPLE_HEIGHT);
    // Throws a SecurityError if the canvas is tainted, which is the whole CORS failure path.
    const { data } = context.ctx.getImageData(0, 0, SAMPLE_WIDTH, SAMPLE_HEIGHT);
    return remember(url, dominantColorFromPixels(data));
  } catch {
    return remember(url, null);
  }
}
