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
use tauri::State;
use tokio_util::sync::CancellationToken;

use crate::state::AppState;

/// Result of a command: the value, or a payload the UI can render and classify.
type CommandResult<T> = Result<T, ErrorPayload>;

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

    Ok(results)
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
    let cancel = CancellationToken::new();
    Ok(state
        .provider
        .suggestions(&prefix, &cancel)
        .await
        .unwrap_or_default())
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
