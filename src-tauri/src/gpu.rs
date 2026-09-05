//! The webview's command line, and the one setting that changes it.
//!
//! ## Why this is not in `tauri.conf.json`
//!
//! The flags used to live there as a fixed string. That is fine until one of them has to depend on
//! a preference: the window is created from the configuration before any application code runs, so
//! there is no moment at which a value read from the database could still reach it. Composing the
//! string here, before the builder, is what makes `playback.hardware_acceleration` able to mean
//! something — and it is why the preference is mirrored into a small file rather than read from
//! SQLite, which is not open yet and would be an odd thing to open twice.
//!
//! ## What was removed, and why
//!
//! Two flags are gone from the set that shipped:
//!
//! - `--ignore-gpu-blocklist` overrode Chromium's list of drivers known to crash, hang or render
//!   incorrectly — on precisely the old integrated graphics this application is meant to support.
//!   The blocklist is not a performance setting; it is a list of machines where the GPU path is
//!   known to be broken, and forcing it on them buys a little decoding speed at the price of the
//!   failure the list exists to avoid.
//! - `--enable-zero-copy` turns on a raster path that Chromium's own Windows defaults decline.
//!
//! What remains is either required (`--autoplay-policy`, without which the player cannot start
//! itself) or a default being stated rather than changed.

use std::ffi::OsString;
use std::path::PathBuf;

/// The flags every launch gets.
///
/// `--disable-features=msWebOOUI,msPdfOOUI` removes Edge's own UI from a window that draws its own,
/// and `msSmartScreenProtection` with it: `SmartScreen` reports the URL of each navigation to
/// Microsoft, and this application's whole premise is that what you watch stays on your machine
/// (§99). Nothing here downloads or executes a file on the viewer's behalf, so the protection it
/// removes is one this window has no use for.
///
/// `--disable-background-timer-throttling` keeps the playhead and the progress bar honest while the
/// window is not focused, which is the ordinary case for a video playing in the background.
///
/// ## The two cache ceilings, which are the whole storage story
///
/// Chromium sizes its disk cache from *free disk space* and will take hundreds of megabytes on
/// a large drive. Measured on this application before these flags, the embedded browser's
/// profile held **487 MB** against a 4.4 MB library: 360 MB of HTTP cache and 88 MB of compiled
/// JavaScript. The privacy screen was reporting "stored data" of 4.4 MB at the time.
///
/// These are ceilings, not reservations. Nothing is allocated up front, a fresh installation
/// uses almost none of it, and they are generous for what is actually cached here — thumbnails,
/// the player script, stylesheets. The video itself streams and never enters the HTTP cache.
///
/// Worth being plain that this is a storage fix and not a speed one: a smaller cache means a
/// slightly higher chance of re-fetching a thumbnail. Against that, half a gigabyte of cache on
/// a machine short of disk is the more expensive of the two.
const BASE_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
     --autoplay-policy=no-user-gesture-required \
     --disable-background-timer-throttling \
     --enable-gpu-rasterization \
     --canvas-oop-rasterization \
     --disk-cache-size=134217728 \
     --media-cache-size=67108864";

/// Added when the viewer has turned hardware acceleration off.
///
/// The remedy of last resort for a broken driver, and the reason the setting exists: a machine that
/// paints a black rectangle where the video should be will usually paint the video once the GPU is
/// out of the path. Slower, and working, which is the right way round.
const SOFTWARE_ARGS: &str =
    "--disable-gpu --disable-gpu-compositing --disable-software-rasterizer=0";

/// The environment variable WebView2 reads its extra command line from.
const WEBVIEW_ARGS_ENV: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";

/// Where the mirrored preference lives.
///
/// Beside the database rather than inside it. This is read before Tauri has resolved a single path
/// and before the database is open, so it resolves `%APPDATA%` itself; the alternative is opening
/// SQLite twice per launch to answer one boolean.
fn preference_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(base)
            .join("app.beastube.desktop")
            .join("software-rendering"),
    )
}

/// Whether the viewer has asked for hardware acceleration to be left on.
///
/// Absent file means on, which is both the default and the right answer for a first run: the file
/// only ever exists because someone turned the setting off.
fn hardware_acceleration_enabled() -> bool {
    preference_path().is_none_or(|path| !path.exists())
}

/// Mirrors the setting so the next launch can act on it.
///
/// Called when settings are saved. Failures are logged and swallowed: the preference is already
/// stored properly in the database, and this copy exists only to be readable earlier than that one.
pub(crate) fn remember_preference(hardware_acceleration: bool) {
    let Some(path) = preference_path() else {
        return;
    };
    let result = if hardware_acceleration {
        match std::fs::remove_file(&path) {
            // Already absent is the state we wanted.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    } else {
        path.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, b"software rendering requested\n"))
    };
    if let Err(error) = result {
        tracing::warn!(%error, path = %path.display(), "could not mirror the rendering preference");
    }
}

/// Puts the composed command line where WebView2 will find it.
///
/// Must run before the Tauri builder, because the window — and with it the webview — is created
/// from the configuration during `build`.
///
/// An existing value is respected and extended rather than replaced. That is not politeness: it is
/// what keeps `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=...` working against a
/// release build, which is how this application is actually measured.
// Scoped opt-out, matching how the workspace handles its other `unsafe`. `set_var` is unsafe
// because another thread reading the environment concurrently is undefined; there is no other
// thread yet, which is the whole reason this runs where it does.
#[allow(unsafe_code)]
pub(crate) fn apply_browser_arguments() {
    let mut args = OsString::from(BASE_ARGS);

    if !hardware_acceleration_enabled() {
        args.push(" ");
        args.push(SOFTWARE_ARGS);
        tracing::info!("hardware acceleration is off; the webview will render in software");
    }

    if let Some(existing) = std::env::var_os(WEBVIEW_ARGS_ENV)
        && !existing.is_empty()
    {
        args.push(" ");
        args.push(&existing);
    }

    // SAFETY-adjacent note rather than an `unsafe` block: this runs on the main thread before any
    // other thread exists, which is the condition that makes setting an environment variable sound.
    unsafe { std::env::set_var(WEBVIEW_ARGS_ENV, &args) };
}
