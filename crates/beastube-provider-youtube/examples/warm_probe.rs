//! Times the adapter's cold and warm paths, to check the visitor-ID fix did what it claims.
//!
//! ```text
//! cargo run -p beastube-provider-youtube --example warm_probe
//! ```
//!
//! Makes real network requests. Compare against `probe`, which drives the extractor directly and
//! measured a first search at ~3.4 s and a first video at ~2.2 s before the fix.

use std::time::Instant;

use beastube_core::ids::VideoId;
use beastube_core::model::SearchFilters;
use beastube_provider::{SearchProvider, VideoProvider};
use beastube_provider_youtube::YouTubeProvider;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join("beastube-warm-probe");
    let provider = YouTubeProvider::new(&dir).expect("provider");
    let cancel = CancellationToken::new();

    let t = Instant::now();
    provider.warm().await;
    println!("warm (visitor id fetch)   {:>5}ms", t.elapsed().as_millis());

    for label in ["search #1", "search #2"] {
        let t = Instant::now();
        let r = provider
            .search("rust programming", &SearchFilters::default(), None, &cancel)
            .await;
        println!(
            "{label:<26}{:>5}ms  {}",
            t.elapsed().as_millis(),
            r.map_or_else(|e| format!("ERR {e}"), |r| format!("{} items", r.page.items.len()))
        );
    }

    let id = VideoId::new("dQw4w9WgXcQ").unwrap();
    for label in ["video #1", "video #2"] {
        let t = Instant::now();
        let r = provider.video(&id, &cancel).await;
        println!(
            "{label:<26}{:>5}ms  {}",
            t.elapsed().as_millis(),
            r.map_or_else(|e| format!("ERR {e}"), |v| v.summary.title)
        );
    }

    let t = Instant::now();
    let r = provider.related(&id, &cancel).await;
    println!(
        "related                   {:>5}ms  {}",
        t.elapsed().as_millis(),
        r.as_ref().map_or_else(
            |e| format!("ERR {e}"),
            |p| format!("{} items", p.items.len())
        )
    );

    // What a card actually has to draw with. A field that is `None` here is a field the grid
    // cannot show however the card is written.
    println!("
-- fields present on the first few items --");
    if let Ok(page) = &r {
        for item in page.items.iter().take(3) {
            println!(
                "  related: channel={:?} avatar={} verified={} views={:?} published={:?}",
                item.channel_name,
                item.channel_avatar.len(),
                item.channel_verified,
                item.view_count,
                item.published_text
            );
        }
    }
    if let Ok(results) = provider
        .search("minecraft", &SearchFilters::default(), None, &cancel)
        .await
    {
        for item in results.page.items.iter().take(3) {
            if let Some(v) = item.as_video() {
                println!(
                    "  search:  channel={:?} avatar={} verified={} views={:?} published={:?}",
                    v.channel_name,
                    v.channel_avatar.len(),
                    v.channel_verified,
                    v.view_count,
                    v.published_text
                );
            }
        }
    }
}
