//! The IPC surface.
//!
//! Commands are domain-oriented rather than one-per-table (§133): the frontend asks for "a page of
//! history", not for rows it then has to assemble. Each is a thin adapter — validate, delegate,
//! convert the error — so the interesting logic stays in the subsystem crates where it can be
//! tested without a webview.
//!
//! ## Errors
//!
//! Every command returns [`ErrorPayload`], never a string. That is what lets the UI classify a
//! failure, decide whether to offer a retry, and render a localized message — none of which is
//! possible with prose (§74).
//!
//! ## Untrusted arguments
//!
//! Identifiers arriving here are re-validated even though the frontend validates them too. The
//! frontend is not a trusted validator: it is the part an attacker or a bug can most easily reach.

// `ErrorPayload` is the IPC wire contract, so its size is fixed by the protocol rather than by a
// choice here; boxing it would add an allocation per failure and change nothing on the wire.
#![allow(clippy::result_large_err)]
// Tauri's command macro requires `State` by value. It is a cheap borrow wrapper, not the copy the
// lint imagines.
#![allow(clippy::needless_pass_by_value)]

use beastube_core::Settings;
use beastube_core::error::{DomainError, ErrorPayload};
use beastube_core::ids::{ChannelId, VideoId};
use beastube_core::model::channel::{ChannelDetails, ChannelTab};
use beastube_core::model::video::{VideoDetails, VideoSummary};
use beastube_core::model::{
    Bookmark, ContinuationToken, HistoryEntry, Page, PlaybackPosition, SearchFilters,
    SearchResults, Suggestion,
};
use beastube_core::time_util::Timestamp;
use beastube_db::repo::history::WatchRecord;
use beastube_db::repo::searches::SearchEntry;
use tauri::State;
use tokio_util::sync::CancellationToken;

use crate::state::AppState;

/// Result of a command: the value, or a payload the UI can render and classify.
type CommandResult<T> = Result<T, ErrorPayload>;

/// How many of the user's own past queries a suggestion list may lead with.
///
/// Small on purpose: the dropdown's value is the one or two entries the user recognizes, and a
/// screenful of their own history in front of the provider's completions makes the field feel like
/// it is refusing to look anything up.
const LOCAL_SUGGESTION_LIMIT: u32 = 4;

/// Longest suggestion list returned, matching what the dropdown renders without scrolling.
const MAX_SUGGESTIONS: usize = 10;

/// Converts any subsystem error into the wire payload.
fn fail<E: DomainError + Sized>(error: E) -> ErrorPayload {
    error.to_payload()
}

/// Validates an identifier arriving from the frontend.
fn video_id(raw: &str) -> CommandResult<VideoId> {
    VideoId::new(raw).map_err(|error| {
        // A malformed id is a caller bug or an attack; either way it never reaches a query.
        ErrorPayload {
            kind: beastube_core::error::ErrorKind::Provider,
            code: "provider.invalid_input".to_owned(),
            message_key: "error.provider.invalid_input".to_owned(),
            params: std::collections::BTreeMap::from([("field".to_owned(), "videoId".to_owned())]),
            recovery: beastube_core::error::Recovery::Unrecoverable,
            diagnostic: Some(error.to_string()),
            correlation_id: None,
        }
    })
}

/// Validates a channel identifier arriving from the frontend.
fn channel_id(raw: &str) -> CommandResult<ChannelId> {
    ChannelId::new(raw).map_err(|error| ErrorPayload {
        kind: beastube_core::error::ErrorKind::Provider,
        code: "provider.invalid_input".to_owned(),
        message_key: "error.provider.invalid_input".to_owned(),
        params: std::collections::BTreeMap::from([("field".to_owned(), "channelId".to_owned())]),
        recovery: beastube_core::error::Recovery::Unrecoverable,
        diagnostic: Some(error.to_string()),
        correlation_id: None,
    })
}

// ---------------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------------

/// The current settings document.
#[tauri::command]
pub(crate) fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings()
}

