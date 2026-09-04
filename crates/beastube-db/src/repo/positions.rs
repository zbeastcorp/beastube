//! Playback positions.
//!
//! This is the hottest write path in the application: a checkpoint lands every few seconds of
//! playback. The table is deliberately narrow and separate from `history` so each checkpoint
//! rewrites one small row, keeping the write-ahead log small during a long session.
//!
//! ## Where the resume rule lives
//!
//! Whether a stored position is worth resuming from is decided by
//! [`PlaybackPosition::resume_at_ms`] and nowhere else: it encodes "a finished video restarts" and
//! "a barely-started video restarts", and every entry point in the application must agree on both.
//!
//! [`PositionsRepo::resumable`] therefore **filters in Rust, not in SQL**. The rule involves a
//! fractional completion threshold, an absolute tail window and a minimum position; expressing it
//! in SQL would mean transcribing three constants and a float comparison into integer arithmetic,
//! and the two copies would drift the first time the threshold is tuned. What SQL does instead is
//! a *necessary* prefilter — `position_ms >= MIN_RESUME_POSITION_MS`, referencing the same
//! constant — so the scan is bounded rather than reading the whole table.
//!
//! Because the prefilter is necessary but not sufficient, the query pages through candidates with
//! a keyset cursor until it has collected the requested number or run out. A wall of completed
//! videos in front of the resumable ones therefore costs extra batches, not a short result.

use beastube_core::ids::VideoId;
use beastube_core::model::library::{MIN_RESUME_POSITION_MS, PlaybackPosition};
use beastube_core::time_util::Timestamp;
use sqlx::sqlite::SqliteRow;

use crate::connection::Database;
use crate::error::{DbError, DbResult};

use super::{
    column, decode_video_id, degrade, from_db_count, from_db_millis, from_db_millis_opt,
    from_db_timestamp, to_db_limit, to_db_millis, to_db_millis_opt,
};

/// How many candidate rows one page of the resume scan fetches.
///
/// Large enough that the common case — the newest checkpoints are resumable — completes in a
/// single round trip, small enough that a library full of finished videos does not pull thousands
/// of rows into memory to answer a request for ten.
const CANDIDATE_BATCH: u32 = 64;

/// Upper bound on one candidate batch, so a caller asking for a huge limit still pages.
const MAX_CANDIDATE_BATCH: u32 = 512;

/// Reads and writes playback checkpoints.
#[derive(Debug, Clone)]
pub struct PositionsRepo {
    db: Database,
}

impl PositionsRepo {
    /// Binds the repository to a database handle.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Writes a checkpoint, replacing any previous one for this video.
    ///
    /// A `duration_ms` of `None` never overwrites a known duration: checkpoints written before the
    /// player has parsed the manifest would otherwise erase the progress bar for the rest of the
    /// session.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the write fails.
    pub async fn upsert(
        &self,
        video: &VideoId,
        position_ms: u64,
        duration_ms: Option<u64>,
        at: Timestamp,
    ) -> DbResult<()> {
        sqlx::query(
            "INSERT INTO playback_positions (video_id, position_ms, duration_ms, updated_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(video_id) DO UPDATE SET
                 position_ms = excluded.position_ms,
                 duration_ms = COALESCE(excluded.duration_ms, playback_positions.duration_ms),
                 updated_at  = excluded.updated_at",
        )
        .bind(video.as_str())
        .bind(to_db_millis(position_ms))
        .bind(to_db_millis_opt(duration_ms))
        .bind(at.as_millis())
        .execute(self.db.writer())
        .await
        .map_err(DbError::from_sqlx)?;
        Ok(())
    }

    /// The stored checkpoint for one video, or `None` if there is none.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails.
    pub async fn get(&self, video: &VideoId) -> DbResult<Option<PlaybackPosition>> {
        let row = sqlx::query(
            "SELECT position_ms, duration_ms, updated_at
             FROM playback_positions WHERE video_id = ?",
        )
        .bind(video.as_str())
        .fetch_optional(self.db.reader())
        .await
        .map_err(DbError::from_sqlx)?;

        row.as_ref().map(position_from_row).transpose()
    }

