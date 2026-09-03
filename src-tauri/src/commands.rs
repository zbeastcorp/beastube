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
use beastube_core::model::search::{SearchItem, SearchResultKind};
use beastube_core::model::video::{VideoDetails, VideoSummary};
use beastube_core::model::{
    Bookmark, ContinuationToken, HistoryEntry, Page, PlaybackPosition, SearchFilters,
    SearchResults, Suggestion,
};
use beastube_core::time_util::Timestamp;
use beastube_db::repo::history::WatchRecord;
use beastube_db::repo::searches::SearchEntry;
use std::collections::HashSet;
use std::sync::Arc;

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

// ---------------------------------------------------------------------------------------------
// Feeds
// ---------------------------------------------------------------------------------------------

/// How many watched videos seed a recommendation pass.
///
/// Each seed costs one request, and they run concurrently, so this is a latency/breadth trade
/// rather than a correctness one. Four gives a visibly varied feed while keeping the slowest path
/// to one round trip.
const RECOMMENDATION_SEEDS: usize = 4;

/// How much history is scanned to decide what the user has already seen.
///
/// Recommendations that lead with videos already watched are the most obvious way for a feed to
/// look broken, so the exclusion set is generous.
const WATCHED_LOOKBACK: u32 = 300;

/// Topics used when there is nothing personal to recommend from.
///
/// A deliberately broad, evergreen spread: this is the first screen of a fresh install, where the
/// honest claim is "here is what is on YouTube", not "here is what we think you want". The UI
/// labels the section accordingly, so the distinction is visible rather than implied.
const DISCOVERY_TOPICS: &[&str] = &[
    "music",
    "technology",
    "science",
    "cooking",
    "travel",
    "documentary",
    "gaming",
    "live performance",
];

/// Topics used to fill the Shorts feed.
///
/// Hashtag queries, because they surface short-form content far more reliably than plain topical
/// ones — measured both ways: `art timelapse` returns a full page of ten-minute videos, while
/// `art #shorts` returns few items but nearly all of them short. Few-but-right beats
/// many-but-wrong, and the expansion pass below is what turns "few" into a feed.
const SHORTS_TOPICS: &[&str] = &[
    "#shorts",
    "funny #shorts",
    "music #shorts",
    "cooking #shorts",
    "sports #shorts",
    "science #shorts",
    "art #shorts",
    "animals #shorts",
];



/// Where a set of recommendations came from.
///
/// Returned so the UI can label the section truthfully. A feed derived from broad topics but
/// presented as "recommended for you" would claim a personalization that did not happen (§131).
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecommendationSource {
    /// Derived from videos the user watched.
    Watched,
    /// Derived from what the user searched for.
    Searched,
    /// Broad topics, because there was nothing personal to derive from.
    Discover,
}

/// A recommendation set and the reason it looks the way it does.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RecommendedFeed {
    /// The videos, best first.
    videos: Vec<VideoSummary>,
    /// What they were derived from.
    source: RecommendationSource,
}

/// Videos to show on the home screen.
///
/// ## How the ranking works, and where it happens
///
/// Entirely on this device. The most recently watched videos are used as seeds, the provider's
/// related list for each is fetched, and the results are interleaved so no single seed dominates.
/// Videos already in the local history are removed. Nothing about the user is sent anywhere: the
/// provider is asked "what is related to this video", never "what should this person watch" — which
/// is the whole reason a useful home screen here needs no account (§43).
///
/// The user can switch this off (`privacy.local_recommendations_enabled`), and incognito suppresses
/// it for the session. Either way the feed falls back to broad topics rather than going empty.
///
/// # Errors
///
/// Never returns an error. Every failure degrades to a smaller feed, because a home screen showing
/// an error instead of content is worse than one showing less content (§81).
#[tauri::command]
pub(crate) async fn get_recommended(
    state: State<'_, AppState>,
    limit: u32,
) -> CommandResult<RecommendedFeed> {
    let limit = limit.clamp(1, 120) as usize;
    let personalize =
        state.settings().privacy.local_recommendations_enabled && !state.is_incognito();

    let history = state
        .repositories
        .history
        .list(WATCHED_LOOKBACK, 0)
        .await
        .unwrap_or_default();

    let watched: HashSet<String> = history
        .iter()
        .map(|entry| entry.video_id.as_str().to_owned())
        .collect();

    if personalize {
        let seeds = spread_seeds(&history, RECOMMENDATION_SEEDS);

        if !seeds.is_empty() {
            let videos = interleave(related_lists(&state, seeds).await, &watched, limit);
            if !videos.is_empty() {
                return Ok(RecommendedFeed {
                    videos,
                    source: RecommendationSource::Watched,
                });
            }
        }

        // Nothing watched, or nothing came back. The user's own searches are the next-best local
        // signal, and they sit under the same permission as the watch history.
        let queries: Vec<String> = state
            .repositories
            .searches
            .recent(RECOMMENDATION_SEEDS_U32)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|entry| entry.query)
            .collect();

        if !queries.is_empty() {
            let videos = interleave(search_lists(&state, queries).await, &watched, limit);
            if !videos.is_empty() {
                return Ok(RecommendedFeed {
                    videos,
                    source: RecommendationSource::Searched,
                });
            }
        }
    }

    let topics = rotating(DISCOVERY_TOPICS, RECOMMENDATION_SEEDS);
    Ok(RecommendedFeed {
        videos: interleave(search_lists(&state, topics).await, &watched, limit),
        source: RecommendationSource::Discover,
    })
}

