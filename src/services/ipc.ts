/**
 * The typed IPC boundary.
 *
 * Every call into the native side goes through {@link invoke} here, and every event subscription
 * through {@link listen}. Nothing else in the frontend imports `@tauri-apps/api` directly. That
 * single choke point buys three things:
 *
 * 1. **Type safety end to end.** {@link CommandMap} pins each command's argument and return types,
 *    so a renamed field is a compile error rather than an `undefined` at runtime.
 * 2. **Errors arrive as {@link ErrorPayload}, always.** Tauri rejects with whatever the command
 *    returned; {@link normalizeError} guarantees callers get one shape, including for the "not
 *    running under Tauri" case.
 * 3. **The app runs in a plain browser.** When the Tauri runtime is absent — Playwright against the
 *    Vite dev server, or a component test — calls route to a registered mock instead of throwing.
 *    Without this, no UI test could run without building the whole native shell.
 */

import type {
  AppEventMap,
  AppEventName,
  Bookmark,
  ChannelDetails,
  ChannelId,
  ChannelTab,
  ContinuationToken,
  ErrorPayload,
  HistoryEntry,
  Page,
  PlaybackPosition,
  SearchFilters,
  SearchResults,
  Settings,
  Suggestion,
  VideoDetails,
  VideoId,
  VideoSummary,
} from '@/types/domain';
import { isErrorPayload } from '@/types/domain';

// ---------------------------------------------------------------------------------------------
// Command contract
// ---------------------------------------------------------------------------------------------

/**
 * What the application is storing on this device.
 *
 * Mirrors `commands::StorageStats`. Paths are included so the privacy panel can name the exact
 * files rather than describing them, which is what makes the local-only claim checkable (§100).
 */
export interface StorageStats {
  database_bytes: number;
  cache_bytes: number;
  history_entries: number;
  bookmark_entries: number;
  position_entries: number;
  database_path: string;
  cache_path: string;
}

/** Rule counts by category, from `filtering::diagnostics::RuleCounts`. */
export interface RuleCounts {
  total: number;
  allow: number;
  block: number;
  hide: number;
  segment: number;
}

/**
 * Content-filtering state, counts only.
 *
 * Mirrors `filtering::diagnostics::FilteringSnapshot`. There is deliberately no field here that
 * could name a URL, host, video or channel — a filtering layer sees every request, so a diagnostic
 * that carried one would amount to a browsing log (§99).
 */
export interface FilteringSnapshot {
  enabled: boolean;
  mode: string;
  active_rule_version: string | null;
  active_checksum: string | null;
  counts: RuleCounts;
  evaluated: number;
  blocked: number;
  allowed_by_rule: number;
  allowed_never_block: number;
  failed_updates: number;
  rolled_back: boolean;
  rollbacks: number;
  rollback_reason_key: string | null;
  last_updated_at: number | null;
}

/**
 * Build and environment facts, from `commands::AppInfo`.
 *
 * Read out of the running process and rendered locally. Nothing on the diagnostics screen is
 * transmitted anywhere (§121); "copy report" puts it on the clipboard so the user decides where it
 * goes.
 */
export interface AppInfo {
  app_version: string;
  target: string;
  os: string;
  arch: string;
  debug_build: boolean;
  /** `null` when the runtime version cannot be determined — absent rather than guessed. */
  webview_version: string | null;
  playback_adapter: string;
  cpu_cores: number;
  uptime_ms: number;
}

/** A creator-marked segment offered for skipping. */
export interface SkippableSegment {
  category: string;
  start_ms: number;
  end_ms: number;
  /** `skip` removes the segment; `poi` is a single point of interest, not a range. */
  action: 'skip' | 'poi' | 'chapter';
}

/**
 * Every IPC command, with its arguments and result.
 *
 * Commands are domain-oriented rather than one-per-CRUD-operation (§133): the frontend asks for
 * "the library page of history", not for a row set it then has to assemble.
 */
export interface CommandMap {
  // --- settings ---
  get_settings: { args: undefined; result: Settings };
  save_settings: { args: { settings: Settings }; result: Settings };
  reset_settings: { args: undefined; result: Settings };

  // --- search and metadata ---
  search: {
    args: { query: string; filters: SearchFilters; continuation?: ContinuationToken };
    result: SearchResults;
  };
  get_suggestions: { args: { prefix: string }; result: Suggestion[] };
  /** The user's own most recent queries; empty in incognito. */
  get_recent_searches: { args: { limit: number }; result: Suggestion[] };
  delete_search: { args: { query: string }; result: boolean };
  clear_search_history: { args: undefined; result: number };
  get_video: { args: { videoId: VideoId }; result: VideoDetails };
  get_related: { args: { videoId: VideoId }; result: Page<VideoSummary> };
  get_channel: { args: { channelId: ChannelId }; result: ChannelDetails };
  get_channel_content: {
    args: { channelId: ChannelId; tab: ChannelTab; continuation?: ContinuationToken };
    result: Page<VideoSummary>;
  };
  get_provider_capabilities: { args: undefined; result: ProviderCapabilities };

