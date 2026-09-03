//! The YouTube metadata adapter.
//!
//! Implements [`beastube_provider`]'s traits over the InnerTube extractor. Everything
//! provider-specific stops here (§19): no crate above this one names YouTube or `rustypipe`.
//!
//! ## Scope
//!
//! **Metadata only.** This adapter reads what the provider serves publicly: search results, video
//! details, channel content. It does not mint proof-of-origin tokens, solve JavaScript challenges,
//! or touch DRM — which is why no JavaScript engine appears in its dependency graph, and why the
//! deobfuscator feature is off. Playback is the embedded player's job (ADR-0001).
//!
//! ## Capabilities are measured, not assumed
//!
//! `cargo run -p beastube-provider-youtube --example probe` exercises every operation against the
//! live service and reports what works. As of the last run:
//!
//! | Operation | Result |
//! |---|---|
//! | search, suggestions | working |
//! | video details, channel content | working |
//! | remote playlists | **broken upstream** — the extractor's parser no longer matches the response |
//! | video stream URLs | **unavailable** — the provider serves them over a transport we do not implement |
//!
//! [`ProviderCapabilities`] reflects exactly that, so the UI hides the playlist surface rather than
//! offering one that always errors (§131). When upstream parsing is fixed, one flag turns it back
//! on.

// Lint policy is set workspace-wide in Cargo.toml. This adapter is pure async Rust over an HTTP
// client; unsafe here would always be a mistake.
#![forbid(unsafe_code)]

mod map;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use beastube_core::ids::{ChannelId, PlaylistId, VideoId};
use beastube_core::model::channel::{ChannelDetails, ChannelTab};
use beastube_core::model::video::{LiveStatus, VideoDetails, VideoSummary};
use beastube_core::model::{
    ContinuationToken, Page, PlaylistDetails, SearchFilters, SearchItem, SearchResultKind,
    SearchResults, Suggestion,
};
use beastube_provider::capabilities::ProviderCapabilities;
use beastube_provider::error::{ProviderError, ProviderResult, Unavailability};
use beastube_provider::traits::{
    ChannelProvider, MetadataProvider, PlaylistProvider, SearchProvider, VideoProvider,
};
use rustypipe::client::{RustyPipe, RustyPipeQuery};
use rustypipe::error::Error as YtError;
use rustypipe::model::YouTubeItem;
use rustypipe::model::richtext::ToPlaintext;
use tokio_util::sync::CancellationToken;

/// Stable adapter name, used in diagnostics and error payloads.
pub const PROVIDER_NAME: &str = "youtube";

/// The YouTube metadata adapter.
///
/// `Debug` is written by hand because the extractor client does not implement it, and printing its
/// internals would be noise regardless.
#[derive(Clone)]
pub struct YouTubeProvider {
    client: Arc<RustyPipe>,
}

impl std::fmt::Debug for YouTubeProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YouTubeProvider")
            .field("name", &PROVIDER_NAME)
            .finish()
    }
}

impl YouTubeProvider {
    /// Builds an adapter storing its extractor cache under `storage_dir`.
    ///
    /// The directory must be supplied: the extractor otherwise writes its cache into the process
    /// working directory, which for an installed Windows application is Program Files.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Transport`] if the client cannot be constructed, which in practice
    /// means the storage directory is unusable.
    pub fn new(storage_dir: &Path) -> ProviderResult<Self> {
        std::fs::create_dir_all(storage_dir).map_err(|error| ProviderError::Transport {
            detail: format!("could not create the provider cache directory: {error}"),
        })?;

        let client = RustyPipe::builder()
            .storage_dir(storage_dir)
            // The extractor otherwise writes a report directory on unexpected responses. Those
            // reports contain raw upstream payloads, which is exactly the kind of content this
            // application does not retain.
            .no_reporter()
            .build()
            .map_err(|error| ProviderError::Transport {
                detail: error.to_string(),
            })?;

        Ok(Self {
            client: Arc::new(client),
        })
    }

    /// A query handle for one operation.
    fn query(&self) -> RustyPipeQuery {
        self.client.query()
    }

