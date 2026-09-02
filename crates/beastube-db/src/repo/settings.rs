//! The settings document.
//!
//! Settings are stored as **one JSON document under one key**, not as a column per field. The
//! reason is upgrade behaviour: [`Settings`] is `#[serde(default)]` throughout, so a document
//! written by any build loads in any other, gaining defaults for fields it does not carry and
//! ignoring fields it does not know. A column-per-field schema would need a migration for every
//! new toggle, and a migration that fails is a startup that fails.
//!
//! ## Why loading can never fail on bad data
//!
//! The settings row is read before anything else on the startup path. If a truncated write, a
//! hand edit, or a partially-flushed page made that read fail, the application would not start —
//! and the user could not reach the screen that would let them fix it. So a missing row, an
//! undecodable row, and a row holding a JSON value of the wrong shape all resolve to
//! [`Settings::default`] (§81). Only a failure of the *database itself* (locked, corrupt file) is
//! reported, because that is a condition settings cannot paper over.
//!
//! Every load is passed through [`Settings::sanitized`], so an out-of-range value clamps instead
//! of reaching a CSS variable or an audio gain node.

use beastube_core::Settings;
use beastube_core::time_util::Timestamp;

use crate::connection::Database;
use crate::error::{DbError, DbResult};

/// Key under which the settings document is stored in the `settings` table.
///
/// A constant rather than a literal at each call site: the table is a general key/value store, and
/// a typo would read as "no settings yet" and silently reset the user's configuration.
pub const SETTINGS_KEY: &str = "app";

/// Reads and writes the user's settings document.
#[derive(Debug, Clone)]
pub struct SettingsRepo {
    db: Database,
}

