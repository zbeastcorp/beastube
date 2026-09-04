/**
 * Application updates.
 *
 * The whole updater surface lives behind these three functions so the plugin is imported in exactly
 * one place. That matters because the plugin only exists inside the desktop shell: a browser-hosted
 * dev session, a test, or a Storybook-style render would otherwise fail at import time rather than
 * at the point of use.
 *
 * ## What an update actually is here
 *
 * BEASTUBE ships as an NSIS installer, and an update is the *next* installer: it is downloaded,
 * its signature is checked against the public key compiled into this binary, and it is then run
 * without a wizard (`installMode: passive` in `tauri.conf.json`) before the application restarts.
 *
 * It is not a patch. Windows offers no delta mechanism through this path, so every update pulls the
 * whole installer down — which for BEASTUBE means roughly 50 MB, most of it the bundled ffmpeg.
 * Callers are expected to show that progress rather than hide it (§86).
 *
 * ## The signature is the security boundary
 *
 * This code installs software without asking a second time, so the verification is the thing that
 * makes it safe rather than a formality. The plugin refuses an update whose signature does not
 * match before any of it is executed; a release signed with the wrong key simply never installs.
 */

import type { Update } from '@tauri-apps/plugin-updater';

/**
 * Looks for a newer release.
 *
 * Resolves to `null` when the running version is current, which is the ordinary case and not a
 * failure. Throws when the release feed cannot be reached — being offline is the usual reason, and
 * the caller reports it as a state rather than an error dialog.
 */
export async function checkForUpdate(): Promise<Update | null> {
  const { check } = await import('@tauri-apps/plugin-updater');
  return await check();
}

/**
 * Whether an install is already running, for the process rather than for a component.
 *
 * The About panel owns the progress it displays, and it unmounts the moment the viewer navigates
 * away — but the download does not stop with it. Coming back showed an idle button over a running
 * install, and pressing it started a second one: two installers fetching and then running against
 * the same files. Module scope is the right home for this because the install belongs to the
 * application, not to whichever screen happens to be showing.
 */
let installing = false;

/** Whether an install started earlier is still running. */
export function isInstallingUpdate(): boolean {
  return installing;
}

/**
 * Downloads and installs an update, reporting progress as a whole percentage.
 *
 * The percentage is derived from the plugin's byte events rather than from a timer. When the server
 * sends no `Content-Length` the total is unknown, and the callback is then given the bytes so far
 * rather than a fabricated percentage — a progress bar that invents its own position is worse than
 * one that admits it cannot measure (§87).
 */
export async function downloadAndInstallUpdate(
  update: Update,
  onProgress: (percent: number | null) => void,
): Promise<'installed' | 'already-running'> {
  // A second install while one is in flight is refused rather than queued. It must be *reported*
  // as refused, not silently resolved: the caller treats resolution as "the install finished, now
  // restart", so a quiet return relaunched the application in the middle of the first download.
  if (installing) return 'already-running';
  installing = true;

  let total = 0;
  let downloaded = 0;

  try {
    await update.downloadAndInstall((event) => {
      switch (event.event) {
        case 'Started':
          total = event.data.contentLength ?? 0;
          onProgress(total > 0 ? 0 : null);
          break;
        case 'Progress':
          downloaded += event.data.chunkLength;
          onProgress(total > 0 ? Math.min(100, Math.round((downloaded / total) * 100)) : null);
          break;
        case 'Finished':
          onProgress(100);
          break;
        default:
          break;
      }
    });
  } finally {
    // Cleared even on failure, so a network error does not leave the button dead for the session.
    installing = false;
  }
  return 'installed';
}

/**
 * Restarts into the newly installed version.
 *
 * Separate from the install because the installer usually takes the process down itself; this is
 * the path for the case where it hands control back instead.
 */
export async function relaunchApp(): Promise<void> {
  const { relaunch } = await import('@tauri-apps/plugin-process');
  await relaunch();
}
