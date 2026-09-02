/**
 * Localization.
 *
 * Three properties matter here, and each is enforced rather than documented-and-hoped-for:
 *
 * 1. **Keys are type-checked.** {@link TranslationKey} is derived from the English catalogue, so
 *    `t('nav.hom')` fails to compile. This is what makes §109 ("all user-facing strings pass
 *    through the localization layer") survive contact with a growing codebase — a missing string
 *    is a build error, not a blank label discovered by a user.
 * 2. **Missing translations degrade, never break.** Lookup falls back locale → English → the key
 *    itself. A half-translated locale is therefore shippable, which is the only way incremental
 *    translation ever happens.
 * 3. **Formatting is delegated to `Intl`.** Plurals, numbers, dates and relative times are locale
 *    rules, not string concatenation. Hand-rolling them produces "1 videos" in English and worse
 *    in languages with more than two plural categories.
 *
 * Nothing here touches the network. Catalogues are bundled, so the application is fully localized
 * offline and no locale request ever reveals anything about the user.
 */

import { en, type Catalogue } from './locales/en';
import { es } from './locales/es';
import { hi } from './locales/hi';

/** Locales with a bundled catalogue. */
export const SUPPORTED_LOCALES = ['en', 'hi', 'es'] as const;

/** A locale this build can render. */
export type Locale = (typeof SUPPORTED_LOCALES)[number];

/** Human-readable, self-referential locale names for the language picker. */
export const LOCALE_NAMES: Record<Locale, string> = {
  en: 'English',
  hi: 'हिन्दी',
  es: 'Español',
};

/**
 * Plural forms for one key: every Intl plural category is optional except `other`, which is the
 * guaranteed fallback in every language.
 */
interface PluralForms {
  zero?: string;
  one?: string;
  two?: string;
  few?: string;
  many?: string;
  other: string;
}

type CatalogueNode = string | PluralForms | CatalogueRecord;

/**
 * A nested catalogue level.
 *
 * An interface with an index signature rather than a mapped `Record`: inside a recursive alias a
 * mapped type weakens inference enough that the locale catalogues stop being assignable to it.
 */
interface CatalogueRecord {
  readonly [key: string]: CatalogueNode;
}

/**
 * Flattens the nested catalogue into dot-separated key paths.
 *
 * A node is a leaf when it is a string or a plural-forms object; anything else recurses. The
 * `other` check is what stops `video.views` (a plural group) from expanding into `video.views.one`.
 */
type Flatten<T, Prefix extends string = ''> = T extends string
  ? Prefix
  : T extends { other: string }
    ? Prefix
    : {
        [K in keyof T & string]: Flatten<T[K], Prefix extends '' ? K : `${Prefix}.${K}`>;
      }[keyof T & string];

/** Every valid translation key, derived from the English catalogue. */
export type TranslationKey = Flatten<Catalogue>;

/** Values substituted into `{placeholder}` slots. */
export type TranslationParams = Record<string, string | number>;

/** A catalogue that may omit keys; omissions fall back to English. */
export type PartialCatalogue = DeepPartial<Catalogue>;

type DeepPartial<T> = T extends string
  ? string
  : T extends { other: string }
    ? PluralForms
    : { [K in keyof T]?: DeepPartial<T[K]> };

const CATALOGUES: Record<Locale, CatalogueNode> = {
  en,
  hi,
  es,
};

/** Whether `value` is a supported locale tag. */
export function isSupportedLocale(value: string): value is Locale {
  return (SUPPORTED_LOCALES as readonly string[]).includes(value);
}

/**
 * Resolves a BCP 47 tag to a bundled locale.
 *
 * Matches the full tag first, then the primary subtag, so `es-419` and `es-MX` both reach `es`
 * rather than silently falling back to English.
 */
export function resolveLocale(requested: string | null | undefined): Locale {
  if (!requested) return 'en';
  const normalized = requested.toLowerCase();
  if (isSupportedLocale(normalized)) return normalized;
  const primary = normalized.split('-')[0];
  if (primary && isSupportedLocale(primary)) return primary;
  return 'en';
}

/** Reads the browser/OS locale list and returns the first one this build can render. */
export function detectLocale(): Locale {
  const candidates =
    typeof navigator !== 'undefined' && navigator.languages.length > 0
      ? navigator.languages
      : ['en'];
  for (const candidate of candidates) {
    const resolved = resolveLocale(candidate);
    // resolveLocale falls back to 'en'; only accept a genuine match, so a machine set to
    // fr-FR does not stop the scan at the first entry.
    if (resolved !== 'en' || candidate.toLowerCase().startsWith('en')) {
      return resolved;
    }
  }
  return 'en';
}

function lookup(catalogue: CatalogueNode, path: readonly string[]): CatalogueNode | undefined {
  let node: CatalogueNode | undefined = catalogue;
  for (const segment of path) {
    if (node === undefined || typeof node === 'string') return undefined;
    node = (node as CatalogueRecord)[segment];
  }
  return node;
}

function isPluralForms(node: CatalogueNode): node is PluralForms {
  return typeof node === 'object' && 'other' in node && typeof node.other === 'string';
}

/**
 * Substitutes `{name}` placeholders.
 *
 * An unmatched placeholder is left verbatim rather than replaced with `undefined`, so a missing
 * parameter is visible during development instead of silently rendering "undefined views".
 */
