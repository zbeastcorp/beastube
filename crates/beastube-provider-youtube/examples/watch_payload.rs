//! Dumps one video's details as the watch screen receives them.
//!
//! ```text
//! cargo run -p beastube-provider-youtube --example watch_payload > payload.json
//! ```
//!
//! Exists so the player can be exercised against a real payload without the desktop shell — the
//! surface reads this through its IPC mock, which is how the control bar's behaviour gets tested
//! in a browser at all.

use beastube_core::ids::VideoId;
use beastube_provider::traits::VideoProvider;
use beastube_provider_youtube::YouTubeProvider;
use tokio_util::sync::CancellationToken;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let cache = tempfile::tempdir().expect("a cache directory");
    let provider = YouTubeProvider::new(cache.path()).expect("a provider");
    let cancel = CancellationToken::new();

    let raw = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "dQw4w9WgXcQ".to_owned());
    let id = VideoId::new(&raw).expect("a video id");

    let details = provider.video(&id, &cancel).await.expect("video details");
    let related = provider.related(&id, &cancel).await.expect("related");

    let payload = serde_json::json!({ "video": details, "related": related.items });
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).expect("serialisable")
    );
}
