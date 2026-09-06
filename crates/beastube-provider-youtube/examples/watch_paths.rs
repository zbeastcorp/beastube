//! Exercises what the watch page and the Shorts feed ask for, on real ids.
//!
//! ```text
//! cargo run -p beastube-provider-youtube --example watch_paths
//! ```
//!
//! Reproduces "BEASTUBE could not read the response" by calling `details` and `related` the way the
//! views do — including on short-form ids, which the Shorts feed requests one at a time as it
//! scrolls, so an id class that fails there fails repeatedly.

use beastube_core::ids::VideoId;
use beastube_core::model::SearchFilters;
use beastube_provider::traits::{SearchProvider, VideoProvider};
use beastube_provider_youtube::YouTubeProvider;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let storage = std::env::temp_dir().join("beastube-probe");
    let provider = YouTubeProvider::new(&storage).expect("provider constructs");
    let cancel = CancellationToken::new();

    // What the added requests cost. A thin first page pays for the shorts recovery and up to
    // three continuations; a page that was already full pays for neither.
    for query in ["rust programming", "roblox trends"] {
        let started = std::time::Instant::now();
        let count = provider
            .search(query, &SearchFilters::default(), None, &cancel)
            .await
            .map_or(0, |r| r.page.items.len());
        println!(
            "search {query:<20} {:>5}ms  {count} items",
            started.elapsed().as_millis()
        );
    }
    println!();

    // Collect real ids: a well-known long-form video, plus whatever shorts a live search returns.
    let mut targets: Vec<(String, bool)> = vec![("dQw4w9WgXcQ".to_owned(), false)];

    if let Ok(results) = provider
        .search("roblox trends", &SearchFilters::default(), None, &cancel)
        .await
    {
        for item in results.page.items.iter().take(6) {
            if let Some(video) = item.as_video() {
                targets.push((video.id.as_str().to_owned(), video.is_short));
            }
        }
    }

    println!("id             short  details                            related");
    println!("{}", "-".repeat(100));

    for (raw, is_short) in &targets {
        let Ok(id) = VideoId::new(raw.as_str()) else {
            println!("{raw:<14} invalid id");
            continue;
        };

        let details = match provider.video(&id, &cancel).await {
            Ok(video) => format!("ok: {}", video.summary.title.chars().take(26).collect::<String>()),
            Err(error) => format!("FAIL {error}"),
        };
        let related = match provider.related(&id, &cancel).await {
            Ok(page) => format!("ok: {} items", page.items.len()),
            Err(error) => format!("FAIL {error}"),
        };

        println!("{raw:<14} {:<6} {details:<34} {related}", is_short.to_string());
    }
}
