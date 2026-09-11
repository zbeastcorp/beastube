//! Measures how recent the home feed's videos actually are.
//!
//! ```text
//! cargo run -p beastube-provider-youtube --example feed_freshness
//! ```
//!
//! A fresh install's home screen used to be built by searching for evergreen words, which returns
//! whatever ranks well for them rather than what was published today. "Our feed should be the
//! latest, like the site's" is a claim about ages, so this counts them: it buckets both the old
//! path and the new one by how long ago each video was published, and prints them side by side.

use std::collections::BTreeMap;

use beastube_core::model::SearchItem;
use beastube_core::model::search::SearchFilters;
use beastube_provider::traits::{MetadataProvider, SearchProvider};
use beastube_provider_youtube::YouTubeProvider;
use tokio_util::sync::CancellationToken;

/// The words the old discovery path searched for.
const TOPICS: &[&str] = &["music", "technology", "science", "cooking"];

/// Buckets a relative age string into something countable.
///
/// Coarse on purpose: the provider publishes prose, and the only question being asked is whether a
/// feed is hours old or years old.
fn bucket(text: &str) -> &'static str {
    let lower = text.to_lowercase();
    if lower.contains("second") || lower.contains("minute") {
        "under an hour"
    } else if lower.contains("hour") {
        "today"
    } else if lower.contains("day") || lower.contains("week") {
        "this month"
    } else if lower.contains("month") {
        "this year"
    } else if lower.contains("year") {
        "over a year"
    } else {
        "no date"
    }
}

fn report(label: &str, ages: &[Option<String>]) {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for age in ages {
        *counts
            .entry(age.as_deref().map_or("no date", bucket))
            .or_default() += 1;
    }
    println!("\n  {label} — {} videos", ages.len());
    for key in [
        "under an hour",
        "today",
        "this month",
        "this year",
        "over a year",
        "no date",
    ] {
        if let Some(count) = counts.get(key) {
            println!("    {key:<16} {count:>3}");
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let cache = tempfile::tempdir().expect("a cache directory");
    let provider = YouTubeProvider::new(cache.path()).expect("a provider");
    let cancel = CancellationToken::new();

    // The old path: search for evergreen words.
    let mut searched = Vec::new();
    for topic in TOPICS {
        if let Ok(results) = provider
            .search(topic, &SearchFilters::default(), None, &cancel)
            .await
        {
            for item in results.page.items {
                if let SearchItem::Video(video) = item {
                    searched.push(video.published_text.clone());
                }
            }
        }
    }
    report("searching evergreen topics (the old home feed)", &searched);

    // The new path: the provider's own category hubs.
    match provider.discovery_feed(None, &cancel).await {
        Err(error) => println!("\n  discovery_feed FAILED: {error}"),
        Ok(page) => {
            let ages: Vec<_> = page
                .items
                .iter()
                .filter_map(|item| match item {
                    SearchItem::Video(video) => Some(video.published_text.clone()),
                    _ => None,
                })
                .collect();
            report("the category hubs (the new home feed)", &ages);

            println!("\n  first twelve, in feed order:");
            for item in page.items.iter().take(12) {
                if let SearchItem::Video(video) = item {
                    println!(
                        "    {:<42} {:>18}  {}",
                        video.title.chars().take(42).collect::<String>(),
                        video.published_text.as_deref().unwrap_or("—"),
                        video.channel_name.as_deref().unwrap_or("?"),
                    );
                }
            }
        }
    }
}
