//! BEASTUBE application shell.
//!
//! Wires the native subsystems together and hands them to the Tauri runtime. Subsystem logic lives
//! in the `beastube-*` crates; this crate is composition and OS integration only, so that no
//! subsystem depends on Tauri and each can be tested without a running webview.
//!
//! ## Startup ordering
//!
//! The window is created hidden and shown once the frontend reports it has painted (§86). Showing
//! it immediately produces a white flash followed by the dark theme — small, but the first thing a
//! user sees. A watchdog shows the window anyway after a short deadline, so a frontend that fails
//! to load can never leave the application running with no visible window.

mod commands;
mod downloads;
mod logging;
// Request interception is a WebView2 facility; there is no cross-platform equivalent, and the
// module is absent rather than stubbed on other targets so a missing capability is a compile error
// rather than a silent no-op (§131).
#[cfg(windows)]
mod request_filter;
mod state;

use std::time::Duration;

use tauri::{Manager, WindowEvent};

use crate::state::AppState;

/// How long to wait for the frontend's ready signal before showing the window regardless.
///
/// Long enough for a cold Vite dev server to compile and paint, short enough that a genuinely
/// broken frontend surfaces as a visible window with an error rather than as nothing at all.
const SHOW_WINDOW_DEADLINE: Duration = Duration::from_secs(10);

/// Label of the main window, matching `tauri.conf.json`.
const MAIN_WINDOW: &str = "main";

/// Called by the frontend once it has rendered its first frame.
///
/// Idempotent: showing an already-visible window is a no-op, so the watchdog and the frontend
/// racing each other is harmless.
// Tauri's command macro requires state arguments by value; the handle is a cheap refcounted
// clone, so this is not the allocation it appears to be.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
fn frontend_ready(app: tauri::AppHandle) {
    show_main_window(&app);
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        // Errors here mean the window is already gone (a fast quit), which is not worth surfacing.
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Builds and runs the desktop application.
///
/// # Panics
///
/// Panics if the Tauri context cannot be constructed, which indicates a malformed
/// `tauri.conf.json` — a build-time defect rather than a runtime condition worth recovering from.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        // Updates. The dependency was declared long before anything used it, which meant the
        // "Check for updates" string existed with nothing behind it; registering the plugin is what
        // turns that into a real control rather than a label (§131).
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            frontend_ready,
            commands::get_settings,
            commands::save_settings,
            commands::reset_settings,
            commands::search,
            commands::get_suggestions,
            commands::get_recent_searches,
            commands::delete_search,
            commands::clear_search_history,
            commands::get_video,
            commands::get_related,
            commands::get_channel,
            commands::get_channel_content,
            commands::get_provider_capabilities,
            commands::record_watch,
            commands::get_history,
            commands::search_history,
            commands::delete_history_entry,
            commands::clear_history,
            commands::get_position,
            commands::checkpoint_playback,
            commands::get_resumable,
            commands::get_bookmarks,
            commands::set_bookmark,
            commands::remove_bookmark,
            commands::is_bookmarked,
            commands::get_playlists,
            commands::create_playlist,
            commands::rename_playlist,
            commands::delete_playlist,
            commands::get_playlist_items,
            commands::add_to_playlist,
            commands::remove_from_playlist,
            commands::playlists_containing,
            commands::get_filtering_diagnostics,
            commands::reset_filter_rules,
            commands::get_storage_stats,
            commands::clear_cache,
            commands::get_app_info,
            commands::get_recommended,
            commands::get_shorts_feed,
            commands::get_more_shorts,
            commands::open_external,
            commands::set_window_theme,
            commands::set_incognito,
            commands::is_incognito,
            downloads::start_download,
            downloads::cancel_download,
            downloads::get_downloads,
            downloads::get_download_tools,
            downloads::reveal_download,
            downloads::open_download_directory,
            downloads::pick_download_directory,
            downloads::pick_downloader_executable,
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // Before anything else, so the startup path is the first thing in the log rather than
            // the first thing missing from it.
            logging::init(&handle);

            // Subsystems are constructed before the window is revealed, but the construction
            // itself makes no network request — so being offline costs nothing at startup (§86).
            let init_handle = handle.clone();
            tauri::async_runtime::block_on(async move {
                match AppState::initialize(&init_handle).await {
                    Ok(state) => {
                        init_handle.manage(state);
                        tracing::info!("application state initialized");
                    }
                    Err(error) => {
                        // Without state every command fails, so this is fatal. It is logged rather
                        // than panicking so the reason survives in the log file.
                        tracing::error!(%error, "could not initialize the application state");
                    }
                }
            });

            // The request filter is attached once the state exists, because it borrows the rule
            // set manager from it. Without this the filtering subsystem is a library nothing calls
            // — which the diagnostics screen would honestly report as zero evaluated requests.
            #[cfg(windows)]
            if let Some(window) = handle.get_webview_window(MAIN_WINDOW) {
                if let Some(state) = handle.try_state::<AppState>() {
                    request_filter::attach(&window, std::sync::Arc::clone(&state.filtering));
                } else {
                    tracing::error!("no application state; the webview runs unfiltered");
                }
            }

            // Watchdog: if the frontend never reports ready — a bundling failure, a JavaScript
            // error before the first paint — show the window anyway rather than leaving a process
            // running with nothing on screen.
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(SHOW_WINDOW_DEADLINE).await;
                if let Some(window) = handle.get_webview_window(MAIN_WINDOW)
                    && !window.is_visible().unwrap_or(false)
                {
                    tracing::warn!(
                        "frontend did not report ready within {}s; showing the window anyway",
                        SHOW_WINDOW_DEADLINE.as_secs()
                    );
                    show_main_window(&handle);
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { .. } = event {
                tracing::info!(label = window.label(), "window close requested");
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to start the BEASTUBE application shell")
        .run(|app, event| {
            // Downloads are separate processes, and nothing was stopping them. Closing the window
            // left `yt-dlp` — and the `ffmpeg` it spawns to join the streams — running with no
            // window to report to, still writing partial files into the download folder. The user
            // saw the application close; the work carried on invisibly and left its litter behind.
            //
            // `cancel` is the path that already knows how to end one properly: it kills the child,
            // waits for it, and removes the partials. Running it for everything unfinished at exit
            // is simply doing at shutdown what the cancel button does on demand.
            if matches!(event, tauri::RunEvent::ExitRequested { .. })
                && let Some(state) = app.try_state::<AppState>()
            {
                let running: Vec<String> = state
                    .downloads
                    .snapshot()
                    .into_iter()
                    .filter(|entry| !entry.status.is_terminal())
                    .map(|entry| entry.id)
                    .collect();
                if !running.is_empty() {
                    tracing::info!(count = running.len(), "cancelling downloads still running");
                    for id in &running {
                        state.downloads.cancel(id);
                    }
                }
            }
        });
}
