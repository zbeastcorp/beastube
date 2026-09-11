/**
 * Settings store.
 *
 * Owns the persisted settings document and the derived presentation state (resolved theme,
 * reduced-motion decision). It is deliberately one of several small stores rather than a slice of a
 * single global store: a component reading the accent colour must not re-render when the
 * network status changes.
 *
 * Writes are debounced before reaching the native side. Dragging a volume slider produces dozens of
 * changes per second, and each one would otherwise be an IPC round trip and a SQLite write. The
 * in-memory value updates immediately so the UI stays responsive; persistence catches up.
 */

import { create } from 'zustand';

import { invoke, normalizeError } from '@/services/ipc';
import type { ErrorPayload, Settings, Theme } from '@/types/domain';

/** The theme actually painted, after resolving `system` against the OS preference. */
export type ResolvedTheme = 'light' | 'dark' | 'amoled';

/** Interval between the last change and the write that persists it. */
const PERSIST_DEBOUNCE_MS = 400;

/**
 * Defaults used before the native side answers, and if it never does.
 *
 * Mirrors `Settings::default()` in Rust. Having them here means the first paint is correct rather
 * than unstyled, and a failure to load settings degrades to a working application.
 */
export const DEFAULT_SETTINGS: Settings = {
  version: 1,
  appearance: {
    theme: 'system',
    accent: '#ff5c5c',
    density: 'comfortable',
    sidebar_collapsed: false,
    reduced_motion: null,
    ui_scale: 1,
    language: null,
    ambient_mode: true,
  },
  playback: {
    default_quality: 'auto',
    max_quality: '1080p',
    volume: 1,
    muted: false,
    speed: 1,
    autoplay_next: true,
    autoplay_on_open: true,
    resume_playback: true,
    captions_enabled: false,
    caption_offset_percent: 10,
    caption_scale_percent: 100,
    caption_background_percent: 60,
    caption_language: null,
    audio_language: null,
    hardware_acceleration: true,
    seek_step_seconds: 5,
    seek_step_large_seconds: 10,
    // Matches the Rust default. YouTube's own bar wins because its gear reaches the player's
    // internal quality API: every tier from 144p, applied immediately.
    player_controls: 'youtube',
  },
  privacy: {
    history_enabled: true,
    search_history_enabled: true,
    local_recommendations_enabled: true,
    incognito_by_default: false,
    history_retention_days: null,
    max_search_history_entries: 500,
    cache_limit_mb: null,
  },
  filtering: {
    enabled: true,
    mode: 'standard',
    auto_update_rules: true,
    update_interval_hours: 12,
    allowlist: [],
    blocklist: [],
    custom_rules: [],
  },
  network: {
    max_concurrent_requests: 6,
    request_timeout_seconds: 30,
    connect_timeout_seconds: 10,
    max_retries: 3,
    prefetch_enabled: true,
    reduce_activity_on_battery: true,
  },
  cache: {
    memory_budget_mb: 192,
    disk_budget_mb: 1024,
    metadata_ttl_hours: 12,
    thumbnail_ttl_days: 30,
  },
  downloads: {
    // `null` rather than a path: where the Downloads folder is, is the native side's question to
    // answer, and guessing one here would show the user a location that is not the real one.
    directory: null,
    max_quality: '1080p',
    tool_path: null,
    ffmpeg_path: null,
  },
  updates: {
    // Mirrors `UpdateSettings::default()`: security fixes reach people only if they arrive, and
    // the measured alternative was that installations never updated at all.
    automatic: true,
    skip_version: null,
  },
};

/** A recursive partial, so callers can patch one nested field without rebuilding the document. */
type DeepPartial<T> = T extends object ? { [K in keyof T]?: DeepPartial<T[K]> } : T;

interface SettingsState {
  settings: Settings;
  /** False until the first load completes; the shell renders with defaults meanwhile. */
  loaded: boolean;
  /** Set when loading or saving failed, so the settings screen can surface it. */
  error: ErrorPayload | null;
  /** True while a debounced write is outstanding, for a subtle "saving" affordance. */
  saving: boolean;

  load: () => Promise<void>;
  update: (patch: DeepPartial<Settings>) => void;
  reset: () => Promise<void>;
  /** Writes any pending debounced change immediately. Called during shutdown. */
  flush: () => Promise<void>;
}

/** Merges `patch` into `base`, replacing arrays wholesale rather than concatenating. */
function mergeDeep<T>(base: T, patch: DeepPartial<T>): T {
  if (Array.isArray(base) || typeof base !== 'object' || base === null) {
    return patch as T;
  }
  const result = { ...base } as Record<string, unknown>;
  for (const [key, value] of Object.entries(patch as Record<string, unknown>)) {
    if (value === undefined) continue;
    const existing = (base as Record<string, unknown>)[key];
    result[key] =
      // `null` is a meaningful value in this document (reduced_motion, language, retention), so it
      // must overwrite rather than be treated as "no change".
      value !== null && typeof value === 'object' && !Array.isArray(value)
        ? mergeDeep(existing, value as DeepPartial<unknown>)
        : value;
  }
  return result as T;
}

let persistTimer: ReturnType<typeof setTimeout> | null = null;
let pendingWrite: Promise<void> | null = null;

