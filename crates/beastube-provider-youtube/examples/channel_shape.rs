//! Measures what a channel page can actually be built from.
//!
//! Run manually — it makes real network requests, so it is an example rather than a test:
//!
//! ```text
//! cargo run -p beastube-provider-youtube --example channel_shape
//! ```
//!
//! ## Why this exists
//!
//! The application's channel page shows an avatar, a name and a subscriber count. The site's shows
//! a banner, a handle, a video count, a description, links, tabs for Shorts and Live, and a
//! Latest/Popular/Oldest sort. [`ChannelDetails`] already has fields for most of that, and the
//! adapter fills them with `None` — but "the adapter discards it" and "the service never sends it"
//! are different problems with different fixes, and only one of them is ours.
//!
//! So this asks the service directly, for several channels at once: which of those fields arrive,
//! which tabs report content, whether the sort orders return anything, and whether the playlists
//! tab works. A field that is absent here must not become a tab or a line of text in the UI.

use rustypipe::client::RustyPipe;
use rustypipe::param::{ChannelOrder, ChannelVideoTab};

/// Channels chosen to disagree with each other.
///
/// A page built against one channel tends to assume every channel looks like it. These differ in
/// the ways that matter: whether Shorts exist, whether Live exists, and how much of the About tab
/// the owner filled in.
const CHANNELS: &[(&str, &str)] = &[
    ("UCWv7vMbMWH4-V0ZXdmDpPBA", "Programming with Mosh"),
    ("UCX6OQ3DkcsbYNE6H8uQQuVA", "MrBeast"),
    ("UCuAXFkgsw1L7xaCfnd5JJOw", "Rick Astley"),
    ("UC_x5XG1OV2P6uZZ5FSM9Ttw", "Google for Developers"),
];

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let client = RustyPipe::new();

    for (id, label) in CHANNELS {
        println!("\n=== {label}  ({id}) ===");

        match client.query().channel_videos(*id).await {
            Err(error) => println!("  channel_videos FAILED: {error}"),
            Ok(channel) => {
                println!("  name            {}", channel.name);
                println!("  handle          {:?}", channel.handle);
                println!("  subscribers     {:?}", channel.subscriber_count);
                println!("  video_count     {:?}", channel.video_count);
                println!("  avatar          {} renditions", channel.avatar.len());
                println!(
                    "  banner          {} renditions{}",
                    channel.banner.len(),
                    channel
                        .banner
                        .last()
                        .map(|t| format!("  (widest {}x{})", t.width, t.height))
                        .unwrap_or_default()
                );
                println!("  verification    {:?}", channel.verification);
                println!("  description     {} chars", channel.description.len());
                println!("  tags            {:?}", channel.tags);
                println!("  has_shorts      {}", channel.has_shorts);
                println!("  has_live        {}", channel.has_live);
                println!("  videos on p1    {}", channel.content.items.len());
            }
        }

        match client.query().channel_info(*id).await {
            Err(error) => println!("  channel_info FAILED: {error}"),
            Ok(info) => {
                println!("  about.url       {}", info.url);
                println!("  about.videos    {:?}", info.video_count);
                println!("  about.views     {:?}", info.view_count);
                println!("  about.created   {:?}", info.create_date);
                println!("  about.country   {:?}", info.country);
                println!("  about.links     {:?}", info.links);
            }
        }

        // Sort orders are a separate endpoint built from a continuation token rather than a field
        // on the page, so working tabs do not imply working sorts.
        for tab in [
            ChannelVideoTab::Videos,
            ChannelVideoTab::Shorts,
            ChannelVideoTab::Live,
        ] {
            for order in [
                ChannelOrder::Latest,
                ChannelOrder::Popular,
                ChannelOrder::Oldest,
            ] {
                let result = client
                    .query()
                    .channel_videos_tab_order(*id, tab, order)
                    .await;
                match result {
                    Ok(page) => println!(
                        "  {tab:?}/{order:?}  {} items, continuation {}",
                        page.items.len(),
                        if page.ctoken.is_some() { "yes" } else { "no" }
                    ),
                    Err(error) => {
                        let compact: String = error.to_string().chars().take(90).collect();
                        println!("  {tab:?}/{order:?}  FAILED: {compact}");
                    }
                }
            }
        }

        match client.query().channel_playlists(*id).await {
            Ok(page) => println!("  playlists       {} items", page.content.items.len()),
            Err(error) => {
                let compact: String = error.to_string().chars().take(90).collect();
                println!("  playlists       FAILED: {compact}");
            }
        }
    }
}
