/**
 * Downloads store.
 *
 * The native side owns every download; this store mirrors it, so the button under a video shows
 * the real state of that video's download without a round trip and keeps showing it after the user
 * navigates away and comes back. The mirror is fed two ways: the whole list once at startup
 * ({@link useDownloadsStore.hydrate}), and one complete record per change afterwards, from the
 * `download:progress` event.
 *
 * Because every record is complete rather than a delta, the two feeds cannot disagree — a record
 * simply replaces the one it supersedes, and a late event for a download that has since finished
 * is discarded by comparing `updated_at`.
 *
 * A download that finishes while the user is on another screen still has to say so. Raising the
 * toast in the store — the one place every transition passes through — means it happens exactly
 * once regardless of how many buttons are mounted, and happens at all when none are.
 */

import { create } from 'zustand';

import { invoke, isCancellation, listen, normalizeError, type DownloadTools } from '@/services/ipc';
import { useUiStore } from '@/stores/ui';
import type { DownloadProgress, ErrorPayload, VideoId, VideoSummary } from '@/types/domain';
import { isTerminalDownload } from '@/types/domain';

/** How long a finished or failed download's toast stays on screen. */
const ANNOUNCEMENT_MS = 6000;

interface DownloadsState {
  /** The most recent record for each video, keyed by video id. */
  byVideo: Record<string, DownloadProgress>;
  /**
   * What is installed locally, or `null` until the native side has answered.
   *
   * `null` reads as "cannot download" everywhere, which is the conservative direction: guessing
   * "available" and being wrong shows a control that fails when pressed, where guessing
   * "unavailable" and being wrong briefly omits one that appears a moment later.
   */
  tools: DownloadTools | null;
  /** True once the session's downloads have been read. */
  hydrated: boolean;

  /** Applies a record from the native side, announcing a newly terminal download. */
  apply: (progress: DownloadProgress) => void;
  /** Reads the session's downloads. Called once, from the shell. */
  hydrate: () => Promise<void>;
  /**
   * Re-reads what is installed.
   *
   * Called at startup and again whenever the user changes a tool path, so installing `yt-dlp` and
   * pointing at it makes the button appear without a restart.
   */
  refreshTools: () => Promise<void>;
  /** Starts downloading a video. Failures surface as a toast, not a thrown error. */
  start: (video: Pick<VideoSummary, 'id' | 'title'>) => Promise<void>;
  /** Stops a video's download. A no-op if there is nothing running for it. */
  cancel: (videoId: VideoId) => Promise<void>;
  /** Shows a finished download in the file manager. */
  reveal: (videoId: VideoId) => Promise<void>;
}

/** Whether a download can be started at all on this computer. */
export function canDownload(state: DownloadsState): boolean {
  return state.tools?.available ?? false;
}

/** Raises the toast for a download that has just reached a terminal state. */
function announce(progress: DownloadProgress): void {
  const ui = useUiStore.getState();

  if (progress.status === 'finished') {
    ui.toast({
      messageKey: 'download.finished',
      params: { title: progress.title },
      tone: 'success',
      durationMs: ANNOUNCEMENT_MS,
      action: {
        labelKey: 'download.showInFolder',
        run: () => {
          void invoke('reveal_download', { id: progress.id }).catch(() => {
            // The file has been moved or deleted since. The toast is already gone by the time
            // anyone could act on a second message about it.
          });
        },
      },
    });
    return;
  }

  if (progress.status === 'failed') {
    ui.toast({
      // The native side classified the failure; rendering its key is what keeps the sentence in
      // the localization layer rather than in Rust.
      messageKey: progress.error?.message_key ?? 'error.generic',
      params: { title: progress.title },
      tone: 'danger',
      durationMs: ANNOUNCEMENT_MS,
    });
  }
  // Cancellation is the user's own doing and needs no announcement.
}

/** Reports a failure that happened before a download record existed. */
function reportStartFailure(error: ErrorPayload): void {
  if (isCancellation(error)) return;
  useUiStore.getState().toast({
    messageKey: error.message_key,
    params: error.params ?? {},
    tone: 'danger',
    durationMs: ANNOUNCEMENT_MS,
  });
}

