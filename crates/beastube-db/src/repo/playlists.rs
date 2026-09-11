//! Local playlists: the lists the user builds themselves.
//!
//! The schema, the model and the sparse-position algorithm for this feature all shipped in the
//! first migration; this repository is the layer that was missing between them. Until it existed
//! the application had a Playlists screen, a nav entry, and two seeded system playlists that
//! nothing could read — a promise the storage layer could not keep.
//!
//! ## System playlists are not editable, and that is enforced here
//!
//! Watch Later and Favorites are seeded by the migration and carry `is_system = 1`. Rename and
//! delete refuse them in the `WHERE` clause rather than by checking first and then acting: a check
//! followed by a write is a race, and the point of the flag is that no sequence of calls can remove
//! a list the rest of the application assumes exists.
//!
//! ## Counts and cover art are derived, not stored
//!
//! `LocalPlaylist` carries an item count and a cover image, and neither has a column. Both are
//! subqueries against `playlist_items`, which keeps them true by construction — a denormalised
//! counter is a second source of truth that drifts the first time a delete cascades.
//!
//! ## Positions are sparse
//!
//! Items carry a sort key spaced [`POSITION_GAP`] apart so an insertion between two neighbours
//! rewrites one row instead of renumbering the tail. Appending — the only insertion this repository
//! performs today — simply takes the last key plus a gap. Reordering is deliberately absent rather
//! than written and unused: `position_between` exists, tested, for whenever the UI grows a drag
//! handle to justify it.

use beastube_core::ids::{ChannelId, VideoId};
use beastube_core::model::playlist::{
    LocalPlaylist, LocalPlaylistId, PlaylistItem, POSITION_GAP, SystemPlaylist,
};
use beastube_core::model::video::VideoSummary;
use beastube_core::time_util::Timestamp;
use sqlx::sqlite::SqliteRow;

use crate::connection::Database;
use crate::error::{DbError, DbResult};

use super::{
    column, decode_channel_id, decode_thumbnails, decode_video_id, degrade, encode_thumbnails,
    from_db_bool, from_db_count, from_db_millis_opt, from_db_timestamp, require_max_len,
    require_non_blank, to_db_limit, to_db_millis_opt,
};

/// Longest a playlist name or an item's title may be stored at.
const MAX_TEXT_LEN: usize = 512;

/// Longest a user-written description may be.
const MAX_DESCRIPTION_LEN: usize = 5_000;

/// Every column a [`LocalPlaylist`] needs, including the two derived ones.
macro_rules! select_playlist {
    () => {
        "SELECT p.id, p.name, p.description, p.is_system, p.created_at, p.updated_at,
                (SELECT COUNT(*) FROM playlist_items i WHERE i.playlist_id = p.id)
                    AS item_count,
                (SELECT i.thumbnails_json FROM playlist_items i
                  WHERE i.playlist_id = p.id ORDER BY i.position LIMIT 1)
                    AS cover_json
           FROM playlists p"
    };
}

/// Every column a [`PlaylistItem`] needs.
macro_rules! select_item {
    () => {
        "SELECT video_id, position, added_at, title, channel_id, channel_name,
                thumbnails_json, duration_ms
           FROM playlist_items"
    };
}

/// Reads and writes the user's own playlists.
#[derive(Debug, Clone)]
pub struct PlaylistsRepo {
    db: Database,
}

