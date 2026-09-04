//! Dumps the shape of the watch page's recommendation shelf.
//!
//! Kept as a tool rather than deleted, for the same reason `probe` is: this shelf is parsed out of
//! a raw response (`map::related_from_next`), so when YouTube changes its renderer the parser goes
//! quiet rather than loud, and the first question is always "what does the response look like now?"
//! This answers it in one run.
//!
//! ```text
//! cargo run -p beastube-provider-youtube --example related_shape
//! ```
//!
//! It made the original diagnosis: the typed extractor returned twenty related items with no
//! channel, no view count and no date, because the shelf had moved to `lockupViewModel` — and the
//! metadata parts turned out to read `["MrBeast 2", "24M", "1d ago"]`, a bare count with no "views"
//! word, which is what the parser had to be written against.

use std::collections::BTreeMap;

use rustypipe::client::{ClientType, RustyPipe};

/// Walks the tree and counts every `somethingRenderer` / `somethingViewModel` key it finds.
fn count_renderers(value: &serde_json::Value, counts: &mut BTreeMap<String, usize>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key.ends_with("Renderer") || key.ends_with("ViewModel") {
                    *counts.entry(key.clone()).or_default() += 1;
                }
                count_renderers(child, counts);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                count_renderers(item, counts);
            }
        }
        _ => {}
    }
}

/// Depth-first collect of every value stored under `key`.
fn collect<'a>(value: &'a serde_json::Value, key: &str, out: &mut Vec<&'a serde_json::Value>) {
    match value {
        serde_json::Value::Object(map) => {
            for (name, child) in map {
                if name == key {
                    out.push(child);
                } else {
                    collect(child, key, out);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect(child, key, out);
            }
        }
        _ => {}
    }
}

/// Every non-blank `content` string under `value`.
fn contents(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (name, child) in map {
                if name == "content"
                    && let Some(text) = child.as_str()
                {
                    if !text.trim().is_empty() {
                        out.push(text.to_owned());
                    }
                    continue;
                }
                contents(child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                contents(child, out);
            }
        }
        _ => {}
    }
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join("beastube-related-shape");
    let client = RustyPipe::builder()
        .storage_dir(&dir)
        .no_reporter()
        .build()
        .expect("client");

    let body = serde_json::json!({ "videoId": "dQw4w9WgXcQ" });
    let raw = client
        .query()
        .raw(ClientType::Desktop, "next", &body)
        .await
        .expect("next");
    let json: serde_json::Value = serde_json::from_str(&raw).expect("json");

    let mut counts = BTreeMap::new();
    count_renderers(&json, &mut counts);
    println!("renderers in the `next` response (more than one of each):");
    for (name, n) in &counts {
        if *n > 1 {
            println!("  {n:>4}  {name}");
        }
    }

    let mut lockups = Vec::new();
    collect(&json, "lockupViewModel", &mut lockups);
    println!("\ntext parts per lockup, which is what the parser classifies:");
    for lockup in lockups.iter().take(8) {
        let mut parts = Vec::new();
        if let Some(rows) = lockup.pointer(
            "/metadata/lockupMetadataViewModel/metadata/contentMetadataViewModel/metadataRows",
        ) {
            contents(rows, &mut parts);
        }
        println!("  {parts:?}");
    }
}