/// [`RECOMMENDATION_SEEDS`] as the width the repository takes.
const RECOMMENDATION_SEEDS_U32: u32 = 4;

/// Short-form videos for the Shorts tab.
///
/// The provider has no login-free Shorts listing, so this searches topics and keeps only what the
/// provider marks as short-form. That is a real feed of real Shorts rather than a text search for
/// the word, which is what the tab did before.
///
/// # Errors
///
/// Never returns an error; a failed topic contributes nothing and the rest still fill the tab.
#[tauri::command]
pub(crate) async fn get_shorts_feed(
    state: State<'_, AppState>,
    limit: u32,
) -> CommandResult<Vec<VideoSummary>> {
    let limit = limit.clamp(1, 120) as usize;

    // The viewer's own searches come first. They are the clearest statement of interest the
    // application has, they cost nothing to read, and they are the reason a tab can be full even
    // when the topic searches happen to come back thin. Consulted under the same permission as the
    // rest of the local ranking, and skipped in incognito.
    let mut queries: Vec<String> = if state.records_searches() {
        state
            .repositories
            .searches
            .recent(SEARCH_SEED_COUNT)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|entry| format!("{} #shorts", entry.query))
            .collect()
    } else {
        Vec::new()
    };
    queries.extend(rotating_often(SHORTS_TOPICS, SHORTS_TOPICS.len()));

    // The viewer's own watch history is the richest source of short-form video available here:
    // related lists are full of it, and unlike a topic search they cannot come back empty for
    // reasons that have nothing to do with the query. Consulted under the same permission as the
    // home recommendations, and skipped entirely in incognito.
    let mut lists = if state.settings().privacy.local_recommendations_enabled
        && !state.is_incognito()
    {
        let history = state
            .repositories
            .history
            .list(WATCHED_LOOKBACK, 0)
            .await
            .unwrap_or_default();
        let seeds = spread_seeds(&history, RECOMMENDATION_SEEDS);
        related_lists(&state, seeds)
            .await
            .into_iter()
            .map(|list| list.into_iter().filter(is_short_form).collect::<Vec<_>>())
            .collect()
    } else {
        Vec::new()
    };

    let searched = fan_out(queries, |topic| {
        let provider = Arc::clone(&state.provider);
        async move {
            let cancel = CancellationToken::new();
            // The dedicated shorts surface, which reads the shelf ordinary search parsing drops.
            // One request returns more short-form video than six pages of the typed search did.
            provider
                .search_shorts(&topic, &cancel)
                .await
                .unwrap_or_default()
        }
    })
    .await;

    lists.extend(searched);

    // Returned as soon as the concurrent wave lands. There used to be a second, sequential
    // expansion pass here, which roughly doubled the time before the first short appeared — and
    // bought nothing, because `get_more_shorts` performs exactly that expansion a moment later
    // while the viewer is already watching. Latency on the first batch is the only thing this
    // command is really competing on.
    Ok(interleave(lists, &HashSet::new(), limit))
}

/// How many of the shorts already found are used to look for more.
const SHORTS_EXPANSION_SEEDS: usize = 6;

