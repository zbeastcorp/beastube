/**
 * React binding for the localization layer.
 *
 * The translator is memoized per locale, so switching language rebuilds the `Intl` formatters once
 * rather than on every render. Components consume it through {@link useTranslation}, which returns
 * a stable object — a component that only formats strings will not re-render when unrelated state
 * changes.
 */

import { createContext, useContext, useEffect, useMemo, type ReactNode } from 'react';

import { createTranslator, type Locale, type Translator } from './index';

const TranslationContext = createContext<Translator | null>(null);

/** Provides a translator bound to `locale` to the tree below it. */
export function TranslationProvider({
  locale,
  children,
}: {
  locale: Locale;
  children: ReactNode;
}): ReactNode {
  const translator = useMemo(() => createTranslator(locale), [locale]);

  // The document's own language, set where the language is actually decided.
  //
  // `index.html` ships `lang="en"` and nothing ever changed it, so a Hindi or Spanish interface
  // was still announced to a screen reader as English: the wrong voice, the wrong pronunciation
  // rules, and for Hindi a Devanagari string read through an English phoneme set. It is also what
  // `:lang()` selectors and the browser's own hyphenation and quotation rules key on.
  //
  // In an effect rather than during render because it is a write to something React does not own.
  // Unlike the theme, nothing about this is visible, so arriving a frame late costs nothing: a
  // screen reader reads the document after it exists, not during its first paint.
  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);

  return <TranslationContext value={translator}>{children}</TranslationContext>;
}

/**
 * The active translator.
 *
 * @throws if called outside a {@link TranslationProvider}. Throwing beats returning a silent
 * English fallback: the latter produces an application that looks translated in development and is
 * not in production.
 */
export function useTranslation(): Translator {
  const translator = useContext(TranslationContext);
  if (!translator) {
    throw new Error('useTranslation must be used within a TranslationProvider');
  }
  return translator;
}