    /// Runs `operation`, abandoning it if `cancel` fires first.
    ///
    /// The extractor has no cancellation channel, so the request itself runs to completion in the
    /// background. What this buys is that the *caller* stops waiting immediately, which is the part
    /// the user perceives (§32).
    async fn with_cancellation<T, F>(cancel: &CancellationToken, operation: F) -> ProviderResult<T>
    where
        F: Future<Output = ProviderResult<T>>,
    {
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = operation => result,
        }
    }
}

/// Classifies an extractor error into the provider taxonomy.
///
/// The distinction that matters is between "this content cannot be shown" and "the extractor no
/// longer understands the response". The second is the early warning that the service changed, and
/// is surfaced as [`ProviderError::SchemaDrift`] so it can be counted rather than lost among
/// ordinary failures (§119).
fn classify(error: &YtError, operation: &'static str) -> ProviderError {
    match error {
        YtError::Extraction(extraction) => {
            let text = extraction.to_string();
            let lower = text.to_lowercase();

            // Unavailability reasons the extractor reports as extraction failures.
            if lower.contains("not found") || lower.contains("does not exist") {
                ProviderError::Unavailable {
                    reason: Unavailability::NotFound,
                }
            } else if lower.contains("private") {
                ProviderError::Unavailable {
                    reason: Unavailability::Private,
                }
            } else if lower.contains("age") && lower.contains("restrict") {
                ProviderError::Unavailable {
                    reason: Unavailability::AgeRestricted,
                }
            } else if lower.contains("members") {
                ProviderError::Unavailable {
                    reason: Unavailability::MembersOnly,
                }
            } else if lower.contains("country") || lower.contains("region") || lower.contains("geo")
            {
                ProviderError::Unavailable {
                    reason: Unavailability::GeoBlocked,
                }
            } else {
                // Everything else from the extractor means it could not read the response. That is
                // schema drift, and saying so is the point.
                ProviderError::SchemaDrift {
                    operation,
                    detail: text,
                }
            }
        }
        // Transport-shaped failures: the request never produced a response we could read.
        YtError::Http(_) | YtError::HttpStatus(..) => ProviderError::Transport {
            detail: error.to_string(),
        },
        other => ProviderError::SchemaDrift {
            operation,
            detail: other.to_string(),
        },
    }
}

/// Wraps a continuation cursor, dropping one that fails validation.
fn continuation(raw: Option<String>) -> Option<ContinuationToken> {
    raw.and_then(|token| ContinuationToken::new(token).ok())
}

#[async_trait]
impl SearchProvider for YouTubeProvider {
    async fn search(
        &self,
        query: &str,
        filters: &SearchFilters,
        continuation_token: Option<&ContinuationToken>,
        cancel: &CancellationToken,
    ) -> ProviderResult<SearchResults> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(ProviderError::InvalidInput {
                field: "query",
                reason: "the query is empty".to_owned(),
            });
        }

        let filters = filters.clone().normalized();
        let client = self.query();
        let owned = trimmed.to_owned();
        let cursor_in = continuation_token.map(|token| token.as_str().to_owned());

        // A continuation is resumed through the extractor's continuation endpoint rather than by
        // re-running the search: re-running would return page one again, which reads as a feed that
        // refuses to scroll. The query is still required and echoed back, so a late page can be
        // matched to the search it belongs to (§32).
        let (page, corrected_query) = Self::with_cancellation(cancel, async move {
            match cursor_in {
                Some(token) => client
                    .continuation::<YouTubeItem, _>(
                        token,
                        rustypipe::model::paginator::ContinuationEndpoint::Search,
                        None,
                    )
                    .await
                    // A continuation carries no spelling correction; the first page already
                    // reported one if there was one.
                    .map(|page| (page, None))
                    .map_err(|error| classify(&error, "search_continuation")),
                None => client
                    .search::<YouTubeItem, _>(owned)
                    .await
                    .map(|results| (results.items, results.corrected_query))
                    .map_err(|error| classify(&error, "search")),
            }
        })
        .await?;

        let cursor = continuation(page.ctoken.clone());
        let mapped: Vec<SearchItem> = page
            .items
            .into_iter()
            .filter_map(map::search_item)
            // The kind filter is applied here rather than upstream: the extractor's own filter
            // encoding is one of the parts most exposed to schema drift, and filtering locally
            // cannot break.
            .filter(|item| matches_kind(item, filters.kind))
            .collect();

        Ok(SearchResults {
            query: trimmed.to_owned(),
            filters,
            page: Page {
                items: mapped,
                continuation: cursor,
                total_estimate: None,
            },
            estimated_total: None,
            corrected_query,
        })
    }

    async fn suggestions(
        &self,
        prefix: &str,
        cancel: &CancellationToken,
    ) -> ProviderResult<Vec<Suggestion>> {
        let trimmed = prefix.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }

        let client = self.query();
        let owned = trimmed.to_owned();

        let suggestions = Self::with_cancellation(cancel, async move {
            client
                .search_suggestion(owned)
                .await
                .map_err(|error| classify(&error, "suggestions"))
        })
        .await?;

        Ok(suggestions
            .into_iter()
            .map(|text| Suggestion {
                text,
                from_history: false,
            })
            .collect())
    }
}