  // --- filtering ---
  get_filtering_diagnostics: { args: undefined; result: FilteringSnapshot };
  reset_filter_rules: { args: undefined; result: FilteringSnapshot };

  // --- storage ---
  get_storage_stats: { args: undefined; result: StorageStats };
  /** Deletes the extractor cache only; history, bookmarks and settings are untouched. */
  clear_cache: { args: undefined; result: StorageStats };
  get_app_info: { args: undefined; result: AppInfo };

  // --- library ---
  record_watch: { args: { video: VideoSummary }; result: null };
  get_history: { args: { limit: number; offset: number }; result: HistoryEntry[] };
  search_history: { args: { query: string; limit: number }; result: HistoryEntry[] };
  delete_history_entry: { args: { videoId: VideoId }; result: boolean };
  clear_history: { args: undefined; result: number };
  get_position: { args: { videoId: VideoId }; result: PlaybackPosition | null };
  checkpoint_playback: {
    args: { videoId: VideoId; positionMs: number; durationMs: number | null };
    result: null;
  };
  get_resumable: { args: { limit: number }; result: HistoryEntry[] };
  get_bookmarks: { args: { limit: number; offset: number }; result: Bookmark[] };
  set_bookmark: { args: { video: VideoSummary }; result: null };
  remove_bookmark: { args: { videoId: VideoId }; result: boolean };

  // --- session ---
  set_incognito: { args: { enabled: boolean }; result: boolean };
  is_incognito: { args: undefined; result: boolean };

  // --- window ---
  /** Reports that the first frame has painted, so the shell can reveal the window. */
  frontend_ready: { args: undefined; result: null };
}

/** What the active metadata provider actually supports, so the UI renders only real controls. */
export interface ProviderCapabilities {
  search_videos: boolean;
  search_channels: boolean;
  search_playlists: boolean;
  search_shorts: boolean;
  suggestions: boolean;
  video_details: boolean;
  related_videos: boolean;
  channel_details: boolean;
  channel_videos: boolean;
  channel_shorts: boolean;
  playlists: boolean;
  discovery_feed: boolean;
  captions: boolean;
  chapters: boolean;
  search_filters: boolean;
  pagination: boolean;
}

/** A valid command name. */
export type CommandName = keyof CommandMap;
/** Arguments for `C`. */
export type CommandArgs<C extends CommandName> = CommandMap[C]['args'];
/** Result of `C`. */
export type CommandResult<C extends CommandName> = CommandMap[C]['result'];

// ---------------------------------------------------------------------------------------------
// Runtime detection and mocking
// ---------------------------------------------------------------------------------------------

/** A stand-in for the native side, used in tests and in browser-only runs. */
export type IpcMock = <C extends CommandName>(
  command: C,
  args: CommandArgs<C>,
) => Promise<CommandResult<C>>;

let mockHandler: IpcMock | null = null;
const mockEventListeners = new Map<string, Set<(payload: unknown) => void>>();

/**
 * Routes IPC to `handler` instead of the native side.
 *
 * Used by component tests and by Playwright runs against the Vite dev server. Passing `null`
 * restores normal behaviour.
 */
export function setIpcMock(handler: IpcMock | null): void {
  mockHandler = handler;
}

/** Delivers a fake event to listeners registered through {@link listen}. Test-only. */
export function emitMockEvent<E extends AppEventName>(event: E, payload: AppEventMap[E]): void {
  for (const listener of mockEventListeners.get(event) ?? []) {
    listener(payload);
  }
}

/**
 * Whether the Tauri runtime is present.
 *
 * Tauri 2 injects `__TAURI_INTERNALS__` into the webview. Checking for it is how the same bundle
 * runs both inside the desktop shell and in a plain browser for tests.
 */