export const useSettingsStore = create<SettingsState>((set, get) => {
  async function persistNow(): Promise<void> {
    if (persistTimer !== null) {
      clearTimeout(persistTimer);
      persistTimer = null;
    }
    set({ saving: true });
    try {
      // The native side sanitizes and returns the authoritative document, so out-of-range values
      // are corrected in one place rather than trusted from the UI.
      const saved = await invoke('save_settings', { settings: get().settings });
      set({ settings: saved, error: null });
    } catch (cause) {
      set({ error: normalizeError(cause) });
    } finally {
      set({ saving: false });
      pendingWrite = null;
    }
  }

  function schedulePersist(): void {
    if (persistTimer !== null) clearTimeout(persistTimer);
    persistTimer = setTimeout(() => {
      pendingWrite = persistNow();
    }, PERSIST_DEBOUNCE_MS);
  }

  return {
    settings: DEFAULT_SETTINGS,
    loaded: false,
    error: null,
    saving: false,

    load: async () => {
      try {
        const settings = await invoke('get_settings', undefined);
        set({ settings, loaded: true, error: null });
      } catch (cause) {
        // Defaults are already in place; record the failure but do not block the application.
        set({ loaded: true, error: normalizeError(cause) });
      }
    },

    update: (patch) => {
      set({ settings: mergeDeep(get().settings, patch) });
      schedulePersist();
    },

    reset: async () => {
      try {
        const settings = await invoke('reset_settings', undefined);
        set({ settings, error: null });
      } catch (cause) {
        set({ error: normalizeError(cause) });
      }
    },

    flush: async () => {
      if (persistTimer !== null) {
        await persistNow();
        return;
      }
      if (pendingWrite) await pendingWrite;
    },
  };
});

// ---------------------------------------------------------------------------------------------
// Derived presentation state
// ---------------------------------------------------------------------------------------------

/** Resolves `system` against the OS colour-scheme preference. */
export function resolveTheme(theme: Theme): ResolvedTheme {
  if (theme === 'light' || theme === 'dark' || theme === 'amoled') return theme;
  // `custom` currently paints on the dark token set; a user-defined palette overrides individual
  // variables on top of it rather than replacing the whole set.
  if (theme === 'custom') return 'dark';
  const prefersDark =
    typeof window !== 'undefined' && window.matchMedia('(prefers-color-scheme: dark)').matches;
  return prefersDark ? 'dark' : 'light';
}

/**
 * The last scheme pushed to the webview, so an unchanged one is not pushed again.
 *
 * Module scope rather than component state: there is one webview, the value is not rendered from,
 * and the check has to survive the shell remounting.
 */
let pushedScheme: 'light' | 'dark' | 'system' | null = null;

/**
 * Tells the webview which colour scheme the application is painted in.
 *
 * This is not decoration. The embedded player's settings panel — quality, speed, captions — is
 * YouTube's own document inside the `<iframe>`, and it styles itself from `prefers-color-scheme`.
 * That query answers from the *webview's* preference, which defaults to the operating system's, so
 * a user running BEASTUBE in dark mode on a light Windows got a white panel over a dark player.
 * No CSS of ours can reach into another origin to correct it; the webview preference can.
 *
 * Everything that is not `light` is dark, including AMOLED and the custom palette, which both
 * paint on the dark token set.
 */
export function applyWebviewScheme(settings: Settings): void {
  // "Match system" is handed back to the system rather than resolved here, and that distinction is
  // the whole of the fix. Pinning the window pins the webview's `prefers-color-scheme` with it, so
  // resolving `system` to a concrete value and pushing it made the next `resolveTheme` read back
  // the value we had just pinned. The setting could follow the OS exactly once and never again.
  const scheme: 'light' | 'dark' | 'system' =
    settings.appearance.theme === 'system'
      ? 'system'
      : resolveTheme(settings.appearance.theme) === 'light'
        ? 'light'
        : 'dark';
  if (scheme === pushedScheme) return;
  pushedScheme = scheme;
  void invoke('set_window_theme', { dark: scheme === 'system' ? null : scheme === 'dark' }).catch(
    () => {
      // Outside the desktop shell there is no window to theme, and inside it a refusal costs only
      // the colour of a panel we do not own.
    },
  );
}

/**
 * Applies presentation settings to the document root.
 *
 * Writing to `documentElement` rather than through React means the theme is in place before the
 * first paint, avoiding the flash of wrong theme that a render-driven approach produces.
 */
export function applyPresentation(settings: Settings): void {
  if (typeof document === 'undefined') return;
  const root = document.documentElement;
  const { appearance } = settings;

  root.dataset['theme'] = resolveTheme(appearance.theme);
  root.dataset['density'] = appearance.density;

  // `null` means "follow the OS", in which case the attribute is removed and the CSS media query
  // governs. An explicit value overrides it in both directions.
  if (appearance.reduced_motion === null) {
    delete root.dataset['reducedMotion'];
  } else {
    root.dataset['reducedMotion'] = String(appearance.reduced_motion);
  }

  if (appearance.theme === 'custom') {
    root.style.setProperty('--color-accent', appearance.accent);
  } else {
    root.style.removeProperty('--color-accent');
  }

  // Scale the root font size rather than using a CSS transform: transforms blur text and break
  // hit testing, whereas the rem-based type scale reflows correctly.
  root.style.fontSize = `${(appearance.ui_scale * 100).toFixed(2)}%`;
}
