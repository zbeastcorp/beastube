//! Connectivity probe for the YouTube metadata paths.
//!
//! Run manually — it makes real network requests, so it is an example rather than a test:
//!
//! ```text
//! cargo run -p beastube-provider-youtube --example probe
//! ```
//!
//! Its job is to answer one question before any adapter is built on top: **which metadata
//! operations actually work today?** The research dossier says the player path is broken and the
//! trending browse id was retired; this reports the truth for this machine, on this day, rather
//! than assuming either way.
//!
//! Nothing here mints tokens, solves challenges, or touches playback. It exercises the read-only
//! metadata surface only.

use std::time::{Duration, Instant};

use rustypipe::client::RustyPipe;
use rustypipe::model::YouTubeItem;

/// One probe result, formatted for the console.
struct Outcome {
    name: &'static str,
    elapsed: Duration,
    detail: Result<String, String>,
}

impl Outcome {
    fn print(&self) {
        let millis = self.elapsed.as_millis();
        match &self.detail {
            Ok(summary) => println!("  OK    {:<22} {millis:>5}ms  {summary}", self.name),
            Err(error) => {
                let compact: String = error.chars().take(160).collect();
                println!("  FAIL  {:<22} {millis:>5}ms  {compact}", self.name);
            }
        }
    }

    fn ok(&self) -> bool {
        self.detail.is_ok()
    }
}

async fn probe<F, Fut>(name: &'static str, operation: F) -> Outcome
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let started = Instant::now();
    let detail = operation().await;
    Outcome {
        name,
        elapsed: started.elapsed(),
        detail,
    }
}


/// Probes for short-form listings, which are the weakest part of this extractor.
///
/// Split out of `main` so each half stays readable; they are otherwise ordinary probes.
async fn shorts_probes(client: &RustyPipe) -> Vec<Outcome> {
    let mut outcomes = Vec::new();

    // The decisive question for the Shorts tab: are the shorts actually present in the response
    // this extractor already receives, and simply discarded by its typed parser?
    outcomes.push(
        probe("raw shorts in search json", || async {
            let json = client
                .query()
                .raw(
                    rustypipe::client::ClientType::Desktop,
                    "search",
                    &serde_json::json!({ "query": "funny #shorts" }),
                )
                .await
                .map_err(|error| error.to_string())?;

            let lockups = json.matches("shortsLockupViewModel").count();
            let reels = json.matches("reelItemRenderer").count();
            let shelves = json.matches("reelShelfRenderer").count();
            let videos = json.matches("videoRenderer").count();
            Ok(format!(
                "{} bytes | lockups={lockups} reels={reels} shelves={shelves} videoRenderer={videos}",
                json.len()
            ))
        })
        .await,
    );


    // The Shorts tab depends on this far more than on search: a channel's Shorts tab is a real
    // listing, whereas shorts in search results arrive inside a shelf the extractor mostly drops.
    outcomes.push(
        probe("channel shorts tab", || async {
            client
                .query()
                .channel_videos_tab(
                    "UCuAXFkgsw1L7xaCfnd5JJOw",
                    rustypipe::param::ChannelVideoTab::Shorts,
                )
                .await
                .map(|channel| {
                    let shorts = channel.content.items.iter().filter(|v| v.is_short).count();
                    format!(
                        "{} items, {shorts} marked short, more: {}",
                        channel.content.items.len(),
                        channel.content.ctoken.is_some()
                    )
                })
                .map_err(|error| error.to_string())
        })
        .await,
    );

    // The decisive experiment for the Shorts tab: the Shorts tab answers with zero items and a
    // continuation token, which is what a lazily-deferred rich grid looks like. If following the
    // token yields items, a channel's Shorts tab is a real short-form listing and the feed can be
    // built on it instead of on search, which drops shorts inside a shelf renderer this extractor
    // does not parse.
    outcomes.push(
        probe("channel shorts continuation", || async {
            let first = client
                .query()
                .channel_videos_tab(
                    "UCuAXFkgsw1L7xaCfnd5JJOw",
                    rustypipe::param::ChannelVideoTab::Shorts,
                )
                .await
                .map_err(|error| error.to_string())?;

            let Some(ctoken) = first.content.ctoken.clone() else {
                return Err(format!(
                    "page 1 had {} items and no continuation",
                    first.content.items.len()
                ));
            };

            let second = client
                .query()
                .continuation::<rustypipe::model::VideoItem, _>(
                    ctoken,
                    rustypipe::model::paginator::ContinuationEndpoint::Browse,
                    first.content.visitor_data.as_deref(),
                )
                .await
                .map_err(|error| error.to_string())?;

            let shorts = second.items.iter().filter(|v| v.is_short).count();
            Ok(format!(
                "page 1: {} items, page 2: {} items ({shorts} short), more: {}",
                first.content.items.len(),
                second.items.len(),
                second.ctoken.is_some()
            ))
        })
        .await,
    );

    outcomes
}

