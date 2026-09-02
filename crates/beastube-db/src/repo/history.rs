//! Watch history.
//!
//! ## Why recording is an upsert, not an append
//!
//! History is keyed by video, not by view. Appending one row per view would make "your history"
//! a list in which the same video appears fourteen times, which is what users are actually asking
//! to be rid of when they ask for a cleaner history. So a re-watch bumps `last_watched_at` and
//! increments `play_count` on the existing row, and `first_watched_at` is never rewritten — that
//! field is what makes "you first watched this two years ago" possible.
//!
//! ## Why this repository has no opinion about privacy
//!
//! Whether history should be recorded at all is decided by the caller from
//! [`beastube_core::Settings`] and from whether the session is incognito. The repository records
//! what it is told. Duplicating the check here would mean two places that can disagree, and the
//! one that is wrong is the one that silently keeps recording.
//!
//! ## Deleting history deletes the resume position with it
//!
//! `playback_positions` is a separate table for write-throughput reasons, but a resume position is
//! evidence that a video was watched. Removing a history row while leaving the position behind
//! would mean "delete from history" did not delete what the user meant, so every deletion here
//! takes both rows in one transaction.

use beastube_core::ids::{ChannelId, VideoId};
use beastube_core::model::library::{HistoryEntry, PlaybackPosition};
use beastube_core::model::thumbnail::ThumbnailSet;
use beastube_core::model::video::VideoSummary;
use beastube_core::time_util::Timestamp;
use sqlx::sqlite::SqliteRow;

use crate::connection::Database;
use crate::error::{DbError, DbResult};

use super::{
    column, decode_channel_id, decode_thumbnails, decode_video_id, degrade, encode_thumbnails,
    from_db_count, from_db_millis, from_db_millis_opt, from_db_timestamp, like_contains,
    search_text, to_db_limit, to_db_millis_opt,
};

/// Maximum stored length of a denormalized title or channel name.
///
/// Provider titles are bounded in practice, but a drifted response could carry a large string into
/// every list query. Truncating on write keeps the read path predictable.
const MAX_TEXT_LEN: usize = 512;

/// What the caller knows about a video at the moment playback starts.
///
/// Carries a copy of the display metadata rather than a reference to the cache, because history
/// must render offline and after a cache clear (§72).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchRecord {
    /// The video being watched.
    pub video_id: VideoId,
    /// Title at the time of watching. Untrusted text.
    pub title: String,
    /// Owning channel, when known.
    pub channel_id: Option<ChannelId>,
    /// Channel name at the time of watching. Untrusted text.
    pub channel_name: Option<String>,
    /// Thumbnails at the time of watching.
    pub thumbnails: ThumbnailSet,
    /// Total duration, when known.
    pub duration_ms: Option<u64>,
}

impl WatchRecord {
    /// Builds a record from the summary the player was opened with.
    #[must_use]
    pub fn from_summary(summary: &VideoSummary) -> Self {
        Self {
            video_id: summary.id.clone(),
            title: summary.title.clone(),
            channel_id: summary.channel_id.clone(),
            channel_name: summary.channel_name.clone(),
            thumbnails: summary.thumbnails.clone(),
            duration_ms: summary.duration_ms,
        }
    }
}

/// Column list shared by every read, including the joined resume position.
///
/// `playback_positions` is joined rather than duplicated so that a checkpoint written every few
/// seconds touches one narrow row, while history reads still return a complete
/// [`HistoryEntry`] with its progress.
/// Columns and joins shared by every history query.
///
/// A macro rather than a `const` so call sites can assemble their full statement with
/// `concat!`, keeping the SQL a compile-time literal. sqlx 0.9 refuses a runtime `String`
/// as a query, and rightly so: that refusal is what makes SQL injection structurally
/// impossible here rather than merely unlikely.
macro_rules! select_entry {
    () => {
        "
            SELECT h.video_id        AS video_id,
                   h.title           AS title,
                   h.channel_id      AS channel_id,
                   h.channel_name    AS channel_name,
                   h.thumbnails_json AS thumbnails_json,
                   h.duration_ms     AS duration_ms,
                   h.first_watched_at,
                   h.last_watched_at,
                   h.play_count,
                   p.position_ms     AS position_ms,
                   p.duration_ms     AS position_duration_ms,
                   p.updated_at      AS position_updated_at
            FROM history h
            LEFT JOIN playback_positions p ON p.video_id = h.video_id
        "
    };
}