/// Persists a settings document, returning the sanitized version that was stored.
///
/// The return value is authoritative: the UI adopts it rather than keeping what it sent, so an
/// out-of-range value is corrected everywhere at once.
///
/// # Errors
///
/// Returns a payload if the write fails.
#[tauri::command]
pub(crate) async fn save_settings(
    state: State<'_, AppState>,
    settings: Settings,
) -> CommandResult<Settings> {
    let sanitized = state.set_settings(settings);
    state
        .repositories
        .settings
        .save(&sanitized, Timestamp::now())
        .await
        .map_err(fail)?;
    Ok(sanitized)
}

/// Restores factory settings.
///
/// # Errors
///
/// Returns a payload if the write fails.
#[tauri::command]
pub(crate) async fn reset_settings(state: State<'_, AppState>) -> CommandResult<Settings> {
    state.repositories.settings.reset().await.map_err(fail)?;
    Ok(state.set_settings(Settings::default()))
}

// ---------------------------------------------------------------------------------------------
// Search and metadata
// ---------------------------------------------------------------------------------------------

/// Searches the active provider.
///
/// # Errors
///
/// Returns a payload if the query is empty, the provider fails, or the response cannot be read.
#[tauri::command]
pub(crate) async fn search(
    state: State<'_, AppState>,
    query: String,
    filters: SearchFilters,
    continuation: Option<ContinuationToken>,
) -> CommandResult<SearchResults> {
    // Cancellation is per-call here; superseding an in-flight search is the frontend's decision,
    // and it abandons the promise rather than needing the native side to know.
    let cancel = CancellationToken::new();
    let results = state
        .provider
        .search(&query, &filters, continuation.as_ref(), &cancel)
        .await
        .map_err(fail)?;

    // Only a first page is a search the user performed; a continuation is the same search
    // scrolling, and recording it again would inflate the query's rank in their own suggestions.
    if continuation.is_none() {
        record_search(&state, &query).await;
    }

    Ok(results)
}

/// Remembers a query, if this session records searches.
///
/// Failure is deliberately swallowed: the search itself succeeded, and turning a bookkeeping write
/// into a visible error would fail an operation the user watched work. The single decision point
/// for whether to record at all is [`AppState::records_searches`] (§50).
async fn record_search(state: &AppState, query: &str) {
    if !state.records_searches() {
        return;
    }
    let repo = &state.repositories.searches;
    if let Err(error) = repo.record(query, Timestamp::now()).await {
        tracing::warn!(%error, "a search could not be remembered");
        return;
    }
    // Pruning here rather than on a timer keeps the ceiling honest without a background task: the
    // table can only exceed it immediately after the write that pushed it over.
    let keep = state.settings().privacy.max_search_history_entries;
    if let Err(error) = repo.prune(keep).await {
        tracing::warn!(%error, "the search history could not be pruned to its limit");
    }
}

/// Autocompletes a partial query.
///
/// Never surfaces a failure: suggestions are an enhancement, and a failed lookup should leave the
/// search box working rather than showing an error under it. The `Result` is required by Tauri for
/// an async command taking a borrowed state, and is always `Ok`.
///
/// # Errors
///
/// Never returns an error; the signature exists to satisfy the command macro.
#[tauri::command]
pub(crate) async fn get_suggestions(
    state: State<'_, AppState>,
    prefix: String,
) -> CommandResult<Vec<Suggestion>> {
    // The user's own previous queries lead, because a query they have run before is far more
    // likely to be what they mean than a popular one they have never typed. They are read only
    // when this session records searches: in incognito, the local list is not consulted at all, so
    // an incognito session cannot reveal the non-incognito history through the dropdown (§50).
    let mut merged: Vec<Suggestion> = Vec::new();
    if state.records_searches() {
        match state
            .repositories
            .searches
            .matching(&prefix, LOCAL_SUGGESTION_LIMIT)
            .await
        {
            Ok(local) => merged.extend(local.into_iter().map(SearchEntry::into_suggestion)),
            Err(error) => tracing::warn!(%error, "local suggestions are unavailable"),
        }
    }

    let cancel = CancellationToken::new();
    let remote = state
        .provider
        .suggestions(&prefix, &cancel)
        .await
        .unwrap_or_default();

    // Case-insensitive dedupe, so a remote suggestion identical to one already offered from
    // history does not appear twice with two different icons.
    let mut seen: std::collections::HashSet<String> = merged
        .iter()
        .map(|suggestion| suggestion.text.to_lowercase())
        .collect();
    for suggestion in remote {
        if seen.insert(suggestion.text.to_lowercase()) {
            merged.push(suggestion);
        }
        if merged.len() >= MAX_SUGGESTIONS {
            break;
        }
    }

    merged.truncate(MAX_SUGGESTIONS);
    Ok(merged)
}

