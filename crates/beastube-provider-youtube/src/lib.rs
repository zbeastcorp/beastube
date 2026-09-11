//! The YouTube metadata adapter.
//!
//! Implements [`beastube_provider`]'s traits over the InnerTube extractor. Everything
//! provider-specific stops here: no crate above this one names YouTube or `rustypipe`.
//!
//! ## Scope
//!
//! **Metadata only.** This adapter reads what the provider serves publicly: search results, video
//! details, channel content. It does not mint proof-of-origin tokens, solve JavaScript challenges,
//! or touch DRM. (It does not follow that no JavaScript engine is present: `rustypipe` brings
//! `rquickjs` with it. The invariant is about what this code does, not about the graph.) The
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
//! | channel header, About tab | working, but see below |
//! | channel Shorts tab | working only off the raw response — the typed parser returns nothing |
//! | channel video sort orders | **broken upstream** — every order, every tab, every channel refuses |
//! | remote playlists | **broken upstream** — the extractor's parser no longer matches the response |
//! | video stream URLs | **unavailable** — the provider serves them over a transport we do not implement |
//!
//! Two of those need naming precisely, because both are cases where the extractor answers
//! confidently and wrongly rather than failing:
//!
//! - The channel header's `subscriber_count` is really the **video** count. See
//!   [`YouTubeProvider::channel`], which takes both figures from the About tab instead.
//! - A channel's Shorts tab parses to an empty list. See [`YouTubeProvider::channel_shorts`].
//!
//! [`ProviderCapabilities`] reflects exactly that, so the UI hides the playlist surface rather than
//! offering one that always errors. When upstream parsing is fixed, one flag turns it back
//! on.

// Lint policy is set workspace-wide in Cargo.toml. This adapter is pure async Rust over an HTTP
// client; unsafe here would always be a mistake.
#![forbid(unsafe_code)]

mod map;
mod visitor;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use beastube_core::ids::{ChannelId, PlaylistId, VideoId};
use beastube_core::model::channel::{ChannelDetails, ChannelTab};
use beastube_core::model::explore::ExploreCategory;
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
use futures::FutureExt as _;
use std::panic::AssertUnwindSafe;
use tokio_util::sync::CancellationToken;

/// Stable adapter name, used in diagnostics and error payloads.
pub const PROVIDER_NAME: &str = "youtube";

/// How many recovered shorts one search page may contribute.
///
/// A live response carries 25 to 30 of them; admitting the lot would bury the long-form results
/// the same query matched, which is the opposite of the problem being fixed.
const SHORTS_PER_SEARCH: usize = 12;

/// Where recovered shorts are spliced into the page.
///
/// The search view lifts shorts into their own shelf regardless of where they sit, so this is for
/// the benefit of anything reading the flat list: appending them would leave a page that reads as
/// long-form results with an unrelated tail, and prepending would bury what was actually asked
/// for. After the first few ordinary results is where the site itself puts the shelf.
const SHORTS_INSERT_AT: usize = 4;

/// How many long-form results a first page should carry before it stops asking for more.
///
/// Below this a page reads as "nothing found" even when the chain has plenty a click away.
const MIN_LONG_FORM_ON_FIRST_PAGE: usize = 6;

/// How many extra pages a thin first page may pull in.
///
/// Each one is a request the user is waiting on, so this trades a little latency on the queries
/// that need it for a page that is worth reading, and never runs on the ones that do not.
const MAX_FILL_PAGES: usize = 3;

/// The YouTube metadata adapter.
///
/// `Debug` is written by hand because the extractor client does not implement it, and printing its
/// internals would be noise regardless.
#[derive(Clone)]
pub struct YouTubeProvider {
    client: Arc<RustyPipe>,
    /// The visitor ID attached to every query, so the extractor never fetches its own. See
    /// [`visitor`] for why that fetch is the single largest cost of a cold request.
    visitor: visitor::VisitorDataPool,
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

        let visitor = visitor::VisitorDataPool::new().map_err(|error| ProviderError::Transport {
            detail: format!("could not build the visitor-data client: {error}"),
        })?;

