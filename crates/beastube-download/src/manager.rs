//! Running downloads and remembering them.
//!
//! One [`DownloadManager`] lives for the whole session. It owns every download's state, bounds
//! how many run at once, and pushes each change through a single [`ProgressSink`] — the shell
//! turns those into events for the UI, which never polls (§69).
//!
//! ## Why the state lives here and not in the UI
//!
//! A download outlives the screen that started it. The user presses Download, navigates to
//! another video, and comes back; the button must show the real state, which only the process
//! owner knows. So the manager keeps a record for every download of the session, and the shell
//! can hand the whole list to a freshly mounted screen.
//!
//! ## Concurrency
//!
//! Downloads beyond the concurrent limit wait in [`DownloadStatus::Queued`] rather than being
//! refused. The limit exists because each run is a separate process pulling at full bandwidth;
//! three at once already saturate a home connection and starve the player.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use beastube_core::error::DomainError;
use beastube_core::events::{DownloadProgress, DownloadStatus};
use beastube_core::ids::VideoId;
use beastube_core::time_util::Timestamp;
use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::runtime::Handle;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::command::{DownloadPlan, arguments};
use crate::error::{DownloadError, DownloadResult, classify_failure};
use crate::locate::hide_console;
use crate::progress::{ProgressSample, ToolLine, parse_line};

/// Receives every state change. Called on whichever task produced the change.
pub type ProgressSink = Arc<dyn Fn(&DownloadProgress) + Send + Sync>;

/// What to download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRequest {
    /// The video.
    pub video_id: VideoId,
    /// Its title, carried on progress so the UI can name the download without a lookup.
    pub title: String,
}

/// How many stderr lines are kept for the diagnostic when a run fails.
const STDERR_TAIL_LINES: usize = 20;

/// Minimum interval between progress emissions for one download.
///
/// The tool reports after every chunk, many times a second; each emission crosses IPC and
/// re-renders a button, so they are coalesced to a rate a person can perceive.
const EMIT_INTERVAL: Duration = Duration::from_millis(250);

/// How many finished, failed or cancelled downloads are remembered before the oldest are dropped.
const RETAINED_TERMINAL: usize = 100;

/// Owns every download of the session.
#[derive(Clone)]
pub struct DownloadManager {
    inner: Arc<Inner>,
}

struct Inner {
    entries: Mutex<Vec<Entry>>,
    sink: ProgressSink,
    slots: Arc<Semaphore>,
    /// The runtime downloads run on, captured at construction so [`DownloadManager::start`] can
    /// be called from any thread — including a synchronous command handler with no runtime of
    /// its own.
    runtime: Handle,
}

struct Entry {
    progress: DownloadProgress,
    cancel: CancellationToken,
}

impl std::fmt::Debug for DownloadManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadManager")
            .field("downloads", &self.inner.entries.lock().len())
            .finish_non_exhaustive()
    }
}