/// How many of the viewer's own past searches seed a feed.
const SEARCH_SEED_COUNT: u32 = 3;

/// Longest a video can be and still be treated as short-form, in milliseconds.
///
/// YouTube's current ceiling for a Short, raised from sixty seconds to three minutes. Used because
/// the extractor's own marker is unreliable here: it comes from the renderer shape the response
/// happened to use, so the same video is marked in one listing and unmarked in another, and most
/// shorts arrive inside a shelf this extractor discards entirely.
///
/// The card surfaces use a stricter sixty-second test, because there a false positive puts a
/// landscape video in a portrait card across the whole application. Here the cost is much lower —
/// the stage is portrait and a wider video is simply letterboxed inside it, the same way YouTube
/// presents a short that is not 9:16 — while the cost of being too strict is a tab with one video
/// in it.
const SHORT_MAX_DURATION_MS: u64 = 180_000;

/// Whether a video belongs in the Shorts tab.
///
/// The marker when it is set, and a sub-minute duration otherwise. This admits the occasional
/// landscape video, and that is a deliberate trade: the stage is portrait with a blurred backdrop
/// filling whatever the video does not, so a landscape item looks like a letterboxed short rather
/// than a broken frame — while the alternative, holding out for a marker that usually is not there,
/// empties the tab entirely.
///
/// A video with no duration at all is excluded. An unknown length is not evidence of a short one.
fn is_short_form(video: &VideoSummary) -> bool {
    video.is_short
        || video
            .duration_ms
            .is_some_and(|duration| duration > 0 && duration <= SHORT_MAX_DURATION_MS)
}

/// Fetches the related list for each seed, concurrently.
///
/// One slow or failing seed costs its own list and nothing else: the point of fanning out is that
/// the feed is assembled from whatever came back.
async fn related_lists(state: &AppState, seeds: Vec<VideoId>) -> Vec<Vec<VideoSummary>> {
    fan_out(seeds, |seed| {
        let provider = Arc::clone(&state.provider);
        async move {
            let cancel = CancellationToken::new();
            provider
                .related(&seed, &cancel)
                .await
                .map(|page| page.items)
                .unwrap_or_default()
        }
    })
    .await
}

/// Runs each query as a search, concurrently, keeping only the videos.
async fn search_lists(state: &AppState, queries: Vec<String>) -> Vec<Vec<VideoSummary>> {
    fan_out(queries, |query| {
        let provider = Arc::clone(&state.provider);
        async move {
            let cancel = CancellationToken::new();
            let filters = SearchFilters {
                kind: SearchResultKind::Videos,
                ..SearchFilters::default()
            };
            provider
                .search(&query, &filters, None, &cancel)
                .await
                .map(|results| videos_of(results.page.items))
                .unwrap_or_default()
        }
    })
    .await
}

/// Runs `operation` over every input concurrently, collecting whatever completed.
///
/// A panicking task contributes an empty list rather than poisoning the feed — a home screen is not
/// worth failing over.
async fn fan_out<I, F, Fut>(inputs: Vec<I>, operation: F) -> Vec<Vec<VideoSummary>>
where
    I: Send + 'static,
    F: Fn(I) -> Fut,
    Fut: std::future::Future<Output = Vec<VideoSummary>> + Send + 'static,
{
    let mut set = tokio::task::JoinSet::new();
    for input in inputs {
        set.spawn(operation(input));
    }

    let mut lists = Vec::new();
    while let Some(joined) = set.join_next().await {
        lists.push(joined.unwrap_or_default());
    }
    lists
}

/// Keeps the video results, discarding channels and playlists.
fn videos_of(items: Vec<SearchItem>) -> Vec<VideoSummary> {
    items
        .into_iter()
        .filter_map(|item| match item {
            SearchItem::Video(video) => Some(video),
            SearchItem::Channel(_) | SearchItem::Playlist(_) => None,
        })
        .collect()
}

/// Most videos any single channel may contribute to one feed.
///
/// Without a cap, seeding from four recently watched videos by the same creator returns four
/// heavily overlapping related lists and the feed becomes one channel repeated — which is what
/// "most same video coming" describes.
const MAX_PER_CHANNEL: usize = 3;