        Ok(Self {
            client: Arc::new(client),
            visitor,
        })
    }

    /// A query handle for one operation, carrying our visitor ID.
    ///
    /// Async because the first call fetches the ID. Every call after that reads it from memory;
    /// the await is a single lock read.
    async fn query(&self) -> RustyPipeQuery {
        self.client
            .query()
            .visitor_data_opt(self.visitor.current().await)
    }

    /// Watch-page details rebuilt from the player payload.
    ///
    /// The fallback for [`VideoProvider::video`] when the typed watch-page parse fails. Returns
    /// `None` if the request or the parse also fails, so the caller reports the original error
    /// rather than a second, less recognisable one.
    async fn details_via_player(
        &self,
        id: &VideoId,
        cancel: &CancellationToken,
    ) -> Option<VideoDetails> {
        let client = self.query().await;
        let body = serde_json::json!({ "videoId": id.as_str() });

        let json = Self::with_cancellation(cancel, async move {
            client
                .raw(rustypipe::client::ClientType::Desktop, "player", &body)
                .await
                .map_err(|error| classify(&error, "video_details_player"))
        })
        .await
        .ok()?;

        map::details_from_player(&json, id)
    }

    /// One channel tab, read from the raw browse response.
    ///
    /// Every tab on a channel is a grid of `lockupViewModel` objects, and the extractor's channel
    /// parser expects `videoRenderer`. Measured against a live channel browse response: **30
    /// lockups, zero `videoRenderer`**. So the typed parser either returns nothing or returns
    /// stubs, and it did both:
    ///
    /// - **Shorts** came back empty outright — 0 items from a tab holding 48.
    /// - **Videos** came back with titles and thumbnails and nothing else: 0 of 30 with a
    ///   duration, 0 of 30 with an upload date, 1 of 30 with a view count. That is what put bare,
    ///   dateless cards on the channel page while search cards were complete.
    ///
    /// Reading the response directly fixes both, and reuses the lockup parser the recommendation
    /// shelf already relies on — the same shape from a different endpoint.
    ///
    /// `params` selects the tab and is the value the site's own navigation sends. It is opaque and
    /// fixed; if the site retires one, the request returns an empty tab rather than the wrong
    /// content, which is why it is safe to hard-code.
    ///
    /// Pagination is not claimed. The response carries a continuation token, but nothing here can
    /// resume from it yet, and a cursor the caller cannot follow is worse than an honest end of
    /// list.
    async fn channel_tab(
        &self,
        id: &ChannelId,
        tab: ChannelTab,
        cancel: &CancellationToken,
    ) -> ProviderResult<Page<VideoSummary>> {
        let params = match tab {
            ChannelTab::Videos => "EgZ2aWRlb3PyBgQKAjoA",
            ChannelTab::Shorts => "EgZzaG9ydHPyBgUKA5oBAA%3D%3D",
            ChannelTab::Live => "EgdzdHJlYW1z8gYECgJ6AA%3D%3D",
            // A different shape entirely, and refused rather than mapped onto this one.
            ChannelTab::Playlists => {
                return Err(ProviderError::Unsupported {
                    operation: "channel_content",
                    provider: PROVIDER_NAME,
                });
            }
        };

        let client = self.query().await;
        let body = serde_json::json!({ "browseId": id.as_str(), "params": params });

        let json = Self::with_cancellation(cancel, async move {
            client
                .raw(rustypipe::client::ClientType::Desktop, "browse", &body)
                .await
                .map_err(|error| classify(&error, "channel_content"))
        })
        .await?;

        // Shorts sit in a lockup of their own, which carries neither a duration nor a date.
        let items = if tab == ChannelTab::Shorts {
            map::shorts_from_json(&json)
        } else {
            map::videos_from_json(&json)
        };

        Ok(Page {
            items,
            continuation: None,
            total_estimate: None,
        })
    }

    /// The video's caption tracks, or an empty list if they cannot be read.
    ///
    /// They live on the player response, not the watch page — which is why the capability used to
    /// say captions were unavailable when they were one request away. Never fails the caller:
    /// a watch page that loads without a caption list is the behaviour that shipped before, and it
    /// is a far better outcome than a watch page that does not load.
    async fn caption_and_audio_tracks(
        &self,
        id: &VideoId,
        cancel: &CancellationToken,
    ) -> (
        Vec<beastube_core::model::video::CaptionTrack>,
        Vec<beastube_core::model::video::AudioTrack>,
    ) {
        let client = self.query().await;
        let owned = id.as_str().to_owned();

        // Boxed: the extractor's player future is around 16 KB, which is too much to carry inline
        // inside the futures this is joined with.
        let request = Box::pin(async move {
            client
                .player(owned)
                .await
                .map_err(|error| classify(&error, "player_subtitles"))
        });

        match Self::with_cancellation(cancel, request).await {
            Ok(player) => (
                map::caption_tracks(player.subtitles.clone()),
                map::audio_tracks(&player.audio_streams),
            ),
            Err(error) => {
                tracing::debug!(video = id.as_str(), %error, "track lists were unavailable");
                (Vec::new(), Vec::new())
            }
        }
    }

    /// Fetches the visitor ID ahead of the first request.
    ///
    /// Called once at startup from a background task, so the first thing the user asks for does
    /// not pay for the page load that gets it. Safe to skip: the first query fetches it itself.
    pub async fn warm(&self) {
        self.visitor.warm().await;
    }

    /// Runs `operation`, abandoning it if `cancel` fires first and surviving it if it panics.
    ///
    /// The extractor has no cancellation channel, so the request itself runs to completion in the
    /// background. What this buys is that the *caller* stops waiting immediately, which is the part
    /// the user perceives.
    ///
    /// # Why a panic is caught here
    ///
    /// The extractor unwraps in places where a failure is possible — `visitor_data.rs` calls
    /// `.unwrap()` on the result of fetching `music.youtube.com`, which currently answers `302
    /// Found`. Unguarded, that panic propagates out of whichever command was running and the user
    /// is shown a failed screen for something a retry would have fixed.
    ///
    /// What this does *not* catch, and it is worth being exact: rustypipe also refreshes visitor
    /// data from a detached `tokio::spawn`, and a panic there is unreachable from here — nothing
    /// awaits that task. Those are the panics visible in the log. This guard covers the
    /// synchronous path, where a panic would otherwise reach the user.
    ///
    /// Catching it converts the panic into [`ProviderError::Transport`], which is classified as
    /// automatically retryable — so the same blip now costs a retry rather than a screen. This is
    /// not a workaround for our own bug; it is a boundary around a dependency whose failure mode is
    /// not ours to fix, and the alternative is letting a third-party `.unwrap()` decide whether the
    /// application works.
    ///
    /// `AssertUnwindSafe` is a deliberate claim, not an oversight: a panic mid-request could leave
    /// the extractor's internal caches inconsistent. They hold fetched metadata, not invariants
    /// anything else depends on, and the next call re-fetches what it needs. Trading that risk for
    /// "the window does not break" is the right way round.
    async fn with_cancellation<T, F>(cancel: &CancellationToken, operation: F) -> ProviderResult<T>
    where
        F: Future<Output = ProviderResult<T>>,
    {
        let guarded = AssertUnwindSafe(operation).catch_unwind();

        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = guarded => match result {
                Ok(value) => value,
                Err(_) => Err(ProviderError::Transport {
                    detail: "the extractor panicked, most likely on a failed request".to_owned(),
                }),
            },
        }
    }
}