impl DownloadManager {
    /// Builds a manager that runs at most `max_concurrent` downloads at once.
    ///
    /// # Panics
    ///
    /// Panics if called outside a tokio runtime, because the runtime is what the downloads run
    /// on. The shell constructs it inside its startup `block_on`.
    #[must_use]
    pub fn new(sink: ProgressSink, max_concurrent: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                entries: Mutex::new(Vec::new()),
                sink,
                slots: Arc::new(Semaphore::new(max_concurrent.max(1))),
                runtime: Handle::current(),
            }),
        }
    }

    /// Starts a download, or returns the one already running for the same video.
    ///
    /// Returns the initial [`DownloadStatus::Queued`] record immediately; everything after that
    /// arrives through the sink. Pressing the button twice is therefore harmless: the second press
    /// gets the first download's record back rather than a second process.
    ///
    /// # Errors
    ///
    /// Returns [`DownloadError::Directory`] if the target directory cannot be created.
    pub fn start(
        &self,
        request: DownloadRequest,
        plan: DownloadPlan,
    ) -> DownloadResult<DownloadProgress> {
        if let Some(existing) = self.active_for(&request.video_id) {
            return Ok(existing);
        }

        std::fs::create_dir_all(&plan.directory).map_err(|error| DownloadError::Directory {
            detail: format!("{}: {error}", plan.directory.display()),
        })?;

        let id = uuid::Uuid::now_v7().to_string();
        let progress = DownloadProgress {
            id: id.clone(),
            video_id: request.video_id.clone(),
            title: request.title,
            status: DownloadStatus::Queued,
            downloaded_bytes: None,
            total_bytes: None,
            fraction: None,
            speed_bps: None,
            eta_seconds: None,
            path: None,
            error: None,
            updated_at: Timestamp::now(),
        };
        let cancel = CancellationToken::new();
        {
            let mut entries = self.inner.entries.lock();
            prune(&mut entries);
            entries.push(Entry {
                progress: progress.clone(),
                cancel: cancel.clone(),
            });
        }
        (self.inner.sink)(&progress);

        let inner = Arc::clone(&self.inner);
        self.inner
            .runtime
            .spawn(run(inner, id, request.video_id, plan, cancel));
        Ok(progress)
    }

    /// Cancels a download. Returns `false` if it is unknown or already over.
    // Not `#[must_use]`: cancelling and not caring whether there was anything to cancel is a
    // legitimate call — the shutdown path makes exactly that one.
    #[allow(clippy::must_use_candidate)]
    pub fn cancel(&self, id: &str) -> bool {
        let entries = self.inner.entries.lock();
        match entries.iter().find(|entry| entry.progress.id == id) {
            Some(entry) if !entry.progress.status.is_terminal() => {
                entry.cancel.cancel();
                true
            }
            _ => false,
        }
    }

    /// Every download of the session, oldest first, after forgetting files that are gone.
    ///
    /// The check happens here rather than on a timer because this is what a screen asks for when
    /// it wants the truth — on mount, and when the window is focused again. Deleting a file in the
    /// file manager and coming back therefore turns "Show in folder" back into "Download", which
    /// is the honest answer to a folder that no longer holds it.
    #[must_use]
    pub fn snapshot(&self) -> Vec<DownloadProgress> {
        let forgotten = self.inner.forget_missing();
        if forgotten > 0 {
            tracing::debug!(forgotten, "forgot downloads whose file is no longer on disk");
        }
        self.inner
            .entries
            .lock()
            .iter()
            .map(|entry| entry.progress.clone())
            .collect()
    }

    /// One download's current state.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<DownloadProgress> {
        self.inner
            .entries
            .lock()
            .iter()
            .find(|entry| entry.progress.id == id)
            .map(|entry| entry.progress.clone())
    }

    /// The in-progress download for a video, if there is one.
    #[must_use]
    pub fn active_for(&self, video_id: &VideoId) -> Option<DownloadProgress> {
        self.inner
            .entries
            .lock()
            .iter()
            .find(|entry| {
                &entry.progress.video_id == video_id && !entry.progress.status.is_terminal()
            })
            .map(|entry| entry.progress.clone())
    }
}

impl Inner {
    /// Applies `change` to one download and emits it if `emit` says so.
    /// Applies a change, and emits it when asked. Returns whether anything was emitted.
    fn update(&self, id: &str, emit: bool, change: impl FnOnce(&mut DownloadProgress)) -> bool {
        let updated = {
            let mut entries = self.entries.lock();
            let Some(entry) = entries.iter_mut().find(|entry| entry.progress.id == id) else {
                return false;
            };
            change(&mut entry.progress);
            entry.progress.updated_at = Timestamp::now();
            emit.then(|| entry.progress.clone())
        };
        match updated {
            Some(progress) => {
                (self.sink)(&progress);
                true
            }
            None => false,
        }
    }

    fn set_status(&self, id: &str, status: DownloadStatus) {
        let _ = self.update(id, true, |progress| progress.status = status);
    }

