/**
 * What the automatic update is doing, so the shell can show it.
 *
 * Held in its own store rather than in component state because the update outlives every screen:
 * it starts moments after launch, continues while the viewer navigates, and ends by taking the
 * application down and bringing it back. Nothing that unmounts can own it.
 *
 * Deliberately not part of the settings or session stores — a progress figure changing several
 * times a second must not re-render anything that reads those (§89).
 */

import { create } from 'zustand';

/** Where an update has got to. */
export type UpdateStage =
  | { kind: 'idle' }
  /** Bytes are arriving. `percent` is absent when the server reports no length. */
  | { kind: 'downloading'; version: string; percent: number | null }
  /** The download is complete and the installer is running. */
  | { kind: 'installing'; version: string }
  /** The installer handed control back; the application is coming back up. */
  | { kind: 'restarting'; version: string }
  /** It did not work. The viewer keeps the version they have. */
  | { kind: 'failed'; version: string };

interface UpdateState {
  stage: UpdateStage;
  /** Whether the viewer has dismissed the card for this attempt. */
  dismissed: boolean;
  set: (stage: UpdateStage) => void;
  dismiss: () => void;
}

export const useUpdateStore = create<UpdateState>((set) => ({
  stage: { kind: 'idle' },
  dismissed: false,
  // A new stage un-dismisses: a failure after a dismissed download is worth seeing, and the next
  // launch's attempt is not the one that was waved away.
  set: (stage) => {
    set({ stage, dismissed: false });
  },
  dismiss: () => {
    set({ dismissed: true });
  },
}));