/// Whether a result belongs in a feed restricted to `kind`.
fn matches_kind(item: &SearchItem, kind: SearchResultKind) -> bool {
    match kind {
        SearchResultKind::All => true,
        SearchResultKind::Videos => item
            .as_video()
            .is_some_and(|video| !video.is_short && video.live_status != LiveStatus::Live),
        SearchResultKind::Shorts => item.as_video().is_some_and(|video| video.is_short),
        SearchResultKind::Live => item
            .as_video()
            .is_some_and(|video| video.live_status == LiveStatus::Live),
        SearchResultKind::Channels => matches!(item, SearchItem::Channel(_)),
        SearchResultKind::Playlists => matches!(item, SearchItem::Playlist(_)),
    }
}

#[async_trait]
impl VideoProvider for YouTubeProvider {
    async fn video(
        &self,
        id: &VideoId,
        cancel: &CancellationToken,
    ) -> ProviderResult<VideoDetails> {
        let client = self.query();
        let owned = id.as_str().to_owned();

        let details = Self::with_cancellation(cancel, async move {
            client
                .video_details(owned)
                .await
                .map_err(|error| classify(&error, "video_details"))
        })
        .await?;

        let channel = details.channel;
        let summary = VideoSummary {
            id: id.clone(),
            title: details.name,
            channel_id: ChannelId::new(channel.id.clone()).ok(),
            channel_name: Some(channel.name.clone()),
            // The watch-page payload reports neither a thumbnail list nor a duration. The
            // thumbnail is derived from the video id (see `map::derived_thumbnails`) so a video
            // opened directly is not recorded into history as a grey rectangle; the duration is
            // left unknown here and filled in by the first playback checkpoint, which reads it
            // from the player rather than inventing it.
            thumbnails: map::derived_thumbnails(id),
            duration_ms: None,
            published_at: details
                .publish_date
                .map(beastube_core::time_util::Timestamp::from),
            published_text: details.publish_date_txt,
            view_count: Some(details.view_count),
            live_status: if details.is_live {
                LiveStatus::Live
            } else {
                LiveStatus::NotLive
            },
            is_short: false,
        };

        let mut video = VideoDetails {
            summary,
            description: Some(details.description.to_plaintext()),
            channel_avatar: map::thumbnails(&channel.avatar),
            channel_subscriber_count: channel.subscriber_count,
            like_count: details.like_count.map(u64::from),
            chapters: map::chapters(details.chapters),
            // Caption tracks are not exposed by this extractor build; the capability says so, and
            // the UI omits the control rather than showing an empty menu.
            captions: Vec::new(),
            category: None,
            is_unlisted: false,
            is_age_restricted: false,
        };
        video.sort_chapters();
        Ok(video)
    }

    async fn related(
        &self,
        id: &VideoId,
        cancel: &CancellationToken,
    ) -> ProviderResult<Page<VideoSummary>> {
        let client = self.query();
        let owned = id.as_str().to_owned();

        let details = Self::with_cancellation(cancel, async move {
            client
                .video_details(owned)
                .await
                .map_err(|error| classify(&error, "related"))
        })
        .await?;

        let cursor = continuation(details.recommended.ctoken.clone());
        Ok(Page {
            items: details
                .recommended
                .items
                .into_iter()
                .filter_map(map::video_summary)
                .collect(),
            continuation: cursor,
            total_estimate: None,
        })
    }
}