/// The queries the user searched most recently, for the empty search box.
///
/// Returns nothing when this session does not record searches, so the dropdown is simply empty in
/// incognito rather than showing another session's activity.
///
/// # Errors
///
/// Never returns an error; a read failure degrades to an empty list.
#[tauri::command]
pub(crate) async fn get_recent_searches(
    state: State<'_, AppState>,
    limit: u32,
) -> CommandResult<Vec<Suggestion>> {
    if !state.records_searches() {
        return Ok(Vec::new());
    }
    let entries = state
        .repositories
        .searches
        .recent(limit.min(LOCAL_SUGGESTION_LIMIT))
        .await
        .unwrap_or_default();
    Ok(entries.into_iter().map(SearchEntry::into_suggestion).collect())
}

/// Forgets one remembered query. Returns whether there was one.
///
/// # Errors
///
/// Returns a payload if the delete fails.
#[tauri::command]
pub(crate) async fn delete_search(state: State<'_, AppState>, query: String) -> CommandResult<bool> {
    state
        .repositories
        .searches
        .delete(&query)
        .await
        .map_err(fail)
}

/// Forgets every remembered query. Returns how many were removed.
///
/// # Errors
///
/// Returns a payload if the delete fails.
#[tauri::command]
pub(crate) async fn clear_search_history(state: State<'_, AppState>) -> CommandResult<u64> {
    state.repositories.searches.clear().await.map_err(fail)
}

/// Full details for one video.
///
/// # Errors
///
/// Returns a payload if the identifier is invalid or the provider fails.
#[tauri::command]
pub(crate) async fn get_video(
    state: State<'_, AppState>,
    video_id: String,
) -> CommandResult<VideoDetails> {
    let id = self::video_id(&video_id)?;
    let cancel = CancellationToken::new();
    state.provider.video(&id, &cancel).await.map_err(fail)
}

/// Videos related to one video.
///
/// # Errors
///
/// Returns a payload if the identifier is invalid or the provider fails.
#[tauri::command]
pub(crate) async fn get_related(
    state: State<'_, AppState>,
    video_id: String,
) -> CommandResult<Page<VideoSummary>> {
    let id = self::video_id(&video_id)?;
    let cancel = CancellationToken::new();
    state.provider.related(&id, &cancel).await.map_err(fail)
}

/// Channel metadata.
///
/// # Errors
///
/// Returns a payload if the identifier is invalid or the provider fails.
#[tauri::command]
pub(crate) async fn get_channel(
    state: State<'_, AppState>,
    channel_id: String,
) -> CommandResult<ChannelDetails> {
    let id = self::channel_id(&channel_id)?;
    let cancel = CancellationToken::new();
    state.provider.channel(&id, &cancel).await.map_err(fail)
}

/// One tab of a channel's content.
///
/// # Errors
///
/// Returns a payload if the identifier is invalid, the tab is unsupported, or the provider fails.
#[tauri::command]
pub(crate) async fn get_channel_content(
    state: State<'_, AppState>,
    channel_id: String,
    tab: ChannelTab,
    continuation: Option<ContinuationToken>,
) -> CommandResult<Page<VideoSummary>> {
    let id = self::channel_id(&channel_id)?;
    let cancel = CancellationToken::new();
    state
        .provider
        .channel_content(&id, tab, continuation.as_ref(), &cancel)
        .await
        .map_err(fail)
}

/// What the active provider supports, so the UI renders only real controls (§131).
#[tauri::command]
pub(crate) fn get_provider_capabilities(
    state: State<'_, AppState>,
) -> beastube_provider::ProviderCapabilities {
    state.provider.capabilities()
}