    /// Videos worth resuming, most recently played first.
    ///
    /// "Worth resuming" is exactly [`PlaybackPosition::resume_at_ms`] returning `Some`, so a
    /// finished video and a barely-started one are both excluded. Note this is a weaker condition
    /// than [`beastube_core::model::library::HistoryEntry::is_resumable`], which additionally
    /// demands a known duration so the surface can draw a progress bar; a caller building
    /// "Continue watching" should apply that check too.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if a query fails. Rows that cannot be decoded are skipped.
    pub async fn resumable(&self, limit: u32) -> DbResult<Vec<(VideoId, PlaybackPosition)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let batch = limit.clamp(CANDIDATE_BATCH, MAX_CANDIDATE_BATCH);
        let mut found: Vec<(VideoId, PlaybackPosition)> = Vec::new();
        let mut cursor: Option<(i64, String)> = None;

        while found.len() < limit as usize {
            let rows = self.candidate_page(cursor.as_ref(), batch).await?;
            let exhausted = rows.len() < batch as usize;

            for row in &rows {
                // The cursor advances from the raw columns, before anything is decoded. It has to:
                // a row that fails to decode is skipped, and if it were skipped *before* the cursor
                // moved then a page whose rows all failed would leave the cursor untouched, return
                // a full page every time, and loop forever. The comment below used to claim this
                // already happened; it did not, because the `continue` jumped over the assignment.
                if let (Ok(updated_at), Ok(raw_id)) = (
                    column::<i64>(row, "updated_at"),
                    column::<String>(row, "video_id"),
                ) {
                    cursor = Some((updated_at, raw_id));
                }
                let Some(entry) = degrade(candidate_from_row(row), "playback_positions") else {
                    continue;
                };
                if entry.1.resume_at_ms().is_some() {
                    found.push(entry);
                    if found.len() == limit as usize {
                        return Ok(found);
                    }
                }
            }

            // Every row advanced the cursor above, decodable or not, so an all-undecodable page
            // moves the scan on rather than spinning; an exhausted page ends it.
            if exhausted || rows.is_empty() {
                break;
            }
        }
        Ok(found)
    }

    /// Fetches one keyset-paged batch of resume candidates.
    ///
    /// Keyset rather than `OFFSET` because a checkpoint landing mid-scan would shift an offset and
    /// make the scan skip or repeat a row.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails.
    async fn candidate_page(
        &self,
        cursor: Option<&(i64, String)>,
        batch: u32,
    ) -> DbResult<Vec<SqliteRow>> {
        // Written out in full rather than composed at runtime: sqlx 0.9 accepts only
        // `&'static str`, which is what keeps every statement in this crate injection-proof by
        // construction. The duplication between the two arms is the price, and it is small.
        const FIRST_PAGE: &str = "SELECT video_id, position_ms, duration_ms, updated_at
             FROM playback_positions
             WHERE position_ms >= ?
             ORDER BY updated_at DESC, video_id ASC LIMIT ?";
        const NEXT_PAGE: &str = "SELECT video_id, position_ms, duration_ms, updated_at
             FROM playback_positions
             WHERE position_ms >= ?
               AND (updated_at < ? OR (updated_at = ? AND video_id > ?))
             ORDER BY updated_at DESC, video_id ASC LIMIT ?";

        let minimum = to_db_millis(MIN_RESUME_POSITION_MS);
        let query = match cursor {
            None => sqlx::query(FIRST_PAGE).bind(minimum),
            Some((updated_at, video_id)) => sqlx::query(NEXT_PAGE)
                .bind(minimum)
                .bind(updated_at)
                .bind(updated_at)
                .bind(video_id),
        };

        query
            .bind(to_db_limit(batch))
            .fetch_all(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)
    }

    /// Number of stored checkpoints.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails.
    pub async fn count(&self) -> DbResult<u64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playback_positions")
            .fetch_one(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(from_db_count(count))
    }

    /// Forgets one video's checkpoint, returning whether there was one.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the delete fails.
    pub async fn delete(&self, video: &VideoId) -> DbResult<bool> {
        let removed = sqlx::query("DELETE FROM playback_positions WHERE video_id = ?")
            .bind(video.as_str())
            .execute(self.db.writer())
            .await
            .map_err(DbError::from_sqlx)?
            .rows_affected();
        Ok(removed > 0)
    }

    /// Forgets every checkpoint, returning how many were removed.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the delete fails.
    pub async fn clear(&self) -> DbResult<u64> {
        let removed = sqlx::query("DELETE FROM playback_positions")
            .execute(self.db.writer())
            .await
            .map_err(DbError::from_sqlx)?
            .rows_affected();
        Ok(removed)
    }
}

