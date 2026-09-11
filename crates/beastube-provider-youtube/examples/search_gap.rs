//! Measures how many search results the typed extractor drops.
//!
//! Run manually — it makes real network requests, so it is an example rather than a test:
//!
//! ```text
//! cargo run -p beastube-provider-youtube --example search_gap
//! ```
//!
//! ## Why this exists
//!
//! Search in the application behaves as though it needs the exact title of a video, where the site
//! returns loosely related results for the same words. [`map::shorts_from_json`] already
//! documents the shape of that problem for short-form results — 26 lockups in the response and
//! zero in the parsed result — and this asks whether the same thing is happening to ordinary
//! videos.
//!
//! For each query it reports what the typed parser returned against what the raw response actually
//! contained, counted by renderer key. A large gap means the results are being discarded on our
//! side rather than never sent.

use rustypipe::client::{ClientType, RustyPipe};
use rustypipe::model::YouTubeItem;

/// Depth-first tally of every object key that names a renderer or a view model.
///
/// Counting a fixed list only answers questions already thought of; YouTube moves results between
/// renderer types, so this reports what the response actually contains.
fn tally_renderers(value: &serde_json::Value, out: &mut std::collections::BTreeMap<String, usize>) {
    match value {
        serde_json::Value::Object(map) => {
            for (name, child) in map {
                if name.ends_with("Renderer") || name.ends_with("ViewModel") {
                    *out.entry(name.clone()).or_default() += 1;
                }
                tally_renderers(child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                tally_renderers(child, out);
            }
        }
        _ => {}
    }
}

/// Depth-first walk counting every occurrence of `key`.
fn count_key(value: &serde_json::Value, key: &str) -> usize {
    match value {
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(name, child)| {
                usize::from(name == key) + if name == key { 0 } else { count_key(child, key) }
            })
            .sum(),
        serde_json::Value::Array(items) => items.iter().map(|c| count_key(c, key)).sum(),
        _ => 0,
    }
}

/// The renderer keys a search response can carry a video in.
const KEYS: &[&str] = &[
    "videoRenderer",
    "lockupViewModel",
    "shortsLockupViewModel",
    "compactVideoRenderer",
    "playlistRenderer",
    "channelRenderer",
    "reelItemRenderer",
];

#[tokio::main]
async fn main() {
    let storage = std::env::temp_dir().join("beastube-probe");
    let _ = std::fs::create_dir_all(&storage);

    let client = match RustyPipe::builder().storage_dir(&storage).build() {
        Ok(client) => client,
        Err(error) => {
            eprintln!("could not construct the client: {error}");
            std::process::exit(1);
        }
    };

    // A deliberate spread: an exact title, a loose topic, a vague phrase, and a misspelling —
    // the last three are the shapes the application is reported to handle badly. Override them
    // by passing queries as arguments.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let queries: Vec<String> = if args.is_empty() {
        [
            "Rick Astley Never Gonna Give You Up",
            "lofi hip hop",
            "how to cook rice",
            "funny cat",
            "rust programing tutoral",
        ]
        .iter()
        .map(|q| (*q).to_owned())
        .collect()
    } else {
        args
    };

    println!("query                                    typed   raw renderers");
    println!("{}", "-".repeat(96));

    for query in &queries {
        let query = query.as_str();
        let typed = client.query().search::<YouTubeItem, _>(query).await;

        let (typed_count, corrected) = match &typed {
            Ok(results) => (
                Some(results.items.items.len()),
                results.corrected_query.clone(),
            ),
            Err(_) => (None, None),
        };
        let typed_count = typed_count.map_or_else(|| "err".to_owned(), |n| n.to_string());

        let raw = client
            .query()
            .raw(ClientType::Desktop, "search", &serde_json::json!({ "query": query }))
            .await;

        let counts = match &raw {
            Ok(body) => serde_json::from_str::<serde_json::Value>(body).map_or_else(
                |error| format!("<unparseable: {error}>"),
                |root| {
                    KEYS.iter()
                        .map(|key| (*key, count_key(&root, key)))
                        .filter(|(_, n)| *n > 0)
                        .map(|(key, n)| format!("{key}={n}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                },
            ),
            Err(error) => format!("<raw failed: {error}>"),
        };

        println!("{query:<40} {typed_count:>5}   {counts}");

        if let Ok(body) = &raw
            && let Ok(root) = serde_json::from_str::<serde_json::Value>(body)
        {
            let mut all = std::collections::BTreeMap::new();
            tally_renderers(&root, &mut all);
            let mut ranked: Vec<(String, usize)> = all.into_iter().collect();
            ranked.sort_by_key(|entry| std::cmp::Reverse(entry.1));
            let top: Vec<String> = ranked
                .iter()
                .take(12)
                .map(|(k, n)| format!("{k}={n}"))
                .collect();
            println!("{:>46}all renderers: {}", "", top.join(" "));
        }
        if let Some(correction) = corrected {
            println!("{:>46}corrected_query: {correction}", "");
        }
        if let Err(error) = &typed {
            println!("{:>46}typed error: {error}", "");
        }
    }

    println!();
    println!("`typed` is what search() returns and the UI renders.");
    println!("A raw count far above it means the response carried results we threw away.");
}
