//! Database handle, connection policy and lifecycle.
//!
//! ## Why two pools
//!
//! SQLite permits many concurrent readers but exactly one writer. A single shared pool therefore
//! makes every writer contend through `SQLITE_BUSY` and the busy timeout, which shows up as
//! occasional multi-second stalls under load and is miserable to reproduce.
//!
//! Instead there are two pools: a **writer pool capped at one connection**, and a **reader pool**
//! sized for concurrency. Write serialization then happens in the connection pool's queue — an
//! ordinary async wait — rather than as lock contention inside SQLite. Readers never block on
//! writers because WAL lets them read the last committed snapshot while a write is in flight.
//!
//! ## Pragmas
//!
//! Each is set for a stated reason; none is cargo-culted:
//!
//! * `journal_mode = WAL` — readers do not block the writer, and the writer does not block readers.
//!   This is the property the whole two-pool design depends on.
//! * `synchronous = NORMAL` — under WAL this is durable across application crashes (the case that
//!   actually happens) and only risks the last transaction on an OS crash or power loss. `FULL`
//!   would fsync on every commit, which is far too expensive for playback checkpoints.
//! * `foreign_keys = ON` — SQLite disables them per connection by default, so the `REFERENCES`
//!   clauses in the schema would otherwise be decorative.
//! * `busy_timeout` — a backstop. With one writer connection it should never be hit; if it is,
//!   something outside our pool (a backup tool, a second instance) holds the lock.
//! * `cache_size = -16000` — 16 MiB of page cache, negative meaning KiB rather than pages.
//! * `temp_store = MEMORY` — keeps sort and join scratch off disk.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};

use crate::error::{DbError, DbResult};

/// Embedded migrations, applied at startup.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// Page cache per connection, in KiB (negative values mean KiB in SQLite's `cache_size` pragma).
const CACHE_SIZE_KIB: i32 = -16_000;

/// Backstop for lock contention. Should be unreachable given the single-writer pool.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum concurrent readers.
///
/// Reads are short and served from the page cache; four is enough to keep the UI, the prefetcher
/// and a maintenance task from queueing behind each other, without holding many file handles open.
const MAX_READERS: u32 = 4;

/// How long an idle pooled connection is kept before being closed.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// A handle to the local database.
///
/// Cloning is cheap: both pools are internally reference-counted, so this is passed by value into
/// tasks rather than wrapped in an `Arc`.
#[derive(Debug, Clone)]
pub struct Database {
    readers: SqlitePool,
    writer: SqlitePool,
    path: Option<PathBuf>,
}

