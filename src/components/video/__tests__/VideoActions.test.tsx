/**
 * The download control's four states, and its absence.
 *
 * Worth testing rather than eyeballing, because the state that matters most cannot be seen on a
 * machine that has the tools: the button is meant to be *absent* when `yt-dlp` or `ffmpeg` is
 * missing, and "absent" is exactly what a passing glance at a working setup looks like.
 *
 * The row is otherwise icon-only, so every assertion goes through the accessible name — which is
 * also the check that dropping the visible labels did not drop the labels a screen reader reads.
 */

import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { VideoActions } from '@/components/video/VideoActions';
import { TranslationProvider } from '@/i18n/context';
import { setIpcMock, type CommandName } from '@/services/ipc';
import { useDownloadsStore } from '@/stores/downloads';
import type { DownloadProgress, DownloadStatus, VideoId, VideoSummary } from '@/types/domain';

const VIDEO: VideoSummary = {
  id: 'dQw4w9WgXcQ' as VideoId,
  title: 'Clip',
  thumbnails: [],
};

/** The tools report for a machine that can and cannot download. */
function tools(available: boolean) {
  return {
    downloader_path: available ? 'C:/Tools/yt-dlp.exe' : null,
    downloader_version: available ? '2026.08.19' : null,
    ffmpeg_path: available ? 'C:/Tools/ffmpeg.exe' : null,
    ffmpeg_version: available ? 'ffmpeg 8.0' : null,
    js_runtime: null,
    directory: 'C:/Users/me/Downloads/BEASTUBE',
    available,
    can_merge: available,
  };
}

function progress(status: DownloadStatus, extra: Partial<DownloadProgress> = {}): DownloadProgress {
  return {
    id: 'd1',
    video_id: VIDEO.id,
    title: VIDEO.title,
    status,
    updated_at: 1,
    ...extra,
  };
}

/** Renders the horizontal row with the store already in the state under test. */
function renderRow(): void {
  render(
    <TranslationProvider locale="en">
      <VideoActions video={VIDEO} orientation="horizontal" />
    </TranslationProvider>,
  );
}

const started: CommandName[] = [];

beforeEach(() => {
  started.length = 0;
  useDownloadsStore.setState({ byVideo: {}, tools: null, hydrated: true });
  setIpcMock((command) => {
    started.push(command);
    // `is_bookmarked` is the only other call the row makes on mount.
    if (command === 'is_bookmarked') return Promise.resolve(false as never);
    if (command === 'start_download') return Promise.resolve(progress('queued') as never);
    return Promise.resolve(null as never);
  });
});

afterEach(() => {
  setIpcMock(null);
  cleanup();
});

describe('the download control', () => {
  it('is absent when no downloader is installed', () => {
    useDownloadsStore.setState({ tools: tools(false) });
    renderRow();

    expect(screen.queryByRole('button', { name: 'Download' })).toBeNull();
    // The rest of the row is unaffected — this is a missing capability, not a broken screen.
    expect(screen.getByRole('button', { name: 'Bookmark' })).toBeInTheDocument();
  });

  it('is absent until the native side has answered', () => {
    // `tools: null` is the startup state. Guessing "available" would show a control that fails.
    renderRow();
    expect(screen.queryByRole('button', { name: 'Download' })).toBeNull();
  });

  it('offers a download once both tools are found', async () => {
    useDownloadsStore.setState({ tools: tools(true) });
    renderRow();

    const button = screen.getByRole('button', { name: 'Download' });
    // Labelled, unlike the icon-only actions beside it: this one also reports progress.
    expect(button).toHaveTextContent('Download');

    await userEvent.click(button);
    expect(started).toContain('start_download');
  });

  it('reports the percentage while running, and cancels rather than starting again', () => {
    useDownloadsStore.setState({
      tools: tools(true),
      byVideo: { [VIDEO.id]: progress('downloading', { fraction: 0.42 }) },
    });
    renderRow();

    expect(screen.getByRole('button', { name: 'Downloading 42%' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Download' })).toBeNull();
  });

  it('says only that it is downloading when the size is unknown', () => {
    // No total means no percentage. A fabricated number is worse than none.
    useDownloadsStore.setState({
      tools: tools(true),
      byVideo: { [VIDEO.id]: progress('downloading') },
    });
    renderRow();

    expect(screen.getByRole('button', { name: 'Downloading' })).toBeInTheDocument();
  });

  it('offers the folder once the file is saved', () => {
    useDownloadsStore.setState({
      tools: tools(true),
      byVideo: { [VIDEO.id]: progress('finished', { path: 'C:/Videos/Clip.mp4' }) },
    });
    renderRow();

    expect(screen.getByRole('button', { name: 'Show in folder' })).toBeInTheDocument();
  });

  it('comes back to Download after a failure, so pressing it again retries', () => {
    useDownloadsStore.setState({
      tools: tools(true),
      byVideo: { [VIDEO.id]: progress('failed') },
    });
    renderRow();

    expect(screen.getByRole('button', { name: 'Download' })).toBeInTheDocument();
  });
});

describe('the icon-only actions', () => {
  it('keep an accessible name even though the label is not painted', () => {
    useDownloadsStore.setState({ tools: tools(true) });
    renderRow();

    // Dropping the visible text must not drop the name; each of these is a bare icon on screen.
    for (const name of ['Bookmark', 'Save to playlist', 'Open in browser', 'Copy link']) {
      expect(screen.getByRole('button', { name })).toBeInTheDocument();
    }
  });
});