export const useDownloadsStore = create<DownloadsState>((set, get) => ({
  byVideo: {},
  tools: null,
  hydrated: false,

  apply: (progress) => {
    const previous = get().byVideo[progress.video_id];

    // Events can arrive out of order across the IPC boundary, and `hydrate` can land after an
    // event it predates. The newer record wins; an older one is dropped rather than rewinding the
    // button to a state that has already passed.
    if (previous?.id === progress.id && previous.updated_at > progress.updated_at) {
      return;
    }

    set((state) => ({ byVideo: { ...state.byVideo, [progress.video_id]: progress } }));

    // Announced on the transition, not on the state: a record re-delivered after the fact must
    // not produce a second toast.
    const becameTerminal =
      isTerminalDownload(progress.status) &&
      (previous?.id !== progress.id || !isTerminalDownload(previous.status));
    if (becameTerminal) announce(progress);
  },

  hydrate: async () => {
    try {
      const downloads = await invoke('get_downloads', undefined);

      // The snapshot is authoritative, including about what is *no longer* there: the native side
      // forgets a finished download whose file has been deleted, and the button must go back to
      // offering the download rather than a folder that no longer holds it. So the map is rebuilt
      // from the snapshot rather than merged into — a merge could only ever add.
      const rebuilt: Record<string, DownloadProgress> = {};
      for (const progress of downloads) {
        rebuilt[progress.video_id] = progress;
      }

      // Except where the local record is newer: an event that landed while this request was in
      // flight describes a later moment than the snapshot does.
      for (const [videoId, local] of Object.entries(get().byVideo)) {
        const fromSnapshot = rebuilt[videoId];
        if (fromSnapshot?.id === local.id && local.updated_at > fromSnapshot.updated_at) {
          rebuilt[videoId] = local;
        }
      }

      set({ byVideo: rebuilt });
    } catch {
      // The mirror is left as it was. Failing to read the list is not evidence that it changed.
    } finally {
      set({ hydrated: true });
    }
  },

  refreshTools: async () => {
    try {
      set({ tools: await invoke('get_download_tools', undefined) });
    } catch {
      // Left as it was. Failing to measure the local setup is not evidence that it changed, and
      // clearing it would make a working download button vanish because one probe timed out.
    }
  },

  start: async (video) => {
    try {
      get().apply(await invoke('start_download', { videoId: video.id, title: video.title }));
    } catch (cause) {
      reportStartFailure(normalizeError(cause));
    }
  },

  cancel: async (videoId) => {
    const current = get().byVideo[videoId];
    if (!current || isTerminalDownload(current.status)) return;
    try {
      await invoke('cancel_download', { id: current.id });
      // The cancelled record arrives as an event; nothing is set optimistically here, because a
      // download that ignored the cancellation must not look stopped.
    } catch {
      // Cancelling something already gone is not a failure worth a message.
    }
  },

  reveal: async (videoId) => {
    const current = get().byVideo[videoId];
    if (current?.status !== 'finished') return;
    try {
      await invoke('reveal_download', { id: current.id });
    } catch (cause) {
      reportStartFailure(normalizeError(cause));
    }
  },
}));

/**
 * Subscribes the store to native download events.
 *
 * Returns an unsubscribe function. Called once by the shell rather than per component, so a
 * download announces itself once however many screens are mounted.
 */
export function subscribeToDownloads(): () => void {
  const unlisten = listen('download:progress', (progress) => {
    useDownloadsStore.getState().apply(progress);
  });

  /**
   * Re-reads the list whenever the window is focused again.
   *
   * Files are deleted, moved and renamed outside this application, and nothing tells it when. The
   * moment the user comes back from the file manager is exactly when the answer may have changed
   * and exactly when they are about to look at the button, so that is when it is checked.
   */
  const onFocus = () => {
    void useDownloadsStore.getState().hydrate();
  };
  window.addEventListener('focus', onFocus);

  return () => {
    unlisten();
    window.removeEventListener('focus', onFocus);
  };
}
