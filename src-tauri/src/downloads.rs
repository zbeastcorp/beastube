//! The download surface.
//!
//! Commands for starting, cancelling and inspecting downloads, plus the pickers the settings
//! screen uses. Everything interesting lives in `beastube-download`; this module is the adapter
//! that turns application state into a plan and a subsystem error into an [`ErrorPayload`], exactly
//! as `commands.rs` does for the provider.
//!
//! Where files go and which tools are used is decided by [`AppState`], not here — one place
//! resolves it, so the plan a download runs under and the paths the settings screen shows can
//! never disagree.
//!
//! ## Nothing is ever fetched
//!
//! [`get_download_tools`] reports what is installed. It does not download `yt-dlp`, update it, or
//! read cookies from a browser. A missing downloader is a visible fact with a stated remedy rather
//! than a button that fails when pressed.

// `ErrorPayload` is the IPC wire contract, so its size is fixed by the protocol rather than by a
// choice here; boxing it would add an allocation per failure and change nothing on the wire.
#![allow(clippy::result_large_err)]
// Tauri's command macro requires `State` and `AppHandle` by value. Both are cheap borrow/refcount
// wrappers, not the copies the lint imagines.
#![allow(clippy::needless_pass_by_value)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::SystemTime;

use beastube_core::error::{ErrorKind, ErrorPayload, Recovery};
use beastube_core::events::DownloadProgress;
use beastube_core::ids::VideoId;
use beastube_download::{DownloadError, DownloadRequest, JsRuntimeKind, version_of};
use parking_lot::Mutex;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::commands::{CommandResult, fail};
use crate::state::AppState;

/// Longest title accepted from the frontend, in characters.
///
/// The title is display text carried alongside the download and never reaches a command line — the
/// filename comes from the downloader's own template — but it is still a string from the webview
/// held for the life of the session, so it is bounded.
const MAX_TITLE_LEN: usize = 300;

/// What the settings screen shows about the local setup.
///
/// Every field reports what was actually found. A version is present only when the tool ran and
/// printed one, so "installed but not working" reads as a missing version rather than as fine.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct DownloadTools {
    /// Absolute path of the downloader, or `None` if none was found.
    downloader_path: Option<String>,
    /// First line of `yt-dlp --version`.
    downloader_version: Option<String>,
    /// Absolute path of the muxer, or `None`.
    ffmpeg_path: Option<String>,
    /// First line of `ffmpeg -version`.
    ffmpeg_version: Option<String>,
    /// Which JavaScript runtime was found for the downloader, if any.
    js_runtime: Option<JsRuntimeKind>,
    /// The directory downloads are written to, resolved.
    directory: String,
    /// Whether a download can be started at all: both tools present.
    available: bool,
    /// Whether separate video and audio tracks can be joined.
    ///
    /// Reported separately from `available` so the settings screen can name which tool is missing,
    /// but it is not optional — see `DownloadError::MuxerMissing`.
    can_merge: bool,
}

/// A payload for something the application was asked to act on and could not find.
fn not_found(detail: &str) -> ErrorPayload {
    ErrorPayload {
        kind: ErrorKind::FileSystem,
        code: "filesystem.not_found".to_owned(),
        message_key: "error.filesystem.not_found".to_owned(),
        params: std::collections::BTreeMap::new(),
        recovery: Recovery::Unrecoverable,
        diagnostic: Some(detail.to_owned()),
        correlation_id: None,
    }
}

/// A payload for a path the system refused to open.
fn could_not_open(error: &dyn std::fmt::Display) -> ErrorPayload {
    ErrorPayload {
        kind: ErrorKind::FileSystem,
        code: "filesystem.not_found".to_owned(),
        message_key: "error.filesystem.not_found".to_owned(),
        params: std::collections::BTreeMap::new(),
        recovery: Recovery::RetryManual,
        diagnostic: Some(error.to_string()),
        correlation_id: None,
    }
}

