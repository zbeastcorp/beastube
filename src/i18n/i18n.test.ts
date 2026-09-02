import { describe, expect, it } from 'vitest';

import { createTranslator, detectLocale, resolveLocale, SUPPORTED_LOCALES } from './index';

describe('locale resolution', () => {
  it('matches an exact tag', () => {
    expect(resolveLocale('hi')).toBe('hi');
    expect(resolveLocale('es')).toBe('es');
  });

  it('falls back from a regional tag to its primary subtag', () => {
    // The case that matters: es-419 and es-MX must reach Spanish, not English.
    expect(resolveLocale('es-419')).toBe('es');
    expect(resolveLocale('es-MX')).toBe('es');
    expect(resolveLocale('en-GB')).toBe('en');
  });

  it('is case-insensitive', () => {
    expect(resolveLocale('ES')).toBe('es');
    expect(resolveLocale('Hi-IN')).toBe('hi');
  });

  it('falls back to English for anything unsupported', () => {
    expect(resolveLocale('fr')).toBe('en');
    expect(resolveLocale('')).toBe('en');
    expect(resolveLocale(null)).toBe('en');
    expect(resolveLocale(undefined)).toBe('en');
  });

  it('detects a locale without throwing when navigator is unusual', () => {
    expect(SUPPORTED_LOCALES).toContain(detectLocale());
  });
});

describe('translation lookup', () => {
  it('returns the string for a known key', () => {
    expect(createTranslator('en').t('nav.home')).toBe('Home');
  });

  it('interpolates parameters', () => {
    expect(createTranslator('en').t('search.resultsFor', { query: 'cats' })).toBe(
      'Results for “cats”',
    );
  });

  it('leaves an unmatched placeholder visible rather than rendering undefined', () => {
    // A missing parameter should be obvious in development, not silently produce "undefined".
    expect(createTranslator('en').t('search.resultsFor')).toContain('{query}');
  });

  it('falls back to English for a key a locale omits', () => {
    const hindi = createTranslator('hi');
    // Translated in the Hindi catalogue.
    expect(hindi.t('nav.home')).toBe('होम');
    // Not translated: must yield the English string, never an empty label.
    expect(hindi.t('settings.about.licenses')).toBe('Open-source licences');
  });

  it('renders the key itself when it exists nowhere', () => {
    // Deliberately bypasses the type system to simulate a stale key surviving a refactor.
    const t = createTranslator('en').t as (key: string) => string;
    expect(t('does.not.exist')).toBe('does.not.exist');
  });

  it('uses the plural group other-form when addressed without a count', () => {
    expect(createTranslator('en').t('video.views')).toBe('{count} views');
  });
});

describe('pluralization', () => {
  it('selects the correct English form', () => {
    const t = createTranslator('en');
    expect(t.plural('video.views', 1)).toBe('1 view');
    expect(t.plural('video.views', 2)).toBe('2 views');
    expect(t.plural('video.views', 0)).toBe('0 views');
  });

  it('formats the interpolated count for the locale', () => {
    // English groups with commas; the count must not appear as a bare 1234567.
    expect(createTranslator('en').plural('video.views', 1_234_567)).toBe('1,234,567 views');
  });

  it('falls back to the other-form for a locale that omits a category', () => {
    // Hindi supplies both forms; asking for a large count must still produce a real string.
    expect(createTranslator('hi').plural('video.views', 5)).not.toContain('video.views');
  });
});

describe('formatting', () => {
  it('formats durations as H:MM:SS or M:SS', () => {
    const t = createTranslator('en');
    expect(t.duration(0)).toBe('0:00');
    expect(t.duration(9_000)).toBe('0:09');
    expect(t.duration(61_000)).toBe('1:01');
    expect(t.duration(3_725_000)).toBe('1:02:05');
  });

  it('clamps a negative duration instead of rendering a minus sign', () => {
    expect(createTranslator('en').duration(-5_000)).toBe('0:00');
  });

  it('formats bytes with a sensible unit', () => {
    const t = createTranslator('en');
    expect(t.bytes(0)).toBe('0 B');
    expect(t.bytes(1023)).toBe('1,023 B');
    expect(t.bytes(1024)).toBe('1 KB');
    expect(t.bytes(1_572_864)).toBe('1.5 MB');
  });

  it('renders an em dash for a nonsensical byte count', () => {
    const t = createTranslator('en');
    expect(t.bytes(Number.NaN)).toBe('—');
    expect(t.bytes(-1)).toBe('—');
  });

  it('formats large counts compactly', () => {
    expect(createTranslator('en').compact(1_200_000)).toBe('1.2M');
  });

  it('truncates relative time toward zero so 29 days is not "last month"', () => {
    const now = Date.UTC(2026, 0, 30);
    const t = createTranslator('en');
    const twentyNineDaysAgo = now - 29 * 86_400_000;
    expect(t.relative(twentyNineDaysAgo, now)).toMatch(/week/);
  });

  it('formats recent times in the nearest sensible unit', () => {
    const now = Date.UTC(2026, 0, 30, 12, 0, 0);
    const t = createTranslator('en');
    expect(t.relative(now - 30_000, now)).toMatch(/second/);
    expect(t.relative(now - 5 * 60_000, now)).toMatch(/minute/);
    expect(t.relative(now - 3 * 3_600_000, now)).toMatch(/hour/);
    expect(t.relative(now - 2 * 86_400_000, now)).toMatch(/day/);
  });
});
