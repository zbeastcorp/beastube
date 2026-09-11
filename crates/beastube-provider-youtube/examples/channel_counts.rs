//! Checks whether the channel header's subscriber count is really a subscriber count.
//!
//! ```text
//! cargo run -p beastube-provider-youtube --example channel_counts
//! ```
//!
//! ## Why this exists
//!
//! [`channel_shape`] reported `MrBeast` at 1000 subscribers, Rick Astley at 436 and Google for
//! Developers at 6000 — each of which happens to sit right next to that channel's *video* count
//! from the About tab. Three coincidences is a hypothesis, not a finding, so this reads the raw
//! header strings and prints them beside what the typed parser made of them.
//!
//! It also asks the plain (unordered) tab endpoint whether Shorts and Live actually return items,
//! since the ordered variant failed for every channel and the two are different requests.

use rustypipe::client::{ClientType, RustyPipe};
use rustypipe::param::ChannelVideoTab;

const CHANNELS: &[(&str, &str)] = &[
    ("UCX6OQ3DkcsbYNE6H8uQQuVA", "MrBeast"),
    ("UCuAXFkgsw1L7xaCfnd5JJOw", "Rick Astley"),
    ("UC_x5XG1OV2P6uZZ5FSM9Ttw", "Google for Developers"),
];

/// Every string in the response that looks like a rendered count, with the path it came from.
fn count_strings(value: &serde_json::Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        serde_json::Value::Object(map) => {
            for (name, child) in map {
                count_strings(child, &format!("{path}.{name}"), out);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                count_strings(child, &format!("{path}[{index}]"), out);
            }
        }
        serde_json::Value::String(text) => {
            let lower = text.to_lowercase();
            if lower.contains("subscriber") || lower.contains("video") && text.len() < 40 {
                out.push((path.to_owned(), text.clone()));
            }
        }
        _ => {}
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let client = RustyPipe::new();

    for (id, label) in CHANNELS {
        println!("\n=== {label} ===");

        let raw = client
            .query()
            .raw(
                ClientType::Desktop,
                "browse",
                &serde_json::json!({ "browseId": id }),
            )
            .await;

        match raw {
            Err(error) => println!("  raw browse FAILED: {error}"),
            Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
                Err(error) => println!("  raw browse unparseable: {error}"),
                Ok(json) => {
                    let mut found = Vec::new();
                    count_strings(json.get("header").unwrap_or(&json), "header", &mut found);
                    for (path, text) in found.iter().take(12) {
                        println!("  {text:<28} <- {path}");
                    }
                    if found.is_empty() {
                        println!("  (no count-shaped strings under header)");
                    }
                }
            },
        }

        match client.query().channel_videos(*id).await {
            Ok(channel) => println!(
                "  parsed: subscriber_count={:?} video_count={:?}",
                channel.subscriber_count, channel.video_count
            ),
            Err(error) => println!("  parse FAILED: {error}"),
        }

        match client.query().channel_info(*id).await {
            Ok(info) => println!(
                "  about:  subscriber_count={:?} video_count={:?}",
                info.subscriber_count, info.video_count
            ),
            Err(error) => println!("  about FAILED: {error}"),
        }

        // The ordered endpoint failed everywhere; the plain tab fetch is a different request.
        for tab in [ChannelVideoTab::Shorts, ChannelVideoTab::Live] {
            match client.query().channel_videos_tab(*id, tab).await {
                Ok(channel) => println!(
                    "  tab {tab:?}: {} items, continuation {}",
                    channel.content.items.len(),
                    if channel.content.ctoken.is_some() {
                        "yes"
                    } else {
                        "no"
                    }
                ),
                Err(error) => {
                    let compact: String = error.to_string().chars().take(90).collect();
                    println!("  tab {tab:?}: FAILED: {compact}");
                }
            }
        }
    }
}