/// Reads and writes the local watch history.
#[derive(Debug, Clone)]
pub struct HistoryRepo {
    db: Database,
}

impl HistoryRepo {
    /// Binds the repository to a database handle.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Records that playback started, inserting a new entry or bumping an existing one.
    ///
    /// On an existing entry the display metadata is refreshed (titles get corrected, channels get
    /// renamed) but `first_watched_at` is preserved and `play_count` is incremented. A duration of
    /// `None` never overwrites a known duration: a re-watch begun before the metadata arrived must
    /// not erase the progress bar.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] if the title is blank, or a [`DbError`] if the write fails.
    pub async fn record_watch(&self, record: &WatchRecord, at: Timestamp) -> DbResult<()> {
        super::require_non_blank("title", &record.title)?;
        let title = truncate(&record.title);
        let channel_name = record.channel_name.as_deref().map(truncate);
        let search = search_text(&title, channel_name.as_deref());

        sqlx::query(
            "INSERT INTO history
                 (video_id, title, channel_id, channel_name, thumbnails_json, duration_ms,
                  first_watched_at, last_watched_at, play_count, search_text)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?)
             ON CONFLICT(video_id) DO UPDATE SET
                 title           = excluded.title,
                 channel_id      = excluded.channel_id,
                 channel_name    = excluded.channel_name,
                 thumbnails_json = excluded.thumbnails_json,
                 duration_ms     = COALESCE(excluded.duration_ms, history.duration_ms),
                 last_watched_at = excluded.last_watched_at,
                 play_count      = history.play_count + 1,
                 search_text     = excluded.search_text",
        )
        .bind(record.video_id.as_str())
        .bind(&title)
        .bind(record.channel_id.as_ref().map(ChannelId::as_str))
        .bind(channel_name)
        .bind(encode_thumbnails(&record.thumbnails)?)
        .bind(to_db_millis_opt(record.duration_ms))
        .bind(at.as_millis())
        .bind(at.as_millis())
        .bind(search)
        .execute(self.db.writer())
        .await
        .map_err(DbError::from_sqlx)?;
        Ok(())
    }

    /// Most recently watched entries first.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails. Individual rows that cannot be decoded are
    /// skipped rather than failing the page.
    pub async fn list(&self, limit: u32, offset: u32) -> DbResult<Vec<HistoryEntry>> {
        let sql = concat!(
            select_entry!(),
            " ORDER BY h.last_watched_at DESC, h.video_id LIMIT ? OFFSET ?"
        );
        let rows = sqlx::query(sql)
            .bind(to_db_limit(limit))
            .bind(to_db_limit(offset))
            .fetch_all(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(map_entries(&rows))
    }

    /// Entries whose title or channel contains `query`, case-insensitively.
    ///
    /// An empty or blank query matches nothing rather than everything: a cleared search box should
    /// show no results, not silently page the entire library through the UI.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails.
    pub async fn search(&self, query: &str, limit: u32) -> DbResult<Vec<HistoryEntry>> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let sql = concat!(
            select_entry!(),
            " WHERE h.search_text LIKE ? ESCAPE '\\'
             ORDER BY h.last_watched_at DESC, h.video_id LIMIT ?"
        );
        let rows = sqlx::query(sql)
            .bind(like_contains(&needle))
            .bind(to_db_limit(limit))
            .fetch_all(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(map_entries(&rows))
    }

    /// Entries belonging to one channel, newest first.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails.
    pub async fn by_channel(
        &self,
        channel: &ChannelId,
        limit: u32,
        offset: u32,
    ) -> DbResult<Vec<HistoryEntry>> {
        let sql = concat!(
            select_entry!(),
            " WHERE h.channel_id = ?
             ORDER BY h.last_watched_at DESC, h.video_id LIMIT ? OFFSET ?"
        );
        let rows = sqlx::query(sql)
            .bind(channel.as_str())
            .bind(to_db_limit(limit))
            .bind(to_db_limit(offset))
            .fetch_all(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(map_entries(&rows))
    }

    /// One entry by video, or `None` if it has never been watched.
    ///
    /// Unlike the listing methods this reports a decode failure, because the caller asked for this
    /// specific row and silently returning "not watched" would be a lie.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails or the row cannot be decoded.
    pub async fn get(&self, video: &VideoId) -> DbResult<Option<HistoryEntry>> {
        let sql = concat!(select_entry!(), " WHERE h.video_id = ?");
        let row = sqlx::query(sql)
            .bind(video.as_str())
            .fetch_optional(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        row.as_ref().map(entry_from_row).transpose()
    }

    /// Number of videos in history.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails.
    pub async fn count(&self) -> DbResult<u64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM history")
            .fetch_one(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(from_db_count(count))
    }

    /// Removes one video from history along with its resume position.
    ///
    /// Returns whether an entry was present.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the transaction fails.
    pub async fn delete(&self, video: &VideoId) -> DbResult<bool> {
        let mut tx = self.db.writer().begin().await.map_err(DbError::from_sqlx)?;
        sqlx::query("DELETE FROM playback_positions WHERE video_id = ?")
            .bind(video.as_str())
            .execute(&mut *tx)
            .await
            .map_err(DbError::from_sqlx)?;
        let removed = sqlx::query("DELETE FROM history WHERE video_id = ?")
            .bind(video.as_str())
            .execute(&mut *tx)
            .await
            .map_err(DbError::from_sqlx)?
            .rows_affected();
        tx.commit().await.map_err(DbError::from_sqlx)?;
        Ok(removed > 0)
    }

    /// Deletes all history and every resume position, returning how many entries were removed.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the transaction fails.
    pub async fn clear(&self) -> DbResult<u64> {
        let mut tx = self.db.writer().begin().await.map_err(DbError::from_sqlx)?;
        sqlx::query("DELETE FROM playback_positions")
            .execute(&mut *tx)
            .await
            .map_err(DbError::from_sqlx)?;
        let removed = sqlx::query("DELETE FROM history")
            .execute(&mut *tx)
            .await
            .map_err(DbError::from_sqlx)?
            .rows_affected();
        tx.commit().await.map_err(DbError::from_sqlx)?;
        Ok(removed)
    }

    /// Deletes entries last watched strictly before `cutoff`, with their resume positions.
    ///
    /// Implements the retention window from
    /// [`beastube_core::settings::PrivacySettings::history_retention_days`]; the caller converts
    /// days into an instant so that the policy and the clock stay in one place.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the transaction fails.
    pub async fn prune(&self, cutoff: Timestamp) -> DbResult<u64> {
        let mut tx = self.db.writer().begin().await.map_err(DbError::from_sqlx)?;
        // Positions first: once the history rows are gone the identifiers cannot be recovered.
        sqlx::query(
            "DELETE FROM playback_positions
             WHERE video_id IN (SELECT video_id FROM history WHERE last_watched_at < ?)",
        )
        .bind(cutoff.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(DbError::from_sqlx)?;
        let removed = sqlx::query("DELETE FROM history WHERE last_watched_at < ?")
            .bind(cutoff.as_millis())
            .execute(&mut *tx)
            .await
            .map_err(DbError::from_sqlx)?
            .rows_affected();
        tx.commit().await.map_err(DbError::from_sqlx)?;
        Ok(removed)
    }
}

/// Truncates untrusted display text to [`MAX_TEXT_LEN`] characters.
///
/// Cuts on a character boundary, never a byte one, so a multi-byte title cannot be stored as
/// invalid UTF-8.
fn truncate(raw: &str) -> String {
    raw.chars().take(MAX_TEXT_LEN).collect()
}

/// Maps a result set, dropping rows that cannot be decoded.
fn map_entries(rows: &[SqliteRow]) -> Vec<HistoryEntry> {
    rows.iter()
        .filter_map(|row| degrade(entry_from_row(row), "history"))
        .collect()
}

/// Builds a [`HistoryEntry`] from a row of [`SELECT_ENTRY`].
///
/// # Errors
///
/// Returns [`DbError::Corrupt`] if the stored video identifier no longer validates, or a
/// [`DbError`] if a column cannot be read.
fn entry_from_row(row: &SqliteRow) -> DbResult<HistoryEntry> {
    let raw_id: String = column(row, "video_id")?;
    let last_watched_at = from_db_timestamp(column::<i64>(row, "last_watched_at")?);
    let history_duration = from_db_millis_opt(column::<Option<i64>>(row, "duration_ms")?);
    let play_count: i64 = column(row, "play_count")?;

    Ok(HistoryEntry {
        video_id: decode_video_id("history", &raw_id)?,
        title: column(row, "title")?,
        channel_id: decode_channel_id(
            "history",
            column::<Option<String>>(row, "channel_id")?.as_deref(),
        ),
        channel_name: column(row, "channel_name")?,
        thumbnails: decode_thumbnails(
            "history",
            column::<Option<String>>(row, "thumbnails_json")?.as_deref(),
        ),
        position: position_from_row(row, history_duration, last_watched_at)?,
        first_watched_at: from_db_timestamp(column::<i64>(row, "first_watched_at")?),
        last_watched_at,
        play_count: u32::try_from(play_count).unwrap_or(u32::MAX),
    })
}

/// Builds the joined resume position, synthesizing one when no checkpoint has been written.
///
/// The synthesized position is at zero with the history row's duration, which reads as
/// [`beastube_core::model::library::WatchState::Unwatched`] — correct for a video that was opened
/// but never progressed far enough for a checkpoint.
///
/// # Errors
///
/// Returns a [`DbError`] if a column cannot be read.
fn position_from_row(
    row: &SqliteRow,
    history_duration: Option<u64>,
    fallback_updated_at: Timestamp,
) -> DbResult<PlaybackPosition> {
    let position_ms: Option<i64> = column(row, "position_ms")?;
    let position_duration = from_db_millis_opt(column::<Option<i64>>(row, "position_duration_ms")?);
    let updated_at: Option<i64> = column(row, "position_updated_at")?;

    Ok(PlaybackPosition {
        position_ms: position_ms.map_or(0, from_db_millis),
        // The checkpoint's duration wins when present: it was written by the player, which knows
        // the real length, whereas the history row's copy came from a list card.
        duration_ms: position_duration.or(history_duration),
        updated_at: updated_at.map_or(fallback_updated_at, from_db_timestamp),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use beastube_core::model::thumbnail::Thumbnail;

    fn video(id: &str) -> VideoId {
        VideoId::new(id).expect("valid test identifier")
    }

    fn record(id: &str, title: &str) -> WatchRecord {
        WatchRecord {
            video_id: video(id),
            title: title.to_owned(),
            channel_id: ChannelId::new("UCtestchannel").ok(),
            channel_name: Some("Test Channel".to_owned()),
            thumbnails: ThumbnailSet::empty(),
            duration_ms: Some(600_000),
        }
    }

    async fn repo() -> (Database, HistoryRepo) {
        let db = Database::open_in_memory()
            .await
            .expect("in-memory database");
        let repo = HistoryRepo::new(db.clone());
        (db, repo)
    }

    #[tokio::test]
    async fn rewatching_bumps_the_entry_instead_of_duplicating_it() {
        let (_db, repo) = repo().await;
        repo.record_watch(
            &record("aaaaaaaaaaa", "First title"),
            Timestamp::from_millis(100),
        )
        .await
        .unwrap();
        repo.record_watch(
            &record("aaaaaaaaaaa", "Corrected title"),
            Timestamp::from_millis(500),
        )
        .await
        .unwrap();

        assert_eq!(repo.count().await.unwrap(), 1);
        let entry = repo
            .get(&video("aaaaaaaaaaa"))
            .await
            .unwrap()
            .expect("entry");
        assert_eq!(entry.play_count, 2);
        assert_eq!(entry.title, "Corrected title", "metadata is refreshed");
        assert_eq!(
            entry.first_watched_at,
            Timestamp::from_millis(100),
            "the first watch is history's whole point and must survive"
        );
        assert_eq!(entry.last_watched_at, Timestamp::from_millis(500));
    }

    #[tokio::test]
    async fn a_rewatch_without_metadata_does_not_erase_a_known_duration() {
        let (_db, repo) = repo().await;
        repo.record_watch(&record("aaaaaaaaaaa", "T"), Timestamp::from_millis(1))
            .await
            .unwrap();

        let mut unknown = record("aaaaaaaaaaa", "T");
        unknown.duration_ms = None;
        repo.record_watch(&unknown, Timestamp::from_millis(2))
            .await
            .unwrap();

        let entry = repo
            .get(&video("aaaaaaaaaaa"))
            .await
            .unwrap()
            .expect("entry");
        assert_eq!(entry.position.duration_ms, Some(600_000));
    }

    #[tokio::test]
    async fn listing_is_newest_first_and_pages() {
        let (_db, repo) = repo().await;
        for (index, id) in ["aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc"]
            .iter()
            .enumerate()
        {
            let at = Timestamp::from_millis(i64::try_from(index).unwrap() * 1_000);
            repo.record_watch(&record(id, "T"), at).await.unwrap();
        }

        let page = repo.list(2, 0).await.unwrap();
        let ids: Vec<&str> = page.iter().map(|e| e.video_id.as_str()).collect();
        assert_eq!(ids, vec!["ccccccccccc", "bbbbbbbbbbb"]);

        let second = repo.list(2, 2).await.unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].video_id.as_str(), "aaaaaaaaaaa");
    }

    #[tokio::test]
    async fn an_empty_history_pages_without_error() {
        let (_db, repo) = repo().await;
        assert!(repo.list(50, 0).await.unwrap().is_empty());
        assert!(repo.list(0, 0).await.unwrap().is_empty());
        assert!(repo.list(50, 9_999).await.unwrap().is_empty());
        assert_eq!(repo.count().await.unwrap(), 0);
        assert!(repo.get(&video("aaaaaaaaaaa")).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn search_matches_title_and_channel_case_insensitively() {
        let (_db, repo) = repo().await;
        let mut entry = record("aaaaaaaaaaa", "Donut Tutorial");
        entry.channel_name = Some("Blender Guru".to_owned());
        repo.record_watch(&entry, Timestamp::from_millis(1))
            .await
            .unwrap();

        assert_eq!(repo.search("donut", 10).await.unwrap().len(), 1);
        assert_eq!(repo.search("DONUT", 10).await.unwrap().len(), 1);
        assert_eq!(repo.search("guru", 10).await.unwrap().len(), 1);
        assert!(repo.search("nothing here", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_does_not_match_across_the_title_channel_boundary() {
        let (_db, repo) = repo().await;
        let mut entry = record("aaaaaaaaaaa", "Blender");
        entry.channel_name = Some("Guru".to_owned());
        repo.record_watch(&entry, Timestamp::from_millis(1))
            .await
            .unwrap();
        assert!(
            repo.search("blender guru", 10).await.unwrap().is_empty(),
            "adjacency across the separator must not count as a match"
        );
    }

    #[tokio::test]
    async fn a_blank_search_returns_nothing_rather_than_the_whole_library() {
        let (_db, repo) = repo().await;
        repo.record_watch(&record("aaaaaaaaaaa", "T"), Timestamp::EPOCH)
            .await
            .unwrap();
        assert!(repo.search("", 10).await.unwrap().is_empty());
        assert!(repo.search("   ", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_treats_like_wildcards_as_literal_text() {
        let (_db, repo) = repo().await;
        repo.record_watch(&record("aaaaaaaaaaa", "100% real"), Timestamp::EPOCH)
            .await
            .unwrap();
        repo.record_watch(&record("bbbbbbbbbbb", "1000 things"), Timestamp::EPOCH)
            .await
            .unwrap();

        let hits = repo.search("100%", 10).await.unwrap();
        assert_eq!(hits.len(), 1, "`%` must not act as a wildcard");
        assert_eq!(hits[0].video_id.as_str(), "aaaaaaaaaaa");

        // A bare `%` is a literal search for a per-cent sign, not "match everything": it finds the
        // one title containing the character and leaves the rest of the history alone.
        let bare = repo.search("%", 10).await.unwrap();
        assert_eq!(
            bare.len(),
            1,
            "a bare wildcard must not select the entire history"
        );
        assert_eq!(bare[0].video_id.as_str(), "aaaaaaaaaaa");

        // `_` is the other LIKE metacharacter, and must be just as literal.
        repo.record_watch(&record("ccccccccccc", "under_score"), Timestamp::EPOCH)
            .await
            .unwrap();
        let underscore = repo.search("_", 10).await.unwrap();
        assert_eq!(
            underscore.len(),
            1,
            "`_` must not match any single character"
        );
        assert_eq!(underscore[0].video_id.as_str(), "ccccccccccc");
    }

    #[tokio::test]
    async fn entries_can_be_listed_by_channel() {
        let (_db, repo) = repo().await;
        let mut other = record("bbbbbbbbbbb", "T");
        other.channel_id = ChannelId::new("UCother").ok();
        repo.record_watch(&record("aaaaaaaaaaa", "T"), Timestamp::EPOCH)
            .await
            .unwrap();
        repo.record_watch(&other, Timestamp::EPOCH).await.unwrap();

        let channel = ChannelId::new("UCtestchannel").unwrap();
        let entries = repo.by_channel(&channel, 10, 0).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].video_id.as_str(), "aaaaaaaaaaa");
    }

    #[tokio::test]
    async fn the_joined_position_drives_resume_state() {
        let (db, repo) = repo().await;
        repo.record_watch(&record("aaaaaaaaaaa", "T"), Timestamp::from_millis(10))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO playback_positions (video_id, position_ms, duration_ms, updated_at)
             VALUES ('aaaaaaaaaaa', 120000, 600000, 99)",
        )
        .execute(db.writer())
        .await
        .unwrap();

        let entry = repo
            .get(&video("aaaaaaaaaaa"))
            .await
            .unwrap()
            .expect("entry");
        assert_eq!(entry.position.position_ms, 120_000);
        assert_eq!(entry.position.updated_at, Timestamp::from_millis(99));
        assert!(entry.is_resumable());
    }

    #[tokio::test]
    async fn an_entry_without_a_checkpoint_reads_as_unwatched_not_missing() {
        let (_db, repo) = repo().await;
        repo.record_watch(&record("aaaaaaaaaaa", "T"), Timestamp::from_millis(7))
            .await
            .unwrap();
        let entry = repo
            .get(&video("aaaaaaaaaaa"))
            .await
            .unwrap()
            .expect("entry");
        assert_eq!(entry.position.position_ms, 0);
        assert_eq!(entry.position.duration_ms, Some(600_000));
        assert_eq!(entry.position.updated_at, Timestamp::from_millis(7));
        assert!(!entry.is_resumable());
    }

    #[tokio::test]
    async fn deleting_an_entry_also_deletes_the_resume_position() {
        let (db, repo) = repo().await;
        repo.record_watch(&record("aaaaaaaaaaa", "T"), Timestamp::EPOCH)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO playback_positions (video_id, position_ms, updated_at)
             VALUES ('aaaaaaaaaaa', 5000, 0)",
        )
        .execute(db.writer())
        .await
        .unwrap();

        assert!(repo.delete(&video("aaaaaaaaaaa")).await.unwrap());
        let positions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playback_positions")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(
            positions, 0,
            "a leftover resume position is still evidence the video was watched"
        );
        assert!(!repo.delete(&video("aaaaaaaaaaa")).await.unwrap());
    }

    #[tokio::test]
    async fn clearing_removes_history_and_positions_together() {
        let (db, repo) = repo().await;
        for id in ["aaaaaaaaaaa", "bbbbbbbbbbb"] {
            repo.record_watch(&record(id, "T"), Timestamp::EPOCH)
                .await
                .unwrap();
        }
        sqlx::query(
            "INSERT INTO playback_positions (video_id, position_ms, updated_at)
             VALUES ('aaaaaaaaaaa', 5000, 0)",
        )
        .execute(db.writer())
        .await
        .unwrap();

        assert_eq!(repo.clear().await.unwrap(), 2);
        assert_eq!(repo.count().await.unwrap(), 0);
        let positions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playback_positions")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(positions, 0);
        assert_eq!(repo.clear().await.unwrap(), 0, "clearing twice is harmless");
    }

    #[tokio::test]
    async fn pruning_keeps_entries_at_the_cutoff_and_drops_older_ones() {
        let (_db, repo) = repo().await;
        repo.record_watch(&record("aaaaaaaaaaa", "T"), Timestamp::from_millis(100))
            .await
            .unwrap();
        repo.record_watch(&record("bbbbbbbbbbb", "T"), Timestamp::from_millis(200))
            .await
            .unwrap();

        assert_eq!(repo.prune(Timestamp::from_millis(200)).await.unwrap(), 1);
        let remaining = repo.list(10, 0).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].video_id.as_str(), "bbbbbbbbbbb");
    }

    #[tokio::test]
    async fn a_blank_title_is_rejected_before_reaching_sql() {
        let (_db, repo) = repo().await;
        let err = repo
            .record_watch(&record("aaaaaaaaaaa", "  "), Timestamp::EPOCH)
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::Invalid { field: "title", .. }));
    }

    #[tokio::test]
    async fn overlong_text_is_truncated_on_a_character_boundary() {
        let (_db, repo) = repo().await;
        let mut long = record("aaaaaaaaaaa", &"é".repeat(MAX_TEXT_LEN + 50));
        long.channel_name = Some("ü".repeat(MAX_TEXT_LEN + 50));
        repo.record_watch(&long, Timestamp::EPOCH).await.unwrap();

        let entry = repo
            .get(&video("aaaaaaaaaaa"))
            .await
            .unwrap()
            .expect("entry");
        assert_eq!(entry.title.chars().count(), MAX_TEXT_LEN);
        assert_eq!(
            entry.channel_name.as_deref().map(|c| c.chars().count()),
            Some(MAX_TEXT_LEN)
        );
    }

    #[tokio::test]
    async fn a_corrupt_identifier_costs_its_row_not_the_page() {
        let (db, repo) = repo().await;
        repo.record_watch(&record("aaaaaaaaaaa", "Good"), Timestamp::from_millis(2))
            .await
            .unwrap();
        // A row an older or broken build could have written: the identifier no longer validates.
        sqlx::query(
            "INSERT INTO history (video_id, title, first_watched_at, last_watched_at, search_text)
             VALUES ('../etc/passwd', 'Hostile', 1, 1, 'hostile\n')",
        )
        .execute(db.writer())
        .await
        .unwrap();

        let entries = repo.list(10, 0).await.unwrap();
        assert_eq!(entries.len(), 1, "the intact row must still render");
        assert_eq!(entries[0].title, "Good");
    }

    #[tokio::test]
    async fn a_corrupt_thumbnail_blob_costs_the_images_not_the_row() {
        let (db, repo) = repo().await;
        let mut with_art = record("aaaaaaaaaaa", "T");
        with_art.thumbnails =
            ThumbnailSet::new(vec![Thumbnail::sized("https://example.com/a.jpg", 120, 90)]);
        repo.record_watch(&with_art, Timestamp::EPOCH)
            .await
            .unwrap();
        sqlx::query("UPDATE history SET thumbnails_json = '{truncated' WHERE video_id = ?")
            .bind("aaaaaaaaaaa")
            .execute(db.writer())
            .await
            .unwrap();

        let entry = repo
            .get(&video("aaaaaaaaaaa"))
            .await
            .unwrap()
            .expect("entry");
        assert!(entry.thumbnails.is_empty());
        assert_eq!(entry.title, "T");
    }

    #[tokio::test]
    async fn concurrent_rewatches_of_one_video_never_lose_a_count() {
        let (_db, repo) = repo().await;
        let mut handles = Vec::new();
        for i in 0..24_i64 {
            let repo = repo.clone();
            handles.push(tokio::spawn(async move {
                repo.record_watch(&record("aaaaaaaaaaa", "T"), Timestamp::from_millis(i))
                    .await
            }));
        }
        for handle in handles {
            handle.await.unwrap().expect("no write should be lost");
        }

        let entry = repo
            .get(&video("aaaaaaaaaaa"))
            .await
            .unwrap()
            .expect("entry");
        assert_eq!(
            entry.play_count, 24,
            "the single-writer pool must serialize the increment, not drop it"
        );
        assert_eq!(repo.count().await.unwrap(), 1);
    }
}
