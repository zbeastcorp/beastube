/**
 * How much navigational work is in flight.
 *
 * Exists for exactly one consumer: the red bar across the top of the window. It is a refcount
 * rather than a boolean because two views can be fetching at once — arriving on Home starts the
 * recommended feed and the shorts shelf together, and the bar should stay up until both are done
 * rather than snapping shut when the first one lands.
 *
 * ## Why not track every IPC call
 *
 * Because most of them are not a navigation. Suggestions fire on every keystroke, watch positions
 * checkpoint on a timer, bookmark state is read per card. A bar that flashed for those would be
 * noise, and worse, it would be *lying* about what it means. Only a view's primary fetch opts in,
 * through `useAsyncResource`'s `navigation` option, so the bar means one thing: the screen you just
 * asked for is still being fetched.
 */

import { create } from 'zustand';

interface ProgressState {
  /** Navigational fetches currently in flight. */
  pending: number;
  begin: () => void;
  end: () => void;
}

export const useProgressStore = create<ProgressState>((set) => ({
  pending: 0,
  begin: () => {
    set((state) => ({ pending: state.pending + 1 }));
  },
  end: () => {
    // Clamped at zero. An unbalanced `end` — a component unmounting mid-flight after its own
    // cleanup already ran — must not drive the count negative, because a negative count would keep
    // the bar hidden for every subsequent navigation.
    set((state) => ({ pending: Math.max(0, state.pending - 1) }));
  },
}));