function interpolate(template: string, params: TranslationParams | undefined): string {
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (match, name: string) => {
    const value = params[name];
    return value === undefined ? match : String(value);
  });
}

/** A bound translator for one locale. */
export interface Translator {
  /** The locale this translator renders. */
  readonly locale: Locale;
  /** Translates `key`, interpolating `params`. */
  t: (key: TranslationKey, params?: TranslationParams) => string;
  /**
   * Translates a plural key using `count` to pick the form, and makes `count` available as a
   * `{count}` placeholder pre-formatted for the locale.
   */
  plural: (key: TranslationKey, count: number, params?: TranslationParams) => string;
  /** Formats a number in the locale's convention. */
  number: (value: number, options?: Intl.NumberFormatOptions) => string;
  /** Formats a large count compactly (`1.2M`), for view and subscriber counts. */
  compact: (value: number) => string;
  /** Formats a timestamp (Unix milliseconds) as a date. */
  date: (millis: number, options?: Intl.DateTimeFormatOptions) => string;
  /** Formats a timestamp as a relative time (`3 weeks ago`). */
  relative: (millis: number, now?: number) => string;
  /** Formats a duration in milliseconds as `H:MM:SS` or `M:SS`. */
  duration: (millis: number) => string;
  /** Formats a byte count as `1.4 MB`. */
  bytes: (value: number) => string;
}

const MILLIS = {
  minute: 60_000,
  hour: 3_600_000,
  day: 86_400_000,
  week: 604_800_000,
  month: 2_629_800_000, // average Gregorian month
  year: 31_557_600_000,
} as const;

/** Creates a translator bound to `locale`. */
export function createTranslator(locale: Locale): Translator {
  const primary = CATALOGUES[locale];
  const fallback = CATALOGUES.en;
  const pluralRules = new Intl.PluralRules(locale);
  const numberFormat = new Intl.NumberFormat(locale);
  const compactFormat = new Intl.NumberFormat(locale, {
    notation: 'compact',
    maximumFractionDigits: 1,
  });
  const relativeFormat = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' });

  function resolveNode(key: string): CatalogueNode | undefined {
    const path = key.split('.');
    return lookup(primary, path) ?? lookup(fallback, path);
  }

  function t(key: TranslationKey, params?: TranslationParams): string {
    const node = resolveNode(key);
    if (typeof node === 'string') return interpolate(node, params);
    // A plural group addressed without a count: use `other`, which is always present.
    if (node !== undefined && isPluralForms(node)) return interpolate(node.other, params);
    // Rendering the key makes an omission obvious in place, rather than showing an empty element
    // that looks like a layout bug.
    return key;
  }

  function plural(key: TranslationKey, count: number, params?: TranslationParams): string {
    const node = resolveNode(key);
    if (node === undefined || typeof node === 'string' || !isPluralForms(node)) {
      return t(key, { count: numberFormat.format(count), ...params });
    }
    const category = pluralRules.select(count);
    const template = node[category] ?? node.other;
    return interpolate(template, { count: numberFormat.format(count), ...params });
  }

  function relative(millis: number, now = Date.now()): string {
    const delta = millis - now;
    const magnitude = Math.abs(delta);
    const [unit, size]: [Intl.RelativeTimeFormatUnit, number] =
      magnitude < MILLIS.minute
        ? ['second', 1000]
        : magnitude < MILLIS.hour
          ? ['minute', MILLIS.minute]
          : magnitude < MILLIS.day
            ? ['hour', MILLIS.hour]
            : magnitude < MILLIS.week
              ? ['day', MILLIS.day]
              : magnitude < MILLIS.month
                ? ['week', MILLIS.week]
                : magnitude < MILLIS.year
                  ? ['month', MILLIS.month]
                  : ['year', MILLIS.year];
    // Truncate toward zero so "29 days ago" does not round up to "1 month ago".
    return relativeFormat.format(Math.trunc(delta / size), unit);
  }

  function duration(millis: number): string {
    const totalSeconds = Math.max(0, Math.floor(millis / 1000));
    const hours = Math.floor(totalSeconds / 3600);
    const minutes = Math.floor((totalSeconds % 3600) / 60);
    const seconds = totalSeconds % 60;
    const pad = (n: number) => n.toString().padStart(2, '0');
    return hours > 0 ? `${hours}:${pad(minutes)}:${pad(seconds)}` : `${minutes}:${pad(seconds)}`;
  }

  function bytes(value: number): string {
    if (!Number.isFinite(value) || value < 0) return '—';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let size = value;
    let unit = 0;
    while (size >= 1024 && unit < units.length - 1) {
      size /= 1024;
      unit += 1;
    }
    const formatted = new Intl.NumberFormat(locale, {
      maximumFractionDigits: unit === 0 ? 0 : 1,
    }).format(size);
    return `${formatted} ${units[unit]}`;
  }

  return {
    locale,
    t,
    plural,
    number: (value, options) =>
      options ? new Intl.NumberFormat(locale, options).format(value) : numberFormat.format(value),
    compact: (value) => compactFormat.format(value),
    date: (millis, options) =>
      new Intl.DateTimeFormat(locale, options ?? { dateStyle: 'medium' }).format(new Date(millis)),
    relative,
    duration,
    bytes,
  };
}