impl SettingsRepo {
    /// Binds the repository to a database handle.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Loads the settings document, falling back to defaults for anything unusable.
    ///
    /// A missing row, a row that is not valid JSON, and a row whose JSON is not a settings object
    /// all yield [`Settings::default`]. The result is always [`Settings::sanitized`].
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] only if the database itself could not be read — the row's *content*
    /// never produces an error, because a corrupt settings row must not prevent startup.
    pub async fn load(&self) -> DbResult<Settings> {
        let raw: Option<String> =
            sqlx::query_scalar("SELECT value_json FROM settings WHERE key = ?")
                .bind(SETTINGS_KEY)
                .fetch_optional(self.db.reader())
                .await
                .map_err(DbError::from_sqlx)?;

        let Some(raw) = raw else {
            return Ok(Settings::default());
        };

        match serde_json::from_str::<Settings>(&raw) {
            Ok(settings) => Ok(settings.sanitized()),
            Err(error) => {
                // Logged, not returned: the diagnostics screen should show that a reset happened,
                // but the startup path must continue.
                tracing::warn!(
                    %error,
                    "settings document is undecodable; falling back to defaults"
                );
                Ok(Settings::default())
            }
        }
    }

    /// Writes the settings document, replacing any existing one.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] if the document cannot be serialized, or a [`DbError`] if the
    /// write fails.
    pub async fn save(&self, settings: &Settings, at: Timestamp) -> DbResult<()> {
        let value = super::encode_json("settings", settings)?;
        sqlx::query(
            "INSERT INTO settings (key, value_json, updated_at)
             VALUES (?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET
                 value_json = excluded.value_json,
                 updated_at = excluded.updated_at",
        )
        .bind(SETTINGS_KEY)
        .bind(value)
        .bind(at.as_millis())
        .execute(self.db.writer())
        .await
        .map_err(DbError::from_sqlx)?;
        Ok(())
    }

    /// When the document was last written, or `None` if it has never been written.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the read fails.
    pub async fn updated_at(&self) -> DbResult<Option<Timestamp>> {
        let raw: Option<i64> = sqlx::query_scalar("SELECT updated_at FROM settings WHERE key = ?")
            .bind(SETTINGS_KEY)
            .fetch_optional(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(super::from_db_timestamp_opt(raw))
    }

    /// Discards the stored document so the next [`SettingsRepo::load`] returns defaults.
    ///
    /// Deleting the row rather than writing a defaults document keeps "never configured" and
    /// "explicitly reset to defaults" indistinguishable, which is what the reset action means.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the delete fails.
    pub async fn reset(&self) -> DbResult<()> {
        sqlx::query("DELETE FROM settings WHERE key = ?")
            .bind(SETTINGS_KEY)
            .execute(self.db.writer())
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use beastube_core::settings::Theme;

    async fn repo() -> (Database, SettingsRepo) {
        let db = Database::open_in_memory()
            .await
            .expect("in-memory database");
        let repo = SettingsRepo::new(db.clone());
        (db, repo)
    }

    async fn write_raw(db: &Database, value: &str) {
        sqlx::query(
            "INSERT INTO settings (key, value_json, updated_at) VALUES (?, ?, 0)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
        )
        .bind(SETTINGS_KEY)
        .bind(value)
        .execute(db.writer())
        .await
        .expect("raw write");
    }

    #[tokio::test]
    async fn a_fresh_database_loads_defaults() {
        let (_db, repo) = repo().await;
        assert_eq!(repo.load().await.unwrap(), Settings::default());
        assert_eq!(repo.updated_at().await.unwrap(), None);
    }

    #[tokio::test]
    async fn settings_round_trip() {
        let (_db, repo) = repo().await;
        let mut settings = Settings::default();
        settings.appearance.theme = Theme::Amoled;
        settings.privacy.history_enabled = false;

        repo.save(&settings, Timestamp::from_millis(1_700))
            .await
            .unwrap();

        let loaded = repo.load().await.unwrap();
        assert_eq!(loaded.appearance.theme, Theme::Amoled);
        assert!(!loaded.privacy.history_enabled);
        assert_eq!(
            repo.updated_at().await.unwrap(),
            Some(Timestamp::from_millis(1_700))
        );
    }

    #[tokio::test]
    async fn saving_twice_updates_rather_than_conflicting() {
        let (db, repo) = repo().await;
        repo.save(&Settings::default(), Timestamp::from_millis(1))
            .await
            .unwrap();
        repo.save(&Settings::default(), Timestamp::from_millis(2))
            .await
            .unwrap();

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM settings")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(rows, 1, "the document is one row, not an append log");
    }

    #[tokio::test]
    async fn a_corrupt_row_loads_defaults_instead_of_failing_startup() {
        let (db, repo) = repo().await;
        for hostile in [
            "{not json at all",
            "",
            "null",
            "[]",
            "\"a string\"",
            "12345",
            "{\"appearance\": 7}",
        ] {
            write_raw(&db, hostile).await;
            let loaded = repo
                .load()
                .await
                .unwrap_or_else(|e| panic!("{hostile:?} must not error: {e}"));
            assert_eq!(
                loaded,
                Settings::default(),
                "{hostile:?} must degrade to defaults"
            );
        }
    }

    #[tokio::test]
    async fn a_truncated_document_keeps_the_fields_that_survived() {
        let (db, repo) = repo().await;
        // A partial write that happens to remain valid JSON: the fields that made it are honoured
        // and the rest default, which is the whole point of `#[serde(default)]`.
        write_raw(&db, r#"{"appearance":{"theme":"dark"}}"#).await;

        let loaded = repo.load().await.unwrap();
        assert_eq!(loaded.appearance.theme, Theme::Dark);
        let default_volume = Settings::default().playback.volume;
        assert!((loaded.playback.volume - default_volume).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn out_of_range_values_are_clamped_on_load() {
        let (db, repo) = repo().await;
        write_raw(
            &db,
            r#"{"playback":{"volume":9.0,"speed":-3.0},"cache":{"disk_budget_mb":99999999}}"#,
        )
        .await;

        let loaded = repo.load().await.unwrap();
        assert!((loaded.playback.volume - 1.0).abs() < f32::EPSILON);
        assert!((loaded.playback.speed - 0.25).abs() < f32::EPSILON);
        assert_eq!(loaded.cache.disk_budget_mb, 65_536);
    }

    #[tokio::test]
    async fn unknown_keys_from_a_newer_build_survive_a_load() {
        let (db, repo) = repo().await;
        write_raw(
            &db,
            r#"{"future_section":{"x":1},"playback":{"muted":true}}"#,
        )
        .await;
        assert!(repo.load().await.unwrap().playback.muted);
    }

    #[tokio::test]
    async fn resetting_returns_to_defaults() {
        let (_db, repo) = repo().await;
        let mut settings = Settings::default();
        settings.appearance.theme = Theme::Light;
        repo.save(&settings, Timestamp::EPOCH).await.unwrap();

        repo.reset().await.unwrap();
        assert_eq!(repo.load().await.unwrap(), Settings::default());
        assert_eq!(repo.updated_at().await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_closed_database_reports_an_error_rather_than_defaults() {
        // The distinction that matters: bad *content* degrades, a broken *database* does not.
        let (db, repo) = repo().await;
        db.close().await;
        assert!(
            repo.load().await.is_err(),
            "an unreadable database must not masquerade as unconfigured settings"
        );
    }

    #[tokio::test]
    async fn concurrent_saves_all_complete_and_leave_one_row() {
        let (db, repo) = repo().await;
        let mut handles = Vec::new();
        for i in 0..16_i64 {
            let repo = repo.clone();
            handles.push(tokio::spawn(async move {
                repo.save(&Settings::default(), Timestamp::from_millis(i))
                    .await
            }));
        }
        for handle in handles {
            handle.await.unwrap().expect("no save should be lost");
        }
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM settings")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }
}
