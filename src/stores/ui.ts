/**
 * UI shell state.
 *
 * Holds only presentation state that several unrelated components need: sidebar collapse, which
 * overlay is open, the toast queue. It deliberately does not hold provider data (that is fetched
 * and cached per view) or playback state (§89), so a toast appearing does not re-render a video
 * grid.
 */

import { create } from 'zustand';

import type { TranslationKey } from '@/i18n';
import type { VideoSummary } from '@/types/domain';

/** A transient message shown in the corner. */
export interface Toast {
  id: string;
  /** i18n key; toasts never carry pre-rendered English. */
  messageKey: string;
  params?: Record<string, string | number>;
  tone: 'info' | 'success' | 'warning' | 'danger';
  /** Milliseconds before auto-dismissal; `null` keeps it until dismissed. */
  durationMs: number | null;
  action?: { labelKey: string; run: () => void };
}

/** Overlays that take focus. At most one is open at a time. */
export type Overlay =
  | { kind: 'none' }
  | { kind: 'commandPalette' }
  | { kind: 'addToPlaylist'; video: VideoSummary }
  | { kind: 'createPlaylist' }
  | { kind: 'renamePlaylist'; id: number; currentName: string }
  | {
      kind: 'confirm';
      // Catalogue keys, not free strings: a dialog is the last place a missing translation should
      // surface, and the type is what stops one being written by hand.
      titleKey: TranslationKey;
      bodyKey: TranslationKey;
      confirmKey: TranslationKey;
      onConfirm: () => void;
    }
  | { kind: 'shortcuts' };

interface UiState {
  sidebarCollapsed: boolean;
  /**
   * True while the window is too narrow to give the expanded sidebar its 240px and still leave a
   * usable content column.
   *
   * Measured in the built application before this existed: at 800px the feed dropped to a single
   * column, and at 480px the sidebar took half the window and cut the cards off at the right edge.
   * The sidebar was a fixed 240px at every size, because the interface had no width rules at all —
   * two `lg:` utilities in the whole codebase and no width media query.
   *
   * Kept here rather than read per component so the threshold is stated once. It is driven by a
   * media query set up beside this store, not by a resize handler.
   */
  shellNarrow: boolean;
  /**
   * Whether the sidebar is open *over* the content, which is the only way it can open when narrow.
   *
   * Separate from `sidebarCollapsed` on purpose. That one is seeded from the saved preference, and
   * a viewer whose preference is "expanded" would otherwise launch a small window with the drawer
   * already covering the feed.
   */
  drawerOpen: boolean;
  overlay: Overlay;
  toasts: Toast[];
  /** True while the window is in OS fullscreen, so the shell can hide its chrome. */
  fullscreen: boolean;

  /**
   * Bumped whenever a playlist changes: created, renamed, deleted, or an item added or removed.
   *
   * Playlist views fold it into their request key, so an edit made in a dialog is reflected by
   * whichever screen is behind that dialog without either of them knowing about the other. The
   * alternative — having the dialog call back into the view that opened it — only works while that
   * view is the one on screen, which is exactly when it is least true.
   */
  playlistRevision: number;

  /**
   * Bumped by the Refresh control, and folded into every mounted request's identity.
   *
   * `useAsyncResource` watches it, so raising it re-runs exactly the fetches the current screen
   * depends on — the same page, the same scroll position, the same player, fresh data. A route
   * change or a remount would refresh far more than the user asked for and lose their place.
   */
  contentRevision: number;

  toggleSidebar: () => void;
  setSidebarCollapsed: (collapsed: boolean) => void;
  /** Closes the sidebar drawer. Called by the scrim, by Escape, and on leaving the narrow range. */
  closeDrawer: () => void;
  openOverlay: (overlay: Overlay) => void;
  /** Tells every playlist view that what it is showing may be out of date. */
  notePlaylistsChanged: () => void;
  /** Re-fetches whatever the current screen is showing, in place. */
  refreshContent: () => void;
  closeOverlay: () => void;
  setFullscreen: (fullscreen: boolean) => void;
  toast: (toast: Omit<Toast, 'id'>) => string;
  dismissToast: (id: string) => void;
}