#[async_trait]
impl ChannelProvider for YouTubeProvider {
    async fn channel(
        &self,
        id: &ChannelId,
        cancel: &CancellationToken,
    ) -> ProviderResult<ChannelDetails> {
        let client = self.query();
        let owned = id.as_str().to_owned();

        let channel = Self::with_cancellation(cancel, async move {
            client
                .channel_videos(owned)
                .await
                .map_err(|error| classify(&error, "channel"))
        })
        .await?;

        Ok(ChannelDetails {
            summary: beastube_core::model::channel::ChannelSummary {
                id: id.clone(),
                name: channel.name,
                avatar: map::thumbnails(&channel.avatar),
                subscriber_count: channel.subscriber_count,
                handle: None,
                is_verified: channel.verification != rustypipe::model::Verification::None,
            },
            description: Some(channel.description),
            banner: map::thumbnails(&channel.banner),
            // Only the uploads tab is verified working; declaring more would offer tabs that fail.
            available_tabs: vec![ChannelTab::Videos],
            video_count: None,
            canonical_url: None,
        })
    }

    async fn channel_content(
        &self,
        id: &ChannelId,
        tab: ChannelTab,
        _continuation: Option<&ContinuationToken>,
        cancel: &CancellationToken,
    ) -> ProviderResult<Page<VideoSummary>> {
        // Videos, Shorts and Live are three tabs of the same endpoint. Playlists is a different
        // shape entirely and is refused rather than mapped onto this one.
        let extractor_tab = match tab {
            ChannelTab::Videos => rustypipe::param::ChannelVideoTab::Videos,
            ChannelTab::Shorts => rustypipe::param::ChannelVideoTab::Shorts,
            ChannelTab::Live => rustypipe::param::ChannelVideoTab::Live,
            ChannelTab::Playlists => {
                return Err(ProviderError::Unsupported {
                    operation: "channel_content",
                    provider: PROVIDER_NAME,
                });
            }
        };

        let client = self.query();
        let owned = id.as_str().to_owned();

        let channel = Self::with_cancellation(cancel, async move {
            client
                .channel_videos_tab(owned, extractor_tab)
                .await
                .map_err(|error| classify(&error, "channel_content"))
        })
        .await?;

        let cursor = continuation(channel.content.ctoken.clone());
        Ok(Page {
            items: channel
                .content
                .items
                .into_iter()
                .filter_map(map::video_summary)
                .collect(),
            continuation: cursor,
            total_estimate: None,
        })
    }
}

#[async_trait]
impl PlaylistProvider for YouTubeProvider {
    /// Not supported by this build.
    ///
    /// The extractor's playlist parser no longer matches the provider's response — the live probe
    /// reports `itemSectionRenderer empty`. Returning a clear refusal beats returning a schema-drift
    /// error on every call, and [`ProviderCapabilities::playlists`] is false so the UI never asks.
    async fn playlist(
        &self,
        _id: &PlaylistId,
        _continuation: Option<&ContinuationToken>,
        _cancel: &CancellationToken,
    ) -> ProviderResult<PlaylistDetails> {
        Err(ProviderError::Unsupported {
            operation: "playlist",
            provider: PROVIDER_NAME,
        })
    }
}