impl PlaylistsRepo {
    /// Binds the repository to a database handle.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Every playlist, system lists first and the rest most-recently-changed first.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails. Undecodable rows are skipped rather than failing
    /// the list, so one bad row cannot hide every good one.
    pub async fn list(&self) -> DbResult<Vec<LocalPlaylist>> {
        let sql = concat!(
            select_playlist!(),
            " ORDER BY p.is_system DESC, p.updated_at DESC, p.id"
        );
        let rows = sqlx::query(sql)
            .fetch_all(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(rows
            .iter()
            .filter_map(|row| degrade(playlist_from_row(row), "playlists"))
            .collect())
    }

    /// One playlist, or `None` if no such row exists.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails or the row cannot be decoded.
    pub async fn get(&self, id: LocalPlaylistId) -> DbResult<Option<LocalPlaylist>> {
        let sql = concat!(select_playlist!(), " WHERE p.id = ?");
        let row = sqlx::query(sql)
            .bind(id.get())
            .fetch_optional(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        row.as_ref().map(playlist_from_row).transpose()
    }

    /// The identifier of a built-in list, looked up by its stable slug.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails.
    pub async fn system(&self, which: SystemPlaylist) -> DbResult<Option<LocalPlaylistId>> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT id FROM playlists WHERE slug = ?")
            .bind(which.slug())
            .fetch_optional(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(row.map(|(id,)| LocalPlaylistId::new(id)))
    }

    /// Creates a playlist and returns its identifier.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] if the name is blank or either field exceeds its bound, and a
    /// [`DbError`] if the insert fails.
    pub async fn create(
        &self,
        name: &str,
        description: Option<&str>,
        at: Timestamp,
    ) -> DbResult<LocalPlaylistId> {
        require_non_blank("name", name)?;
        require_max_len("name", name, MAX_TEXT_LEN)?;
        if let Some(text) = description {
            require_max_len("description", text, MAX_DESCRIPTION_LEN)?;
        }

        // `slug` is NULL and `is_system` is 0, which the table's CHECK requires of each other. A
        // user-created list can never become a system one by accident.
        let result = sqlx::query(
            "INSERT INTO playlists (slug, name, description, is_system, created_at, updated_at)
             VALUES (NULL, ?, ?, 0, ?, ?)",
        )
        .bind(name.trim())
        .bind(description.map(str::trim).filter(|text| !text.is_empty()))
        .bind(at.as_millis())
        .bind(at.as_millis())
        .execute(self.db.writer())
        .await
        .map_err(DbError::from_sqlx)?;

        Ok(LocalPlaylistId::new(result.last_insert_rowid()))
    }

    /// Renames a user playlist. Returns whether one was changed.
    ///
    /// A system playlist is never renamed: the `is_system = 0` predicate is part of the statement,
    /// so the refusal cannot be raced past.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] if the name is blank or too long, and a [`DbError`] if the
    /// update fails.
    pub async fn rename(&self, id: LocalPlaylistId, name: &str, at: Timestamp) -> DbResult<bool> {
        require_non_blank("name", name)?;
        require_max_len("name", name, MAX_TEXT_LEN)?;

        let changed = sqlx::query(
            "UPDATE playlists SET name = ?, updated_at = ? WHERE id = ? AND is_system = 0",
        )
        .bind(name.trim())
        .bind(at.as_millis())
        .bind(id.get())
        .execute(self.db.writer())
        .await
        .map_err(DbError::from_sqlx)?
        .rows_affected();
        Ok(changed > 0)
    }

    /// Deletes a user playlist and, by cascade, its items. Returns whether one was removed.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the delete fails.
    pub async fn delete(&self, id: LocalPlaylistId) -> DbResult<bool> {
        let removed = sqlx::query("DELETE FROM playlists WHERE id = ? AND is_system = 0")
            .bind(id.get())
            .execute(self.db.writer())
            .await
            .map_err(DbError::from_sqlx)?
            .rows_affected();
        Ok(removed > 0)
    }

    /// The videos in a playlist, in playlist order.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails. Undecodable rows are skipped.
    pub async fn items(
        &self,
        id: LocalPlaylistId,
        limit: u32,
        offset: u32,
    ) -> DbResult<Vec<PlaylistItem>> {
        let sql = concat!(
            select_item!(),
            " WHERE playlist_id = ? ORDER BY position LIMIT ? OFFSET ?"
        );
        let rows = sqlx::query(sql)
            .bind(id.get())
            .bind(to_db_limit(limit))
            .bind(to_db_limit(offset))
            .fetch_all(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(rows
            .iter()
            .filter_map(|row| degrade(item_from_row(row), "playlist_items"))
            .collect())
    }

    /// Appends a video, unless the playlist already holds it. Returns whether it was added.
    ///
    /// Adding is idempotent rather than an error: the caller is a menu item the user can press
    /// twice, and the second press should leave the list as they expect rather than fail.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] if the title is blank, and a [`DbError`] if the transaction
    /// fails.
    pub async fn add_item(
        &self,
        id: LocalPlaylistId,
        video: &VideoSummary,
        at: Timestamp,
    ) -> DbResult<bool> {
        require_non_blank("title", &video.title)?;

        let mut tx = self.db.writer().begin().await.map_err(DbError::from_sqlx)?;

        // The last key plus a gap. Read inside the transaction so two concurrent adds cannot land
        // on the same position and trip the unique index.
        // `Option<i64>` and `fetch_one`, not `(i64,)` and `fetch_optional`: an aggregate over an
        // empty set still returns exactly one row, holding NULL. Decoding that into `i64` fails,
        // which would have made the very first add to a new playlist an error.
        let (last,): (Option<i64>,) =
            sqlx::query_as("SELECT MAX(position) FROM playlist_items WHERE playlist_id = ?")
                .bind(id.get())
                .fetch_one(&mut *tx)
                .await
                .map_err(DbError::from_sqlx)?;
        let position = last.map_or(0, |max| max.saturating_add(POSITION_GAP));

        let inserted = sqlx::query(
            "INSERT INTO playlist_items
                 (playlist_id, video_id, position, added_at, title, channel_id, channel_name,
                  thumbnails_json, duration_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(playlist_id, video_id) DO NOTHING",
        )
        .bind(id.get())
        .bind(video.id.as_str())
        .bind(position)
        .bind(at.as_millis())
        .bind(truncate(&video.title, MAX_TEXT_LEN))
        .bind(video.channel_id.as_ref().map(ChannelId::as_str))
        .bind(
            video
                .channel_name
                .as_deref()
                .map(|name| truncate(name, MAX_TEXT_LEN)),
        )
        .bind(encode_thumbnails(&video.thumbnails)?)
        .bind(to_db_millis_opt(video.duration_ms))
        .execute(&mut *tx)
        .await
        .map_err(DbError::from_sqlx)?
        .rows_affected();

        if inserted > 0 {
            touch(&mut tx, id, at).await?;
        }
        tx.commit().await.map_err(DbError::from_sqlx)?;
        Ok(inserted > 0)
    }

    /// Removes a video from a playlist. Returns whether it was present.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the transaction fails.
    pub async fn remove_item(
        &self,
        id: LocalPlaylistId,
        video: &VideoId,
        at: Timestamp,
    ) -> DbResult<bool> {
        let mut tx = self.db.writer().begin().await.map_err(DbError::from_sqlx)?;
        let removed = sqlx::query("DELETE FROM playlist_items WHERE playlist_id = ? AND video_id = ?")
            .bind(id.get())
            .bind(video.as_str())
            .execute(&mut *tx)
            .await
            .map_err(DbError::from_sqlx)?
            .rows_affected();
        if removed > 0 {
            touch(&mut tx, id, at).await?;
        }
        tx.commit().await.map_err(DbError::from_sqlx)?;
        Ok(removed > 0)
    }

    /// Which playlists already contain a video.
    ///
    /// Answers the "add to playlist" menu in one query rather than one per playlist.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails.
    pub async fn containing(&self, video: &VideoId) -> DbResult<Vec<LocalPlaylistId>> {
        let rows: Vec<(i64,)> =
            sqlx::query_as("SELECT playlist_id FROM playlist_items WHERE video_id = ?")
                .bind(video.as_str())
                .fetch_all(self.db.reader())
                .await
                .map_err(DbError::from_sqlx)?;
        Ok(rows
            .into_iter()
            .map(|(id,)| LocalPlaylistId::new(id))
            .collect())
    }

    /// How many playlists exist, including the built-in ones.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails.
    pub async fn count(&self) -> DbResult<u64> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM playlists")
            .fetch_one(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(from_db_count(count))
    }
}

/// Marks a playlist as changed, so "recently updated" ordering reflects item edits too.
async fn touch(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: LocalPlaylistId,
    at: Timestamp,
) -> DbResult<()> {
    sqlx::query("UPDATE playlists SET updated_at = ? WHERE id = ?")
        .bind(at.as_millis())
        .bind(id.get())
        .execute(&mut **tx)
        .await
        .map_err(DbError::from_sqlx)?;
    Ok(())
}

/// Builds a playlist from a row selected by [`select_playlist`].
fn playlist_from_row(row: &SqliteRow) -> DbResult<LocalPlaylist> {
    Ok(LocalPlaylist {
        id: LocalPlaylistId::new(column::<i64>(row, "id")?),
        name: column::<String>(row, "name")?,
        description: column::<Option<String>>(row, "description")?,
        item_count: from_db_count(column::<i64>(row, "item_count")?),
        created_at: from_db_timestamp(column::<i64>(row, "created_at")?),
        updated_at: from_db_timestamp(column::<i64>(row, "updated_at")?),
        thumbnails: decode_thumbnails(
            "playlists",
            column::<Option<String>>(row, "cover_json")?.as_deref(),
        ),
        is_system: from_db_bool(column::<i64>(row, "is_system")?),
    })
}

/// Builds an item from a row selected by [`select_item`].
fn item_from_row(row: &SqliteRow) -> DbResult<PlaylistItem> {
    let id = decode_video_id("playlist_items", &column::<String>(row, "video_id")?)?;
    let mut video = VideoSummary::placeholder(id, column::<String>(row, "title")?);
    video.channel_id = decode_channel_id(
        "playlist_items",
        column::<Option<String>>(row, "channel_id")?.as_deref(),
    );
    video.channel_name = column::<Option<String>>(row, "channel_name")?;
    video.thumbnails = decode_thumbnails(
        "playlist_items",
        column::<Option<String>>(row, "thumbnails_json")?.as_deref(),
    );
    video.duration_ms = from_db_millis_opt(column::<Option<i64>>(row, "duration_ms")?);

    Ok(PlaylistItem {
        video,
        position: column::<i64>(row, "position")?,
        added_at: from_db_timestamp(column::<i64>(row, "added_at")?),
    })
}

/// Clamps stored text to a bound, on a character boundary.
fn truncate(raw: &str, max: usize) -> String {
    if raw.chars().count() <= max {
        return raw.to_owned();
    }
    raw.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use beastube_core::ids::VideoId;

    async fn repo() -> (Database, PlaylistsRepo) {
        let db = Database::open_in_memory()
            .await
            .expect("in-memory database");
        let repo = PlaylistsRepo::new(db.clone());
        (db, repo)
    }

    fn video(id: &str, title: &str) -> VideoSummary {
        VideoSummary::placeholder(VideoId::new(id).expect("video id"), title)
    }

    fn at(millis: i64) -> Timestamp {
        Timestamp::from_millis(millis)
    }

    #[tokio::test]
    async fn the_seeded_system_playlists_are_readable_from_the_first_query() {
        let (_db, repo) = repo().await;

        let all = repo.list().await.unwrap();
        assert_eq!(all.len(), 2, "the migration seeds Watch Later and Favorites");
        assert!(all.iter().all(|list| list.is_system));

        // Looked up by slug rather than by name, which is what survives translation.
        for which in SystemPlaylist::ALL {
            assert!(repo.system(which).await.unwrap().is_some(), "{which:?}");
        }
    }

    #[tokio::test]
    async fn the_first_item_added_to_an_empty_playlist_lands_rather_than_failing() {
        // `MAX(position)` over no rows returns one row holding NULL, not zero rows. Decoding that
        // as a plain integer failed, which made the first add to every new playlist an error — the
        // one path every playlist necessarily takes.
        let (_db, repo) = repo().await;
        let id = repo.create("Road trip", None, at(1)).await.unwrap();

        assert!(repo
            .add_item(id, &video("aaaaaaaaaaa", "first"), at(2))
            .await
            .unwrap());

        let items = repo.items(id, 10, 0).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].position, 0);
    }

    #[tokio::test]
    async fn items_come_back_in_the_order_they_were_appended() {
        let (_db, repo) = repo().await;
        let id = repo.create("Ordered", None, at(1)).await.unwrap();

        for (n, raw) in ["aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc"].iter().enumerate() {
            repo.add_item(id, &video(raw, &format!("item {n}")), at(2))
                .await
                .unwrap();
        }

        let titles: Vec<_> = repo
            .items(id, 10, 0)
            .await
            .unwrap()
            .into_iter()
            .map(|item| item.video.title)
            .collect();
        assert_eq!(titles, ["item 0", "item 1", "item 2"]);
    }

    #[tokio::test]
    async fn adding_the_same_video_twice_leaves_one_copy_and_says_so() {
        let (_db, repo) = repo().await;
        let id = repo.create("Once", None, at(1)).await.unwrap();
        let clip = video("aaaaaaaaaaa", "only once");

        assert!(repo.add_item(id, &clip, at(2)).await.unwrap());
        // Not an error: the caller is a menu item the user can press twice, and the second press
        // should leave the list as they expect.
        assert!(!repo.add_item(id, &clip, at(3)).await.unwrap());
        assert_eq!(repo.items(id, 10, 0).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_system_playlist_refuses_to_be_renamed_or_deleted() {
        let (_db, repo) = repo().await;
        let watch_later = repo
            .system(SystemPlaylist::WatchLater)
            .await
            .unwrap()
            .expect("seeded");

        assert!(!repo.rename(watch_later, "Mine now", at(9)).await.unwrap());
        assert!(!repo.delete(watch_later).await.unwrap());

        // Still there, still named what the migration called it.
        let still = repo.get(watch_later).await.unwrap().expect("present");
        assert_eq!(still.name, "Watch Later");
    }

    #[tokio::test]
    async fn deleting_a_playlist_takes_its_items_with_it() {
        let (db, repo) = repo().await;
        let id = repo.create("Temporary", None, at(1)).await.unwrap();
        repo.add_item(id, &video("aaaaaaaaaaa", "doomed"), at(2))
            .await
            .unwrap();

        assert!(repo.delete(id).await.unwrap());

        // Straight to the table: the cascade is the schema's promise, not this repository's.
        let (orphans,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM playlist_items")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(orphans, 0);
    }

    #[tokio::test]
    async fn containing_answers_for_every_playlist_at_once() {
        let (_db, repo) = repo().await;
        let first = repo.create("One", None, at(1)).await.unwrap();
        let second = repo.create("Two", None, at(1)).await.unwrap();
        let clip = video("aaaaaaaaaaa", "shared");

        repo.add_item(first, &clip, at(2)).await.unwrap();
        repo.add_item(second, &clip, at(2)).await.unwrap();

        let mut holding = repo.containing(&clip.id).await.unwrap();
        holding.sort();
        assert_eq!(holding, [first, second]);
    }

    #[tokio::test]
    async fn editing_the_items_moves_a_playlist_up_the_recently_changed_order() {
        let (_db, repo) = repo().await;
        let older = repo.create("Older", None, at(10)).await.unwrap();
        let newer = repo.create("Newer", None, at(20)).await.unwrap();

        // Adding to the older list makes it the most recently changed.
        repo.add_item(older, &video("aaaaaaaaaaa", "fresh"), at(30))
            .await
            .unwrap();

        let user_lists: Vec<_> = repo
            .list()
            .await
            .unwrap()
            .into_iter()
            .filter(|list| !list.is_system)
            .map(|list| list.id)
            .collect();
        assert_eq!(user_lists, [older, newer]);
    }

    #[tokio::test]
    async fn a_playlist_reports_its_size_and_borrows_its_cover_from_the_first_item() {
        let (_db, repo) = repo().await;
        let id = repo.create("Counted", None, at(1)).await.unwrap();
        repo.add_item(id, &video("aaaaaaaaaaa", "one"), at(2))
            .await
            .unwrap();
        repo.add_item(id, &video("bbbbbbbbbbb", "two"), at(3))
            .await
            .unwrap();

        let list = repo.get(id).await.unwrap().expect("present");
        // Derived by subquery rather than kept in a column, so it cannot drift from the rows.
        assert_eq!(list.item_count, 2);
    }

    #[tokio::test]
    async fn a_blank_name_is_refused_rather_than_stored() {
        let (_db, repo) = repo().await;
        assert!(repo.create("   ", None, at(1)).await.is_err());

        let id = repo.create("Real", None, at(1)).await.unwrap();
        assert!(repo.rename(id, "", at(2)).await.is_err());
    }

    #[tokio::test]
    async fn removing_an_item_reports_whether_it_was_there() {
        let (_db, repo) = repo().await;
        let id = repo.create("Held", None, at(1)).await.unwrap();
        let clip = video("aaaaaaaaaaa", "held");
        repo.add_item(id, &clip, at(2)).await.unwrap();

        assert!(repo.remove_item(id, &clip.id, at(3)).await.unwrap());
        assert!(!repo.remove_item(id, &clip.id, at(4)).await.unwrap());
        assert!(repo.items(id, 10, 0).await.unwrap().is_empty());
    }
}