/// Classifies an extractor error into the provider taxonomy.
///
/// The distinction that matters is between "this content cannot be shown" and "the extractor no
/// longer understands the response". The second is the early warning that the service changed, and
/// is surfaced as [`ProviderError::SchemaDrift`] so it can be counted rather than lost among
/// ordinary failures.
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
        let client = self.query().await;
        let owned = trimmed.to_owned();
        let cursor_in = continuation_token.map(|token| token.as_str().to_owned());

        // A continuation is resumed through the extractor's continuation endpoint rather than by
        // re-running the search: re-running would return page one again, which reads as a feed that
        // refuses to scroll. The query is still required and echoed back, so a late page can be
        // matched to the search it belongs to.
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

        let mut cursor = continuation(page.ctoken.clone());
        let mut mapped: Vec<SearchItem> = page
            .items
            .into_iter()
            .filter_map(map::search_item)
            // The kind filter is applied here rather than upstream: the extractor's own filter
            // encoding is one of the parts most exposed to schema drift, and filtering locally
            // cannot break.
            .filter(|item| matches_kind(item, filters.kind))
            .collect();

        // The typed extractor drops every short in the response, for the reason
        // `map::shorts_from_json` documents. Measured across five live searches it discarded 25
        // to 30 per query, and on a casual one — "funny cat" — the response held 8 ordinary videos
        // against 30 shorts, so a search whose results were mostly short-form came back looking
        // almost empty and appeared to demand the exact title of a video. They are recovered here
        // out of the same response the Shorts feed reads.
        //
        // Only on a first page: the shorts shelf appears once, and a continuation carries none, so
        // asking again while scrolling would spend a request to merge nothing.
        if continuation_token.is_none()
            && matches!(
                filters.kind,
                SearchResultKind::All | SearchResultKind::Shorts
            )
        {
            // A failure here leaves the long-form results standing rather than failing a search
            // the user watched work; shorts are an addition to the page, not the page.
            let recovered = match self.search_shorts(trimmed, cancel).await {
                Ok(shorts) => shorts,
                Err(error) => {
                    tracing::warn!(%error, "shorts could not be recovered for this search");
                    Vec::new()
                }
            };

            splice_shorts(&mut mapped, recovered, filters.kind);
        }

        // Some queries answer their first page entirely with a shorts shelf and send the ordinary
        // videos later — "roblox trends" returns 25 shorts and no long-form result at all on page
        // one, and two videos on page two. A feed that asked once and stopped showed nothing for
        // those queries, so a first page that came back thin keeps pulling until it has enough to
        // be worth reading or the chain runs out.
        //
        // The cursor advances with it, so scrolling resumes after the last page consumed rather
        // than repeating what was already shown.
        if continuation_token.is_none() && filters.kind != SearchResultKind::Shorts {
            let mut pulled = 0;
            while long_form(&mapped) < MIN_LONG_FORM_ON_FIRST_PAGE && pulled < MAX_FILL_PAGES {
                let Some(token) = cursor.clone() else { break };
                let client = self.query().await;
                let next = Self::with_cancellation(cancel, async move {
                    client
                        .continuation::<YouTubeItem, _>(
                            token.as_str().to_owned(),
                            rustypipe::model::paginator::ContinuationEndpoint::Search,
                            None,
                        )
                        .await
                        .map_err(|error| classify(&error, "search_fill"))
                })
                .await;

                let next = match next {
                    Ok(page) => page,
                    // A thin page is still a page; failing the whole search because the filler
                    // request failed would be worse than showing what page one did have.
                    Err(error) => {
                        tracing::warn!(%error, "a sparse first page could not be filled");
                        break;
                    }
                };

                cursor = continuation(next.ctoken.clone());
                let seen: std::collections::HashSet<String> = mapped
                    .iter()
                    .filter_map(|item| item.as_video().map(|v| v.id.as_str().to_owned()))
                    .collect();
                let more: Vec<SearchItem> = next
                    .items
                    .into_iter()
                    .filter_map(map::search_item)
                    .filter(|item| matches_kind(item, filters.kind))
                    .filter(|item| {
                        item.as_video()
                            .is_none_or(|video| !seen.contains(video.id.as_str()))
                    })
                    .collect();

                let gained = more.len();
                mapped.extend(more);
                pulled += 1;

                // A page that added nothing and offers no successor is the end of the chain.
                if gained == 0 && cursor.is_none() {
                    break;
                }
            }
        }

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

    /// Short-form results, read out of the raw response.
    ///
    /// Goes around the typed extractor deliberately. Shorts arrive as `shortsLockupViewModel`
    /// objects inside a shelf renderer it does not recognise, so its parser drops them — measured
    /// on a live search, 26 shorts in the response and none in the parsed result. This issues the
    /// same request through the same client and reads the part that was being thrown away.
    async fn search_shorts(
        &self,
        query: &str,
        cancel: &CancellationToken,
    ) -> ProviderResult<Vec<VideoSummary>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(ProviderError::InvalidInput {
                field: "query",
                reason: "the query is empty".to_owned(),
            });
        }

        let client = self.query().await;
        let body = serde_json::json!({ "query": trimmed });

        let json = Self::with_cancellation(cancel, async move {
            client
                .raw(rustypipe::client::ClientType::Desktop, "search", &body)
                .await
                .map_err(|error| classify(&error, "search_shorts"))
        })
        .await?;

        Ok(map::shorts_from_json(&json))
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

        let client = self.query().await;
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

