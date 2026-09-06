//! The provider traits.
//!
//! Split by concern so an adapter implements only what it supports, and a composite provider can
//! source each concern from a different backend without any of them knowing.
//!
//! Every method takes a [`CancellationToken`]. Provider calls are the slowest thing the application
//! does, and they are routinely made obsolete before they finish — a search superseded by the next
//! keystroke, a channel page abandoned by navigation. Threading cancellation through the trait is
//! what makes §32 ("cancel obsolete work") implementable rather than aspirational; an adapter that
//! ignores the token merely wastes its own bandwidth instead of blocking the caller.

use async_trait::async_trait;
use beastube_core::ids::{ChannelId, PlaylistId, VideoId};
use beastube_core::model::channel::ChannelTab;
use beastube_core::model::{
    ChannelDetails, ContinuationToken, Page, PlaylistDetails, SearchFilters, SearchItem,
    SearchResults, Suggestion, VideoDetails, VideoSummary,
};
use tokio_util::sync::CancellationToken;

use crate::capabilities::ProviderCapabilities;
use crate::error::ProviderResult;

/// Free-text search.
#[async_trait]
pub trait SearchProvider: Send + Sync {
    /// Searches for `query` under `filters`.
    ///
    /// `continuation` requests the page after a previous result; `None` requests the first.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ProviderError`] if the request fails, is cancelled, or the response cannot
    /// be read.
    async fn search(
        &self,
        query: &str,
        filters: &SearchFilters,
        continuation: Option<&ContinuationToken>,
        cancel: &CancellationToken,
    ) -> ProviderResult<SearchResults>;

    /// Autocompletes a partial query.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ProviderError`] if the request fails or is cancelled.
    /// Short-form video matching `query`.
    ///
    /// Separate from [`SearchProvider::search`] because a provider may need a different request to
    /// find shorts at all — YouTube returns them in a shelf whose contents ordinary search parsing
    /// does not reach.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ProviderError::Unsupported`] when the provider has no shorts surface, or a
    /// [`crate::ProviderError`] if the request fails.
    async fn search_shorts(
        &self,
        query: &str,
        cancel: &CancellationToken,
    ) -> ProviderResult<Vec<VideoSummary>> {
        let _ = (query, cancel);
        Err(crate::ProviderError::Unsupported {
            operation: "search_shorts",
            provider: "unknown",
        })
    }

    /// Autocompletions for a partial query.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ProviderError`] if the request fails or is cancelled.
    async fn suggestions(
        &self,
        prefix: &str,
        cancel: &CancellationToken,
    ) -> ProviderResult<Vec<Suggestion>>;
}

/// Video metadata.
#[async_trait]
pub trait VideoProvider: Send + Sync {
    /// Full details for one video.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ProviderError::Unavailable`] when the video exists but cannot be shown, or
    /// another variant if the request fails.
    async fn video(&self, id: &VideoId, cancel: &CancellationToken)
    -> ProviderResult<VideoDetails>;

    /// The lines of one subtitle track, fetched from the URL the provider gave for it.
    ///
    /// Exists so the application can draw captions itself rather than leaving them to the embedded
    /// player, which renders them inside a frame nothing outside can style or move.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ProviderError::Unsupported`] by default, or another variant if the request
    /// fails or the track cannot be read.
    async fn caption_cues(
        &self,
        _url: &str,
        _cancel: &CancellationToken,
    ) -> ProviderResult<Vec<beastube_core::model::Cue>> {
        Err(crate::ProviderError::Unsupported {
            operation: "caption_cues",
            provider: "unknown",
        })
    }

    /// Videos related to one video.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ProviderError`] if the request fails or is cancelled.
    async fn related(
        &self,
        id: &VideoId,
        cancel: &CancellationToken,
    ) -> ProviderResult<Page<VideoSummary>>;
}

/// Channel metadata and content.
#[async_trait]
pub trait ChannelProvider: Send + Sync {
    /// Channel metadata, including which tabs actually have content.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ProviderError`] if the request fails or is cancelled.
    async fn channel(
        &self,
        id: &ChannelId,
        cancel: &CancellationToken,
    ) -> ProviderResult<ChannelDetails>;

    /// One tab's content.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ProviderError::Unsupported`] for a tab this adapter cannot read, or another
    /// variant if the request fails.
    async fn channel_content(
        &self,
        id: &ChannelId,
        tab: ChannelTab,
        continuation: Option<&ContinuationToken>,
        cancel: &CancellationToken,
    ) -> ProviderResult<Page<VideoSummary>>;
}

/// Provider-hosted playlists.
///
/// Distinct from the local library: these are read-only and belong to the provider. Playlists the
/// user creates live in SQLite and never involve this trait.
#[async_trait]
pub trait PlaylistProvider: Send + Sync {
    /// A playlist and its first page of items.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ProviderError`] if the request fails, is cancelled, or the response cannot
    /// be read.
    async fn playlist(
        &self,
        id: &PlaylistId,
        continuation: Option<&ContinuationToken>,
        cancel: &CancellationToken,
    ) -> ProviderResult<PlaylistDetails>;
}

/// Everything the application asks of a content source.
///
/// The supertrait the application holds, so a single `Arc<dyn MetadataProvider>` covers every read
/// path. Adapters that genuinely cannot do something return
/// [`crate::ProviderError::Unsupported`] and declare it absent in [`ProviderCapabilities`] — the
/// capability is what the UI reads, and the error is the backstop for a caller that ignored it.
#[async_trait]
pub trait MetadataProvider:
    SearchProvider + VideoProvider + ChannelProvider + PlaylistProvider
{
    /// A short stable name for diagnostics and logs. Never user-facing.
    fn name(&self) -> &'static str;

    /// What this provider actually supports.
    fn capabilities(&self) -> ProviderCapabilities;

    /// A discovery feed requiring no query and no account.
    ///
    /// The home surface. Providers with no login-free discovery return
    /// [`crate::ProviderError::Unsupported`], and the home view falls back to the local library —
    /// which is why an account is never needed to have a useful first screen (§43).
    ///
    /// # Errors
    ///
    /// Returns [`crate::ProviderError`] if the request fails, is cancelled, or is unsupported.
    async fn discovery_feed(
        &self,
        continuation: Option<&ContinuationToken>,
        cancel: &CancellationToken,
    ) -> ProviderResult<Page<SearchItem>>;
}
