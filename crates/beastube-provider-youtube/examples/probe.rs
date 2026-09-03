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