#[async_trait]
impl MetadataProvider for YouTubeProvider {
    fn name(&self) -> &'static str {
        PROVIDER_NAME
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            search_videos: true,
            search_channels: true,
            search_playlists: true,
            search_shorts: true,
            suggestions: true,
            video_details: true,
            related_videos: true,
            channel_details: true,
            channel_videos: true,
            // Verified absent by the live probe rather than assumed either way.
            channel_shorts: false,
            playlists: false,
            discovery_feed: false,
            captions: false,
            chapters: true,
            // The kind filter is applied locally; date and duration filters are not implemented,
            // so the UI must not offer them.
            search_filters: false,
            // Search resumes through the extractor's continuation endpoint. Other surfaces still
            // return a single page, which is why this flag lives with the search capabilities
            // rather than standing for the provider as a whole.
            pagination: true,
        }
    }

    /// Not supported.
    ///
    /// The provider retired its login-free trending surface in 2025, and everything replacing it is
    /// personalized — which requires exactly the account this application does not have. The home
    /// view falls back to the local library, so a first screen never depends on signing in (§43).
    async fn discovery_feed(
        &self,
        _continuation: Option<&ContinuationToken>,
        _cancel: &CancellationToken,
    ) -> ProviderResult<Page<SearchItem>> {
        Err(ProviderError::Unsupported {
            operation: "discovery_feed",
            provider: PROVIDER_NAME,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> YouTubeProvider {
        let dir = std::env::temp_dir().join("beastube-provider-tests");
        YouTubeProvider::new(&dir).expect("client constructs")
    }

    #[tokio::test]
    async fn an_empty_query_is_rejected_before_any_request() {
        let provider = provider();
        let filters = SearchFilters::default();
        let cancel = CancellationToken::new();

        for query in ["", "   ", "\t\n"] {
            let error = provider
                .search(query, &filters, None, &cancel)
                .await
                .expect_err("an empty query is invalid");
            assert!(matches!(error, ProviderError::InvalidInput { .. }));
        }
    }

    #[tokio::test]
    async fn an_empty_suggestion_prefix_returns_nothing_without_a_request() {
        let provider = provider();
        let suggestions = provider
            .suggestions("  ", &CancellationToken::new())
            .await
            .expect("an empty prefix is not an error");
        assert!(suggestions.is_empty());
    }

    #[tokio::test]
    async fn an_already_cancelled_token_short_circuits() {
        let provider = provider();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let error = provider
            .video(&VideoId::new("dQw4w9WgXcQ").unwrap(), &cancel)
            .await
            .expect_err("cancelled");
        assert!(matches!(error, ProviderError::Cancelled));
    }

    #[tokio::test]
    async fn unsupported_operations_refuse_clearly_rather_than_erroring_obscurely() {
        let provider = provider();
        let cancel = CancellationToken::new();

        let playlist = provider
            .playlist(&PlaylistId::new("PLabc").unwrap(), None, &cancel)
            .await
            .expect_err("playlists are not supported by this build");
        assert!(matches!(playlist, ProviderError::Unsupported { .. }));

        let feed = provider
            .discovery_feed(None, &cancel)
            .await
            .expect_err("there is no login-free discovery feed");
        assert!(matches!(feed, ProviderError::Unsupported { .. }));
    }

    #[tokio::test]
    async fn the_playlists_tab_is_refused_rather_than_returning_nothing() {
        // Videos, Shorts and Live are one endpoint with a tab parameter and are all attempted.
        // Playlists is a different response shape, and refusing beats returning an empty page that
        // looks like a channel with no playlists.
        let provider = provider();
        let error = provider
            .channel_content(
                &ChannelId::new("UCabc").unwrap(),
                ChannelTab::Playlists,
                None,
                &CancellationToken::new(),
            )
            .await
            .expect_err("the playlists tab is a different endpoint");
        assert!(matches!(error, ProviderError::Unsupported { .. }));
    }

    #[test]
    fn declared_capabilities_match_what_is_implemented() {
        // The capability set is the UI's only source of truth for which controls to render, so it
        // must not claim anything the adapter refuses.
        let capabilities = provider().capabilities();
        assert!(capabilities.is_usable());
        assert!(capabilities.supports_any_search());
        assert!(
            !capabilities.playlists,
            "playlist() returns Unsupported, so the capability must be false"
        );
        assert!(
            !capabilities.discovery_feed,
            "discovery_feed() returns Unsupported, so the capability must be false"
        );
        assert!(!capabilities.captions, "caption tracks are not extracted");
    }

    #[test]
    fn the_kind_filter_partitions_results() {
        use beastube_core::model::video::VideoSummary;

        let video = SearchItem::Video(VideoSummary::placeholder(
            VideoId::new("dQw4w9WgXcQ").unwrap(),
            "A video",
        ));
        let short = SearchItem::Video(VideoSummary {
            is_short: true,
            ..VideoSummary::placeholder(VideoId::new("shortsid123").unwrap(), "A short")
        });

        assert!(matches_kind(&video, SearchResultKind::All));
        assert!(matches_kind(&video, SearchResultKind::Videos));
        assert!(!matches_kind(&video, SearchResultKind::Shorts));

        assert!(matches_kind(&short, SearchResultKind::Shorts));
        assert!(
            !matches_kind(&short, SearchResultKind::Videos),
            "a short must not appear under the long-form filter"
        );
    }
}