/// Picks seeds spread across the history rather than the newest few.
///
/// The last four watched videos are usually four episodes of the same thing, so their related lists
/// agree with each other and the feed collapses onto one topic. Sampling at a stride across a wider
/// window gives the merge genuinely different material to interleave, and the offset moves with the
/// clock so two launches do not produce the same feed.
fn spread_seeds(history: &[HistoryEntry], count: usize) -> Vec<VideoId> {
    if history.is_empty() || count == 0 {
        return Vec::new();
    }
    let stride = (history.len() / count).max(1);
    // Minutes rather than milliseconds: within one launch the feed stays stable, across launches it
    // moves.
    let offset = usize::try_from(Timestamp::now().as_millis().div_euclid(60_000).max(0))
        .unwrap_or(0)
        % history.len();

    let mut seeds = Vec::with_capacity(count);
    let mut seen = HashSet::new();
    for step in 0..count {
        let index = (offset + step * stride) % history.len();
        let entry = &history[index];
        if seen.insert(entry.video_id.as_str().to_owned()) {
            seeds.push(entry.video_id.clone());
        }
    }
    seeds
}

/// Merges lists round-robin, dropping duplicates and anything already watched.
///
/// Round-robin rather than concatenation: taking one from each list in turn means the first screen
/// reflects every seed, whereas concatenating would show several screens of whatever the first seed
/// was related to before the second seed appeared at all.
fn interleave(
    lists: Vec<Vec<VideoSummary>>,
    exclude: &HashSet<String>,
    limit: usize,
) -> Vec<VideoSummary> {
    let mut cursors: Vec<std::vec::IntoIter<VideoSummary>> =
        lists.into_iter().map(Vec::into_iter).collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut per_channel: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut merged = Vec::with_capacity(limit);

    while merged.len() < limit {
        let mut progressed = false;
        for cursor in &mut cursors {
            let Some(video) = cursor.next() else {
                continue;
            };
            progressed = true;
            let id = video.id.as_str().to_owned();
            if exclude.contains(&id) || !seen.insert(id) {
                continue;
            }
            // One channel cannot take over the feed. Related lists are dominated by the seed's own
            // channel, so without this a feed built from four videos by one creator is that
            // creator, over and over.
            if let Some(channel) = video.channel_id.as_ref().map(|id| id.as_str().to_owned()) {
                let count = per_channel.entry(channel).or_insert(0);
                if *count >= MAX_PER_CHANNEL {
                    continue;
                }
                *count += 1;
            }
            merged.push(video);
            if merged.len() >= limit {
                break;
            }
        }
        if !progressed {
            break;
        }
    }

    merged
}

/// Picks `count` entries from `pool`, starting at an offset that advances every few minutes.
///
/// A daily offset meant the Shorts tab was identical all day however often it was reopened, which
/// is the "same shorts every reload" complaint. Minutes are short enough that a reload brings
/// something new and long enough that a single session is stable.
fn rotating_often(pool: &[&str], count: usize) -> Vec<String> {
    if pool.is_empty() {
        return Vec::new();
    }
    let ticks = Timestamp::now().as_millis().div_euclid(5 * 60_000).max(0);
    let start = usize::try_from(ticks).unwrap_or(0) % pool.len();
    (0..count.min(pool.len()))
        .map(|offset| pool[(start + offset) % pool.len()].to_owned())
        .collect()
}

/// Picks `count` entries from `pool`, starting at an offset that advances once a day.
///
/// Deterministic within a day so a relaunch does not reshuffle the screen under the user, and
/// different across days so the tab is not frozen. Derived from the wall clock rather than from
/// anything about the user.
fn rotating(pool: &[&str], count: usize) -> Vec<String> {
    if pool.is_empty() {
        return Vec::new();
    }
    let days = Timestamp::now().as_millis().div_euclid(86_400_000).max(0);
    let start = usize::try_from(days).unwrap_or(0) % pool.len();
    (0..count.min(pool.len()))
        .map(|offset| pool[(start + offset) % pool.len()].to_owned())
        .collect()
}

#[cfg(test)]
mod feed_tests {
    use super::*;
    use beastube_core::model::thumbnail::ThumbnailSet;
    use beastube_core::model::video::LiveStatus;

    fn video(id: &str) -> VideoSummary {
        VideoSummary {
            id: VideoId::new(id).expect("valid id"),
            title: id.to_owned(),
            channel_id: None,
            channel_name: None,
            thumbnails: ThumbnailSet::empty(),
            duration_ms: None,
            published_at: None,
            published_text: None,
            view_count: None,
            live_status: LiveStatus::NotLive,
            is_short: false,
        }
    }

