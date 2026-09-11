//! What the channel page will actually receive, straight from the adapter.
//!
//! ```text
//! cargo run -p beastube-provider-youtube --example channel_page
//! ```
//!
//! The earlier probes measured the endpoints; this measures the fix on top of them. It prints the
//! `ChannelDetails` the surface is handed, plus the item count for every tab that channel claims to
//! have — so a tab that is offered but empty shows up here rather than in front of a viewer.
//!
//! Pass `--json` to emit the first channel's payload exactly as the surface receives it, which is
//! what makes it possible to lay the page out against real data without the desktop shell.

use beastube_core::ids::ChannelId;
use beastube_core::model::channel::ChannelTab;
use beastube_provider::traits::ChannelProvider;
use beastube_provider_youtube::YouTubeProvider;
use tokio_util::sync::CancellationToken;

const CHANNELS: &[(&str, &str)] = &[
    ("UCX6OQ3DkcsbYNE6H8uQQuVA", "MrBeast"),
    ("UCuAXFkgsw1L7xaCfnd5JJOw", "Rick Astley (no banner)"),
    ("UC_x5XG1OV2P6uZZ5FSM9Ttw", "Google for Developers"),
];

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let cache = tempfile::tempdir().expect("a cache directory");
    let provider = YouTubeProvider::new(cache.path()).expect("a provider");
    let cancel = CancellationToken::new();

    if std::env::args().any(|argument| argument == "--json") {
        let (id, _) = CHANNELS[0];
        let id = ChannelId::new(id).expect("a channel id");
        let details = provider.channel(&id, &cancel).await.expect("channel details");
        let videos = provider
            .channel_content(&id, ChannelTab::Videos, None, &cancel)
            .await
            .expect("the videos tab");
        let shorts = provider
            .channel_content(&id, ChannelTab::Shorts, None, &cancel)
            .await
            .expect("the shorts tab");
        let payload = serde_json::json!({
            "details": details,
            "videos": videos.items,
            "shorts": shorts.items,
        });
        println!("{}", serde_json::to_string_pretty(&payload).expect("serialisable"));
        return;
    }

    for (id, label) in CHANNELS {
        let id = ChannelId::new(*id).expect("a channel id");
        println!("\n=== {label} ===");

        let details = match provider.channel(&id, &cancel).await {
            Ok(details) => details,
            Err(error) => {
                println!("  channel FAILED: {error}");
                continue;
            }
        };

        println!("  name          {}", details.summary.name);
        println!("  handle        {:?}", details.summary.display_handle());
        println!("  verified      {}", details.summary.is_verified);
        println!("  subscribers   {:?}", details.summary.subscriber_count);
        println!("  videos        {:?}", details.video_count);
        println!("  views         {:?}", details.view_count);
        println!(
            "  joined        {:?}",
            details
                .joined_at
                .map(beastube_core::time_util::Timestamp::as_millis)
        );
        println!("  country       {:?}", details.country);
        println!(
            "  avatar        {} renditions",
            details.summary.avatar.len()
        );
        println!("  banner        {} renditions", details.banner.len());
        println!(
            "  description   {} chars",
            details.description.as_deref().unwrap_or_default().len()
        );
        println!("  canonical     {:?}", details.canonical_url);
        println!("  links         {}", details.links.len());
        for link in details.links.iter().take(3) {
            println!("                {} -> {}", link.title, link.url);
        }
        println!("  tabs          {:?}", details.available_tabs);

        for tab in &details.available_tabs {
            match provider.channel_content(&id, *tab, None, &cancel).await {
                Ok(page) => {
                    let verdict = if page.items.is_empty() {
                        "  <-- EMPTY TAB"
                    } else {
                        ""
                    };
                    println!(
                        "  tab {:<10} {} items{verdict}",
                        tab.as_str(),
                        page.items.len()
                    );
                }
                Err(error) => println!("  tab {:<10} FAILED: {error}", tab.as_str()),
            }
        }
    }
}
