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
use beastube_core::model::playlist::{LocalPlaylist, LocalPlaylistId, PlaylistItem};
use beastube_core::model::search::{SearchItem, SearchResultKind};
use beastube_core::model::video::{Cue, VideoDetails, VideoSummary};
use beastube_core::model::{
    Bookmark, ContinuationToken, HistoryEntry, Page, PlaybackPosition, SearchFilters,
    SearchResults, Suggestion,
};
use beastube_core::time_util::Timestamp;
use beastube_db::repo::history::WatchRecord;
use beastube_db::repo::searches::SearchEntry;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tauri::State;
use tokio_util::sync::CancellationToken;

use crate::state::AppState;

/// Result of a command: the value, or a payload the UI can render and classify.
pub(crate) type CommandResult<T> = Result<T, ErrorPayload>;

/// How many of the user's own past queries a suggestion list may lead with.
///
/// Small on purpose: the dropdown's value is the one or two entries the user recognizes, and a
/// screenful of their own history in front of the provider's completions makes the field feel like
/// it is refusing to look anything up.
const LOCAL_SUGGESTION_LIMIT: u32 = 4;

/// Longest suggestion list returned, matching what the dropdown renders without scrolling.
const MAX_SUGGESTIONS: usize = 10;

/// Converts any subsystem error into the wire payload.
pub(crate) fn fail<E: DomainError + Sized>(error: E) -> ErrorPayload {
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
    // Mirrored beside the database, because the next launch has to know this before the database
    // can be opened: the webview's command line is fixed when the window is created, which is
    // before any of this exists. See `gpu`.
    crate::gpu::remember_preference(sanitized.playback.hardware_acceleration);
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

/// The longest free-text string any command will accept.
///
/// Generous for a search box and small enough that no caller can make the native side allocate on
/// demand. The interface would never send more, which is exactly the reason to check here: the
/// renderer is not the only thing that can call a command, and a bound the frontend enforces is a
/// bound nothing enforces.
const MAX_QUERY_LEN: usize = 512;

/// The most rows any command will return from the library in one call.
const MAX_PAGE_SIZE: u32 = 500;

/// Rejects free text that is longer than this application ever has reason to handle.
///
/// # Errors
///
/// Returns a payload naming the field when the text is over-long.
fn bounded_text(field: &str, text: &str) -> Result<(), ErrorPayload> {
    if text.len() > MAX_QUERY_LEN {
        return Err(ErrorPayload {
            kind: beastube_core::error::ErrorKind::Configuration,
            code: "validation.invalid_input".to_owned(),
            message_key: "error.provider.invalid_input".to_owned(),
            params: std::collections::BTreeMap::new(),
            recovery: beastube_core::error::Recovery::Unrecoverable,
            diagnostic: Some(format!(
                "{field} was {} bytes; the limit is {MAX_QUERY_LEN}",
                text.len()
            )),
            correlation_id: None,
        });
    }
    Ok(())
}

/// Searches the active provider.
///
/// # Errors
///
/// Returns a payload if the query is empty or over-long, the provider fails, or the response cannot
/// be read.
#[tauri::command]
pub(crate) async fn search(
    state: State<'_, AppState>,
    query: String,
    filters: SearchFilters,
    continuation: Option<ContinuationToken>,
) -> CommandResult<SearchResults> {
    bounded_text("query", &query)?;
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

/// The lines of one subtitle track.
///
/// The application draws captions itself, so it needs the cues rather than leaving them to the
/// embedded player, which renders them inside a frame nothing outside can move or restyle.
///
/// # Errors
///
/// Returns a payload if the address is not one the provider serves caption files from, or the
/// track cannot be fetched or read.
#[tauri::command]
pub(crate) async fn get_caption_cues(
    state: State<'_, AppState>,
    url: String,
) -> CommandResult<Vec<Cue>> {
    bounded_text("url", &url)?;
    let cancel = CancellationToken::new();
    state
        .provider
        .caption_cues(&url, &cancel)
        .await
        .map_err(fail)
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
        .map_err(fail)?;
    enforce_history_retention(&state).await;
    Ok(())
}

/// Applies the configured retention window to the watch history.
///
/// After the write, in the same shape as the search-history ceiling above: a table can only exceed
/// its policy immediately after the write that pushed it over, so enforcing here keeps the promise
/// honest without a timer to run or a task to supervise.
///
/// This is the code the setting was missing. `history_retention_days` was declared in the settings
/// struct, typed in the frontend, offered on the privacy screen as "Delete history older than", and
/// implemented in the database as [`HistoryRepo::prune`] — and nothing anywhere called it. The
/// screen made a promise about deletion that no code kept.
pub(crate) async fn enforce_history_retention(state: &AppState) {
    let days = state.settings().privacy.history_retention_days;
    let Some(cutoff) = retention_cutoff(days, Timestamp::now()) else {
        return;
    };
    match state.repositories.history.prune(cutoff).await {
        Ok(0) => {}
        Ok(removed) => tracing::info!(removed, "pruned history past its retention window"),
        Err(error) => tracing::warn!(%error, "history could not be pruned to its retention window"),
    }
}

/// The instant before which watch history has outlived its retention window.
///
/// `None` means prune nothing, and it covers three separate cases that all have that same answer:
/// no window configured, a window of zero days, and a window so long the arithmetic leaves the
/// clock. Zero is refused rather than honoured because "delete everything the moment it is written"
/// is not a retention window, is not what any offered choice means, and would quietly empty the
/// library of anyone who reached it.
///
/// Days are converted here rather than in the repository, which takes a cutoff and is deliberately
/// given no opinion about the calendar.
fn retention_cutoff(days: Option<u32>, now: Timestamp) -> Option<Timestamp> {
    const MILLIS_PER_DAY: i64 = 24 * 60 * 60 * 1000;

    let days = days?;
    if days == 0 {
        return None;
    }
    i64::from(days)
        .checked_mul(MILLIS_PER_DAY)
        .and_then(|window| now.as_millis().checked_sub(window))
        // A cutoff before the epoch means the window reaches back past anything that can have been
        // recorded. Arithmetically it prunes nothing, but returning it would have this function
        // answer "prune before 1969" where it means "there is nothing to prune".
        .filter(|millis| *millis >= 0)
        .map(Timestamp::from_millis)
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
    bounded_text("query", &query)?;
    state
        .repositories
        .history
        // Bounded here rather than trusted: `limit` reaches SQL as a row count, and the interface
        // asking for twenty is not a reason the native side should answer a request for millions.
        .search(&query, limit.min(MAX_PAGE_SIZE))
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

// ---------------------------------------------------------------------------------------------
// Playlists
// ---------------------------------------------------------------------------------------------
//
// Local only, and deliberately so. These are lists the user builds on this machine; nothing here
// talks to the provider, nothing syncs, and no account is involved (§42). The schema, the model and
// the ordering algorithm all shipped in the first migration — these commands are the layer that
// finally makes the Playlists screen more than a promise.

/// How many playlist items one request may return.
const MAX_PLAYLIST_PAGE: u32 = 500;

/// Every playlist, built-in ones first.
///
/// # Errors
///
/// Returns a payload if the read fails.
#[tauri::command]
pub(crate) async fn get_playlists(state: State<'_, AppState>) -> CommandResult<Vec<LocalPlaylist>> {
    state.repositories.playlists.list().await.map_err(fail)
}

/// Creates a playlist and returns it as it now stands.
///
/// Returns the whole record rather than just an identifier, so the caller can render the new list
/// without a second round trip.
///
/// # Errors
///
/// Returns a payload if the name is blank or too long, or if the write fails.
#[tauri::command]
pub(crate) async fn create_playlist(
    state: State<'_, AppState>,
    name: String,
    description: Option<String>,
) -> CommandResult<LocalPlaylist> {
    let repo = &state.repositories.playlists;
    let id = repo
        .create(&name, description.as_deref(), Timestamp::now())
        .await
        .map_err(fail)?;

    // Read back rather than assembling a record here: the derived count and cover belong to the
    // storage layer, and inventing them at this level is how the two drift apart.
    //
    // A miss is not a "not found" the user can act on — the row was inserted a statement ago, so
    // its absence means the database contradicted itself. It is reported as such rather than as an
    // empty result the caller would have to invent a meaning for.
    repo.get(id).await.map_err(fail)?.ok_or_else(|| ErrorPayload {
        kind: beastube_core::error::ErrorKind::Database,
        code: "database.write_lost".to_owned(),
        message_key: "error.generic".to_owned(),
        params: std::collections::BTreeMap::new(),
        recovery: beastube_core::error::Recovery::RetryManual,
        diagnostic: Some(format!("playlist {id} vanished between insert and read")),
        correlation_id: None,
    })
}

/// Renames a user playlist. Answers `false` for a built-in one, which cannot be renamed.
///
/// # Errors
///
/// Returns a payload if the name is blank or too long, or if the write fails.
#[tauri::command]
pub(crate) async fn rename_playlist(
    state: State<'_, AppState>,
    playlist_id: i64,
    name: String,
) -> CommandResult<bool> {
    state
        .repositories
        .playlists
        .rename(LocalPlaylistId::new(playlist_id), &name, Timestamp::now())
        .await
        .map_err(fail)
}

/// Deletes a user playlist and its items. Answers `false` for a built-in one.
///
/// # Errors
///
/// Returns a payload if the write fails.
#[tauri::command]
pub(crate) async fn delete_playlist(
    state: State<'_, AppState>,
    playlist_id: i64,
) -> CommandResult<bool> {
    state
        .repositories
        .playlists
        .delete(LocalPlaylistId::new(playlist_id))
        .await
        .map_err(fail)
}

/// The videos in a playlist, in playlist order.
///
/// # Errors
///
/// Returns a payload if the read fails.
#[tauri::command]
pub(crate) async fn get_playlist_items(
    state: State<'_, AppState>,
    playlist_id: i64,
    limit: u32,
    offset: u32,
) -> CommandResult<Vec<PlaylistItem>> {
    state
        .repositories
        .playlists
        .items(
            LocalPlaylistId::new(playlist_id),
            limit.clamp(1, MAX_PLAYLIST_PAGE),
            offset,
        )
        .await
        .map_err(fail)
}

/// Adds a video to a playlist. Answers `false` if it was already there.
///
/// Like bookmarking, this is an explicit user action and is recorded even in incognito: the user
/// asked for it, and silently discarding it would be the surprising behaviour.
///
/// # Errors
///
/// Returns a payload if the write fails.
#[tauri::command]
pub(crate) async fn add_to_playlist(
    state: State<'_, AppState>,
    playlist_id: i64,
    video: VideoSummary,
) -> CommandResult<bool> {
    state
        .repositories
        .playlists
        .add_item(LocalPlaylistId::new(playlist_id), &video, Timestamp::now())
        .await
        .map_err(fail)
}

/// Removes a video from a playlist. Answers `false` if it was not in it.
///
/// # Errors
///
/// Returns a payload if the identifier is invalid or the write fails.
#[tauri::command]
pub(crate) async fn remove_from_playlist(
    state: State<'_, AppState>,
    playlist_id: i64,
    video_id: String,
) -> CommandResult<bool> {
    let id = self::video_id(&video_id)?;
    state
        .repositories
        .playlists
        .remove_item(LocalPlaylistId::new(playlist_id), &id, Timestamp::now())
        .await
        .map_err(fail)
}

/// Which playlists already hold a video.
///
/// One query for the whole "add to playlist" menu, rather than one per playlist.
///
/// # Errors
///
/// Returns a payload if the identifier is invalid or the read fails.
#[tauri::command]
pub(crate) async fn playlists_containing(
    state: State<'_, AppState>,
    video_id: String,
) -> CommandResult<Vec<LocalPlaylistId>> {
    let id = self::video_id(&video_id)?;
    state
        .repositories
        .playlists
        .containing(&id)
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

/// One place on disk that belongs to this application.
///
/// The privacy screen used to report two numbers — the library and the provider's extractor cache —
/// and call that "stored data". Measured on a working installation, those two came to 4.4 MB while
/// the application actually occupied about 629 MB. The rest was the embedded browser's own profile,
/// which nothing counted and nothing could remove. A storage figure that is out by two orders of
/// magnitude is not a smaller version of the truth; it is a different claim (§100).
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct StorageLocation {
    /// Stable key the interface maps to a translated name.
    id: &'static str,
    /// Absolute path, so the claim can be checked rather than believed.
    path: String,
    bytes: u64,
    /// Whether this application will delete it on request.
    ///
    /// False for the library, which is the user's own data and is cleared by the specific controls
    /// beside it, and false for the download folder: that is a directory the user chose and may
    /// share with other things, and a button that empties it would be a foot-gun wearing the word
    /// "clean".
    clearable: bool,
    /// How much of `bytes` a clear would actually remove.
    ///
    /// Not the same number as `bytes`, and the difference is the point. The browser profile is
    /// mostly the viewer's own data and security material this application will not touch, so a row
    /// that offered `bytes` beside a button removing a fraction of it was making a promise the
    /// button did not keep. This is measured from the very list the button deletes.
    clearable_bytes: u64,
}

/// What the application is storing on this device.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct StorageStats {
    /// Every location, largest first. Additive: the fields below are unchanged for the diagnostics
    /// screen, which reports the library and the extractor cache specifically.
    locations: Vec<StorageLocation>,
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

    let cache_bytes = directory_size(&state.provider_cache_dir);
    // Named once: the breakdown below and the flat field beneath it are the same path.
    let database_path = state
        .database
        .path()
        .map_or_else(|| "(in memory)".to_owned(), |path| path.display().to_string());
    // Measured from the same list the button deletes, so the two cannot disagree.
    let removable = |id: &str| -> u64 {
        clearable_roots(&state, id).map_or(0, |roots| {
            roots.iter().map(|root| directory_size(root)).sum()
        })
    };

    let mut locations = vec![
        StorageLocation {
            id: "library",
            path: database_path.clone(),
            bytes: database_bytes,
            clearable: false,
            clearable_bytes: 0,
        },
        StorageLocation {
            id: "webview",
            path: state.webview_data_dir.display().to_string(),
            bytes: directory_size(&state.webview_data_dir),
            clearable: true,
            clearable_bytes: removable("webview"),
        },
        StorageLocation {
            id: "provider_cache",
            path: state.provider_cache_dir.display().to_string(),
            bytes: cache_bytes,
            clearable: true,
            clearable_bytes: cache_bytes,
        },
        StorageLocation {
            id: "downloader_cache",
            path: state.downloader_cache_dir().display().to_string(),
            bytes: directory_size(&state.downloader_cache_dir()),
            clearable: true,
            clearable_bytes: removable("downloader_cache"),
        },
        StorageLocation {
            id: "logs",
            path: state.log_dir.display().to_string(),
            bytes: directory_size(&state.log_dir),
            clearable: true,
            clearable_bytes: removable("logs"),
        },
        StorageLocation {
            id: "downloads",
            path: state.download_directory().display().to_string(),
            bytes: directory_size(&state.download_directory()),
            clearable: false,
            clearable_bytes: 0,
        },
    ];
    // Largest first: the point of the list is that the big one is not the one people expect.
    locations.sort_by_key(|location| std::cmp::Reverse(location.bytes));

    Ok(StorageStats {
        locations,
        database_bytes,
        cache_bytes,
        history_entries,
        bookmark_entries,
        position_entries,
        database_path,
        cache_path: state.provider_cache_dir.display().to_string(),
    })
}

/// Sums the size of every file under `root`, ignoring what it cannot read.
///
/// A directory the process cannot traverse contributes zero rather than failing the whole
/// measurement: an approximate size is more useful on a storage screen than an error.
///
/// Bounded by a visit budget as well as by depth. Every directory measured here is one this
/// application owns except the download folder, which is whatever the user pointed at — and on the
/// machine this was written on that default resolved to a source checkout, so drawing one row meant
/// walking a `node_modules` and a Rust `target/`. The budget is far above any plausible folder of
/// videos, so the figure stays exact in the ordinary case and becomes a floor rather than a stall
/// in the pathological one.
fn directory_size(root: &std::path::Path) -> u64 {
    /// Entries visited before the walk gives up. Chosen to be unreachable for a media folder and
    /// reachable within a second or so for a source tree.
    const VISIT_BUDGET: u32 = 60_000;

    fn walk(path: &std::path::Path, total: &mut u64, depth: usize, budget: &mut u32) {
        // Bounded so a symlink loop or a pathological tree cannot spin here.
        if depth > 8 || *budget == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            if *budget == 0 {
                return;
            }
            *budget -= 1;
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                walk(&entry.path(), total, depth + 1, budget);
            } else {
                *total = total.saturating_add(metadata.len());
            }
        }
    }

    let mut total = 0;
    let mut budget = VISIT_BUDGET;
    walk(root, &mut total, 0, &mut budget);
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

/// The directories a "clear" removes for a given target, or `None` if the target is not one this
/// application will delete.
///
/// Shared with `get_storage_stats` deliberately. The screen used to measure one set of paths and
/// delete a different, much smaller one: the browser row reported the whole 29 MB profile while the
/// button removed five subdirectories worth about 2 MB. So the figure barely moved, the button
/// never disabled, and clearing looked broken — because in every way the viewer could observe, it
/// was. Measuring and deleting now read the same list and cannot drift apart.
fn clearable_roots(state: &AppState, target: &str) -> Option<Vec<std::path::PathBuf>> {
    match target {
        "provider_cache" => Some(vec![state.provider_cache_dir.clone()]),
        "downloader_cache" => Some(vec![state.downloader_cache_dir()]),
        "logs" => Some(vec![state.log_dir.clone()]),
        "webview" => Some(webview_cache_roots(&state.webview_data_dir)),
        // Every cache above in one press, which is the only thing most people want from this
        // screen. It is still only caches: the library, the settings and the download folder are
        // not in any of these lists.
        "all" => Some(
            ["webview", "provider_cache", "downloader_cache", "logs"]
                .into_iter()
                .filter_map(|id| clearable_roots(state, id))
                .flatten()
                .collect(),
        ),
        _ => None,
    }
}

/// The disposable parts of the embedded browser's profile.
///
/// Chromium keeps two very different kinds of thing under one directory. Some of it is the viewer's
/// — cookies, local storage, the embed's own preferences — and none of that is listed here: it is
/// not where the size is, and removing it costs them something for nothing. The rest is either a
/// cache the browser rebuilds locally or a component payload it re-downloads on its own schedule,
/// and that is what this returns.
///
/// Deliberately absent: `PKIMetadata`, `Trust Protection Lists`, `TrustTokenKeyCommitments` and
/// `CertificateRevocation`. They are component payloads and would come back, but they are the data
/// certificate revocation is checked against, and trading a working revocation check for one
/// megabyte is not a trade a "free up space" button should make on someone's behalf.
fn webview_cache_roots(data_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let profile = data_dir.join("Default");
    vec![
        // Rebuilt locally, for free, as the viewer watches.
        profile.join("Cache"),
        profile.join("Code Cache"),
        profile.join("Service Worker"),
        profile.join("GPUCache"),
        data_dir.join("GrShaderCache"),
        data_dir.join("ShaderCache"),
        data_dir.join("GPUPersistentCache"),
        // Component payloads. Re-downloaded when the browser next updates them, which costs
        // bandwidth once and nothing else. These are where the profile's size actually is: the
        // subresource filter ruleset alone was 12 MB of the 29 MB measured here.
        data_dir.join("Subresource Filter"),
        data_dir.join("component_crx_cache"),
        data_dir.join("extensions_crx_cache"),
        data_dir.join("Speech Recognition"),
        data_dir.join("hyphen-data"),
        data_dir.join("MEIPreload"),
        data_dir.join("OriginTrials"),
        // Crash reports for a process that is no longer running.
        data_dir.join("Crashpad"),
    ]
}

/// Empties the caches if they have grown past the configured ceiling.
///
/// Called once at startup. Measuring and clearing both go through [`clearable_roots`], so this
/// removes exactly what the "Clear every cache" button removes and counts exactly what the storage
/// screen counts — the ceiling cannot come to mean something different from the button beside it.
///
/// Synchronous and blocking on purpose: it runs before the window is revealed, so there is nothing
/// yet for it to block, and doing it here is what makes the browser profile reachable at all. Once
/// the webview starts it holds that profile open.
pub(crate) fn enforce_cache_limit(state: &AppState) {
    let Some(limit_mb) = state.settings().privacy.cache_limit_mb else {
        return;
    };
    let limit_bytes = u64::from(limit_mb).saturating_mul(1024 * 1024);
    let Some(roots) = clearable_roots(state, "all") else {
        return;
    };
    let total: u64 = roots.iter().map(|root| directory_size(root)).sum();
    if total <= limit_bytes {
        return;
    }
    tracing::info!(
        total_mb = total / (1024 * 1024),
        limit_mb,
        "the caches are over their ceiling; clearing"
    );
    for root in roots {
        remove_contents(&root);
    }
}

/// Removes everything inside `root`, leaving the directory itself.
///
/// Entry by entry rather than `remove_dir_all` on the root: a single file the process still holds
/// open makes a whole-tree delete fail and leaves everything else in place, which is the difference
/// between clearing all but one file and appearing to do nothing.
fn remove_contents(root: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let removed = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(error) = removed {
            tracing::debug!(%error, path = %path.display(), "left a file that could not be removed");
        }
    }
}

/// Removes one of the locations `get_storage_stats` reports as clearable.
///
/// ## Why the browser profile is emptied rather than deleted
///
/// The embedded browser holds its profile open for as long as the window exists, so removing the
/// directory would fail on Windows and, if it half-succeeded, would leave the webview with a
/// profile missing files it believes are there. Only the caches inside it are removed — the HTTP
/// cache, the compiled-JavaScript cache and the shader cache, which is where essentially all of the
/// size is — and each is a directory the browser recreates on demand and treats as disposable by
/// design. Cookies, local storage and the embed's own settings are deliberately left alone: they
/// are not size, and clearing them signs the viewer out of nothing but costs them their preferences.
///
/// ## Best effort, honestly reported
///
/// A file the browser has open cannot be deleted while it is open, so some of it may survive. This
/// deletes what it can and then re-measures, which is why it returns the fresh statistics: the
/// number the viewer sees afterwards is what is actually left, not what was expected to go.
///
/// # Errors
///
/// Returns a payload only for an unknown target. A deletion that fails is reported by the size that
/// comes back, not by an error dialog over a screen the viewer is already reading.
#[tauri::command]
pub(crate) async fn clear_storage(
    state: State<'_, AppState>,
    target: String,
) -> CommandResult<StorageStats> {
    let Some(roots) = clearable_roots(&state, target.as_str()) else {
        return Err(ErrorPayload {
            kind: beastube_core::error::ErrorKind::Configuration,
            code: "validation.invalid_input".to_owned(),
            message_key: "error.provider.invalid_input".to_owned(),
            params: std::collections::BTreeMap::new(),
            recovery: beastube_core::error::Recovery::Unrecoverable,
            diagnostic: Some(format!("unknown storage target: {target}")),
            correlation_id: None,
        });
    };

    for root in roots {
        remove_contents(&root);
    }

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
    variant: u32,
) -> CommandResult<RecommendedFeed> {
    let limit = limit.clamp(1, 120) as usize;
    // Asking for more than fits, because `interleave` drops duplicates and caps how many come from
    // any one channel — gathering exactly `limit` would routinely fall short of it.
    let enough = limit.saturating_mul(2);
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
        let seeds = spread_seeds(&history, RECOMMENDATION_SEEDS, variant);

        if !seeds.is_empty() {
            let videos = interleave(related_lists(&state, seeds, enough).await, &watched, limit);
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
            let videos = interleave(search_lists(&state, queries, enough).await, &watched, limit);
            if !videos.is_empty() {
                return Ok(RecommendedFeed {
                    videos,
                    source: RecommendationSource::Searched,
                });
            }
        }
    }

    let topics = rotating(DISCOVERY_TOPICS, RECOMMENDATION_SEEDS, variant);
    Ok(RecommendedFeed {
        videos: interleave(search_lists(&state, topics, enough).await, &watched, limit),
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
    // Asking for more than fits, because `interleave` drops duplicates and caps how many come from
    // any one channel — gathering exactly `limit` would routinely fall short of it.
    let enough = limit.saturating_mul(2);
    let personalize =
        state.settings().privacy.local_recommendations_enabled && !state.is_incognito();
    let records_searches = state.records_searches();

    // Both local reads at once. They are cheap, but they were sequential, and everything downstream
    // waited on the pair of them before a single network request left the machine.
    //
    // The viewer's own searches are the clearest statement of interest the application has, and are
    // why the tab can be full even when the topic searches come back thin. The watch history is the
    // richest source of short-form video available here — related lists are full of it, and unlike a
    // topic search they cannot come back empty for reasons unrelated to the query. Both sit under
    // the same permission as the rest of the local ranking, and both are skipped in incognito.
    let (recent, history) = tokio::join!(
        async {
            if records_searches {
                state
                    .repositories
                    .searches
                    .recent(SEARCH_SEED_COUNT)
                    .await
                    .unwrap_or_default()
            } else {
                Vec::new()
            }
        },
        async {
            if personalize {
                state
                    .repositories
                    .history
                    .list(WATCHED_LOOKBACK, 0)
                    .await
                    .unwrap_or_default()
            } else {
                Vec::new()
            }
        }
    );

    let mut queries: Vec<String> = recent
        .into_iter()
        .map(|entry| format!("{} #shorts", entry.query))
        .collect();
    queries.extend(rotating_often(SHORTS_TOPICS, SHORTS_TOPICS.len()));
    // The Shorts tab has its own refresh path and does not take a variant, so it keeps the plain
    // clock-based spread it always had.
    let seeds = spread_seeds(&history, RECOMMENDATION_SEEDS, 0);

    // And both network waves at once. These used to run one after the other, so the tab cost the
    // *sum* of two concurrent waves rather than the longer of them — the single largest reason
    // opening Shorts took as long as it did. Nothing in the second wave depends on the first.
    let (related, searched) = tokio::join!(
        async {
            if seeds.is_empty() {
                Vec::new()
            } else {
                related_lists(&state, seeds, enough)
                    .await
                    .into_iter()
                    .map(|list| list.into_iter().filter(is_short_form).collect::<Vec<_>>())
                    .collect::<Vec<_>>()
            }
        },
        fan_out(queries, enough, |topic| {
            let provider = Arc::clone(&state.provider);
            async move {
                let cancel = CancellationToken::new();
                // The dedicated shorts surface, which reads the shelf ordinary search parsing
                // drops. One request returns more short-form video than six pages of the typed
                // search did.
                provider
                    .search_shorts(&topic, &cancel)
                    .await
                    .unwrap_or_default()
            }
        })
    );

    let mut lists = related;
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
async fn related_lists(
    state: &AppState,
    seeds: Vec<VideoId>,
    enough: usize,
) -> Vec<Vec<VideoSummary>> {
    fan_out(seeds, enough, |seed| {
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
async fn search_lists(
    state: &AppState,
    queries: Vec<String>,
    enough: usize,
) -> Vec<Vec<VideoSummary>> {
    fan_out(queries, enough, |query| {
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

/// How many requests in a wave may be in flight at once.
///
/// Chosen to be comfortably above what the early stop usually needs, so the cap costs no latency in
/// the ordinary case, while keeping a launch — where three feeds fan out at once — from opening
/// dozens of simultaneous connections to a single host.
const FAN_OUT_CONCURRENCY: usize = 6;

/// How long a concurrent wave is given before whatever has landed is used.
///
/// Not a network timeout — the provider has its own. This bounds the *wave*: a dozen searches run
/// at once and the feed cannot appear until the collection loop ends, so one request that hangs
/// holds up eleven that have already answered. Chosen well above a normal response so it only ever
/// bites when something has genuinely gone wrong.
const FAN_OUT_DEADLINE: Duration = Duration::from_secs(5);

/// Runs `operation` over every input concurrently, collecting whatever completed.
///
/// Stops early once `enough` videos have been gathered, and abandons the rest: the feed is capped
/// anyway, so waiting on requests whose results would be discarded is pure latency.
///
/// A panicking task contributes an empty list rather than poisoning the feed — a home screen is not
/// worth failing over.
async fn fan_out<I, F, Fut>(inputs: Vec<I>, enough: usize, operation: F) -> Vec<Vec<VideoSummary>>
where
    I: Send + 'static,
    F: Fn(I) -> Fut,
    Fut: std::future::Future<Output = Vec<VideoSummary>> + Send + 'static,
{
    // Bounded, not unbounded. At launch three feeds preload together, each fanning out over a dozen
    // topics, so an unbounded wave opened dozens of simultaneous connections to one host — more
    // than the remote will answer well and more than there is any use for, since the wave stops as
    // soon as it holds enough anyway.
    //
    // This is connection hygiene, not a bug fix. It was first written believing it would stop the
    // extractor's visitor-data panics; that was wrong, and the note is left here rather than
    // removed because the measurement is worth keeping. Those panics come from a *detached* task
    // rustypipe spawns to refresh visitor data, they report `302 Found` rather than a transport
    // failure, and no amount of concurrency limiting touches them.
    //
    // The cap costs almost nothing here because the wave already stops as soon as it holds enough,
    // so the tail was usually being abandoned anyway.
    let permits = Arc::new(tokio::sync::Semaphore::new(FAN_OUT_CONCURRENCY));
    let mut set = tokio::task::JoinSet::new();
    for input in inputs {
        let gate = Arc::clone(&permits);
        let work = operation(input);
        set.spawn(async move {
            // Dropped with the task, so aborting the wave releases the slot immediately.
            let Ok(_permit) = gate.acquire_owned().await else {
                // The semaphore is never closed; if that ever changes, contributing nothing is the
                // right answer rather than panicking inside a feed.
                return Vec::new();
            };
            work.await
        });
    }

    let deadline = tokio::time::Instant::now() + FAN_OUT_DEADLINE;
    let mut lists = Vec::new();
    let mut gathered = 0usize;

    loop {
        match tokio::time::timeout_at(deadline, set.join_next()).await {
            // Everything finished on its own. The ordinary case.
            Ok(None) => break,
            Ok(Some(joined)) => {
                let list = joined.unwrap_or_default();
                gathered += list.len();
                lists.push(list);
                // Enough to fill the screen. The requests still running would contribute videos
                // nobody is going to reach, so they are dropped rather than waited on — this is
                // the difference between a feed that appears when it is ready and one that appears
                // when the *slowest* of a dozen searches happens to answer.
                if gathered >= enough {
                    set.abort_all();
                    break;
                }
            }
            // Out of time. Whatever landed is what the viewer gets: a smaller feed beats a feed
            // held hostage by one request that is never coming back (§81). Without this a single
            // hung search stalled the whole surface even when every other one had already answered.
            Err(_) => {
                set.abort_all();
                break;
            }
        }
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
fn spread_seeds(history: &[HistoryEntry], count: usize, variant: u32) -> Vec<VideoId> {
    if history.is_empty() || count == 0 {
        return Vec::new();
    }
    let stride = (history.len() / count).max(1);
    // Minutes rather than milliseconds, so the feed holds still while it is being read rather than
    // reshuffling under the cursor — plus `variant`, which is how the caller says "not that one
    // again". Refresh raises it, and only Home passes it, so pressing Refresh on Home genuinely
    // draws from different watched videos while every other screen refetches what it already had.
    let clock = usize::try_from(Timestamp::now().as_millis().div_euclid(60_000).max(0)).unwrap_or(0);
    let offset = (clock + variant as usize * stride.max(1)) % history.len();

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
fn rotating(pool: &[&str], count: usize, variant: u32) -> Vec<String> {
    if pool.is_empty() {
        return Vec::new();
    }
    let days = Timestamp::now().as_millis().div_euclid(86_400_000).max(0);
    // `variant` advances the window by a whole page of topics, so a refresh moves onto ones the
    // previous draw did not use rather than shuffling the same few.
    let start = (usize::try_from(days).unwrap_or(0) + variant as usize * count.max(1)) % pool.len();
    (0..count.min(pool.len()))
        .map(|offset| pool[(start + offset) % pool.len()].to_owned())
        .collect()
}

#[cfg(test)]
mod retention_tests {
    use super::*;

    const DAY: i64 = 24 * 60 * 60 * 1000;

    #[test]
    fn a_window_becomes_a_cutoff_that_many_days_back() {
        let now = Timestamp::from_millis(30 * DAY);
        assert_eq!(
            retention_cutoff(Some(7), now),
            Some(Timestamp::from_millis(23 * DAY))
        );
    }

    #[test]
    fn no_window_prunes_nothing() {
        assert_eq!(retention_cutoff(None, Timestamp::from_millis(30 * DAY)), None);
    }

    #[test]
    fn zero_days_prunes_nothing_rather_than_everything() {
        // The dangerous reading of "keep for 0 days" is "delete on write". Refusing it is what
        // stops a library being emptied by a setting nobody meant to reach.
        assert_eq!(retention_cutoff(Some(0), Timestamp::from_millis(30 * DAY)), None);
    }

    #[test]
    fn a_window_longer_than_the_clock_prunes_nothing() {
        assert_eq!(retention_cutoff(Some(u32::MAX), Timestamp::from_millis(0)), None);
    }
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
            channel_avatar: ThumbnailSet::empty(),
            channel_verified: false,
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
        let picked = rotating(&pool, 2, 0);
        assert_eq!(picked.len(), 2);
        assert!(picked.iter().all(|topic| pool.contains(&topic.as_str())));

        assert_eq!(rotating(&pool, 99, 0).len(), 3, "never more than the pool holds");
        assert!(rotating(&[], 4, 0).is_empty());
        // A variant must not walk off the end of the pool either.
        assert_eq!(rotating(&pool, 2, 7).len(), 2);
    }

    #[test]
    fn rotation_is_stable_within_a_run() {
        // A feed that reshuffles between two renders on the same day looks broken.
        assert_eq!(
            rotating(DISCOVERY_TOPICS, 4, 0),
            rotating(DISCOVERY_TOPICS, 4, 0)
        );
    }

    #[test]
    fn a_new_variant_draws_different_topics() {
        // What the Refresh button is for: asking again must not hand back the same page. The
        // offset moves by a whole `count`, so consecutive variants share no topic.
        let first = rotating(DISCOVERY_TOPICS, 4, 0);
        let second = rotating(DISCOVERY_TOPICS, 4, 1);
        assert_ne!(first, second, "refreshing must change what is offered");
        assert!(
            first.iter().all(|topic| !second.contains(topic)),
            "consecutive draws should not overlap: {first:?} vs {second:?}"
        );
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
    let enough = limit.saturating_mul(2);
    let lists: Vec<Vec<VideoSummary>> = related_lists(&state, seeds, enough)
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
    let fallback = fan_out(topics, enough, |topic| {
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

/// Tells the webview which colour scheme the application is painted in.
///
/// This exists for one visible problem: the embedded player's own settings panel — quality, speed,
/// captions — is rendered by YouTube inside the `<iframe>`, styled by *its* document, which follows
/// `prefers-color-scheme`. That media query answers from the webview's preferred colour scheme,
/// which defaults to the operating system's. On a machine set to light Windows, a user running
/// BEASTUBE in dark mode got a white panel over a dark player.
///
/// Nothing in CSS can reach across an origin to fix it. Setting the webview's own preference can,
/// and it is the only thing that can: `WebviewWindow::set_theme` reaches WebView2's
/// `SetPreferredColorScheme`, and the embed then renders its panel dark.
///
/// Driven from the frontend rather than read from settings here, because "system" has to be
/// resolved against the OS preference and the webview is where that question is already answered.
///
/// # Errors
///
/// Returns a payload if the window is gone or the platform refuses the change. Neither is
/// recoverable in the UI, and neither breaks anything: the panel is merely the wrong colour.
#[tauri::command]
pub(crate) fn set_window_theme(app: tauri::AppHandle, dark: Option<bool>) -> CommandResult<()> {
    use tauri::{Manager as _, Theme};

    let Some(window) = app.get_webview_window("main") else {
        return Err(ErrorPayload {
            kind: beastube_core::error::ErrorKind::Configuration,
            code: "configuration.invalid".to_owned(),
            message_key: "error.configuration.invalid".to_owned(),
            params: std::collections::BTreeMap::new(),
            recovery: beastube_core::error::Recovery::Unrecoverable,
            diagnostic: Some("no main window".to_owned()),
            correlation_id: None,
        });
    };

    // `None` is not "leave it alone" — it is "follow the operating system", and it is what makes
    // the "Match system" setting mean anything. Pinning the window pins the webview's
    // `prefers-color-scheme` with it, so a window pinned dark reported dark to `matchMedia` for
    // ever after, and the one setting whose whole job is to track the OS could never see it
    // change.
    window
        .set_theme(dark.map(|dark| if dark { Theme::Dark } else { Theme::Light }))
        .map_err(|error| ErrorPayload {
            kind: beastube_core::error::ErrorKind::Configuration,
            code: "configuration.invalid".to_owned(),
            message_key: "error.configuration.invalid".to_owned(),
            params: std::collections::BTreeMap::new(),
            recovery: beastube_core::error::Recovery::Unrecoverable,
            diagnostic: Some(error.to_string()),
            correlation_id: None,
        })
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
