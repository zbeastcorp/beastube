//! Application state.
//!
//! One struct owns every subsystem handle, constructed once at startup and shared by every command.
//! It is composition only: no logic lives here, so a command reads exactly like the subsystem call
//! it makes.
//!
//! ## Startup is staged
//!
//! Construction is deliberately ordered so the window can appear before the slow parts finish
//! (§86). Opening the database and running migrations is fast and must succeed before anything can
//! read; building the provider touches the filesystem but makes no network request. Nothing here
//! blocks on the network, so a machine that is offline still starts in the same time as one that is
//! not.
//!
//! ## Incognito is enforced here, not at the call site
//!
//! Whether a write happens is decided in one place — [`AppState::records_history`] — rather than at
//! each of the several places that would otherwise have to remember. A mode that silently changes
//! what is persisted is exactly the kind of thing that leaks through a forgotten branch (§50).

// Held for subsystems that land next (the storage panel reads both); keeping them on the state now
// avoids reconstructing it when they arrive.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use beastube_core::Settings;
use beastube_core::events::AppEvent;
use beastube_db::{Database, DbError, Repositories};
use beastube_download::{
    DownloadError, DownloadManager, DownloadPlan, LocateOptions, Tools, locate, simplified,
};
use beastube_filtering::builtin::builtin_rule_set;
use beastube_filtering::diagnostics::{FilteringDiagnostics, FilteringSnapshot};
use beastube_filtering::engine::{EngineConfig, NeverBlockList};
use beastube_filtering::ruleset::{RuleSetManager, ValidatedRuleSet};
use beastube_provider::MetadataProvider;
use beastube_provider_youtube::YouTubeProvider;
use parking_lot::RwLock;
use tauri::{AppHandle, Emitter, Manager};

/// How many downloads run at once. Each is a separate process pulling at full bandwidth; two
/// already share a home connection with the player, and anything beyond waits in the queue.
const MAX_CONCURRENT_DOWNLOADS: usize = 2;

/// Everything a command needs.
pub(crate) struct AppState {
    /// The local database handle.
    pub(crate) database: Database,
    /// Repositories over it.
    pub(crate) repositories: Repositories,
    /// The active metadata provider.
    pub(crate) provider: Arc<dyn MetadataProvider>,
    /// The in-memory settings document, kept in step with the persisted one.
    settings: RwLock<Settings>,
    /// Whether this session records anything to the library.
    incognito: AtomicBool,
    /// Where the provider keeps its extractor cache, for the storage panel.
    pub(crate) provider_cache_dir: PathBuf,
    /// Owns the active filtering rule set and the decision to roll it back.
    pub(crate) filtering: Arc<RuleSetManager>,
    /// Live filtering counters, shared with the engine.
    pub(crate) filtering_diagnostics: Arc<FilteringDiagnostics>,
    /// Owns every download of the session.
    pub(crate) downloads: DownloadManager,
    /// Where downloads go when the setting is unset: a folder of ours inside the user's Downloads.
    pub(crate) default_download_dir: PathBuf,
    /// Where the downloader keeps its own cache, under the application's cache directory.
    download_cache_dir: PathBuf,
    /// Directories a downloader shipped with the application would be in.
    tool_search_dirs: Vec<PathBuf>,
    /// When the state was constructed, for the uptime reading on the diagnostics screen.
    started_at: std::time::Instant,
}