/// How many results on a page are long-form — the ones a thin page is short of.
fn long_form(items: &[SearchItem]) -> usize {
    items
        .iter()
        .filter(|item| item.as_video().is_none_or(|video| !video.is_short))
        .count()
}

/// Merges recovered shorts into a page of search results, in place.
///
/// Split out from [`SearchProvider::search`] so the ordering and de-duplication can be tested
/// without a network round trip. Anything already present by video id is dropped, so a short the
/// typed parser did happen to return is not shown twice.
fn splice_shorts(mapped: &mut Vec<SearchItem>, recovered: Vec<VideoSummary>, kind: SearchResultKind) {
    let seen: std::collections::HashSet<String> = mapped
        .iter()
        .filter_map(|item| item.as_video().map(|video| video.id.as_str().to_owned()))
        .collect();

    let mut extra: Vec<SearchItem> = recovered
        .into_iter()
        .filter(|video| !seen.contains(video.id.as_str()))
        .map(SearchItem::Video)
        .filter(|item| matches_kind(item, kind))
        .take(SHORTS_PER_SEARCH)
        .collect();

    if extra.is_empty() {
        return;
    }

    let at = mapped.len().min(SHORTS_INSERT_AT);
    let tail = mapped.split_off(at);
    mapped.append(&mut extra);
    mapped.extend(tail);
}