// ---------------------------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------------------------

/// Records that a video was opened.
///
/// Silently does nothing in incognito or with history disabled — the decision lives in
/// [`AppState::records_history`], not here.
///
/// # Errors
///
/// Returns a payload if the write fails.
#[tauri::command]
pub(crate) async fn record_watch(
    state: State<'_, AppState>,
    video: VideoSummary,
) -> CommandResult<()> {
    if !state.records_history() {
        return Ok(());
    }
    let record = WatchRecord::from_summary(&video);
    state
        .repositories
        .history
        .record_watch(&record, Timestamp::now())
        .await
        .map_err(fail)
}

/// A page of watch history, newest first.
///
/// # Errors
///
/// Returns a payload if the read fails.
#[tauri::command]
pub(crate) async fn get_history(
    state: State<'_, AppState>,
    limit: u32,
    offset: u32,
) -> CommandResult<Vec<HistoryEntry>> {
    state
        .repositories
        .history
        .list(limit, offset)
        .await
        .map_err(fail)
}

/// Searches watch history.
///
/// # Errors
///
/// Returns a payload if the read fails.
#[tauri::command]
pub(crate) async fn search_history(
    state: State<'_, AppState>,
    query: String,
    limit: u32,
) -> CommandResult<Vec<HistoryEntry>> {
    state
        .repositories
        .history
        .search(&query, limit)
        .await
        .map_err(fail)
}

/// Removes one history entry.
///
/// # Errors
///
/// Returns a payload if the write fails.
#[tauri::command]
pub(crate) async fn delete_history_entry(
    state: State<'_, AppState>,
    video_id: String,
) -> CommandResult<bool> {
    let id = self::video_id(&video_id)?;
    state.repositories.history.delete(&id).await.map_err(fail)
}

/// Deletes the entire watch history.
///
/// # Errors
///
/// Returns a payload if the write fails.
#[tauri::command]
pub(crate) async fn clear_history(state: State<'_, AppState>) -> CommandResult<u64> {
    state.repositories.history.clear().await.map_err(fail)
}

/// The stored playback position for a video, if any.
///
/// # Errors
///
/// Returns a payload if the identifier is invalid or the read fails.
#[tauri::command]
pub(crate) async fn get_position(
    state: State<'_, AppState>,
    video_id: String,
) -> CommandResult<Option<PlaybackPosition>> {
    let id = self::video_id(&video_id)?;
    state.repositories.positions.get(&id).await.map_err(fail)
}

/// Checkpoints a playback position.
///
/// Called periodically and on pause, navigation and shutdown — never every frame (§47). Suppressed
/// in incognito by the same rule as history.
///
/// # Errors
///
/// Returns a payload if the identifier is invalid or the write fails.
#[tauri::command]
pub(crate) async fn checkpoint_playback(
    state: State<'_, AppState>,
    video_id: String,
    position_ms: u64,
    duration_ms: Option<u64>,
) -> CommandResult<()> {
    if !state.records_history() {
        return Ok(());
    }
    let id = self::video_id(&video_id)?;
    state
        .repositories
        .positions
        .upsert(&id, position_ms, duration_ms, Timestamp::now())
        .await
        .map_err(fail)
}