/// Validates an identifier arriving from the frontend.
///
/// Re-validated here even though `commands.rs` validates the same shape, because the frontend is
/// not a trusted validator and this value becomes the URL a child process is given.
fn video_id(raw: &str) -> CommandResult<VideoId> {
    VideoId::new(raw).map_err(|error| ErrorPayload {
        kind: ErrorKind::Provider,
        code: "provider.invalid_input".to_owned(),
        message_key: "error.provider.invalid_input".to_owned(),
        params: std::collections::BTreeMap::from([("field".to_owned(), "videoId".to_owned())]),
        recovery: Recovery::Unrecoverable,
        diagnostic: Some(error.to_string()),
        correlation_id: None,
    })
}

/// Trims a title to something bounded, on a character boundary.
fn bounded_title(raw: &str) -> String {
    raw.chars().take(MAX_TITLE_LEN).collect()
}

/// Starts a download, or returns the one already running for this video.
///
/// Returns as soon as the download is accepted; everything after that arrives on the
/// `download:progress` event, so the button shows real state without polling. Pressing it
/// twice is harmless — the second press gets the first download's record, not a second process.
///
/// # Errors
///
/// Returns a payload if the identifier is malformed, no downloader is installed, or the target
/// directory cannot be created.
#[tauri::command]
pub(crate) async fn start_download(
    state: State<'_, AppState>,
    video_id: String,
    title: String,
) -> CommandResult<DownloadProgress> {
    let id = self::video_id(&video_id)?;
    let plan = state.download_plan().map_err(fail)?;

    state
        .downloads
        .start(
            DownloadRequest {
                video_id: id,
                title: bounded_title(&title),
            },
            plan,
        )
        .map_err(fail)
}

/// Stops a download and removes what it had written. `false` if it is unknown or already over.
#[tauri::command]
pub(crate) fn cancel_download(state: State<'_, AppState>, id: String) -> bool {
    state.downloads.cancel(&id)
}

/// Every download of this session, oldest first.
///
/// A screen that mounts mid-download reads this once and follows the event afterwards, which is
/// what lets the button on a revisited video show a download that is still running.
// Off the UI thread: this touches the filesystem (see `forget_missing`), and a
// synchronous command runs inline on the main thread — so a folder of finished
// downloads on a slow or sleeping disk stalled the window on every focus.
#[tauri::command(async)]
pub(crate) fn get_downloads(state: State<'_, AppState>) -> Vec<DownloadProgress> {
    state.downloads.snapshot()
}

/// Why the provider will not play a video, in its own words.
///
/// Called only after the embedded player has already refused. The player reports a number, and the
/// numbers it uses for a refusal cover several unrelated situations — an age gate, a region block, a
/// copyright claim, a video that has gone — so the application used to guess at the cause and put
/// that guess on screen as a fact. It guessed wrong often enough to be worth fixing.
///
/// `yt-dlp` is already bundled and already driven as a child process, and it will say. Measured on
/// a video the player refused: the web client answers "Video unavailable" while the android client
/// answers "It was blocked due to the claimed content by Netflix" — same video, same moment.
///
/// `None` when there is nothing better to say than the caller already has: no downloader installed,
/// the probe timed out, or the provider offered no reason. The caller keeps its own wording then,
/// rather than replacing it with a worse one.
///
/// # Errors
///
/// Never. A diagnosis that cannot be made is `None`; the caller is already showing a failure and a
/// second failure stacked on it would help nobody.
#[tauri::command(async)]
pub(crate) async fn diagnose_playback(
    state: State<'_, AppState>,
    video_id: String,
) -> CommandResult<Option<String>> {
    let Some(downloader) = state.download_tools().downloader else {
        return Ok(None);
    };
    match beastube_download::playability(&downloader, &video_id).await {
        beastube_download::Playability::Refused { reason } => Ok(Some(reason)),
        beastube_download::Playability::Available | beastube_download::Playability::Unknown => {
            Ok(None)
        }
    }
}

