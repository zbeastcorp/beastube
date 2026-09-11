//! Where the log goes.
//!
//! Every crate in the workspace emits `tracing` events, and until this module nothing subscribed to
//! them: `tracing::error!` with no subscriber is a no-op, so a production build wrote nothing —
//! not the startup failure that makes every command return an error, not a download that died, not
//! the panic the provider catches and converts. The diagnostics screen said "check the log" about a
//! file that did not exist.
//!
//! ## Two sinks
//!
//! * **A daily-rotated file** under the platform's log directory for this application
//!   (`%LOCALAPPDATA%\app.beastube.desktop\logs` on Windows), written through a non-blocking
//!   worker so a slow disk never stalls the thread that logged. This is the one that matters in
//!   the field.
//! * **Stderr**, in debug builds only, so `tauri dev` shows the same stream in the terminal.
//!
//! ## Nothing personal in it
//!
//! Log lines carry identifiers, counts and error text — never a query string, a watch history entry
//! or a URL the user navigated to. The filtering diagnostics are built the same way, and the
//! log file is subject to the same rule: it must be safe to attach to a bug report.
//!
//! ## Panics are recorded
//!
//! A panic hook writes the message and location through the same subscriber before the default
//! hook runs. Without it a panic on a worker thread is a line on a stderr nobody is watching.

use std::sync::OnceLock;

use tauri::{AppHandle, Manager};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

/// Keeps the non-blocking writer's worker alive for the life of the process.
///
/// Dropping the guard flushes and stops the worker, so it must live as long as anything can log —
/// which is to say, forever.
static WRITER_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

/// Environment variable that overrides the default filter, using `tracing`'s directive syntax.
const FILTER_ENV: &str = "BEASTUBE_LOG";

/// Default verbosity when the variable is unset.
///
/// Our own crates at `debug` in a development build and `info` in release; every dependency at
/// `warn`, because the extractor and the HTTP stack are chatty at `info` and none of it helps read
/// a bug report.
fn default_filter() -> EnvFilter {
    let ours = if cfg!(debug_assertions) { "debug" } else { "info" };
    // Every directive here is a literal that parses; the fallback exists so a typo in this file is
    // a silent `warn`-only filter rather than a startup panic.
    EnvFilter::try_new(format!(
        "warn,beastube_app_lib={ours},beastube_core={ours},beastube_db={ours},\
         beastube_provider={ours},beastube_provider_youtube={ours},beastube_download={ours},\
         beastube_filtering={ours},beastube_network={ours}"
    ))
    .unwrap_or_else(|_| EnvFilter::new("warn"))
}

/// Installs the subscriber and the panic hook. Idempotent; a second call is a no-op.
///
/// Called first thing in `setup`, before the state is built, so the startup path is the first
/// thing in the log rather than the first thing missing from it.
pub(crate) fn init(app: &AppHandle) {
    if WRITER_GUARD.get().is_some() {
        return;
    }

    let filter = EnvFilter::try_from_env(FILTER_ENV).unwrap_or_else(|_| default_filter());

    let file_layer = app.path().app_log_dir().ok().and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        let appender = tracing_appender::rolling::daily(dir, "beastube.log");
        let (writer, guard) = tracing_appender::non_blocking(appender);
        // The guard is kept even if the subscriber below fails to install, which is harmless.
        let _ = WRITER_GUARD.set(guard);
        Some(fmt::layer().with_ansi(false).with_target(true).with_writer(writer))
    });

    let stderr_layer = cfg!(debug_assertions).then(|| fmt::layer().with_target(true));

    // `try_init` rather than `init`: a test harness or an embedding process may already have a
    // subscriber, and replacing it is not this module's call to make.
    let installed = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .is_ok();

    if installed {
        install_panic_hook();
        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            debug = cfg!(debug_assertions),
            "logging started"
        );
    }
}

/// Routes panics through the subscriber, then on to the default hook.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info.location().map_or_else(
            || "unknown".to_owned(),
            |l| format!("{}:{}", l.file(), l.line()),
        );
        // The payload is a `&str` or a `String` for every panic the standard macros produce.
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_owned());
        tracing::error!(%location, "panic: {message}");
        previous(info);
    }));
}