    /// Records a reading, emitting at most every [`EMIT_INTERVAL`] unless the status changed.
    ///
    /// `last_emit` is `None` until something has been emitted, which is what makes the first
    /// reading arrive immediately rather than a quarter-second into the download.
    fn apply_sample(&self, id: &str, sample: ProgressSample, last_emit: &mut Option<Instant>) {
        let now = Instant::now();
        let elapsed = last_emit.is_none_or(|last| now.duration_since(last) >= EMIT_INTERVAL);
        // The decision has to be made *before* the update, because that is where it is used. It
        // used to be computed inside the closure and then thrown away: `update` was called with a
        // hardcoded `true`, so every line yt-dlp printed crossed the IPC boundary and re-rendered
        // the UI — several times a second, per download — while the value that was supposed to
        // throttle it only decided whether to move a timestamp.
        let status_changed = self
            .entries
            .lock()
            .iter()
            .find(|entry| entry.progress.id == id)
            .is_none_or(|entry| entry.progress.status != DownloadStatus::Downloading);
        let emit = status_changed || elapsed || sample.finished;

        let emitted = self.update(id, emit, |progress| {
            progress.status = DownloadStatus::Downloading;
            progress.downloaded_bytes = sample.downloaded_bytes;
            progress.total_bytes = sample.total_bytes;
            progress.fraction = sample.fraction();
            progress.speed_bps = sample.speed_bps;
            progress.eta_seconds = sample.eta_seconds;
        });
        if emitted {
            *last_emit = Some(now);
        }
    }

    /// Marks a download finished, which requires a file to exist.
    ///
    /// `path` is what the tool said it wrote. It is checked rather than believed, because the tool
    /// can exit successfully having written nothing — seen in the field when YouTube answers the
    /// format request with an error the tool reports on stderr and still returns zero. Reporting
    /// that as a finished download gives the user a "Show in folder" button over an empty folder,
    /// which is the plainest possible version of claiming a feature that did not happen.
    fn finish(&self, id: &str, path: Option<PathBuf>) {
        self.update(id, true, |progress| {
            progress.status = DownloadStatus::Finished;
            progress.fraction = Some(1.0);
            progress.speed_bps = None;
            progress.eta_seconds = None;
            progress.path = path.map(|path| path.display().to_string());
        });
    }

    /// Drops finished downloads whose file is no longer on disk.
    ///
    /// A file the user deleted, moved or renamed is, from the interface's point of view, a download
    /// that did not happen: the control should offer to fetch it again rather than to reveal
    /// something that is not there. Forgetting the record is what produces that, with no new status
    /// to thread through the contract.
    ///
    /// Returns how many were forgotten.
    fn forget_missing(&self) -> usize {
        let mut entries = self.entries.lock();
        let before = entries.len();
        entries.retain(|entry| {
            if entry.progress.status != DownloadStatus::Finished {
                return true;
            }
            entry
                .progress
                .path
                .as_ref()
                .is_some_and(|path| Path::new(path).is_file())
        });
        before - entries.len()
    }

    fn cancelled(&self, id: &str) {
        self.update(id, true, |progress| {
            progress.status = DownloadStatus::Cancelled;
            progress.speed_bps = None;
            progress.eta_seconds = None;
        });
    }

    fn fail(&self, id: &str, error: &DownloadError) {
        tracing::warn!(download = id, %error, "download failed");
        self.update(id, true, |progress| {
            progress.status = DownloadStatus::Failed;
            progress.speed_bps = None;
            progress.eta_seconds = None;
            progress.error = Some(error.to_payload());
        });
    }
}