/// Enough of a file to notice it being replaced, without reading it.
///
/// Length and modification time together: a new `yt-dlp.exe` written over the old one changes at
/// least one of them, and neither costs more than a `stat`.
#[derive(PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

/// What has already been read from one executable: the stamp it had, and what it said.
type Known = (Stamp, Option<String>);

/// Version strings already read, keyed by the executable they were read from.
///
/// Reading a version means launching the tool and waiting for it to print one. For `yt-dlp` that is
/// a `PyInstaller` bundle unpacking itself before it will answer, and measured in the built
/// application the pair cost **960 ms every time `get_download_tools` was called** — at every
/// launch, and again on every visit to the settings screen.
///
/// Nothing about that answer changes while the file does not, so it is read once per file. The
/// stamp is what makes that safe: the settings screen updates `yt-dlp` in place and then asks
/// again, and a cache keyed only on the path would have gone on reporting the version it replaced.
static VERSIONS: LazyLock<Mutex<HashMap<PathBuf, Known>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Stats `tool` for a stamp, or `None` if it cannot be read.
fn stamp_of(tool: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(tool).ok()?;
    Some(Stamp {
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}

/// The tool's version, launching it only when this build has not already asked this exact file.
async fn cached_version_of(tool: &Path) -> Option<String> {
    let Some(stamp) = stamp_of(tool) else {
        // Gone since it was located. Nothing to run, and nothing worth remembering.
        return None;
    };
    if let Some((seen, version)) = VERSIONS.lock().get(tool)
        && *seen == stamp
    {
        return version.clone();
    }

    // Deliberately not holding the lock across the launch: two screens asking at once should wait
    // for the tool, not for each other, and running it twice is merely wasteful rather than wrong.
    let version = version_of(tool).await;
    VERSIONS
        .lock()
        .insert(tool.to_path_buf(), (stamp, version.clone()));
    version
}

/// What is installed, and where files will go.
#[tauri::command]
pub(crate) async fn get_download_tools(state: State<'_, AppState>) -> CommandResult<DownloadTools> {
    let tools = state.download_tools();
    let directory = state.download_directory();

    // Both versions at once: on the first call for a given pair of files each is a process launch,
    // and running them in sequence would make the settings screen wait for the sum rather than for
    // the slower of the two. Afterwards both are answered from `VERSIONS` without launching
    // anything.
    let (downloader_version, ffmpeg_version) = tokio::join!(
        async {
            match &tools.downloader {
                Some(path) => cached_version_of(path).await,
                None => None,
            }
        },
        async {
            match &tools.ffmpeg {
                Some(path) => cached_version_of(path).await,
                None => None,
            }
        }
    );

    Ok(DownloadTools {
        // Both, not just the downloader. YouTube offers no combined audio-and-video format to the
        // clients yt-dlp reaches, so on a machine without ffmpeg a download button would be a
        // control that cannot do its job.
        available: tools.downloader.is_some() && tools.ffmpeg.is_some(),
        can_merge: tools.ffmpeg.is_some(),
        downloader_path: tools.downloader.map(|path| path.display().to_string()),
        downloader_version,
        ffmpeg_path: tools.ffmpeg.map(|path| path.display().to_string()),
        ffmpeg_version,
        js_runtime: tools.js_runtime.map(|runtime| runtime.kind),
        directory: directory.display().to_string(),
    })
}

/// Shows a finished download in the file manager, selected.
///
/// # Errors
///
/// Returns a payload if the download is unknown or unfinished, or if its file has since been moved
/// or deleted — which is why the path is checked rather than trusted.
// Off the UI thread: opens a file manager, which blocks while the shell starts.
#[tauri::command(async)]
pub(crate) fn reveal_download(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<()> {
    let Some(path) = state
        .downloads
        .get(&id)
        .and_then(|download| download.path)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    else {
        return Err(not_found(
            "the downloaded file is no longer where it was saved",
        ));
    };

    app.opener()
        .reveal_item_in_dir(&path)
        .map_err(|error| could_not_open(&error))
}

/// Opens the download directory in the file manager.
///
/// The directory is created if it does not exist. Created rather than refused: it is missing
/// precisely when nothing has been downloaded yet, which is the moment someone is most likely to
/// want to see where files will go.
///
/// # Errors
///
/// Returns a payload if the directory cannot be created or the system refuses to open it.
// Off the UI thread: same shell call, same stall.
#[tauri::command(async)]
pub(crate) fn open_download_directory(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    let directory = state.download_directory();
    std::fs::create_dir_all(&directory).map_err(|error| {
        fail(DownloadError::Directory {
            detail: format!("{}: {error}", directory.display()),
        })
    })?;

    app.opener()
        .open_path(directory.display().to_string(), None::<&str>)
        .map_err(|error| could_not_open(&error))
}

/// A payload for a picker that closed without answering.
///
/// Distinct from the user cancelling, which is `Ok(None)`: this is the dialog going away without
/// either a choice or a cancellation, and reporting it as "cancelled" would leave a setting
/// silently unchanged with no explanation.
fn dialog_failed() -> ErrorPayload {
    ErrorPayload {
        kind: ErrorKind::Permission,
        code: "permission.denied".to_owned(),
        message_key: "error.permission.denied".to_owned(),
        params: std::collections::BTreeMap::new(),
        recovery: Recovery::RetryManual,
        diagnostic: Some("the file dialog closed without a result".to_owned()),
        correlation_id: None,
    }
}

/// Asks the user for a download directory. `Ok(None)` if they cancelled.
///
/// A picker rather than a text field: a typed path has to be validated, refused and explained,
/// where a picker can only return a directory that exists and that the user chose.
///
/// # Errors
///
/// Returns a payload if the dialog could not be shown or closed without answering.
#[tauri::command]
pub(crate) async fn pick_download_directory(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<Option<String>> {
    let start = state.download_directory();
    let (sender, receiver) = tokio::sync::oneshot::channel();

    let mut builder = app.dialog().file();
    if start.is_dir() {
        builder = builder.set_directory(&start);
    }
    builder.pick_folder(move |picked| {
        // The receiver is gone only if the caller stopped waiting, which is not a failure here.
        let _ = sender.send(picked);
    });

    Ok(receiver
        .await
        .map_err(|_| dialog_failed())?
        .and_then(|path| path.into_path().ok())
        .map(|path| path.display().to_string()))
}

/// Asks the user to point at a `yt-dlp` executable. `Ok(None)` if they cancelled.
///
/// # Errors
///
/// Returns a payload if the dialog could not be shown or closed without answering.
#[tauri::command]
pub(crate) async fn pick_downloader_executable(app: AppHandle) -> CommandResult<Option<String>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();

    let builder = app.dialog().file();
    // A convenience, not a validation: the chosen file is checked when it is used, and on
    // platforms without extensions a filter would exclude the right answer.
    #[cfg(windows)]
    let builder = builder.add_filter("yt-dlp", &["exe", "cmd", "bat", "com"]);

    builder.pick_file(move |picked| {
        let _ = sender.send(picked);
    });

    Ok(receiver
        .await
        .map_err(|_| dialog_failed())?
        .and_then(|path| path.into_path().ok())
        .map(|path| path.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_title_is_cut_on_a_character_boundary() {
        // Multi-byte on purpose: truncating by bytes would split a character and panic.
        let long = "é".repeat(MAX_TITLE_LEN + 50);
        assert_eq!(bounded_title(&long).chars().count(), MAX_TITLE_LEN);
    }

    #[test]
    fn a_short_title_is_untouched() {
        assert_eq!(bounded_title("Clip"), "Clip");
    }

    #[test]
    fn a_malformed_identifier_never_becomes_a_url() {
        assert!(video_id("../../etc/passwd").is_err());
        assert!(video_id("dQw4w9WgXcQ").is_ok());
    }
}
