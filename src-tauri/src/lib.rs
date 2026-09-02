//! BEASTUBE application shell.
//!
//! Wires the native subsystems together and hands them to the Tauri runtime. Subsystem logic lives
//! in the `beastube-*` crates; this crate is composition and OS integration only, so that no
//! subsystem depends on Tauri and each can be tested without a running webview.

/// Builds and runs the desktop application.
///
/// # Panics
///
/// Panics if the Tauri context cannot be constructed, which indicates a malformed
/// `tauri.conf.json` — a build-time defect rather than a runtime condition worth recovering from.
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("failed to start the BEASTUBE application shell");
}
