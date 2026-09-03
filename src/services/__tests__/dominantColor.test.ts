/**
 * The weighting is the whole design, so it is what gets tested.
 *
 * These run against synthetic pixel buffers rather than real thumbnails: no DOM, no canvas, no
 * network, and each case states one property the glow depends on.
 */

import { describe, expect, it } from 'vitest';

import { dominantColorFromPixels } from '@/services/dominantColor';

/** Builds an RGBA buffer from a list of `[r, g, b, count]` runs. */
function pixels(...runs: [number, number, number, number][]): Uint8ClampedArray {
  const total = runs.reduce((sum, run) => sum + run[3], 0);
  const data = new Uint8ClampedArray(total * 4);
  let at = 0;
  for (const [r, g, b, count] of runs) {
    for (let i = 0; i < count; i += 1) {
      data[at] = r;
      data[at + 1] = g;
      data[at + 2] = b;
      data[at + 3] = 255;
      at += 4;
    }
  }
  return data;
}

/** Hue in degrees, for asserting on colour identity rather than exact bytes. */
function hueOf(hex: string): number {
  const r = Number.parseInt(hex.slice(1, 3), 16) / 255;
  const g = Number.parseInt(hex.slice(3, 5), 16) / 255;
  const b = Number.parseInt(hex.slice(5, 7), 16) / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const delta = max - min;
  if (delta === 0) return 0;
  let hue: number;
  if (max === r) hue = ((g - b) / delta) % 6;
  else if (max === g) hue = (b - r) / delta + 2;
  else hue = (r - g) / delta + 4;
  hue *= 60;
  return hue < 0 ? hue + 360 : hue;
}

describe('dominantColorFromPixels', () => {
  it('returns a hue that is actually in the image', () => {
    const colour = dominantColorFromPixels(pixels([220, 30, 40, 100]));
    expect(colour).not.toBeNull();
    // Red is around 0/360 degrees.
    const hue = hueOf(colour!);
    expect(Math.min(hue, 360 - hue)).toBeLessThan(20);
  });

  it('ignores letterbox bars instead of reporting black', () => {
    // The failure a plain mode would produce: the bars are the largest flat region by far.
    const colour = dominantColorFromPixels(pixels([0, 0, 0, 900], [40, 90, 220, 100]));
    expect(colour).not.toBeNull();
    const hue = hueOf(colour!);
    expect(hue).toBeGreaterThan(190);
    expect(hue).toBeLessThan(260);
  });

  it('lets a small vivid region outvote a large pale one', () => {
    // A studio thumbnail that is mostly white background must not glow white.
    const colour = dominantColorFromPixels(pixels([246, 246, 246, 800], [10, 190, 90, 60]));
    expect(colour).not.toBeNull();
    const hue = hueOf(colour!);
    expect(hue).toBeGreaterThan(90);
    expect(hue).toBeLessThan(190);
  });

  it('declines to glow for a greyscale image', () => {
    // A grey glow looks like a rendering bug, so no glow is the better answer.
    expect(dominantColorFromPixels(pixels([120, 120, 120, 400]))).toBeNull();
  });

  it('declines when everything is discarded as too dark or too bright', () => {
    expect(dominantColorFromPixels(pixels([2, 2, 2, 200], [254, 254, 254, 200]))).toBeNull();
    expect(dominantColorFromPixels(new Uint8ClampedArray(0))).toBeNull();
  });

  it('ignores transparent pixels', () => {
    const data = pixels([220, 30, 40, 4]);
    for (let i = 3; i < data.length; i += 4) data[i] = 0;
    expect(dominantColorFromPixels(data)).toBeNull();
  });

  it('normalises intensity so only the hue varies', () => {
    // A near-black blue and a near-white blue must produce the same glow: the app owns saturation
    // and lightness, the video owns only the hue.
    // Dark, but above the discard threshold: [12, 20, 70] has luma 0.086 and is correctly thrown
    // away as shadow, which is the behaviour the letterbox test relies on.
    const dark = dominantColorFromPixels(pixels([20, 34, 110, 100]));
    const light = dominantColorFromPixels(pixels([160, 180, 250, 100]));
    expect(dark).not.toBeNull();
    expect(light).not.toBeNull();
    expect(Math.abs(hueOf(dark!) - hueOf(light!))).toBeLessThan(25);
  });

  it('is deterministic', () => {
    // Two visits to the same card must not glow differently.
    const data = pixels([200, 40, 160, 50], [20, 120, 200, 50], [0, 0, 0, 200]);
    expect(dominantColorFromPixels(data)).toBe(dominantColorFromPixels(data));
  });
});
