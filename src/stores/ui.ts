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

  toggleSidebar: () => void;
  setSidebarCollapsed: (collapsed: boolean) => void;
  openOverlay: (overlay: Overlay) => void;
  /** Tells every playlist view that what it is showing may be out of date. */
  notePlaylistsChanged: () => void;
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
  overlay: { kind: 'none' },
  playlistRevision: 0,
  toasts: [],
  fullscreen: false,

  toggleSidebar: () => {
    set({ sidebarCollapsed: !get().sidebarCollapsed });
  },
  setSidebarCollapsed: (sidebarCollapsed) => {
    set({ sidebarCollapsed });
  },
  openOverlay: (overlay) => {
    set({ overlay });
  },
  notePlaylistsChanged: () => {
    set((state) => ({ playlistRevision: state.playlistRevision + 1 }));
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