/// Why startup failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StartupError {
    /// The application data directory could not be resolved.
    #[error("could not resolve the application data directory")]
    DataDirectory(#[source] tauri::Error),

    /// The database could not be opened or migrated.
    #[error("could not open the local library")]
    Database(#[from] DbError),

    /// The provider could not be constructed.
    #[error("could not construct the content provider")]
    Provider(#[source] beastube_provider::ProviderError),
}

impl AppState {
    /// Builds the state, opening the database and running migrations.
    ///
    /// # Errors
    ///
    /// Returns [`StartupError`] if the data directory cannot be resolved, the database cannot be
    /// opened or migrated, or the provider cannot be constructed.
    pub(crate) async fn initialize(app: &AppHandle) -> Result<Self, StartupError> {
        let data_dir = app
            .path()
            .app_data_dir()
            .map_err(StartupError::DataDirectory)?;

        let database = Database::open(data_dir.join("library.db")).await?;

        // An unclean previous shutdown means the file may have been damaged mid-write. Checking
        // once at startup finds that before anything writes more into it (§127).
        if let Err(error) = database.integrity_check().await {
            tracing::error!(%error, "the local library failed its integrity check");
            return Err(StartupError::Database(error));
        }

        let repositories = Repositories::new(&database);

        // A corrupt settings row must not prevent startup; the repository returns defaults.
        let settings = repositories.settings.load().await.unwrap_or_else(|error| {
            tracing::warn!(%error, "settings could not be read; using defaults");
            Settings::default()
        });

        let provider_cache_dir = app
            .path()
            .app_cache_dir()
            .unwrap_or_else(|_| data_dir.join("cache"))
            .join("provider");

        let provider = YouTubeProvider::new(&provider_cache_dir).map_err(StartupError::Provider)?;

        // The first request the user makes should not also pay for the visitor ID the extractor
        // wants on every request. Fetched now, off the startup path: this is the one network
        // request made at launch, it is speculative, and a failure costs nothing but the saving.
        let warm = provider.clone();
        tauri::async_runtime::spawn(async move { warm.warm().await });

        // Filtering starts from the compiled-in rule set, so it works on first launch and offline.
        // A validation failure here would mean the shipped set is broken — a build defect — so it
        // degrades to the inert set rather than preventing startup.
        let filtering_diagnostics = Arc::new(FilteringDiagnostics::new());
        let active = builtin_rule_set().validate().unwrap_or_else(|error| {
            tracing::error!(%error, "the built-in rule set failed validation; filtering is inert");
            ValidatedRuleSet::inert()
        });
        let filtering = Arc::new(RuleSetManager::new(
            active,
            EngineConfig::new(
                settings.filtering.enabled,
                settings.filtering.mode,
                NeverBlockList::playback_critical(),
            ),
            Arc::clone(&filtering_diagnostics),
        ));

        let incognito = settings.privacy.incognito_by_default;

        // Downloads default to a folder of ours inside the user's Downloads, so the files land
        // where every other download on the machine does rather than in application data.
        let default_download_dir = app
            .path()
            .download_dir()
            .unwrap_or_else(|_| data_dir.join("downloads"))
            .join("BEASTUBE");
        let download_cache_dir = provider_cache_dir
            .parent()
            .map_or_else(|| data_dir.join("cache"), Path::to_path_buf)
            .join("yt-dlp");
        // Where the installer puts the tools it ships, searched before `PATH` so a bundled copy
        // wins over whatever else happens to be on the machine. `resources/binaries` is where
        // `tauri.conf.json` lands them; the executable's own directory covers a portable layout
        // and a copy dropped in by hand.
        let mut tool_search_dirs: Vec<PathBuf> = Vec::new();
        if let Ok(resources) = app.path().resource_dir() {
            tool_search_dirs.push(resources.join("binaries"));
            tool_search_dirs.push(resources);
        }
        if let Ok(exe) = std::env::current_exe()
            && let Some(parent) = exe.parent()
        {
            tool_search_dirs.push(parent.join("binaries"));
            tool_search_dirs.push(parent.to_path_buf());
        }
        let emitter = app.clone();
        let downloads = DownloadManager::new(
            Arc::new(move |progress| {
                let event = AppEvent::DownloadProgress(progress.clone());
                if let Err(error) = emitter.emit(event.name(), progress) {
                    tracing::warn!(%error, "could not emit download progress");
                }
            }),
            MAX_CONCURRENT_DOWNLOADS,
        );

        Ok(Self {
            database,
            repositories,
            provider: Arc::new(provider),
            settings: RwLock::new(settings),
            incognito: AtomicBool::new(incognito),
            provider_cache_dir,
            filtering,
            filtering_diagnostics,
            downloads,
            default_download_dir,
            download_cache_dir,
            tool_search_dirs,
            started_at: std::time::Instant::now(),
        })
    }

    /// The directory downloads go into: the setting when set, the default otherwise.
    ///
    /// Simplified on the way out. The default comes from `download_dir()` and is already ordinary,
    /// but a configured one is whatever the folder picker returned, and that answers in the
    /// verbatim `\\?\` form — which is what the settings screen would then show the user.
    #[must_use]
    pub(crate) fn download_directory(&self) -> PathBuf {
        let chosen = self
            .settings
            .read()
            .downloads
            .directory
            .as_deref()
            .map_or_else(|| self.default_download_dir.clone(), PathBuf::from);
        simplified(&chosen)
    }

    /// The tools a download would use right now, honouring the paths set in settings.
    #[must_use]
    pub(crate) fn download_tools(&self) -> Tools {
        let settings = self.settings.read();
        locate(&LocateOptions {
            downloader: settings.downloads.tool_path.as_deref().map(Path::new),
            ffmpeg: settings.downloads.ffmpeg_path.as_deref().map(Path::new),
            beside: &self.tool_search_dirs,
        })
    }

    /// Everything one download needs, resolved from the current settings.
    ///
    /// # Errors
    ///
    /// Returns [`DownloadError::ToolMissing`] if no downloader can be found, or
    /// [`DownloadError::MuxerMissing`] if no `ffmpeg` can — YouTube serves video and audio as
    /// separate tracks, so without a muxer there is nothing a download could produce.
    pub(crate) fn download_plan(&self) -> Result<DownloadPlan, DownloadError> {
        let tools = self.download_tools();
        let tool = tools.downloader.ok_or(DownloadError::ToolMissing)?;
        let ffmpeg = tools.ffmpeg.ok_or(DownloadError::MuxerMissing)?;
        Ok(DownloadPlan {
            tool,
            ffmpeg,
            js_runtime: tools.js_runtime,
            directory: self.download_directory(),
            cache_dir: Some(self.download_cache_dir.clone()),
            max_height: self.settings.read().downloads.max_quality.height(),
        })
    }

    /// A snapshot of the current settings.
    #[must_use]
    pub(crate) fn settings(&self) -> Settings {
        self.settings.read().clone()
    }

    /// Replaces the settings document, sanitizing before it is stored or returned.
    ///
    /// Sanitization happens here rather than being trusted from the UI, so an out-of-range value
    /// from any source — a hand-edited file, an older build, a bug — is corrected in one place.
    pub(crate) fn set_settings(&self, settings: Settings) -> Settings {
        let sanitized = settings.sanitized();
        // Filtering configuration is pushed to the engine here rather than being read from settings
        // on the request path: a decision happens per request, and re-reading a lock each time
        // would put the settings store on the hot path.
        self.filtering
            .set_filtering(sanitized.filtering.enabled, sanitized.filtering.mode);
        *self.settings.write() = sanitized.clone();
        sanitized
    }

    /// A snapshot of the filtering counters, for the settings and diagnostics screens.
    #[must_use]
    pub(crate) fn filtering_snapshot(&self) -> FilteringSnapshot {
        self.filtering_diagnostics.snapshot()
    }

    /// How long the application has been serving commands, in milliseconds.
    #[must_use]
    pub(crate) fn uptime_ms(&self) -> u64 {
        // Saturating rather than `as`: an uptime beyond `u64::MAX` milliseconds is impossible, but
        // a silent wrap would be a nonsense reading rather than a clamped one.
        u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Whether this session is incognito.
    #[must_use]
    pub(crate) fn is_incognito(&self) -> bool {
        self.incognito.load(Ordering::Relaxed)
    }

    /// Enters or leaves incognito.
    pub(crate) fn set_incognito(&self, enabled: bool) {
        self.incognito.store(enabled, Ordering::Relaxed);
    }

    /// Whether a watch should be recorded.
    ///
    /// The single decision point: incognito suppresses recording regardless of the setting, and the
    /// privacy setting suppresses it otherwise. Every write path consults this rather than
    /// re-deriving it (§50).
    #[must_use]
    pub(crate) fn records_history(&self) -> bool {
        !self.is_incognito() && self.settings.read().privacy.history_enabled
    }

    /// Whether a search query should be recorded.
    #[must_use]
    pub(crate) fn records_searches(&self) -> bool {
        !self.is_incognito() && self.settings.read().privacy.search_history_enabled
    }
}

#[cfg(test)]
mod tests {
    use beastube_core::Settings;

    /// The incognito rule, exercised without a Tauri handle.
    ///
    /// Written against the same boolean logic `records_history` uses, because that decision is the
    /// one place a privacy regression could hide.
    fn records_history(incognito: bool, settings: &Settings) -> bool {
        !incognito && settings.privacy.history_enabled
    }

    #[test]
    fn incognito_suppresses_recording_even_when_history_is_enabled() {
        let mut settings = Settings::default();
        settings.privacy.history_enabled = true;

        assert!(records_history(false, &settings));
        assert!(
            !records_history(true, &settings),
            "incognito must win over the setting, not the other way round"
        );
    }

    #[test]
    fn disabling_history_suppresses_recording_outside_incognito_too() {
        let mut settings = Settings::default();
        settings.privacy.history_enabled = false;

        assert!(!records_history(false, &settings));
        assert!(!records_history(true, &settings));
    }
}