/// Videos worth resuming, newest first.
///
/// # Errors
///
/// Returns a payload if the read fails.
#[tauri::command]
pub(crate) async fn get_resumable(
    state: State<'_, AppState>,
    limit: u32,
) -> CommandResult<Vec<HistoryEntry>> {
    // The positions table knows what is resumable; history knows what to display. Joining here
    // rather than in SQL keeps the resume rule (`PlaybackPosition::resume_at_ms`) in one place.
    let candidates = state
        .repositories
        .positions
        .resumable(limit)
        .await
        .map_err(fail)?;

    let mut entries = Vec::with_capacity(candidates.len());
    for (video_id, _) in candidates {
        if let Some(entry) = state
            .repositories
            .history
            .get(&video_id)
            .await
            .map_err(fail)?
            && entry.is_resumable()
        {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// Saved bookmarks, newest first.
///
/// # Errors
///
/// Returns a payload if the read fails.
#[tauri::command]
pub(crate) async fn get_bookmarks(
    state: State<'_, AppState>,
    limit: u32,
    offset: u32,
) -> CommandResult<Vec<Bookmark>> {
    state
        .repositories
        .bookmarks
        .list(limit, offset)
        .await
        .map_err(fail)
}

/// Saves a bookmark.
///
/// Unlike history, bookmarking is an explicit user action, so it is recorded even in incognito —
/// the user asked for it, and silently discarding it would be the surprising behaviour.
///
/// # Errors
///
/// Returns a payload if the write fails.
#[tauri::command]
pub(crate) async fn set_bookmark(
    state: State<'_, AppState>,
    video: VideoSummary,
) -> CommandResult<()> {
    let mut bookmark =
        beastube_db::repo::bookmarks::NewBookmark::of(video.id.clone(), &video.title);
    bookmark.channel_id = video.channel_id.clone();
    bookmark.channel_name = video.channel_name.clone();
    bookmark.thumbnails = video.thumbnails.clone();

    state
        .repositories
        .bookmarks
        .add(&bookmark, Timestamp::now())
        .await
        .map_err(fail)
}

/// Removes a bookmark.
///
/// # Errors
///
/// Returns a payload if the identifier is invalid or the write fails.
#[tauri::command]
pub(crate) async fn remove_bookmark(
    state: State<'_, AppState>,
    video_id: String,
) -> CommandResult<bool> {
    let id = self::video_id(&video_id)?;
    state.repositories.bookmarks.remove(&id).await.map_err(fail)
}

// ---------------------------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------------------------

/// Enters or leaves incognito, returning the resulting state.
#[tauri::command]
pub(crate) fn set_incognito(state: State<'_, AppState>, enabled: bool) -> bool {
    state.set_incognito(enabled);
    state.is_incognito()
}

/// Whether this session is incognito.
#[tauri::command]
pub(crate) fn is_incognito(state: State<'_, AppState>) -> bool {
    state.is_incognito()
}

// ---------------------------------------------------------------------------------------------
// Filtering
// ---------------------------------------------------------------------------------------------

/// Filtering state and counters, for the settings and diagnostics screens.
///
/// Counts only: this never carries a URL, host, video or channel, because a filtering layer sees
/// every request and a diagnostic that carried one would put a browsing log in the subsystem best
/// placed to build it (§99).
#[tauri::command]
pub(crate) fn get_filtering_diagnostics(
    state: State<'_, AppState>,
) -> beastube_filtering::diagnostics::FilteringSnapshot {
    state.filtering_snapshot()
}

/// Restores the rule set shipped with the application.
///
/// Used when a downloaded set has misbehaved and the user wants a known-good baseline without
/// waiting for the automatic rollback window.
///
/// # Errors
///
/// Returns a payload if the built-in set fails validation, which would indicate a build defect.
#[tauri::command]
pub(crate) fn reset_filter_rules(
    state: State<'_, AppState>,
) -> CommandResult<beastube_filtering::diagnostics::FilteringSnapshot> {
    let candidate = beastube_filtering::builtin::builtin_rule_set();
    state.filtering.activate(candidate).map_err(fail)?;
    Ok(state.filtering_snapshot())
}

// ---------------------------------------------------------------------------------------------
// Storage and diagnostics
// ---------------------------------------------------------------------------------------------

/// What the application is storing on this device.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct StorageStats {
    /// Size of the library database in bytes.
    database_bytes: u64,
    /// Size of the provider's extractor cache in bytes.
    cache_bytes: u64,
    /// Number of history rows.
    history_entries: u64,
    /// Number of saved bookmarks.
    bookmark_entries: u64,
    /// Number of stored resume positions.
    position_entries: u64,
    /// Absolute path of the library database, so the privacy screen can name it.
    database_path: String,
    /// Absolute path of the cache directory.
    cache_path: String,
}

/// Measures on-disk usage.
///
/// # Errors
///
/// Returns a payload if the database cannot be read.
#[tauri::command]
pub(crate) async fn get_storage_stats(state: State<'_, AppState>) -> CommandResult<StorageStats> {
    let database_bytes = state.database.size_bytes().await.map_err(fail)?;
    let history_entries = state.repositories.history.count().await.map_err(fail)?;
    let bookmark_entries = state.repositories.bookmarks.count().await.map_err(fail)?;
    let position_entries = state.repositories.positions.count().await.map_err(fail)?;

    Ok(StorageStats {
        database_bytes,
        cache_bytes: directory_size(&state.provider_cache_dir),
        history_entries,
        bookmark_entries,
        position_entries,
        database_path: state
            .database
            .path()
            .map_or_else(|| "(in memory)".to_owned(), |path| path.display().to_string()),
        cache_path: state.provider_cache_dir.display().to_string(),
    })
}

/// Sums the size of every file under `root`, ignoring what it cannot read.
///
/// A directory the process cannot traverse contributes zero rather than failing the whole
/// measurement: an approximate size is more useful on a storage screen than an error.
fn directory_size(root: &std::path::Path) -> u64 {
    fn walk(path: &std::path::Path, total: &mut u64, depth: usize) {
        // Bounded so a symlink loop or a pathological tree cannot spin here.
        if depth > 8 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                walk(&entry.path(), total, depth + 1);
            } else {
                *total = total.saturating_add(metadata.len());
            }
        }
    }

    let mut total = 0;
    walk(root, &mut total, 0);
    total
}