impl Database {
    /// Opens (creating if absent) the database at `path` and applies migrations.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Open`] if the file cannot be opened or created, or
    /// [`DbError::Migration`] if the schema cannot be brought up to date.
    pub async fn open(path: impl AsRef<Path>) -> DbResult<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| DbError::Open {
                    path: parent.display().to_string(),
                    source: sqlx::Error::Io(e),
                })?;
        }

        let options = Self::connect_options(path);
        let db = Self::from_options(options, Some(path.to_path_buf())).await?;
        db.migrate().await?;
        Ok(db)
    }

    /// Opens a private in-memory database with migrations applied, for tests.
    ///
    /// Both pools address the same shared in-memory database, so a write made through the writer
    /// is visible to readers — which a plain `:memory:` per connection would not be.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Open`] or [`DbError::Migration`] if the in-memory database cannot be
    /// prepared.
    pub async fn open_in_memory() -> DbResult<Self> {
        // A named shared-cache in-memory database so both pools see one instance. `mode=memory`
        // with a distinct name keeps concurrent tests isolated from each other.
        let name = format!("beastube-test-{}", uuid_like());
        let uri = format!("file:{name}?mode=memory&cache=shared");
        let options = SqliteConnectOptions::from_str(&uri)
            .map_err(|e| DbError::Open {
                path: uri.clone(),
                source: e,
            })?
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT);

        let db = Self::from_options(options, None).await?;
        db.migrate().await?;
        Ok(db)
    }

    fn connect_options(path: &Path) -> SqliteConnectOptions {
        SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT)
            .pragma("cache_size", CACHE_SIZE_KIB.to_string())
            .pragma("temp_store", "MEMORY")
            // Runs `PRAGMA optimize` when a connection closes, so the query planner's statistics
            // stay current without a separate maintenance pass.
            .optimize_on_close(true, None)
    }

    async fn from_options(options: SqliteConnectOptions, path: Option<PathBuf>) -> DbResult<Self> {
        let describe = || {
            path.as_ref()
                .map_or_else(|| ":memory:".to_owned(), |p| p.display().to_string())
        };

        let writer = SqlitePoolOptions::new()
            // Exactly one: this is what makes write serialization a queue rather than lock
            // contention. Raising it would reintroduce SQLITE_BUSY.
            .max_connections(1)
            // Kept warm so a playback checkpoint never pays connection setup.
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options.clone())
            .await
            .map_err(|source| DbError::Open {
                path: describe(),
                source,
            })?;

        let readers = SqlitePoolOptions::new()
            .max_connections(MAX_READERS)
            .min_connections(1)
            .idle_timeout(IDLE_TIMEOUT)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options.read_only(false))
            .await
            .map_err(|source| DbError::Open {
                path: describe(),
                source,
            })?;

        Ok(Self {
            readers,
            writer,
            path,
        })
    }

    /// Applies any outstanding migrations.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Migration`] if a migration fails or the recorded history diverges from
    /// the embedded set.
    pub async fn migrate(&self) -> DbResult<()> {
        MIGRATOR.run(&self.writer).await?;
        Ok(())
    }

    /// The pool to use for reads.
    #[must_use]
    pub const fn reader(&self) -> &SqlitePool {
        &self.readers
    }

    /// The pool to use for writes and for transactions that write.
    #[must_use]
    pub const fn writer(&self) -> &SqlitePool {
        &self.writer
    }

    /// Path of the database file, or `None` for an in-memory database.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Runs `PRAGMA integrity_check` and returns `Ok(())` when the result is `ok`.
    ///
    /// Called on startup after an unclean shutdown, so a damaged file is found before the
    /// application writes more into it.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Corrupt`] with the reported problems, or a query error if the check
    /// itself could not run.
    pub async fn integrity_check(&self) -> DbResult<()> {
        let rows = sqlx::query("PRAGMA integrity_check")
            .fetch_all(&self.readers)
            .await
            .map_err(DbError::from_sqlx)?;

        let problems: Vec<String> = rows
            .iter()
            .filter_map(|row| row.try_get::<String, _>(0).ok())
            .filter(|line| line != "ok")
            .collect();

        if problems.is_empty() {
            Ok(())
        } else {
            Err(DbError::Corrupt {
                detail: problems.join("; "),
            })
        }
    }

    /// Total size of the database in bytes, computed from SQLite's own page accounting.
    ///
    /// Used by the storage panel; it excludes the WAL file, which is transient.
    ///
    /// # Errors
    ///
    /// Returns a query error if the pragmas cannot be read.
    pub async fn size_bytes(&self) -> DbResult<u64> {
        let page_count: i64 = sqlx::query_scalar("PRAGMA page_count")
            .fetch_one(&self.readers)
            .await
            .map_err(DbError::from_sqlx)?;
        let page_size: i64 = sqlx::query_scalar("PRAGMA page_size")
            .fetch_one(&self.readers)
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(u64::try_from(page_count.max(0)).unwrap_or(0)
            * u64::try_from(page_size.max(0)).unwrap_or(0))
    }

    /// Folds the write-ahead log back into the main database file.
    ///
    /// Part of the shutdown sequence (§82): without it the WAL can stay large after a long
    /// session, and the next startup pays to replay it.
    ///
    /// # Errors
    ///
    /// Returns a query error if the checkpoint cannot run.
    pub async fn checkpoint(&self) -> DbResult<()> {
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.writer)
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(())
    }

    /// Rebuilds the database file, reclaiming space freed by deletions.
    ///
    /// Expensive and exclusive, so it is a background maintenance task, never part of startup or
    /// shutdown.
    ///
    /// # Errors
    ///
    /// Returns a query error if the vacuum cannot run.
    pub async fn vacuum(&self) -> DbResult<()> {
        sqlx::query("VACUUM")
            .execute(&self.writer)
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(())
    }

    /// Closes both pools, waiting for in-flight statements to finish.
    ///
    /// Idempotent, so the shutdown path can call it without tracking whether it already ran.
    pub async fn close(&self) {
        // Readers first: a reader can hold a snapshot the writer's checkpoint would otherwise wait
        // on, so closing in this order avoids a stall during shutdown.
        self.readers.close().await;
        self.writer.close().await;
    }
}