    fn ids(videos: &[VideoSummary]) -> Vec<&str> {
        videos.iter().map(|video| video.id.as_str()).collect()
    }

    #[test]
    fn merging_takes_one_from_each_list_in_turn() {
        // Concatenating would put every result of the first seed before any result of the second,
        // so the first screen would reflect one seed instead of all of them.
        let merged = interleave(
            vec![
                vec![video("aaaaaaaaaaa"), video("bbbbbbbbbbb")],
                vec![video("ccccccccccc"), video("ddddddddddd")],
            ],
            &HashSet::new(),
            4,
        );
        assert_eq!(
            ids(&merged),
            [
                "aaaaaaaaaaa",
                "ccccccccccc",
                "bbbbbbbbbbb",
                "ddddddddddd"
            ]
        );
    }

    #[test]
    fn a_video_appears_once_however_many_seeds_suggested_it() {
        let merged = interleave(
            vec![
                vec![video("aaaaaaaaaaa")],
                vec![video("aaaaaaaaaaa"), video("bbbbbbbbbbb")],
            ],
            &HashSet::new(),
            10,
        );
        assert_eq!(ids(&merged), ["aaaaaaaaaaa", "bbbbbbbbbbb"]);
    }

    #[test]
    fn already_watched_videos_are_not_recommended_back() {
        let exclude: HashSet<String> = ["aaaaaaaaaaa".to_owned()].into_iter().collect();
        let merged = interleave(
            vec![vec![video("aaaaaaaaaaa"), video("bbbbbbbbbbb")]],
            &exclude,
            10,
        );
        assert_eq!(ids(&merged), ["bbbbbbbbbbb"]);
    }