/// The lifetime of one download, from waiting for a slot to a terminal status.
///
/// The emitted state is only ever updated by the sample stream and the four terminal paths, so
/// the status the UI sees is always one this function set on purpose. Wait, kill and cleanup
/// happen here rather than being left to `kill_on_drop`, so the partial files are gone by the time
/// the cancelled status is reported.
#[allow(clippy::too_many_lines)]
async fn run(
    inner: Arc<Inner>,
    id: String,
    video_id: VideoId,
    plan: DownloadPlan,
    cancel: CancellationToken,
) {
    // Cancelling while queued must not wait for a slot, so the wait itself is racing the token.
    let acquired = tokio::select! {
        biased;
        () = cancel.cancelled() => None,
        permit = Arc::clone(&inner.slots).acquire_owned() => Some(permit),
    };
    let Some(permit) = acquired else {
        inner.cancelled(&id);
        return;
    };
    let Ok(permit) = permit else {
        inner.fail(
            &id,
            &DownloadError::Failed {
                exit_code: None,
                detail: "the download queue was closed".to_owned(),
            },
        );
        return;
    };
    inner.set_status(&id, DownloadStatus::Starting);

    let mut command = Command::new(&plan.tool);
    command
        .args(arguments(&plan, &video_id))
        // The tool is a Python program; on Windows its console encoding defaults to the legacy
        // code page, which garbles any title outside Latin-1 and then the filename it prints.
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    hide_console(&mut command);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            inner.fail(
                &id,
                &DownloadError::Spawn {
                    detail: format!("{}: {error}", plan.tool.display()),
                },
            );
            return;
        }
    };
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        inner.fail(
            &id,
            &DownloadError::Spawn {
                detail: "the downloader's output could not be captured".to_owned(),
            },
        );
        return;
    };

    let mut out_lines = BufReader::new(stdout).lines();
    let mut err_lines = BufReader::new(stderr).lines();
    let mut out_open = true;
    let mut err_open = true;
    let mut stderr_tail: VecDeque<String> = VecDeque::with_capacity(STDERR_TAIL_LINES);
    let mut final_path: Option<PathBuf> = None;
    let mut last_emit: Option<Instant> = None;

    while out_open || err_open {
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                // Kill, wait, then clean up: waiting first is what guarantees the process has
                // released its file handles before the partial files are removed.
                //
                // The tree, not just the child. `yt-dlp` starts `ffmpeg` to join the separate
                // video and audio streams, and ending only the parent left that muxer running with
                // no download to belong to — still holding the very partial files the next line
                // tries to delete. Cancelling looked instant and left a process behind.
                if let Some(pid) = child.id() {
                    crate::locate::kill_process_tree(pid).await;
                }
                let _ = child.start_kill();
                let _ = child.wait().await;
                remove_partials(&plan.directory, &video_id);
                inner.cancelled(&id);
                drop(permit);
                return;
            }
            line = out_lines.next_line(), if out_open => match line {
                Ok(Some(line)) => match parse_line(&line) {
                    ToolLine::Progress(sample) => inner.apply_sample(&id, sample, &mut last_emit),
                    ToolLine::File(path) => final_path = Some(path),
                    postprocess @ ToolLine::Postprocess { .. } => {
                        if postprocess.is_media_postprocess() {
                            inner.set_status(&id, DownloadStatus::Merging);
                        }
                    }
                    ToolLine::Other(text) => tracing::debug!(download = %id, "{text}"),
                },
                Ok(None) | Err(_) => out_open = false,
            },
            line = err_lines.next_line(), if err_open => match line {
                Ok(Some(line)) => {
                    if stderr_tail.len() == STDERR_TAIL_LINES {
                        stderr_tail.pop_front();
                    }
                    stderr_tail.push_back(line);
                }
                Ok(None) | Err(_) => err_open = false,
            },
        }
    }

    let status = child.wait().await;
    drop(permit);
    let tail = stderr_tail.iter().cloned().collect::<Vec<_>>().join("\n");

    match status {
        Ok(status) if status.success() => {
            // The tool names the file on stdout; if that line was lost (an old version, or a file
            // that already existed and was not moved), the directory is searched for it.
            let path = final_path
                .or_else(|| find_output(&plan.directory, &video_id))
                .filter(|path| path.is_file());
            match path {
                Some(path) => inner.finish(&id, Some(path)),
                // Exit zero and no file. Whatever the tool said on stderr is the real story, so it
                // is classified like any other failure; an empty stderr gets a plain statement
                // rather than a success the user would go looking for.
                None if tail.is_empty() => inner.fail(
                    &id,
                    &DownloadError::Failed {
                        exit_code: status.code(),
                        detail: "the downloader reported success but wrote no file".to_owned(),
                    },
                ),
                None => inner.fail(&id, &classify_failure(status.code(), &tail)),
            }
        }
        Ok(status) => inner.fail(&id, &classify_failure(status.code(), &tail)),
        Err(error) => inner.fail(
            &id,
            &DownloadError::Failed {
                exit_code: None,
                detail: format!("could not read the downloader's exit status: {error}"),
            },
        ),
    }
}