/// Deletes the provider's extractor cache.
///
/// Cache data only: history, bookmarks, playlists and settings are untouched, which is what makes
/// this distinct from resetting application data (§101).
///
/// # Errors
///
/// Returns a payload if the directory cannot be removed.
#[tauri::command]
pub(crate) async fn clear_cache(state: State<'_, AppState>) -> CommandResult<StorageStats> {
    let cache_dir = state.provider_cache_dir.clone();
    // Removing the directory rather than its contents keeps this a single syscall-level operation;
    // the provider recreates it on next use.
    if let Err(error) = std::fs::remove_dir_all(&cache_dir)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(ErrorPayload {
            kind: beastube_core::error::ErrorKind::FileSystem,
            code: "filesystem.permission_denied".to_owned(),
            message_key: "error.filesystem.permission_denied".to_owned(),
            params: std::collections::BTreeMap::new(),
            recovery: beastube_core::error::Recovery::RetryManual,
            diagnostic: Some(error.to_string()),
            correlation_id: None,
        });
    }
    let _ = std::fs::create_dir_all(&cache_dir);

    get_storage_stats(state).await
}

// ---------------------------------------------------------------------------------------------
// Build and environment
// ---------------------------------------------------------------------------------------------

/// Facts about this installation, for the diagnostics screen.
///
/// Everything here is read from the running process. Nothing is transmitted anywhere — the screen
/// exists so a user can answer "what am I running?" without trusting a support channel to ask
/// (§121).
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct AppInfo {
    /// Version from the crate manifest.
    app_version: String,
    /// Target triple this binary was built for.
    target: String,
    /// Operating system family, from the compilation target.
    os: String,
    /// CPU architecture, from the compilation target.
    arch: String,
    /// Whether this is a debug build.
    debug_build: bool,
    /// WebView2 runtime version, or `None` if it cannot be determined.
    webview_version: Option<String>,
    /// Identifier of the active playback adapter (ADR-0001).
    playback_adapter: &'static str,
    /// Number of logical CPUs available to the process.
    cpu_cores: usize,
    /// Milliseconds since the process started serving commands.
    uptime_ms: u64,
}

/// Reports build and environment facts.
#[tauri::command]
pub(crate) fn get_app_info(state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        target: format!(
            "{}-{}",
            std::env::consts::ARCH,
            std::env::consts::OS
        ),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        debug_build: cfg!(debug_assertions),
        // Absent rather than guessed: a wrong version on a diagnostics screen is worse than none.
        webview_version: tauri::webview_version().ok(),
        playback_adapter: "iframe",
        cpu_cores: std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get),
        uptime_ms: state.uptime_ms(),
    }
}