export function isTauriRuntime(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

// ---------------------------------------------------------------------------------------------
// Error normalization
// ---------------------------------------------------------------------------------------------

/**
 * An IPC failure, as a real `Error`.
 *
 * The payload is what the UI renders; the `Error` wrapper is what makes the value throwable
 * without losing a stack trace and what lets `instanceof` work in a `catch`. Throwing a bare
 * object — which Tauri itself does — leaves callers with a value no error boundary can classify.
 */
export class IpcError extends Error {
  /** The typed failure the UI renders. */
  readonly payload: ErrorPayload;

  constructor(payload: ErrorPayload) {
    // The message is engineer-facing only; the user-facing string comes from `message_key`.
    super(payload.diagnostic ?? payload.code);
    this.name = 'IpcError';
    this.payload = payload;
  }
}

/**
 * Coerces anything thrown across IPC into an {@link ErrorPayload}.
 *
 * A command that rejects with a plain string, or a transport that fails before reaching Rust,
 * would otherwise reach error boundaries as an untyped value that no error UI can render.
 */
export function normalizeError(cause: unknown): ErrorPayload {
  if (cause instanceof IpcError) return cause.payload;
  if (isErrorPayload(cause)) return cause;

  const diagnostic =
    cause instanceof Error
      ? `${cause.name}: ${cause.message}`
      : typeof cause === 'string'
        ? cause
        : (() => {
            try {
              return JSON.stringify(cause);
            } catch {
              return String(cause);
            }
          })();

  return {
    kind: 'permission',
    code: 'ipc.transport_failed',
    message_key: 'error.generic',
    recovery: { strategy: 'retry_manual' },
    diagnostic,
  };
}

// ---------------------------------------------------------------------------------------------
// Invocation
// ---------------------------------------------------------------------------------------------

/**
 * Calls a native command.
 *
 * Rejects with an {@link ErrorPayload}, never with a raw string or `Error`.
 *
 * `signal` aborts the *caller's interest*, not the native work: Tauri has no cancellation channel
 * for an in-flight command, so aborting stops the frontend waiting and lets it discard the result.
 * Native-side cancellation is expressed through the task scheduler, keyed by session or request
 * identifiers passed in the arguments.
 */
export async function invoke<C extends CommandName>(
  command: C,
  args: CommandArgs<C>,
  options?: { signal?: AbortSignal },
): Promise<CommandResult<C>> {
  if (options?.signal?.aborted) {
    throw new IpcError(abortedError());
  }

  const call = async (): Promise<CommandResult<C>> => {
    if (mockHandler) {
      return mockHandler(command, args);
    }
    if (!isTauriRuntime()) {
      throw new IpcError({
        kind: 'configuration',
        code: 'ipc.no_runtime',
        message_key: 'error.generic',
        recovery: { strategy: 'unrecoverable' },
        diagnostic: `IPC command "${command}" was called with no Tauri runtime and no mock installed`,
      });
    }
    const { invoke: tauriInvoke } = await import('@tauri-apps/api/core');
    return tauriInvoke<CommandResult<C>>(command, args ?? undefined);
  };

  try {
    if (!options?.signal) {
      return await call();
    }
    // Race the call against the abort signal so an obsolete request stops blocking the caller
    // immediately (§32), even though the native side keeps running to completion.
    return await new Promise<CommandResult<C>>((resolve, reject) => {
      const onAbort = () => {
        reject(new IpcError(abortedError()));
      };
      options.signal?.addEventListener('abort', onAbort, { once: true });
      call()
        .then(resolve, reject)
        .finally(() => {
          options.signal?.removeEventListener('abort', onAbort);
        });
    });
  } catch (cause) {
    throw new IpcError(normalizeError(cause));
  }
}

function abortedError(): ErrorPayload {
  return {
    kind: 'network',
    code: 'network.cancelled',
    message_key: 'error.network.cancelled',
    recovery: { strategy: 'unrecoverable' },
  };
}

/** Whether a payload represents a request the caller abandoned, which the UI should ignore. */
export function isCancellation(error: ErrorPayload): boolean {
  return error.code === 'network.cancelled';
}

// ---------------------------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------------------------

/**
 * Subscribes to a native event.
 *
 * Returns an unsubscribe function. Because subscription is asynchronous under Tauri, the returned
 * function is safe to call before the subscription completes — it records the intent and unlistens
 * as soon as the handle exists, which is what makes it correct to use directly in a React effect
 * cleanup that runs before the promise settles.
 */
export function listen<E extends AppEventName>(
  event: E,
  handler: (payload: AppEventMap[E]) => void,
): () => void {
  if (mockHandler || !isTauriRuntime()) {
    const wrapped = (payload: unknown) => {
      handler(payload as AppEventMap[E]);
    };
    const listeners = mockEventListeners.get(event) ?? new Set();
    listeners.add(wrapped);
    mockEventListeners.set(event, listeners);
    return () => {
      listeners.delete(wrapped);
    };
  }

  let unlisten: (() => void) | null = null;
  let cancelled = false;

  void import('@tauri-apps/api/event').then(async ({ listen: tauriListen }) => {
    const handle = await tauriListen<AppEventMap[E]>(event, (e) => {
      handler(e.payload);
    });
    if (cancelled) {
      handle();
    } else {
      unlisten = handle;
    }
  });

  return () => {
    cancelled = true;
    unlisten?.();
    unlisten = null;
  };
}
