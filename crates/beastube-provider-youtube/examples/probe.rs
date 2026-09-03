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
