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

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use beastube_core::Settings;
use beastube_db::{Database, DbError, Repositories};
use beastube_provider::MetadataProvider;
use beastube_provider_youtube::YouTubeProvider;
use parking_lot::RwLock;
use tauri::{AppHandle, Manager};

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

        let incognito = settings.privacy.incognito_by_default;

        Ok(Self {
            database,
            repositories,
            provider: Arc::new(provider),
            settings: RwLock::new(settings),
            incognito: AtomicBool::new(incognito),
            provider_cache_dir,
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
        *self.settings.write() = sanitized.clone();
        sanitized
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
