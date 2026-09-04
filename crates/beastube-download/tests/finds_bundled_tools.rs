//! Discovery against the directory the installer actually populates.
//!
//! `scripts/fetch-tools.ps1` puts the tools in `src-tauri/binaries/`, and Tauri copies them beside
//! the executable. This checks the copy the build produced is where `locate` looks, which is the
//! one link in the chain no unit test can cover: it depends on the build, not on the code.

use std::path::PathBuf;

use beastube_download::{LocateOptions, locate};

/// `<repo>/target/debug/binaries`, where the dev build lands them.
fn bundled_dir() -> Option<PathBuf> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent()?.parent()?.to_path_buf();
    let dir = repo.join("target/debug/binaries");
    dir.is_dir().then_some(dir)
}

#[test]
fn the_bundled_tools_are_discovered_before_anything_on_path() {
    let Some(dir) = bundled_dir() else {
        eprintln!("skipped: no bundled directory yet — run `pnpm tools:fetch` and build once");
        return;
    };

    let tools = locate(&LocateOptions {
        downloader: None,
        ffmpeg: None,
        beside: std::slice::from_ref(&dir),
    });

    let downloader = tools.downloader.expect("yt-dlp was not found in the bundled directory");
    let ffmpeg = tools.ffmpeg.expect("ffmpeg was not found in the bundled directory");

    assert!(
        downloader.starts_with(&dir),
        "a copy on PATH won instead of the bundled one: {}",
        downloader.display()
    );
    assert!(ffmpeg.starts_with(&dir), "{}", ffmpeg.display());
}