    #[test]
    fn a_short_list_does_not_stall_the_merge() {
        // The loop must terminate on exhausted cursors rather than spinning until the limit.
        let merged = interleave(
            vec![vec![video("aaaaaaaaaaa")], vec![], vec![video("bbbbbbbbbbb")]],
            &HashSet::new(),
            50,
        );
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn one_channel_cannot_take_over_the_feed() {
        // The reported symptom: seeds from one creator return overlapping related lists, and the
        // feed becomes that creator repeated.
        let channel = beastube_core::ids::ChannelId::new("UCaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let hogging: Vec<VideoSummary> = ["aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc", "ddddddddddd", "eeeeeeeeeee"]
            .iter()
            .map(|id| {
                let mut item = video(id);
                item.channel_id = Some(channel.clone());
                item
            })
            .collect();

        let merged = interleave(vec![hogging], &HashSet::new(), 10);
        assert_eq!(merged.len(), MAX_PER_CHANNEL);
    }

    #[test]
    fn a_video_without_a_channel_is_not_capped() {
        // The cap keys on the channel; an unknown channel must not collapse into one bucket.
        let anonymous = vec![video("aaaaaaaaaaa"), video("bbbbbbbbbbb"), video("ccccccccccc"), video("ddddddddddd")];
        assert_eq!(interleave(vec![anonymous], &HashSet::new(), 10).len(), 4);
    }

    #[test]
    fn an_empty_input_produces_an_empty_feed() {
        assert!(interleave(Vec::new(), &HashSet::new(), 10).is_empty());
        assert!(interleave(vec![Vec::new()], &HashSet::new(), 10).is_empty());
    }

    #[test]
    fn topic_rotation_stays_in_bounds_and_asks_for_no_more_than_exists() {
        let pool = ["a", "b", "c"];
        let picked = rotating(&pool, 2);
        assert_eq!(picked.len(), 2);
        assert!(picked.iter().all(|topic| pool.contains(&topic.as_str())));

        assert_eq!(rotating(&pool, 99).len(), 3, "never more than the pool holds");
        assert!(rotating(&[], 4).is_empty());
    }

    #[test]
    fn rotation_is_stable_within_a_run() {
        // A feed that reshuffles between two renders on the same day looks broken.
        assert_eq!(rotating(DISCOVERY_TOPICS, 4), rotating(DISCOVERY_TOPICS, 4));
    }
}

/// Whether a video is bookmarked.
///
/// Exists so the save control can render its real state rather than assuming "not saved" and
/// telling the user something untrue about their own library (§131).
///
/// # Errors
///
/// Returns a payload if the identifier is invalid or the read fails.
#[tauri::command]
pub(crate) async fn is_bookmarked(
    state: State<'_, AppState>,
    video_id: String,
) -> CommandResult<bool> {
    let id = self::video_id(&video_id)?;
    state
        .repositories
        .bookmarks
        .get(&id)
        .await
        .map(|bookmark| bookmark.is_some())
        .map_err(fail)
}

/// More shorts, continuing from what the viewer has already seen.
///
/// This is what makes the tab endless, and it is endless in the way YouTube's is: a short's related
/// list is mostly other shorts, so the videos just watched become the seeds for the next batch and
/// the feed bends toward what is actually being watched. No profile and no account are involved —
/// the seeds are ids the caller already has on screen, and they are sent nowhere except to the
/// provider as "what is related to this video" (§43).
///
/// `exclude` is the set already shown, so the feed does not circle back on itself.
///
/// # Errors
///
/// Never returns an error. A seed whose related list fails contributes nothing and the rest still
/// extend the feed.
#[tauri::command]
pub(crate) async fn get_more_shorts(
    state: State<'_, AppState>,
    seeds: Vec<String>,
    exclude: Vec<String>,
    limit: u32,
) -> CommandResult<Vec<VideoSummary>> {
    let limit = limit.clamp(1, 60) as usize;

    // Ids arriving from the frontend are re-validated like any other untrusted input.
    let seeds: Vec<VideoId> = seeds
        .iter()
        .filter_map(|raw| VideoId::new(raw).ok())
        .take(SHORTS_EXPANSION_SEEDS)
        .collect();
    if seeds.is_empty() {
        return Ok(Vec::new());
    }

    let seen: HashSet<String> = exclude.into_iter().collect();
    let lists: Vec<Vec<VideoSummary>> = related_lists(&state, seeds)
        .await
        .into_iter()
        .map(|list| list.into_iter().filter(is_short_form).collect())
        .collect();

    // Related *and* fresh, always — not related-with-a-fallback. Relying on related alone made the
    // feed run dry after a handful of shorts, because a seed whose related list holds no short-form
    // video contributes nothing and the next batch has nothing new to seed from. Mixing a topic
    // search into every batch means the feed cannot converge on a dead end, and it keeps introducing
    // material the viewer has not already been shown.
    let topics = rotating_often(SHORTS_TOPICS, SHORTS_TOPICS.len());
    let fallback = fan_out(topics, |topic| {
        let provider = Arc::clone(&state.provider);
        async move {
            let cancel = CancellationToken::new();
            provider
                .search_shorts(&topic, &cancel)
                .await
                .unwrap_or_default()
        }
    })
    .await;

    // Interleaved together so each batch is part "more like this" and part "something else".
    let mut combined = lists;
    combined.extend(fallback);
    Ok(interleave(combined, &seen, limit))
}

/// Opens a link in the user's own browser.
///
/// Validated before it is handed to the system: [`validate_external_url`] admits only `https` and
/// rejects anything that could name a local resource, so a malformed or hostile link cannot become
/// an arbitrary shell open.
///
/// # Errors
///
/// Returns a payload if the URL is not an acceptable external link, or if the system refuses it.
#[tauri::command]
pub(crate) fn open_external(app: tauri::AppHandle, url: String) -> CommandResult<()> {
    use tauri_plugin_opener::OpenerExt;

    let validated = beastube_core::security::validate_external_url(&url).map_err(|error| {
        ErrorPayload {
            kind: beastube_core::error::ErrorKind::Network,
            code: "network.blocked_url".to_owned(),
            message_key: "error.network.blocked_url".to_owned(),
            params: std::collections::BTreeMap::new(),
            recovery: beastube_core::error::Recovery::Unrecoverable,
            diagnostic: Some(error.to_string()),
            correlation_id: None,
        }
    })?;

    app.opener()
        .open_url(validated.as_str(), None::<&str>)
        .map_err(|error| ErrorPayload {
            kind: beastube_core::error::ErrorKind::FileSystem,
            code: "filesystem.not_found".to_owned(),
            message_key: "error.filesystem.not_found".to_owned(),
            params: std::collections::BTreeMap::new(),
            recovery: beastube_core::error::Recovery::RetryManual,
            diagnostic: Some(error.to_string()),
            correlation_id: None,
        })
}
