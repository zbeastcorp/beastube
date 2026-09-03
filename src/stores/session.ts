/**
 * Session state: incognito and connectivity.
 *
 * Incognito lives here rather than in settings because it is a property of *this run*, not a
 * persisted preference — leaving it in the settings document would risk it surviving a restart,
 * which is the opposite of what the mode promises (§50).
 */

import { create } from 'zustand';

import { invoke, normalizeError } from '@/services/ipc';
import type { ErrorPayload, NetworkStatus } from '@/types/domain';

interface SessionState {
  /** When true, nothing this session does is written to the library. */
  incognito: boolean;
  networkStatus: NetworkStatus;
  /** Set once the native side reports it is ready to serve commands. */
  ready: boolean;
  error: ErrorPayload | null;

  setIncognito: (enabled: boolean) => Promise<void>;
  setNetworkStatus: (status: NetworkStatus) => void;
  setReady: (ready: boolean) => void;
}

export const useSessionStore = create<SessionState>((set) => ({
  incognito: false,
  networkStatus: 'online',
  ready: false,
  error: null,

  setIncognito: async (enabled) => {
    // Flip optimistically so the incognito chrome appears instantly; the native side is the
    // authority on whether history writes are actually suppressed, and corrects us if it disagrees.
    set({ incognito: enabled });
    try {
      const actual = await invoke('set_incognito', { enabled });
      set({ incognito: actual, error: null });
    } catch (cause) {
      set({ incognito: !enabled, error: normalizeError(cause) });
    }
  },

  setNetworkStatus: (networkStatus) => {
    set({ networkStatus });
  },
  setReady: (ready) => {
    set({ ready });
  },
}));

/** Whether speculative work should be suppressed for the current connectivity. */
export function shouldSuppressPrefetch(status: NetworkStatus): boolean {
  return status !== 'online';
}
