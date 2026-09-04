/**
 * The downloads mirror.
 *
 * These cover the three behaviours that are easy to get wrong and unpleasant when they are: a
 * stale record must not rewind a button that has moved on, a download must announce itself exactly
 * once, and a control must not appear on a computer that cannot use it.
 *
 * They are written against the store rather than a component, because the store is where all three
 * decisions are made — a component test would be testing the same logic through a keyhole.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { setIpcMock } from '@/services/ipc';
import { canDownload, useDownloadsStore } from '@/stores/downloads';
import { useUiStore } from '@/stores/ui';
import type { DownloadProgress, VideoId } from '@/types/domain';

const VIDEO = 'dQw4w9WgXcQ' as VideoId;

function record(overrides: Partial<DownloadProgress> = {}): DownloadProgress {
  return {
    id: 'd1',
    video_id: VIDEO,
    title: 'Clip',
    status: 'downloading',
    updated_at: 1000,
    ...overrides,
  };
}

beforeEach(() => {
  useDownloadsStore.setState({ byVideo: {}, tools: null, hydrated: false });
  useUiStore.setState({ toasts: [] });
});

afterEach(() => {
  setIpcMock(null);
  vi.restoreAllMocks();
});

describe('applying records', () => {
  it('keeps the newest record for a video', () => {
    const { apply } = useDownloadsStore.getState();

    apply(record({ updated_at: 2000, fraction: 0.5 }));
    // The same download, but an older reading — this is what an event reordered behind `hydrate`
    // looks like, and adopting it would rewind the button.
    apply(record({ updated_at: 1000, fraction: 0.1 }));

    expect(useDownloadsStore.getState().byVideo[VIDEO]?.fraction).toBe(0.5);
  });

  it('adopts a newer download for the same video even when its clock reads earlier', () => {
    const { apply } = useDownloadsStore.getState();

    apply(record({ id: 'first', updated_at: 5000, status: 'cancelled' }));
    // A second download started afterwards is a different download; the ordering rule applies
    // within one download, not across two.
    apply(record({ id: 'second', updated_at: 10, status: 'queued' }));

    expect(useDownloadsStore.getState().byVideo[VIDEO]?.id).toBe('second');
  });
});

describe('announcements', () => {
  it('announces a finished download once, however many times the record arrives', () => {
    const { apply } = useDownloadsStore.getState();

    apply(record({ status: 'downloading', updated_at: 1 }));
    apply(record({ status: 'finished', updated_at: 2, path: 'C:\\Videos\\Clip.mp4' }));
    // A redelivery of the same terminal state must not produce a second toast.
    apply(record({ status: 'finished', updated_at: 3, path: 'C:\\Videos\\Clip.mp4' }));

    const toasts = useUiStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0]?.messageKey).toBe('download.finished');
    expect(toasts[0]?.action?.labelKey).toBe('download.showInFolder');
  });

  it('announces a failure with the key the native side chose', () => {
    useDownloadsStore.getState().apply(
      record({
        status: 'failed',
        updated_at: 2,
        error: {
          kind: 'provider',
          code: 'provider.download_refused',
          message_key: 'error.provider.download_refused',
          recovery: { strategy: 'retry_manual' },
        },
      }),
    );

    const toasts = useUiStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0]?.messageKey).toBe('error.provider.download_refused');
    expect(toasts[0]?.tone).toBe('danger');
  });

  it('says nothing when the user cancelled', () => {
    const { apply } = useDownloadsStore.getState();

    apply(record({ status: 'downloading', updated_at: 1 }));
    apply(record({ status: 'cancelled', updated_at: 2 }));

    expect(useUiStore.getState().toasts).toHaveLength(0);
  });
});

describe('availability', () => {
  it('reads as unavailable until the native side has answered', () => {
    // The conservative direction: a control that appears a moment late is better than one that
    // appears and fails.
    expect(canDownload(useDownloadsStore.getState())).toBe(false);
  });

  it('keeps the last known answer when a re-check fails', async () => {
    setIpcMock((command) => {
      if (command === 'get_download_tools') {
        return Promise.resolve({
          downloader_path: 'C:\\Tools\\yt-dlp.exe',
          downloader_version: '2026.08.19',
          ffmpeg_path: null,
          ffmpeg_version: null,
          js_runtime: null,
          directory: 'C:\\Users\\me\\Downloads\\BEASTUBE',
          available: true,
          can_merge: false,
          // The mock is typed against the whole command map, and this is the one command under
          // test; the cast keeps the test honest about that rather than widening the map.
        } as never);
      }
      return Promise.reject(new Error(`unexpected command ${command}`));
    });

    await useDownloadsStore.getState().refreshTools();
    expect(canDownload(useDownloadsStore.getState())).toBe(true);

    setIpcMock(() => Promise.reject(new Error('probe timed out')));
    await useDownloadsStore.getState().refreshTools();

    expect(canDownload(useDownloadsStore.getState())).toBe(true);
  });
});

describe('cancelling', () => {
  it('does nothing for a download that is already over', async () => {
    const invoked: string[] = [];
    setIpcMock((command) => {
      invoked.push(command);
      return Promise.resolve(true as never);
    });
    useDownloadsStore.getState().apply(record({ status: 'finished', updated_at: 1 }));

    await useDownloadsStore.getState().cancel(VIDEO);

    expect(invoked).toEqual([]);
  });

  it('does not mark a download stopped until the native side says so', async () => {
    setIpcMock(() => Promise.resolve(true as never));
    useDownloadsStore.getState().apply(record({ status: 'downloading', updated_at: 1 }));

    await useDownloadsStore.getState().cancel(VIDEO);

    // A download that ignored the request must not look stopped; the cancelled record arrives as
    // an event, and only then does the button change.
    expect(useDownloadsStore.getState().byVideo[VIDEO]?.status).toBe('downloading');
  });
});
