/**
 * UI shell state.
 *
 * Holds only presentation state that several unrelated components need: sidebar collapse, which
 * overlay is open, the toast queue. It deliberately does not hold provider data (that is fetched
 * and cached per view) or playback state (§89), so a toast appearing does not re-render a video
 * grid.
 */

import { create } from 'zustand';

import type { ErrorPayload, VideoSummary } from '@/types/domain';

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
      titleKey: string;
      bodyKey: string;
      confirmKey: string;
      onConfirm: () => void;
    }
  | { kind: 'shortcuts' };

interface UiState {
  sidebarCollapsed: boolean;
  overlay: Overlay;
  toasts: Toast[];
  /** True while the window is in OS fullscreen, so the shell can hide its chrome. */
  fullscreen: boolean;
  /** Set when a fatal shell-level error occurs, rendered by the root error surface. */
  fatalError: ErrorPayload | null;

  toggleSidebar: () => void;
  setSidebarCollapsed: (collapsed: boolean) => void;
  openOverlay: (overlay: Overlay) => void;
  closeOverlay: () => void;
  setFullscreen: (fullscreen: boolean) => void;
  setFatalError: (error: ErrorPayload | null) => void;
  toast: (toast: Omit<Toast, 'id'>) => string;
  dismissToast: (id: string) => void;
}

/** Newest-first cap. Beyond this, older toasts are dropped rather than stacking off-screen. */
const MAX_TOASTS = 4;

let toastSequence = 0;

export const useUiStore = create<UiState>((set, get) => ({
  sidebarCollapsed: false,
  overlay: { kind: 'none' },
  toasts: [],
  fullscreen: false,
  fatalError: null,

  toggleSidebar: () => {
    set({ sidebarCollapsed: !get().sidebarCollapsed });
  },
  setSidebarCollapsed: (sidebarCollapsed) => {
    set({ sidebarCollapsed });
  },
  openOverlay: (overlay) => {
    set({ overlay });
  },
  closeOverlay: () => {
    set({ overlay: { kind: 'none' } });
  },
  setFullscreen: (fullscreen) => {
    set({ fullscreen });
  },
  setFatalError: (fatalError) => {
    set({ fatalError });
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