/// Builds a [`PlaybackPosition`] from a row carrying the three position columns.
///
/// # Errors
///
/// Returns a [`DbError`] if a column cannot be read.
fn position_from_row(row: &SqliteRow) -> DbResult<PlaybackPosition> {
    Ok(PlaybackPosition {
        position_ms: from_db_millis(column::<i64>(row, "position_ms")?),
        duration_ms: from_db_millis_opt(column::<Option<i64>>(row, "duration_ms")?),
        updated_at: from_db_timestamp(column::<i64>(row, "updated_at")?),
    })
}

/// Builds a candidate pair, revalidating the stored identifier.
///
/// # Errors
///
/// Returns [`DbError::Corrupt`] if the identifier no longer validates, or a [`DbError`] if a
/// column cannot be read.
fn candidate_from_row(row: &SqliteRow) -> DbResult<(VideoId, PlaybackPosition)> {
    let raw_id: String = column(row, "video_id")?;
    Ok((
        decode_video_id("playback_positions", &raw_id)?,
        position_from_row(row)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(id: &str) -> VideoId {
        VideoId::new(id).expect("valid test identifier")
    }

    async fn repo() -> (Database, PositionsRepo) {
        let db = Database::open_in_memory()
            .await
            .expect("in-memory database");
        let repo = PositionsRepo::new(db.clone());
        (db, repo)
    }

    /// Eleven-character identifiers built from an index, matching provider shape.
    fn id_for(index: usize) -> String {
        format!("vid{index:08}")
    }

    #[tokio::test]
    async fn a_checkpoint_round_trips() {
        let (_db, repo) = repo().await;
        repo.upsert(
            &video("aaaaaaaaaaa"),
            120_000,
            Some(600_000),
            Timestamp::from_millis(5),
        )
        .await
        .unwrap();

        let position = repo.get(&video("aaaaaaaaaaa")).await.unwrap().expect("row");
        assert_eq!(position.position_ms, 120_000);
        assert_eq!(position.duration_ms, Some(600_000));
        assert_eq!(position.updated_at, Timestamp::from_millis(5));
    }

    /// A page of rows that all fail to decode must move the scan on, not restart it.
    ///
    /// The cursor used to advance only *after* a row decoded, and the `continue` for a bad row
    /// jumped over that. A full page of undecodable rows therefore left the cursor untouched, and
    /// because a full page also means "not exhausted" the same page was fetched again, forever.
    /// This test hangs rather than fails if that returns, which is exactly the defect.
    #[tokio::test]
    async fn an_undecodable_page_does_not_spin() {
        let (db, repo) = repo().await;

        // More than one batch of rows the decoder will reject: `video_id` is not a valid provider
        // identifier, so `candidate_from_row` fails for every one of them.
        for index in 0..(CANDIDATE_BATCH as usize + 5) {
            sqlx::query(
                "INSERT INTO playback_positions (video_id, position_ms, duration_ms, updated_at)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(format!("!not a valid id!{index}"))
            .bind(60_000_i64)
            .bind(Some(600_000_i64))
            .bind(index as i64)
            .execute(db.writer())
            .await
            .expect("insert");
        }

        let resumable = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            repo.resumable(10),
        )
        .await
        .expect("resumable must terminate rather than re-reading the same page forever")
        .expect("query");

        assert!(
            resumable.is_empty(),
            "nothing decodes, so nothing is resumable"
        );
    }

    #[tokio::test]
    async fn a_missing_checkpoint_is_none_not_an_error() {
        let (_db, repo) = repo().await;
        assert!(repo.get(&video("aaaaaaaaaaa")).await.unwrap().is_none());
        assert_eq!(repo.count().await.unwrap(), 0);
        assert!(!repo.delete(&video("aaaaaaaaaaa")).await.unwrap());
    }

    #[tokio::test]
    async fn a_later_checkpoint_replaces_the_earlier_one() {
        let (_db, repo) = repo().await;
        let id = video("aaaaaaaaaaa");
        repo.upsert(&id, 1_000, Some(600_000), Timestamp::from_millis(1))
            .await
            .unwrap();
        repo.upsert(&id, 90_000, Some(600_000), Timestamp::from_millis(2))
            .await
            .unwrap();

        assert_eq!(repo.count().await.unwrap(), 1);
        assert_eq!(
            repo.get(&id).await.unwrap().expect("row").position_ms,
            90_000
        );
    }

    #[tokio::test]
    async fn a_checkpoint_without_a_duration_does_not_erase_a_known_one() {
        let (_db, repo) = repo().await;
        let id = video("aaaaaaaaaaa");
        repo.upsert(&id, 1_000, Some(600_000), Timestamp::EPOCH)
            .await
            .unwrap();
        repo.upsert(&id, 2_000, None, Timestamp::EPOCH)
            .await
            .unwrap();
        assert_eq!(
            repo.get(&id).await.unwrap().expect("row").duration_ms,
            Some(600_000)
        );
    }

    #[tokio::test]
    async fn a_position_beyond_i64_saturates_rather_than_wrapping_negative() {
        // The CHECK constraint forbids negatives, so a wrapping conversion would fail the write
        // outright; saturating keeps the checkpoint.
        let (_db, repo) = repo().await;
        repo.upsert(&video("aaaaaaaaaaa"), u64::MAX, None, Timestamp::EPOCH)
            .await
            .unwrap();
        let position = repo.get(&video("aaaaaaaaaaa")).await.unwrap().expect("row");
        assert_eq!(position.position_ms, u64::MAX / 2);
    }

    #[tokio::test]
    async fn resumable_applies_the_domain_rule_not_a_reimplementation_of_it() {
        let (_db, repo) = repo().await;
        // Genuinely partway through.
        repo.upsert(
            &video("partwayaaaa"),
            120_000,
            Some(600_000),
            Timestamp::from_millis(4),
        )
        .await
        .unwrap();
        // Finished: the last frame is not a resume point.
        repo.upsert(
            &video("finishedaaa"),
            600_000,
            Some(600_000),
            Timestamp::from_millis(3),
        )
        .await
        .unwrap();
        // Barely started: below the minimum resume position.
        repo.upsert(
            &video("barelyaaaaa"),
            3_000,
            Some(600_000),
            Timestamp::from_millis(2),
        )
        .await
        .unwrap();
        // In the completion tail window despite being under the fraction.
        repo.upsert(
            &video("tailaaaaaaa"),
            14_385_000,
            Some(14_400_000),
            Timestamp::from_millis(1),
        )
        .await
        .unwrap();

        let ids: Vec<String> = repo
            .resumable(10)
            .await
            .unwrap()
            .into_iter()
            .map(|(id, _)| id.into_inner())
            .collect();
        assert_eq!(ids, vec!["partwayaaaa".to_owned()]);
    }

    #[tokio::test]
    async fn resumable_orders_by_most_recent_checkpoint() {
        let (_db, repo) = repo().await;
        for (index, id) in ["aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc"]
            .iter()
            .enumerate()
        {
            let at = Timestamp::from_millis(i64::try_from(index).unwrap());
            repo.upsert(&video(id), 60_000, Some(600_000), at)
                .await
                .unwrap();
        }
        let ids: Vec<String> = repo
            .resumable(10)
            .await
            .unwrap()
            .into_iter()
            .map(|(id, _)| id.into_inner())
            .collect();
        assert_eq!(
            ids,
            vec![
                "ccccccccccc".to_owned(),
                "bbbbbbbbbbb".to_owned(),
                "aaaaaaaaaaa".to_owned()
            ]
        );
    }

    #[tokio::test]
    async fn a_live_stream_without_a_duration_is_still_resumable() {
        let (_db, repo) = repo().await;
        repo.upsert(&video("liveaaaaaaa"), 3_600_000, None, Timestamp::EPOCH)
            .await
            .unwrap();
        assert_eq!(repo.resumable(10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn resumable_pages_past_a_wall_of_finished_videos() {
        // The SQL prefilter cannot exclude a completed video, so more than one batch of candidates
        // must be scanned before the resumable ones are reached. This is the case a naive
        // "fetch limit rows and filter" implementation gets wrong.
        let (_db, repo) = repo().await;
        let wall = (CANDIDATE_BATCH as usize) * 2 + 5;
        for index in 0..wall {
            let at = Timestamp::from_millis(i64::try_from(wall - index).unwrap() + 100);
            repo.upsert(&video(&id_for(index)), 600_000, Some(600_000), at)
                .await
                .unwrap();
        }
        for index in 0..3 {
            let at = Timestamp::from_millis(i64::try_from(index).unwrap());
            repo.upsert(&video(&id_for(wall + index)), 120_000, Some(600_000), at)
                .await
                .unwrap();
        }

        let found = repo.resumable(3).await.unwrap();
        assert_eq!(found.len(), 3, "the scan must not stop at the first batch");
        assert!(found.iter().all(|(_, p)| p.resume_at_ms().is_some()));
    }

    #[tokio::test]
    async fn resumable_stops_at_the_limit_and_at_zero() {
        let (_db, repo) = repo().await;
        for index in 0..5 {
            repo.upsert(
                &video(&id_for(index)),
                120_000,
                Some(600_000),
                Timestamp::from_millis(i64::try_from(index).unwrap()),
            )
            .await
            .unwrap();
        }
        assert_eq!(repo.resumable(2).await.unwrap().len(), 2);
        assert_eq!(repo.resumable(99).await.unwrap().len(), 5);
        assert!(repo.resumable(0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn resumable_terminates_when_every_candidate_is_undecodable() {
        let (db, repo) = repo().await;
        for index in 0..3 {
            sqlx::query(
                "INSERT INTO playback_positions (video_id, position_ms, duration_ms, updated_at)
                 VALUES (?, 120000, 600000, ?)",
            )
            .bind(format!("../hostile/{index}"))
            .bind(i64::from(index))
            .execute(db.writer())
            .await
            .unwrap();
        }
        assert!(
            repo.resumable(5).await.unwrap().is_empty(),
            "undecodable rows must be skipped, and the scan must still terminate"
        );
    }

    #[tokio::test]
    async fn ties_on_the_checkpoint_time_do_not_repeat_or_skip_rows() {
        // Every row shares an `updated_at`, so the cursor has to fall back to the identifier to
        // make progress; without that tiebreak the scan would loop on the same page.
        let (_db, repo) = repo().await;
        let count = (CANDIDATE_BATCH as usize) + 7;
        for index in 0..count {
            repo.upsert(
                &video(&id_for(index)),
                120_000,
                Some(600_000),
                Timestamp::EPOCH,
            )
            .await
            .unwrap();
        }
        let found = repo.resumable(u32::try_from(count).unwrap()).await.unwrap();
        assert_eq!(found.len(), count);

        let mut ids: Vec<String> = found.into_iter().map(|(id, _)| id.into_inner()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), count, "no row may appear twice");
    }

    #[tokio::test]
    async fn clearing_removes_every_checkpoint() {
        let (_db, repo) = repo().await;
        repo.upsert(&video("aaaaaaaaaaa"), 1_000, None, Timestamp::EPOCH)
            .await
            .unwrap();
        repo.upsert(&video("bbbbbbbbbbb"), 1_000, None, Timestamp::EPOCH)
            .await
            .unwrap();

        assert_eq!(repo.clear().await.unwrap(), 2);
        assert_eq!(repo.count().await.unwrap(), 0);
        assert_eq!(repo.clear().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn concurrent_checkpoints_for_one_video_all_land() {
        let (_db, repo) = repo().await;
        let mut handles = Vec::new();
        for i in 1..=32_i64 {
            let repo = repo.clone();
            handles.push(tokio::spawn(async move {
                repo.upsert(
                    &video("aaaaaaaaaaa"),
                    u64::try_from(i).unwrap() * 1_000,
                    Some(600_000),
                    Timestamp::from_millis(i),
                )
                .await
            }));
        }
        for handle in handles {
            handle.await.unwrap().expect("no checkpoint should fail");
        }
        assert_eq!(repo.count().await.unwrap(), 1);
    }
}