/// Whether a host is one the provider serves caption files from.
///
/// Exact matches and subdomains only, compared against the registrable domain — a prefix check
/// would accept `youtube.com.example.net`.
fn is_caption_host(host: &str) -> bool {
    const ALLOWED: &[&str] = &["youtube.com", "www.youtube.com", "video.google.com"];
    ALLOWED
        .iter()
        .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
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
        let client = self.query().await;
        let owned = id.as_str().to_owned();

        let (watch_page, (captions, audio_tracks)) = futures::join!(
            Self::with_cancellation(cancel, async move {
                client
                    .video_details(owned)
                    .await
                    .map_err(|error| classify(&error, "video_details"))
            }),
            self.caption_and_audio_tracks(id, cancel),
        );

        let details = match watch_page {
            Ok(details) => details,
            // The watch-page parser treats one absent optional section as fatal — it reports
            // `could not find secondary_info` and yields nothing — so a video that is perfectly
            // playable opens on an error page instead. Before giving up, ask the player endpoint,
            // whose payload is a flat object rather than a tree of renderers and which answers for
            // ids the watch page refuses. See `map::details_from_player`.
            //
            // Only for a response we could not read. `Unavailable` is an answer, not a failure:
            // the player endpoint still returns metadata for videos that are private, withdrawn or
            // blocked in the region, so retrying those would replace a truthful "this cannot be
            // played" with an ordinary-looking watch page for a video that will not play.
            Err(error @ ProviderError::SchemaDrift { .. }) => {
                let Some(mut details) = self.details_via_player(id, cancel).await else {
                    return Err(error);
                };
                details.captions = captions;
                details.audio_tracks = audio_tracks;
                tracing::debug!(
                    video = id.as_str(),
                    "watch-page details were unreadable; used the player payload"
                );
                return Ok(details);
            }
            Err(error) => return Err(error),
        };

        let channel = details.channel;
        let summary = VideoSummary {
            id: id.clone(),
            title: details.name,
            channel_id: ChannelId::new(channel.id.clone()).ok(),
            channel_name: Some(channel.name.clone()),
            channel_avatar: map::thumbnails(&channel.avatar),
            channel_verified: map::is_verified(channel.verification),
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
            // Fetched alongside the watch page rather than from it: the watch-page payload has no
            // caption list, but the player response does. Failure here leaves the list empty, which
            // reads as "this video has no captions" — the same outcome as before, rather than a
            // failed watch page.
            captions,
            audio_tracks,
            category: None,
            is_unlisted: false,
            is_age_restricted: false,
        };
        video.sort_chapters();
        Ok(video)
    }

    async fn caption_cues(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> ProviderResult<Vec<beastube_core::model::Cue>> {
        // The URL comes from this adapter's own track list, but it crosses the IPC boundary and
        // comes back, so it is checked rather than trusted: without this the command would fetch
        // whatever address the front end handed it.
        let parsed = url::Url::parse(url).map_err(|error| ProviderError::InvalidInput {
            field: "url",
            reason: error.to_string(),
        })?;
        let host = parsed.host_str().unwrap_or_default();
        if parsed.scheme() != "https" || !is_caption_host(host) {
            return Err(ProviderError::InvalidInput {
                field: "url",
                reason: "not a caption track address".to_owned(),
            });
        }

        // `json3` rather than the default XML: a flat array of start, duration and text, with no
        // entity decoding to get wrong.
        let mut target = parsed;
        target.query_pairs_mut().append_pair("fmt", "json3");

        let request = Box::pin(async move {
            reqwest::get(target)
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(|error| ProviderError::Transport {
                    detail: error.to_string(),
                })?
                .text()
                .await
                .map_err(|error| ProviderError::Transport {
                    detail: error.to_string(),
                })
        });

        let body = Self::with_cancellation(cancel, request).await?;
        Ok(map::cues_from_json3(&body))
    }

    async fn related(
        &self,
        id: &VideoId,
        cancel: &CancellationToken,
    ) -> ProviderResult<Page<VideoSummary>> {
        let client = self.query().await;
        let body = serde_json::json!({ "videoId": id.as_str() });

        // The raw response, read directly, for the same reason `search_shorts` does it: the typed
        // parser turns this shelf into items with no channel, no view count and no date — measured
        // against the live service, all twenty of them — because YouTube moved it to
        // `lockupViewModel`, which the parser does not recognise. A Home feed built on those showed
        // titles and nothing else, while the identical cards from search showed everything.
        let json = Self::with_cancellation(cancel, async move {
            client
                .raw(rustypipe::client::ClientType::Desktop, "next", &body)
                .await
                .map_err(|error| classify(&error, "related"))
        })
        .await?;

        Ok(Page {
            items: map::videos_from_json(&json),
            // The shelf paginates by continuation token, which this reader does not yet follow.
            // Twenty recommendations is already more than the surface shows, and claiming a cursor
            // that nothing can resume would be worse than reporting the end of the list.
            continuation: None,
            total_estimate: None,
        })
    }
}

#[async_trait]
impl ChannelProvider for YouTubeProvider {
    /// Everything the channel page draws, from two requests made together.
    ///
    /// The channel page header and the About tab disagree, and the About tab is the one that is
    /// right. Measured against the live service, the header parse reports `MrBeast` at **1000**
    /// subscribers and Google for Developers at **6000** — those are their *video* counts,
    /// rounded. The extractor reads `metadataParts[1]` ("1K videos") into `subscriber_count` and
    /// leaves `video_count` empty, because the site moved the parts of that row. The About tab
    /// reports 516,000,000 and 2,670,000 for the same two channels, which is what the site itself
    /// displays.
    ///
    /// So both counts come from About, and the header supplies only what About does not carry:
    /// name, avatar, banner, handle, verification, and which tabs exist. The two go out
    /// concurrently, so the page costs one round trip rather than two.
    ///
    /// A failed About request is not fatal. It costs the counts, the links and the join date; the
    /// page still has a banner, a name and videos, and half a header beats an error screen.
    async fn channel(
        &self,
        id: &ChannelId,
        cancel: &CancellationToken,
    ) -> ProviderResult<ChannelDetails> {
        let page_client = self.query().await;
        let about_client = self.query().await;
        let for_page = id.as_str().to_owned();
        let for_about = id.as_str().to_owned();

        let (page, about) = futures::join!(
            Self::with_cancellation(cancel, async move {
                page_client
                    .channel_videos(for_page)
                    .await
                    .map_err(|error| classify(&error, "channel"))
            }),
            Self::with_cancellation(cancel, async move {
                about_client
                    .channel_info(for_about)
                    .await
                    .map_err(|error| classify(&error, "channel_info"))
            }),
        );

        let page = page?;
        let about = about.ok();

        // Videos always; the others only where the channel reports having them, so nobody opens a
        // tab onto an empty page. Playlists is absent on purpose: the playlists endpoint
        // returns zero items for every channel measured, so offering the tab would be a promise
        // this build cannot keep.
        let mut available_tabs = vec![ChannelTab::Videos];
        if page.has_shorts {
            available_tabs.push(ChannelTab::Shorts);
        }
        if page.has_live {
            available_tabs.push(ChannelTab::Live);
        }

        Ok(ChannelDetails {
            summary: beastube_core::model::channel::ChannelSummary {
                id: id.clone(),
                name: page.name,
                avatar: map::thumbnails(&page.avatar),
                subscriber_count: about.as_ref().and_then(|info| info.subscriber_count),
                handle: page
                    .handle
                    .map(|handle| handle.trim_start_matches('@').to_owned()),
                is_verified: map::is_verified(page.verification),
            },
            // An owner who wrote nothing has no description rather than an empty one, so the page
            // hides the section instead of drawing a blank.
            description: Some(page.description).filter(|text| !text.trim().is_empty()),
            banner: map::thumbnails(&page.banner),
            available_tabs,
            video_count: about.as_ref().and_then(|info| info.video_count),
            // Built here rather than taken from About, which reports an `http` URL the external
            // opener refuses by design.
            canonical_url: Some(format!("https://www.youtube.com/channel/{}", id.as_str())),
            links: about
                .as_ref()
                .map(|info| map::channel_links(&info.links))
                .unwrap_or_default(),
            view_count: about.as_ref().and_then(|info| info.view_count),
            joined_at: about
                .as_ref()
                .and_then(|info| map::joined_at(info.create_date)),
            country: about.as_ref().and_then(|info| map::country_code(info.country)),
        })
    }

