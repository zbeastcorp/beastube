//! Measures what every Explore category actually returns through the adapter.
//!
//! ```text
//! cargo run -p beastube-provider-youtube --example explore_content
//! ```
//!
//! A category that returns nothing is one the surface must not offer, so this is the check that
//! decides membership of [`ExploreCategory`]. It also counts how complete the cards are, because a
//! category that returns titles with no view counts or ages looks broken next to the rest of the
//! application even though nothing errored.

use beastube_core::model::explore::ExploreCategory;
use beastube_provider::traits::MetadataProvider;
use beastube_provider_youtube::YouTubeProvider;
use tokio_util::sync::CancellationToken;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let cache = tempfile::tempdir().expect("a cache directory");
    let provider = YouTubeProvider::new(cache.path()).expect("a provider");
    let cancel = CancellationToken::new();

    // `--json` emits every category's payload exactly as the surface receives it, so the section
    // can be laid out against real content without the desktop shell.
    if std::env::args().any(|argument| argument == "--json") {
        let mut out = serde_json::Map::new();
        for category in ExploreCategory::ALL {
            let items = provider
                .explore(category, None, &cancel)
                .await
                .map(|page| page.items)
                .unwrap_or_default();
            out.insert(
                category.as_str().to_owned(),
                serde_json::to_value(items).expect("serialisable"),
            );
        }
        println!(
            "{}",
            serde_json::to_string(&serde_json::Value::Object(out)).expect("serialisable")
        );
        return;
    }

    println!("Explore categories, as the surface receives them:\n");

    for category in ExploreCategory::ALL {
        match provider.explore(category, None, &cancel).await {
            Err(error) => println!("  {:<10} FAILED: {error}", category.as_str()),
            Ok(page) => {
                let n = page.items.len();
                if n == 0 {
                    println!("  {:<10} EMPTY — must not be offered", category.as_str());
                    continue;
                }
                let with = |f: fn(&beastube_core::model::video::VideoSummary) -> bool| {
                    page.items.iter().filter(|v| f(v)).count()
                };
                println!(
                    "  {:<10} {n:>3} videos  duration {:>3}  views {:>3}  date {:>3}  channel {:>3}  live {:>2}",
                    category.as_str(),
                    with(|v| v.duration_ms.is_some()),
                    with(|v| v.view_count.is_some()),
                    with(|v| v.published_text.is_some()),
                    with(|v| v.channel_name.is_some()),
                    with(|v| v.live_status == beastube_core::model::video::LiveStatus::Live),
                );
                for video in page.items.iter().take(2) {
                    println!(
                        "      {:?} — {:?} — {:?} views",
                        video.title.chars().take(46).collect::<String>(),
                        video.channel_name.as_deref().unwrap_or("?"),
                        video.view_count,
                    );
                }
            }
        }
    }
}