/// A short unique-enough suffix for isolating in-memory test databases.
///
/// Deliberately not the `uuid` crate: this is test scaffolding, and pulling a dependency into the
/// production build for it would not be justified.
fn uuid_like() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    // Truncating to the low 64 bits is fine: this only needs to be unique per test process.
    #[allow(clippy::cast_possible_truncation)]
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    nanos ^ (seq << 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_apply_to_a_fresh_database() {
        let db = Database::open_in_memory().await.unwrap();

        let tables: Vec<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .fetch_all(db.reader())
                .await
                .unwrap();

        for expected in [
            "bookmarks",
            "cache_entries",
            "channels",
            "filtering_preferences",
            "filtering_rule_sets",
            "filtering_rules",
            "history",
            "playback_positions",
            "playlist_items",
            "playlists",
            "remote_playlists",
            "search_history",
            "settings",
            "videos",
        ] {
            assert!(
                tables.iter().any(|t| t == expected),
                "missing table {expected}; got {tables:?}"
            );
        }
    }

    #[tokio::test]
    async fn migrations_are_idempotent() {
        let db = Database::open_in_memory().await.unwrap();
        db.migrate().await.unwrap();
        db.migrate().await.unwrap();
    }

    #[tokio::test]
    async fn system_playlists_exist_from_the_first_query() {
        let db = Database::open_in_memory().await.unwrap();
        let slugs: Vec<String> =
            sqlx::query_scalar("SELECT slug FROM playlists WHERE is_system = 1 ORDER BY slug")
                .fetch_all(db.reader())
                .await
                .unwrap();
        assert_eq!(
            slugs,
            vec!["favorites".to_owned(), "watch_later".to_owned()]
        );
    }

    #[tokio::test]
    async fn foreign_keys_are_enforced_on_both_pools() {
        let db = Database::open_in_memory().await.unwrap();
        for (label, pool) in [("writer", db.writer()), ("reader", db.reader())] {
            let enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
                .fetch_one(pool)
                .await
                .unwrap();
            assert_eq!(enabled, 1, "foreign keys must be on for the {label} pool");
        }
    }

    #[tokio::test]
    async fn strict_tables_reject_wrongly_typed_values() {
        let db = Database::open_in_memory().await.unwrap();
        let result = sqlx::query(
            "INSERT INTO playback_positions (video_id, position_ms, updated_at)
             VALUES ('abc', 'not-a-number', 0)",
        )
        .execute(db.writer())
        .await;
        assert!(
            result.is_err(),
            "STRICT must reject a TEXT into an INTEGER column"
        );
    }

    #[tokio::test]
    async fn check_constraints_reject_negative_positions() {
        let db = Database::open_in_memory().await.unwrap();
        let result = sqlx::query(
            "INSERT INTO playback_positions (video_id, position_ms, updated_at)
             VALUES ('abc', -1, 0)",
        )
        .execute(db.writer())
        .await;
        assert!(
            result.is_err(),
            "a negative playback position must be rejected"
        );
    }

    #[tokio::test]
    async fn deleting_a_playlist_cascades_to_its_items() {
        let db = Database::open_in_memory().await.unwrap();
        sqlx::query("INSERT INTO playlists (name, created_at, updated_at) VALUES ('Mix', 0, 0)")
            .execute(db.writer())
            .await
            .unwrap();
        let id: i64 = sqlx::query_scalar("SELECT id FROM playlists WHERE name = 'Mix'")
            .fetch_one(db.reader())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO playlist_items (playlist_id, video_id, position, added_at, title)
             VALUES (?, 'dQw4w9WgXcQ', 0, 0, 'Test')",
        )
        .bind(id)
        .execute(db.writer())
        .await
        .unwrap();

        sqlx::query("DELETE FROM playlists WHERE id = ?")
            .bind(id)
            .execute(db.writer())
            .await
            .unwrap();

        let remaining: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM playlist_items WHERE playlist_id = ?")
                .bind(id)
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(remaining, 0, "orphaned playlist items must not survive");
    }

    #[tokio::test]
    async fn evicting_a_channel_does_not_delete_its_videos() {
        let db = Database::open_in_memory().await.unwrap();
        sqlx::query("INSERT INTO channels (id, name, fetched_at) VALUES ('UCabc', 'Test', 0)")
            .execute(db.writer())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO videos (id, title, channel_id, fetched_at) VALUES ('vid1', 'T', 'UCabc', 0)",
        )
        .execute(db.writer())
        .await
        .unwrap();

        sqlx::query("DELETE FROM channels WHERE id = 'UCabc'")
            .execute(db.writer())
            .await
            .unwrap();

        let channel: Option<String> =
            sqlx::query_scalar("SELECT channel_id FROM videos WHERE id = 'vid1'")
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(channel, None, "the video must survive with a null channel");
    }

    #[tokio::test]
    async fn only_one_rule_set_can_be_active() {
        let db = Database::open_in_memory().await.unwrap();
        let insert = |version: &'static str, state: &'static str| {
            let pool = db.writer().clone();
            async move {
                sqlx::query(
                    "INSERT INTO filtering_rule_sets
                     (version, state, rule_count, checksum, source, installed_at)
                     VALUES (?, ?, 0, 'x', 'builtin', 0)",
                )
                .bind(version)
                .bind(state)
                .execute(&pool)
                .await
            }
        };

        insert("1", "active").await.unwrap();
        assert!(
            insert("2", "active").await.is_err(),
            "a second active rule set must be rejected"
        );
        // Non-active states are unconstrained, so history is retained for rollback.
        insert("3", "superseded").await.unwrap();
        insert("4", "superseded").await.unwrap();
    }

    #[tokio::test]
    async fn a_system_playlist_requires_a_slug() {
        let db = Database::open_in_memory().await.unwrap();
        let bad = sqlx::query(
            "INSERT INTO playlists (name, is_system, created_at, updated_at) VALUES ('X', 1, 0, 0)",
        )
        .execute(db.writer())
        .await;
        assert!(
            bad.is_err(),
            "a system playlist without a slug must be rejected"
        );

        let also_bad = sqlx::query(
            "INSERT INTO playlists (slug, name, is_system, created_at, updated_at)
             VALUES ('x', 'X', 0, 0, 0)",
        )
        .execute(db.writer())
        .await;
        assert!(
            also_bad.is_err(),
            "a user playlist with a slug must be rejected"
        );
    }

    #[tokio::test]
    async fn two_items_cannot_share_a_playlist_position() {
        let db = Database::open_in_memory().await.unwrap();
        sqlx::query("INSERT INTO playlists (name, created_at, updated_at) VALUES ('Mix', 0, 0)")
            .execute(db.writer())
            .await
            .unwrap();
        let id: i64 = sqlx::query_scalar("SELECT id FROM playlists WHERE name = 'Mix'")
            .fetch_one(db.reader())
            .await
            .unwrap();

        let add = |video: &'static str, position: i64| {
            let pool = db.writer().clone();
            async move {
                sqlx::query(
                    "INSERT INTO playlist_items (playlist_id, video_id, position, added_at, title)
                     VALUES (?, ?, ?, 0, 'T')",
                )
                .bind(id)
                .bind(video)
                .bind(position)
                .execute(&pool)
                .await
            }
        };

        add("a", 0).await.unwrap();
        assert!(
            add("b", 0).await.is_err(),
            "a duplicate position must fail rather than produce an ambiguous order"
        );
        add("b", 1024).await.unwrap();
    }

    #[tokio::test]
    async fn integrity_check_passes_on_a_healthy_database() {
        let db = Database::open_in_memory().await.unwrap();
        db.integrity_check().await.unwrap();
    }

    #[tokio::test]
    async fn size_is_reported_from_page_accounting() {
        let db = Database::open_in_memory().await.unwrap();
        assert!(db.size_bytes().await.unwrap() > 0);
    }

    #[tokio::test]
    async fn readers_observe_writes_made_through_the_writer_pool() {
        let db = Database::open_in_memory().await.unwrap();
        sqlx::query("INSERT INTO settings (key, value_json, updated_at) VALUES ('k', '1', 0)")
            .execute(db.writer())
            .await
            .unwrap();
        let value: String = sqlx::query_scalar("SELECT value_json FROM settings WHERE key = 'k'")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(value, "1");
    }

    #[tokio::test]
    async fn closing_is_idempotent() {
        let db = Database::open_in_memory().await.unwrap();
        db.close().await;
        db.close().await;
    }

    #[tokio::test]
    async fn opening_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deeper").join("beastube.db");
        let db = Database::open(&path).await.unwrap();
        assert!(path.exists(), "the database file should have been created");
        assert_eq!(db.path(), Some(path.as_path()));
        db.close().await;
    }

    #[tokio::test]
    async fn a_file_database_uses_wal() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("beastube.db"))
            .await
            .unwrap();
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(db.writer())
            .await
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        db.checkpoint().await.unwrap();
        db.close().await;
    }

    #[tokio::test]
    async fn concurrent_writes_serialize_without_lock_errors() {
        // The point of the single-writer pool: many concurrent writers queue rather than failing
        // with SQLITE_BUSY.
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("beastube.db"))
            .await
            .unwrap();

        let mut handles = Vec::new();
        for i in 0..32 {
            let db = db.clone();
            handles.push(tokio::spawn(async move {
                sqlx::query("INSERT INTO settings (key, value_json, updated_at) VALUES (?, '1', 0)")
                    .bind(format!("key-{i}"))
                    .execute(db.writer())
                    .await
            }));
        }
        for handle in handles {
            handle
                .await
                .unwrap()
                .expect("no write should fail with SQLITE_BUSY");
        }

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM settings")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(count, 32);
        db.close().await;
    }
}