    /// The videos behind one tab.
    ///
    /// Every tab is served from the raw browse response; [`Self::channel_tab`] records why the
    /// typed parser cannot be used for any of them.
    async fn channel_content(
        &self,
        id: &ChannelId,
        tab: ChannelTab,
        _continuation: Option<&ContinuationToken>,
        cancel: &CancellationToken,
    ) -> ProviderResult<Page<VideoSummary>> {
        self.channel_tab(id, tab, cancel).await
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
            // Read straight out of the raw response, because the typed extractor drops the shelf
            // they arrive in and returns nothing for a shorts query.
            search_shorts: true,
            suggestions: true,
            video_details: true,
            related_videos: true,
            channel_details: true,
            channel_videos: true,
            // Read straight out of the raw response, for the same reason `search_shorts` is: the
            // typed tab parser returns an empty list for a tab the live probe finds 48 items in.
            channel_shorts: true,
            playlists: false,
            // Built from the category hubs rather than from the retired trending feed.
            discovery_feed: true,
            // Seven categories, each measured returning videos without an account. The site's
            // Trending, Movies & TV and Podcasts are not among them; see `ExploreCategory`.
            explore: true,
            // Read from the player response; verified against the live service, which returned six
            // tracks for a long-form video and one auto-generated track for a short.
            captions: true,
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

    /// A login-free feed of what is on the provider right now.
    ///
    /// It used to be refused, on the grounds that the trending surface was retired and everything
    /// replacing it needs an account. The first half is still true — the trending feed answers
    /// `400` — but the second was too broad: the category hubs behind [`Self::explore`] are
    /// editorial, identical for everyone, and need no account. Several of them together are a real
    /// feed of what the provider is showing today.
    ///
    /// This is worth having because of what it replaces. With nothing here, a fresh install's home
    /// screen was assembled by *searching* for evergreen words — "music", "science", "cooking" —
    /// which returns whatever ranks well for them, frequently years old. Read from the hubs
    /// instead, the same screen carries videos published minutes ago.
    ///
    /// Each hub already arrives newest-first, so taking them in turn keeps the front of the feed
    /// fresh while keeping it varied. Sorting the merged list by age would do neither: news is
    /// republished every few minutes and would crowd out everything else, and the only date these
    /// surfaces publish is relative text, so any sort would be built on a timestamp inferred from
    /// prose rather than one the provider actually sent.
    ///
    /// A hub that fails contributes nothing and the rest still fill the feed — one slow or broken
    /// category must not cost the viewer their home screen.
    async fn discovery_feed(
        &self,
        _continuation: Option<&ContinuationToken>,
        cancel: &CancellationToken,
    ) -> ProviderResult<Page<SearchItem>> {
        // Every category except Gaming, whose hub serves five videos to a signed-out reader and
        // would contribute almost nothing while costing a request like any other.
        let categories = [
            ExploreCategory::News,
            ExploreCategory::Music,
            ExploreCategory::Learning,
            ExploreCategory::Sport,
            ExploreCategory::Fashion,
            ExploreCategory::Live,
        ];

        let lists = futures::future::join_all(
            categories
                .into_iter()
                .map(|category| self.explore(category, None, cancel)),
        )
        .await;

        let mut columns: Vec<std::vec::IntoIter<VideoSummary>> = lists
            .into_iter()
            .filter_map(|page| page.ok().map(|page| page.items.into_iter()))
            .collect();

        if columns.is_empty() {
            return Err(ProviderError::Transport {
                detail: "no category hub could be read".to_owned(),
            });
        }

        // Round-robin across the categories, dropping repeats: the same video legitimately appears
        // on more than one hub.
        let mut seen = std::collections::HashSet::new();
        let mut drawn: Vec<VideoSummary> = Vec::new();
        loop {
            let mut drew_any = false;
            for column in &mut columns {
                if let Some(video) = column.next() {
                    drew_any = true;
                    if seen.insert(video.id.as_str().to_owned()) {
                        drawn.push(video);
                    }
                }
            }
            if !drew_any {
                break;
            }
        }

        // Then newest-first, by band rather than by exact age.
        //
        // Interleaving alone put a three-year-old lesson third in the feed, between two items from
        // the last hour: every hub is newest-first internally, but they are not newest-first
        // against each other, and an evergreen hub's best video is old. Sorting strictly by age
        // instead would hand the whole screen to news, which republishes every few minutes.
        //
        // Banding gets both. Videos are grouped by roughly how old they are, the bands run newest
        // first, and the round-robin order survives *within* each band — so the top of the feed is
        // genuinely recent and still drawn from every category.
        //
        // A live stream is happening now, so it sorts as new. Something with no date at all sorts
        // last, because an unknown age is not evidence of freshness.
        let band = |video: &VideoSummary| -> u8 {
            if video.live_status == LiveStatus::Live {
                return 0;
            }
            match video
                .published_text
                .as_deref()
                .and_then(map::approximate_age_ms)
            {
                Some(age) if age < 3_600_000 => 0,           // the last hour
                Some(age) if age < 86_400_000 => 1,          // today
                Some(age) if age < 7 * 86_400_000 => 2,      // this week
                Some(age) if age < 2_629_800_000 => 3,       // this month
                Some(age) if age < 31_557_600_000 => 4,      // this year
                Some(_) => 5,
                None => 6,
            }
        };
        // Stable, so the interleaving within each band is preserved exactly.
        drawn.sort_by_key(band);

        let items: Vec<SearchItem> = drawn.into_iter().map(SearchItem::Video).collect();

        Ok(Page {
            items,
            continuation: None,
            total_estimate: None,
        })
    }

    /// One Explore category, read from the hub the site links to for it.
    ///
    /// The site's Explore entries are channels — editorial hubs whose front page collects videos
    /// from across the service — so the whole of this is a browse of a channel's home tab. That is
    /// also why the content matches the site exactly: it is the same page, read directly.
    ///
    /// These hubs have not moved to the lockup format the channel grids now use. Measured live:
    /// the News hub returns **183 `videoRenderer` and zero lockups**. `Music` is the exception and
    /// sends 104 lockups, so both readers are run and whichever finds cards wins. Running both is
    /// cheaper than deciding per category and, more to the point, it keeps working when a category
    /// migrates — which is exactly the change that broke the channel grids.
    ///
    /// Trending, Movies & TV and Podcasts are not members of [`ExploreCategory`] at all; the
    /// reasons are recorded there. Nothing here has to refuse them, because nothing can ask.
    ///
    /// Pagination is not claimed: the hubs paginate by continuation and nothing here can resume
    /// from one yet, so the page reports an honest end of list.
    async fn explore(
        &self,
        category: ExploreCategory,
        _continuation: Option<&ContinuationToken>,
        cancel: &CancellationToken,
    ) -> ProviderResult<Page<VideoSummary>> {
        // The hub behind each category, as the site's own Explore links point at them. Opaque
        // identifiers, and deliberately kept next to the operation that uses them rather than in
        // the model: which page a category reads is provider knowledge, not vocabulary.
        let browse_id = match category {
            ExploreCategory::Music => "UC-9-kyTW8ZkZNDHQJ6FgpwQ",
            ExploreCategory::Gaming => "UCOpNcN46UbXVtpKMrmU4Abg",
            ExploreCategory::Live => "UC4R8DWoMoI7CAwX8_LjQHig",
            ExploreCategory::News => "UCYfdidRxbB8Qhf0Nx7ioOYw",
            ExploreCategory::Sport => "UCEgdi0XIXXZ-qJOFPf4JSKw",
            ExploreCategory::Learning => "UCtFRv9O2AHqOZjjynzrv-xg",
            ExploreCategory::Fashion => "UCrpQ4p1Ql_hG8rKXIKM1MOQ",
        };

        let client = self.query().await;
        // No `params`: the home tab is what carries a hub's collected videos, and six of the seven
        // have no Videos tab at all — asking for one answers `NotFound`.
        let body = serde_json::json!({ "browseId": browse_id });

        let json = Self::with_cancellation(cancel, async move {
            client
                .raw(rustypipe::client::ClientType::Desktop, "browse", &body)
                .await
                .map_err(|error| classify(&error, "explore"))
        })
        .await?;

        let mut items = map::videos_from_renderers(&json);
        if items.is_empty() {
            items = map::videos_from_json(&json);
        }

        Ok(Page {
            items,
            continuation: None,
            total_estimate: None,
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

    /// A minimal summary; only the id and the short flag matter to the splice.
    fn video(id: &str, is_short: bool) -> VideoSummary {
        let mut summary = VideoSummary::placeholder(VideoId::new(id).expect("a valid id"), id);
        summary.is_short = is_short;
        summary
    }

    fn ids(items: &[SearchItem]) -> Vec<String> {
        items
            .iter()
            .filter_map(|item| item.as_video().map(|video| video.id.as_str().to_owned()))
            .collect()
    }

    #[test]
    fn recovered_shorts_land_after_the_first_few_results() {
        let mut mapped: Vec<SearchItem> = (0..8)
            .map(|i| SearchItem::Video(video(&format!("longvideo{i:03}"), false)))
            .collect();

        splice_shorts(&mut mapped, vec![video("shortaaaaaa", true)], SearchResultKind::All);

        let order = ids(&mapped);
        assert_eq!(order.len(), 9, "nothing is dropped by the splice");
        assert_eq!(
            order[SHORTS_INSERT_AT], "shortaaaaaa",
            "the short sits where the site puts its shelf"
        );
        assert_eq!(order[0], "longvideo000", "the leading results keep their place");
    }

    #[test]
    fn a_short_already_in_the_page_is_not_repeated() {
        let mut mapped = vec![SearchItem::Video(video("shortaaaaaa", true))];

        splice_shorts(
            &mut mapped,
            vec![video("shortaaaaaa", true), video("shortbbbbbb", true)],
            SearchResultKind::All,
        );

        assert_eq!(ids(&mapped), vec!["shortaaaaaa", "shortbbbbbb"]);
    }

    #[test]
    fn the_page_admits_only_a_bounded_number_of_shorts() {
        let mut mapped = Vec::new();
        let recovered: Vec<VideoSummary> = (0..30)
            .map(|i| video(&format!("shortvid{i:03}"), true))
            .collect();

        splice_shorts(&mut mapped, recovered, SearchResultKind::All);

        assert_eq!(
            mapped.len(),
            SHORTS_PER_SEARCH,
            "a response carrying 30 shorts must not bury the long-form results"
        );
    }

    #[test]
    fn a_long_form_feed_takes_no_shorts() {
        let mut mapped = vec![SearchItem::Video(video("longvideo000", false))];

        splice_shorts(&mut mapped, vec![video("shortaaaaaa", true)], SearchResultKind::Videos);

        assert_eq!(ids(&mapped), vec!["longvideo000"]);
    }

    #[test]
    fn only_the_providers_own_caption_hosts_are_fetched() {
        assert!(is_caption_host("www.youtube.com"));
        assert!(is_caption_host("youtube.com"));
        assert!(
            !is_caption_host("youtube.com.example.net"),
            "a prefix check would accept a lookalike domain"
        );
        assert!(!is_caption_host("evil.example"));
    }

    #[tokio::test]
    async fn a_caption_url_off_the_allowed_hosts_is_refused_before_any_request() {
        let provider = provider();
        let cancel = CancellationToken::new();
        for url in [
            "https://evil.example/timedtext",
            "http://www.youtube.com/api/timedtext",
            "not a url",
        ] {
            let error = provider
                .caption_cues(url, &cancel)
                .await
                .expect_err("only the provider's own https addresses are fetched");
            assert!(matches!(error, ProviderError::InvalidInput { .. }));
        }
    }

    #[test]
    fn long_form_counts_everything_that_is_not_a_short() {
        let items = vec![
            SearchItem::Video(video("longvideo000", false)),
            SearchItem::Video(video("shortaaaaaa", true)),
            SearchItem::Video(video("longvideo001", false)),
        ];
        assert_eq!(long_form(&items), 2, "a page of shorts is still a thin page");
        assert_eq!(long_form(&[]), 0);
    }

    #[test]
    fn nothing_recovered_leaves_the_page_untouched() {
        let mut mapped = vec![SearchItem::Video(video("longvideo000", false))];

        splice_shorts(&mut mapped, Vec::new(), SearchResultKind::All);

        assert_eq!(ids(&mapped), vec!["longvideo000"]);
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

        // `discovery_feed` is deliberately absent from this list. It used to be refused and is
        // now served from the category hubs, which is exactly the kind of change this test exists
        // to notice: an operation that starts working must stop being listed here.
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
            capabilities.discovery_feed,
            "discovery_feed() is served from the category hubs"
        );
        assert!(
            capabilities.explore,
            "seven categories are offered, each measured returning videos"
        );
        assert!(
            capabilities.captions,
            "caption tracks are read from the player response"
        );
        assert!(
            capabilities.channel_shorts,
            "the Shorts tab is served from the raw browse response"
        );
    }

    #[test]
    fn the_shorts_tab_is_not_routed_through_the_typed_parser() {
        // The typed parser returns an empty list for a populated Shorts tab, so `channel_content`
        // must divert that tab before it reaches the tab endpoint. Reaching it would mean the tab
        // is offered, is not refused, and is silently empty — the worst of the three outcomes.
        let provider = provider();
        let id = ChannelId::new("UCX6OQ3DkcsbYNE6H8uQQuVA").unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();

        // A cancelled token stops the request before any network work, so what this observes is
        // purely which branch was taken: the raw path reports `Cancelled`, and the typed path
        // would have reported `Unsupported` from the match arm.
        let error = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(provider.channel_content(&id, ChannelTab::Shorts, None, &cancel))
            .expect_err("a cancelled request cannot succeed");
        assert!(
            matches!(error, ProviderError::Cancelled),
            "expected the raw shorts path, got {error:?}"
        );
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
