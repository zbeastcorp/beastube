//! Measures how much of a video card each surface actually fills in.
//!
//! ```text
//! cargo run -p beastube-provider-youtube --example card_metadata
//! ```
//!
//! ## Why this exists
//!
//! A card shows a title, a duration, a view count and an age. The channel page was observed
//! showing only the title — no "2.1M views • 3 days ago", no duration badge — for every video on
//! it, while search cards were complete. That is a field-level failure rather than a request-level
//! one: items arrive, so nothing errors, and the page just looks impoverished. Counting the fields
//! that survive on every card-producing surface is the only way to see it.
//!
//! Channel tabs are read from `available_tabs` rather than hard-coded, because asking for a tab a
//! channel does not have does not fail — the Live tab of a channel with no Live tab returns that
//! channel's ordinary uploads, which would score well here and mean nothing.

use beastube_core::ids::{ChannelId, VideoId};
use beastube_core::model::search::SearchFilters;
use beastube_core::model::video::VideoSummary;
use beastube_provider::traits::{ChannelProvider, SearchProvider, VideoProvider};
use beastube_provider_youtube::YouTubeProvider;
use tokio_util::sync::CancellationToken;

/// Channels chosen to differ in which tabs they have.
const CHANNELS: &[(&str, &str)] = &[
    ("UCX6OQ3DkcsbYNE6H8uQQuVA", "MrBeast"),
    ("UC_x5XG1OV2P6uZZ5FSM9Ttw", "Google for Developers"),
];

/// Reports how many of `items` carry each field.
fn tally(label: &str, items: &[VideoSummary]) {
    if items.is_empty() {
        println!("  {label:<34} (no items)");
        return;
    }
    let total = items.len();
    let count = |f: fn(&VideoSummary) -> bool| items.iter().filter(|v| f(v)).count();
    println!(
        "  {label:<34} n={total:<3} duration {:>3}  views {:>3}  date {:>3}  date_txt {:>3}  channel {:>3}",
        count(|v| v.duration_ms.is_some()),
        count(|v| v.view_count.is_some()),
        count(|v| v.published_at.is_some()),
        count(|v| v.published_text.is_some()),
        count(|v| v.channel_name.is_some()),
    );
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let cache = tempfile::tempdir().expect("a cache directory");
    let provider = YouTubeProvider::new(cache.path()).expect("a provider");
    let cancel = CancellationToken::new();

    println!("Fields present per surface (higher is better; n is the sample size):\n");

    if let Ok(page) = provider
        .search("rust programming", &SearchFilters::default(), None, &cancel)
        .await
    {
        let videos: Vec<_> = page
            .page
            .items
            .into_iter()
            .filter_map(|item| match item {
                beastube_core::model::SearchItem::Video(video) => Some(video),
                _ => None,
            })
            .collect();
        let (shorts, long): (Vec<_>, Vec<_>) = videos.into_iter().partition(|v| v.is_short);
        tally("search (long-form)", &long);
        tally("search (shorts)", &shorts);
    }

    let id = VideoId::new("dQw4w9WgXcQ").expect("a video id");
    if let Ok(page) = provider.related(&id, &cancel).await {
        tally("related", &page.items);
    }

    for (raw, label) in CHANNELS {
        let channel = ChannelId::new(*raw).expect("a channel id");
        let Ok(details) = provider.channel(&channel, &cancel).await else {
            println!("  {label}: channel lookup failed");
            continue;
        };
        for tab in &details.available_tabs {
            if let Ok(page) = provider
                .channel_content(&channel, *tab, None, &cancel)
                .await
            {
                tally(&format!("{label} / {}", tab.as_str()), &page.items);
            }
        }
    }
}