/// Drops the oldest finished records once more than [`RETAINED_TERMINAL`] have accumulated.
///
/// Running downloads are never pruned: a record whose process is alive must stay reachable by
/// [`DownloadManager::cancel`].
fn prune(entries: &mut Vec<Entry>) {
    let terminal = entries
        .iter()
        .filter(|entry| entry.progress.status.is_terminal())
        .count();
    let mut excess = terminal.saturating_sub(RETAINED_TERMINAL);
    entries.retain(|entry| {
        if excess > 0 && entry.progress.status.is_terminal() {
            excess -= 1;
            false
        } else {
            true
        }
    });
}

/// The marker the output template puts in every filename for `video_id`.
fn id_marker(video_id: &VideoId) -> String {
    format!("[{}]", video_id.as_str())
}

/// Whether a filename is one of the tool's in-progress files.
///
/// Compared case-insensitively: the tool writes these suffixes in lower case, but a filesystem
/// that preserves case differently must not turn a partial file into one this refuses to clean up.
fn is_partial(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    // Fragment files of a multi-part download, e.g. `…mp4.part-Frag12`.
    if lower.contains(".part-frag") {
        return true;
    }
    matches!(
        Path::new(&lower).extension().and_then(|ext| ext.to_str()),
        Some("part" | "ytdl")
    )
}

/// Everything a killed run can leave behind for one video.
///
/// Deliberately wider than [`is_partial`], and deliberately a separate predicate rather than a
/// widening of it: [`find_output`] uses `is_partial` to decide which file *is* the download, so
/// broadening that would change what a finished download resolves to. This is only ever asked
/// about files that already carry the video's id marker, and only when a run has been killed.
///
/// Beyond the `.part`/`.ytdl` files `is_partial` knows about, a cancelled run leaves two kinds
/// behind that it does not: the per-format streams yt-dlp downloads separately before joining them
/// (`Title [id].f137.mp4`), and the scratch file the merge writes into (`Title [id].temp.mp4`).
/// Both survived a cancel and sat in the download folder looking like real files.
fn is_cancel_leftover(name: &str) -> bool {
    if is_partial(name) {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    // Anchored to the segment immediately before the extension, which is where yt-dlp puts both
    // markers — `Title.f137.mp4`, `Title.temp.mp4`. Scanning *every* segment was too eager: a
    // finished download whose title happens to contain a dotted `f` and digits, say
    // `Some.Film.f22.1080p.mkv`, matched and would have been deleted when a *different* download
    // for the same video was cancelled. Deleting a file the viewer already has is far worse than
    // leaving a stray partial.
    match lower.rsplit('.').nth(1) {
        Some("temp") => true,
        Some(segment) => {
            segment.len() > 1
                && segment.starts_with('f')
                && segment[1..].bytes().all(|byte| byte.is_ascii_digit())
        }
        None => false,
    }
}

/// Removes the partial files a killed run left behind for `video_id`. Best effort.
fn remove_partials(directory: &Path, video_id: &VideoId) {
    let marker = id_marker(video_id);
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.contains(&marker)
            && is_cancel_leftover(&name)
            && let Err(error) = std::fs::remove_file(entry.path())
        {
            // `warn`, not `debug`. A release build logs at `warn` and above, so at `debug` the one
            // symptom of a muxer still holding the file — a sharing violation — was invisible in
            // exactly the builds where it happens, and the leftovers were blamed on nothing.
            tracing::warn!(%error, file = %name, "could not remove a partial download");
        }
    }
}