/** Newest-first cap. Beyond this, older toasts are dropped rather than stacking off-screen. */
const MAX_TOASTS = 4;

let toastSequence = 0;

export const useUiStore = create<UiState>((set, get) => ({
  sidebarCollapsed: false,
  shellNarrow: false,
  drawerOpen: false,
  overlay: { kind: 'none' },
  playlistRevision: 0,
  contentRevision: 0,
  toasts: [],
  fullscreen: false,

  // One control, two meanings, because the sidebar itself has two. Wide, it toggles between the
  // rail and the expanded column, both of which sit in the layout. Narrow, the expanded column
  // does not fit beside anything, so the same press opens it over the content instead.
  //
  // The alternative was to force the rail when narrow and leave the button toggling a state with
  // no visible effect — a control that looks like a feature and does nothing, which is exactly
  // what §131 forbids.
  toggleSidebar: () => {
    if (get().shellNarrow) {
      set({ drawerOpen: !get().drawerOpen });
      return;
    }
    set({ sidebarCollapsed: !get().sidebarCollapsed });
  },
  setSidebarCollapsed: (sidebarCollapsed) => {
    set({ sidebarCollapsed });
  },
  closeDrawer: () => {
    set({ drawerOpen: false });
  },
  openOverlay: (overlay) => {
    set({ overlay });
  },
  notePlaylistsChanged: () => {
    set((state) => ({ playlistRevision: state.playlistRevision + 1 }));
  },
  refreshContent: () => {
    set((state) => ({ contentRevision: state.contentRevision + 1 }));
  },
  closeOverlay: () => {
    set({ overlay: { kind: 'none' } });
  },
  setFullscreen: (fullscreen) => {
    set({ fullscreen });
  },

  toast: (toast) => {
    toastSequence += 1;
    const id = `toast-${toastSequence}`;
    set({ toasts: [...get().toasts, { ...toast, id }].slice(-MAX_TOASTS) });
    return id;
  },

  dismissToast: (id) => {
    set({ toasts: get().toasts.filter((t) => t.id !== id) });
  },
}));

/**
 * The width below which the expanded sidebar stops being affordable.
 *
 * 1312px = 240 sidebar + 24 gap + 1000 content, where 1000 is what the watch page needs before its
 * two-column layout gives the player more width than the 402px rail beside it. Above this the
 * interface is exactly what it was; below it the sidebar becomes a 72px rail and the expanded form
 * is reached from the same button, over the content.
 *
 * Chosen from measurement rather than from a device category. At 1366x768 — the commonest cheap
 * Windows laptop, and 1092 CSS px once its default 125% scaling is applied — the old layout put a
 * 378px player next to 402px of recommendations. This is the threshold that stops that.
 */
const NARROW_SHELL = '(max-width: 1311px)';

/**
 * Tracks the window width once, for the whole application.
 *
 * At module scope rather than in an effect: the answer is needed by the first render, and a
 * component that discovered it afterwards would paint the wide layout and then correct itself —
 * the visible jump this exists to prevent. Nothing here needs tearing down, because it lives
 * exactly as long as the document does.
 *
 * Guarded because the store is imported by tests and by any non-browser environment, where
 * `matchMedia` is absent; those simply stay wide, which is the previous behaviour.
 */
if (typeof window !== 'undefined' && typeof window.matchMedia === 'function') {
  const query = window.matchMedia(NARROW_SHELL);
  const apply = (narrow: boolean): void => {
    // Leaving the narrow range closes the drawer: the sidebar it was standing in for is now back
    // in the layout, and leaving both would show it twice.
    useUiStore.setState(narrow ? { shellNarrow: true } : { shellNarrow: false, drawerOpen: false });
  };
  query.addEventListener('change', (event) => {
    apply(event.matches);
  });
  apply(query.matches);
}