#[tokio::main]
async fn main() {
    // A dedicated cache directory so the probe never writes into the process working directory,
    // which for an installed application would be Program Files.
    let storage = std::env::temp_dir().join("beastube-probe");
    let _ = std::fs::create_dir_all(&storage);

    let client = match RustyPipe::builder().storage_dir(&storage).build() {
        Ok(client) => client,
        Err(error) => {
            eprintln!("could not construct the client: {error}");
            std::process::exit(1);
        }
    };

    println!("BEASTUBE metadata probe");
    println!("storage: {}", storage.display());
    println!();

    let mut outcomes = Vec::new();

    outcomes.push(
        probe("search", || async {
            client
                .query()
                .search::<YouTubeItem, _>("rust programming")
                .await
                .map(|results| {
                    let first = results
                        .items
                        .items
                        .first()
                        .map_or_else(|| "<none>".to_owned(), |item| format!("{item:?}"));
                    format!(
                        "{} items; first: {}",
                        results.items.items.len(),
                        first.chars().take(70).collect::<String>()
                    )
                })
                .map_err(|error| error.to_string())
        })
        .await,
    );

    outcomes.push(
        probe("search_suggestion", || async {
            client
                .query()
                .search_suggestion("rust prog")
                .await
                .map(|suggestions| format!("{} suggestions", suggestions.len()))
                .map_err(|error| error.to_string())
        })
        .await,
    );

    outcomes.push(
        probe("video_details", || async {
            client
                .query()
                .video_details("dQw4w9WgXcQ")
                .await
                .map(|video| format!("{:?} by {:?}", video.name, video.channel.name))
                .map_err(|error| error.to_string())
        })
        .await,
    );

    outcomes.push(
        probe("channel_videos", || async {
            client
                .query()
                .channel_videos("UCuAXFkgsw1L7xaCfnd5JJOw")
                .await
                .map(|channel| {
                    format!("{:?}: {} videos", channel.name, channel.content.items.len())
                })
                .map_err(|error| error.to_string())
        })
        .await,
    );

    outcomes.push(
        probe("playlist", || async {
            client
                .query()
                .playlist("UUuAXFkgsw1L7xaCfnd5JJOw")
                .await
                .map(|playlist| {
                    format!(
                        "{:?}: {} videos",
                        playlist.name,
                        playlist.videos.items.len()
                    )
                })
                .map_err(|error| error.to_string())
        })
        .await,
    );

    outcomes.extend(shorts_probes(&client).await);

    // Paging depends on this: without a working continuation endpoint a feed cannot scroll and the
    // Shorts tab ends after whatever the first page happened to contain.
    outcomes.push(
        probe("search page 2", || async {
            let first = client
                .query()
                .search::<YouTubeItem, _>("lofi hip hop")
                .await
                .map_err(|error| error.to_string())?;
            let Some(ctoken) = first.items.ctoken.clone() else {
                return Err("the first page reported no continuation".to_owned());
            };
            let second = client
                .query()
                .continuation::<YouTubeItem, _>(
                    ctoken,
                    rustypipe::model::paginator::ContinuationEndpoint::Search,
                    None,
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "page 1: {} items, page 2: {} items, more: {}",
                first.items.items.len(),
                second.items.len(),
                second.ctoken.is_some()
            ))
        })
        .await,
    );

    // The home feed depends on this: without it there is no login-free discovery surface and the
    // first screen can only be built from the local library.
    outcomes.push(
        probe("trending", || async {
            client
                .query()
                .trending()
                .await
                .map(|videos| {
                    let shorts = videos.iter().filter(|video| video.is_short).count();
                    format!("{} videos ({shorts} shorts)", videos.len())
                })
                .map_err(|error| error.to_string())
        })
        .await,
    );

    // The Shorts tab depends on this: a search that returns items marked `is_short` is the only
    // login-free way to fill it.
    // What actually distinguishes a short in this data. Prints the three candidate signals side by
    // side so the classifier is chosen from evidence rather than from a guess.
    outcomes.push(
        probe("shorts signal survey", || async {
            let results = client
                .query()
                .search::<rustypipe::model::VideoItem, _>("funny #shorts")
                .await
                .map_err(|error| error.to_string())?;
            let mut lines = Vec::new();
            for video in results.items.items.iter().take(12) {
                let thumb = video
                    .thumbnail
                    .iter()
                    .max_by_key(|t| t.width)
                    .map_or("none".to_owned(), |t| format!("{}x{}", t.width, t.height));
                lines.push(format!(
                    "short={} dur={:?} thumb={} | {}",
                    video.is_short,
                    video.duration,
                    thumb,
                    video.name.chars().take(38).collect::<String>()
                ));
            }
            Ok(format!("
      {}", lines.join("
      ")))
        })
        .await,
    );

    outcomes.push(
        probe("shorts via search", || async {
            client
                .query()
                .search::<rustypipe::model::VideoItem, _>("#shorts")
                .await
                .map(|results| {
                    let shorts = results
                        .items
                        .items
                        .iter()
                        .filter(|video| video.is_short)
                        .count();
                    format!("{} videos, {shorts} marked short", results.items.items.len())
                })
                .map_err(|error| error.to_string())
        })
        .await,
    );

    // The one the dossier expects to fail. Reported, not relied on.
    outcomes.push(
        probe("player (streams)", || async {
            client
                .query()
                .player("dQw4w9WgXcQ")
                .await
                .map(|player| {
                    format!(
                        "{} video / {} audio streams",
                        player.video_streams.len(),
                        player.audio_streams.len()
                    )
                })
                .map_err(|error| error.to_string())
        })
        .await,
    );

    for outcome in &outcomes {
        outcome.print();
    }

    let working = outcomes.iter().filter(|outcome| outcome.ok()).count();
    println!();
    println!("{working}/{} operations succeeded", outcomes.len());
}