/// The most recently written complete file for `video_id` in `directory`, if any.
fn find_output(directory: &Path, video_id: &VideoId) -> Option<PathBuf> {
    let marker = format!("{}.", id_marker(video_id));
    let entries = std::fs::read_dir(directory).ok()?;
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.contains(&marker) && !is_partial(&name)
        })
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_cancelled_run_leaves_nothing_recognisable_behind() {
        // What `is_partial` already knew about.
        assert!(is_cancel_leftover("Title [abc].mp4.part"));
        assert!(is_cancel_leftover("Title [abc].mp4.ytdl"));
        assert!(is_cancel_leftover("Title [abc].mp4.part-Frag12"));

        // What it did not, and what therefore survived a cancel: the separate streams yt-dlp
        // fetches before joining them, and the scratch file the merge writes into.
        assert!(is_cancel_leftover("Title [abc].f137.mp4"));
        assert!(is_cancel_leftover("Title [abc].f251.webm"));
        assert!(is_cancel_leftover("Title [abc].temp.mp4"));

        // The finished download must never match, or cancelling one video would delete another's
        // output from the same folder.
        assert!(!is_cancel_leftover("Title [abc].mp4"));
        assert!(!is_cancel_leftover("Some Film [xyz].mkv"));
        // Nor should an ordinary name that merely starts a segment with "f".
        assert!(!is_cancel_leftover("Title [abc].final.mp4"));
        // And not a finished file whose own title carries a dotted format-looking segment. The
        // marker only counts in the position yt-dlp actually writes it: just before the extension.
        assert!(!is_cancel_leftover("Some.Film.f22.1080p [abc].mkv"));
        assert!(!is_cancel_leftover("Temp.Diaries [abc].mp4"));
    }

    use super::*;

    fn video() -> VideoId {
        VideoId::new("dQw4w9WgXcQ").unwrap()
    }

    #[test]
    fn partial_files_for_the_video_are_removed_and_nothing_else_is() {
        let dir = tempfile::tempdir().unwrap();
        let keep_done = dir.path().join("Clip [dQw4w9WgXcQ].mp4");
        let keep_other = dir.path().join("Other [zzzzzzzzzzz].mp4.part");
        let drop_part = dir.path().join("Clip [dQw4w9WgXcQ].f137.mp4.part");
        let drop_ytdl = dir.path().join("Clip [dQw4w9WgXcQ].f137.mp4.ytdl");
        for path in [&keep_done, &keep_other, &drop_part, &drop_ytdl] {
            std::fs::write(path, b"x").unwrap();
        }

        remove_partials(dir.path(), &video());

        assert!(keep_done.exists());
        assert!(keep_other.exists());
        assert!(!drop_part.exists());
        assert!(!drop_ytdl.exists());
    }

    #[tokio::test]
    async fn a_finished_download_is_forgotten_once_its_file_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Clip [dQw4w9WgXcQ].mp4");
        std::fs::write(&file, b"x").unwrap();

        let manager = DownloadManager::new(Arc::new(|_: &DownloadProgress| {}), 1);
        manager.inner.entries.lock().push(Entry {
            progress: DownloadProgress {
                id: "d1".to_owned(),
                video_id: video(),
                title: "Clip".to_owned(),
                status: DownloadStatus::Finished,
                downloaded_bytes: None,
                total_bytes: None,
                fraction: Some(1.0),
                speed_bps: None,
                eta_seconds: None,
                path: Some(file.display().to_string()),
                error: None,
                updated_at: Timestamp::from_millis(0),
            },
            cancel: CancellationToken::new(),
        });

        assert_eq!(manager.snapshot().len(), 1);

        // What the user did: deleted it from the file manager.
        std::fs::remove_file(&file).unwrap();

        assert!(
            manager.snapshot().is_empty(),
            "a download whose file is gone must stop claiming to be downloaded"
        );
        assert!(manager.active_for(&video()).is_none());
    }

    #[test]
    fn the_finished_file_is_found_by_its_marker() {
        let dir = tempfile::tempdir().unwrap();
        let done = dir.path().join("Clip [dQw4w9WgXcQ].mp4");
        std::fs::write(&done, b"x").unwrap();
        std::fs::write(dir.path().join("Clip [dQw4w9WgXcQ].mp4.part"), b"x").unwrap();
        std::fs::write(dir.path().join("Unrelated.mp4"), b"x").unwrap();

        assert_eq!(find_output(dir.path(), &video()), Some(done));
        assert_eq!(
            find_output(dir.path(), &VideoId::new("zzzzzzzzzzz").unwrap()),
            None
        );
    }

    #[test]
    fn pruning_keeps_every_running_download() {
        let mut entries: Vec<Entry> = (0..RETAINED_TERMINAL + 5)
            .map(|index| Entry {
                progress: DownloadProgress {
                    id: index.to_string(),
                    video_id: video(),
                    title: String::new(),
                    status: if index % 2 == 0 {
                        DownloadStatus::Finished
                    } else {
                        DownloadStatus::Downloading
                    },
                    downloaded_bytes: None,
                    total_bytes: None,
                    fraction: None,
                    speed_bps: None,
                    eta_seconds: None,
                    path: None,
                    error: None,
                    updated_at: Timestamp::from_millis(0),
                },
                cancel: CancellationToken::new(),
            })
            .collect();
        let running = entries
            .iter()
            .filter(|entry| !entry.progress.status.is_terminal())
            .count();

        prune(&mut entries);

        assert_eq!(
            entries
                .iter()
                .filter(|entry| !entry.progress.status.is_terminal())
                .count(),
            running
        );
        assert!(
            entries
                .iter()
                .filter(|entry| entry.progress.status.is_terminal())
                .count()
                <= RETAINED_TERMINAL
        );
    }

    #[tokio::test]
    async fn starting_the_same_video_twice_returns_the_first_download() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let manager = DownloadManager::new(
            Arc::new(move |progress: &DownloadProgress| {
                sink_seen.lock().push(progress.status);
            }),
            1,
        );
        let dir = tempfile::tempdir().unwrap();
        let plan = DownloadPlan {
            // A tool that does not exist: the run fails at spawn, which is fine for this test —
            // the point is the dedup decision made before the process is involved.
            tool: dir.path().join("no-such-tool"),
            ffmpeg: dir.path().join("no-such-ffmpeg"),
            js_runtime: None,
            directory: dir.path().join("out"),
            cache_dir: None,
            max_height: None,
        };
        let request = DownloadRequest {
            video_id: video(),
            title: "Clip".to_owned(),
        };

        let first = manager.start(request.clone(), plan.clone()).unwrap();
        let second = manager.start(request, plan).unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(first.status, DownloadStatus::Queued);
        assert!(plan_dir_exists(&dir.path().join("out")));

        // Let the spawned run fail, then confirm it reported so through the sink.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let statuses = seen.lock().clone();
        assert_eq!(statuses.first(), Some(&DownloadStatus::Queued));
        assert_eq!(statuses.last(), Some(&DownloadStatus::Failed));
        assert!(manager.get(&first.id).unwrap().error.is_some());
        assert!(!manager.cancel(&first.id), "a finished download cannot be cancelled");
    }

    fn plan_dir_exists(path: &Path) -> bool {
        path.is_dir()
    }
}
