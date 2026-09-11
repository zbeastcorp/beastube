//! Bookmarks and their tags.
//!
//! A bookmark is keyed by video, so bookmarking the same video twice updates the existing entry
//! rather than creating a second one — the same reasoning as history: the user's mental model is
//! "this video is saved", not "I pressed save twice".
//!
//! ## Tags are normalized on write, never on read
//!
//! Tags arrive from a free-text field, so `Rust`, `rust ` and `RUST` are the same tag as far as the
//! user is concerned. They are lowercased, trimmed and deduplicated **before** they reach the
//! table, which means `list_by_tag` is an index lookup on an exact value rather than a case-folding
//! scan, and the tag list shown in the UI has no near-duplicates in it.
//!
//! ## Why tags are loaded in a second query
//!
//! Joining `bookmark_tags` into the listing query would multiply each bookmark by its tag count and
//! force the mapper to regroup rows. Instead the page is fetched, then its tags are fetched in one
//! batched lookup keyed by the identifiers already in hand. That is two round trips regardless of
//! page size, not one per bookmark.

use std::collections::{BTreeMap, BTreeSet};

use beastube_core::ids::{ChannelId, VideoId};
use beastube_core::model::library::Bookmark;
use beastube_core::model::thumbnail::ThumbnailSet;
use beastube_core::time_util::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{Sqlite, Transaction};

use crate::connection::Database;
use crate::error::{DbError, DbResult};
use super::{MAX_TEXT_LEN, truncate};

use super::{
    column, decode_channel_id, decode_thumbnails, decode_video_id, degrade, encode_thumbnails,
    from_db_count, from_db_millis_opt, from_db_timestamp, like_contains, require_max_len,
    require_non_blank, search_text, to_db_limit, to_db_millis_opt,
};

/// Maximum stored length of a tag, in characters.
///
/// Tags are a filing mechanism, not a note field; a bound keeps the tag list renderable and the
/// index narrow.
pub const MAX_TAG_LEN: usize = 64;

/// Maximum stored length of a note.
pub const MAX_NOTE_LEN: usize = 4_096;


/// How many identifiers one batched tag lookup binds at a time.
///
/// SQLite's compiled parameter ceiling is far higher, but chunking keeps the statement cache from
/// holding one prepared statement per distinct page size.
const TAG_LOOKUP_CHUNK: usize = 256;

/// Column list shared by every bookmark read.
/// Columns shared by every bookmark query.
///
/// A macro rather than a `const` so call sites can assemble their full statement with
/// `concat!`, keeping the SQL a compile-time literal. sqlx 0.9 refuses a runtime `String`
/// as a query, and rightly so: that refusal is what makes SQL injection structurally
/// impossible here rather than merely unlikely.
macro_rules! select_bookmark {
    () => {
        "
            SELECT video_id, title, channel_id, channel_name, thumbnails_json, note, timestamp_ms,
                   created_at
            FROM bookmarks
        "
    };
}

/// What the caller supplies when saving a bookmark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewBookmark {
    /// The video being saved.
    pub video_id: VideoId,
    /// Title at the time of saving. Untrusted text.
    pub title: String,
    /// Owning channel, when known.
    pub channel_id: Option<ChannelId>,
    /// Channel name at the time of saving. Untrusted text.
    pub channel_name: Option<String>,
    /// Thumbnails at the time of saving.
    pub thumbnails: ThumbnailSet,
    /// User-written note.
    pub note: Option<String>,
    /// A moment within the video, when the bookmark marks a point rather than the whole video.
    pub timestamp_ms: Option<u64>,
    /// Tags, lowercased and deduplicated on write.
    pub tags: Vec<String>,
}

impl NewBookmark {
    /// A bookmark of a whole video, with no note and no tags.
    #[must_use]
    pub fn of(video_id: VideoId, title: impl Into<String>) -> Self {
        Self {
            video_id,
            title: title.into(),
            channel_id: None,
            channel_name: None,
            thumbnails: ThumbnailSet::empty(),
            note: None,
            timestamp_ms: None,
            tags: Vec::new(),
        }
    }
}

/// Reads and writes bookmarks and their tags.
#[derive(Debug, Clone)]
pub struct BookmarksRepo {
    db: Database,
}

impl BookmarksRepo {
    /// Binds the repository to a database handle.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Saves a bookmark, replacing the tags and note of an existing one.
    ///
    /// `created_at` is preserved across a re-save: it records when the user first saved the video,
    /// which is what the default ordering is built on.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] if the title is blank or a tag or note exceeds its bound, and
    /// a [`DbError`] if the transaction fails.
    pub async fn add(&self, bookmark: &NewBookmark, at: Timestamp) -> DbResult<()> {
        require_non_blank("title", &bookmark.title)?;
        if let Some(note) = &bookmark.note {
            require_max_len("note", note, MAX_NOTE_LEN)?;
        }
        let tags = normalize_tags(&bookmark.tags)?;

        let mut tx = self.db.writer().begin().await.map_err(DbError::from_sqlx)?;
        self.write_bookmark(&mut tx, bookmark, at).await?;
        // Replace rather than merge: the record the caller passed is the whole truth about this
        // bookmark, so a tag it omits was removed.
        sqlx::query("DELETE FROM bookmark_tags WHERE video_id = ?")
            .bind(bookmark.video_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(DbError::from_sqlx)?;
        for tag in &tags {
            insert_tag(&mut tx, bookmark.video_id.as_str(), tag).await?;
        }
        tx.commit().await.map_err(DbError::from_sqlx)?;
        Ok(())
    }

    /// Writes the bookmark row itself inside an open transaction.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the statement fails.
    async fn write_bookmark(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        bookmark: &NewBookmark,
        at: Timestamp,
    ) -> DbResult<()> {
        let title = truncate(&bookmark.title, MAX_TEXT_LEN);
        let channel_name = bookmark
            .channel_name
            .as_deref()
            .map(|name| truncate(name, MAX_TEXT_LEN));
        let search = search_text(&title, channel_name.as_deref());

        sqlx::query(
            "INSERT INTO bookmarks
                 (video_id, title, channel_id, channel_name, thumbnails_json, note, timestamp_ms,
                  created_at, search_text)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(video_id) DO UPDATE SET
                 title           = excluded.title,
                 channel_id      = excluded.channel_id,
                 channel_name    = excluded.channel_name,
                 thumbnails_json = excluded.thumbnails_json,
                 note            = excluded.note,
                 timestamp_ms    = excluded.timestamp_ms,
                 search_text     = excluded.search_text",
        )
        .bind(bookmark.video_id.as_str())
        .bind(&title)
        .bind(bookmark.channel_id.as_ref().map(ChannelId::as_str))
        .bind(channel_name)
        .bind(encode_thumbnails(&bookmark.thumbnails)?)
        .bind(bookmark.note.as_deref().map(|n| truncate(n, MAX_NOTE_LEN)))
        .bind(to_db_millis_opt(bookmark.timestamp_ms))
        .bind(at.as_millis())
        .bind(search)
        .execute(&mut **tx)
        .await
        .map_err(DbError::from_sqlx)?;
        Ok(())
    }

    /// Removes a bookmark and, by cascade, its tags. Returns whether one was present.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the delete fails.
    pub async fn remove(&self, video: &VideoId) -> DbResult<bool> {
        let removed = sqlx::query("DELETE FROM bookmarks WHERE video_id = ?")
            .bind(video.as_str())
            .execute(self.db.writer())
            .await
            .map_err(DbError::from_sqlx)?
            .rows_affected();
        Ok(removed > 0)
    }

    /// One bookmark with its tags, or `None` if the video is not bookmarked.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if a query fails or the row cannot be decoded.
    pub async fn get(&self, video: &VideoId) -> DbResult<Option<Bookmark>> {
        let sql = concat!(select_bookmark!(), " WHERE video_id = ?");
        let row = sqlx::query(sql)
            .bind(video.as_str())
            .fetch_optional(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;

        let Some(row) = row else {
            return Ok(None);
        };
        let mut bookmark = bookmark_from_row(&row)?;
        bookmark.tags = self.tags(video).await?;
        Ok(Some(bookmark))
    }

    /// Bookmarks, most recently saved first.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if a query fails. Undecodable rows are skipped.
    pub async fn list(&self, limit: u32, offset: u32) -> DbResult<Vec<Bookmark>> {
        let sql = concat!(
            select_bookmark!(),
            " ORDER BY created_at DESC, video_id LIMIT ? OFFSET ?"
        );
        let rows = sqlx::query(sql)
            .bind(to_db_limit(limit))
            .bind(to_db_limit(offset))
            .fetch_all(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        self.with_tags(&rows).await
    }

    /// Bookmarks whose title or channel contains `query`, case-insensitively.
    ///
    /// A blank query matches nothing, for the same reason as in history: a cleared search box is
    /// not a request for everything.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if a query fails.
    pub async fn search(&self, query: &str, limit: u32) -> DbResult<Vec<Bookmark>> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let sql = concat!(
            select_bookmark!(),
            " WHERE search_text LIKE ? ESCAPE '\\'
             ORDER BY created_at DESC, video_id LIMIT ?"
        );
        let rows = sqlx::query(sql)
            .bind(like_contains(&needle))
            .bind(to_db_limit(limit))
            .fetch_all(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        self.with_tags(&rows).await
    }

    /// Bookmarks carrying `tag`, most recently saved first.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] if the tag is blank or overlong, or a [`DbError`] if a query
    /// fails.
    pub async fn list_by_tag(&self, tag: &str, limit: u32, offset: u32) -> DbResult<Vec<Bookmark>> {
        let tag = normalize_tag(tag)?;
        let sql = concat!(
            select_bookmark!(),
            " WHERE video_id IN (SELECT video_id FROM bookmark_tags WHERE tag = ?)
             ORDER BY created_at DESC, video_id LIMIT ? OFFSET ?"
        );
        let rows = sqlx::query(sql)
            .bind(&tag)
            .bind(to_db_limit(limit))
            .bind(to_db_limit(offset))
            .fetch_all(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        self.with_tags(&rows).await
    }

    /// Adds one tag to an existing bookmark. Adding a tag twice is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] if the tag is blank or overlong, [`DbError::NotFound`] if the
    /// video is not bookmarked, or a [`DbError`] if the transaction fails.
    pub async fn add_tag(&self, video: &VideoId, tag: &str) -> DbResult<()> {
        let tag = normalize_tag(tag)?;
        let mut tx = self.db.writer().begin().await.map_err(DbError::from_sqlx)?;
        // Checked inside the transaction: the foreign key would reject the insert anyway, but as
        // an opaque constraint error rather than an actionable "this video is not bookmarked".
        require_bookmark(&mut tx, video).await?;
        insert_tag(&mut tx, video.as_str(), &tag).await?;
        tx.commit().await.map_err(DbError::from_sqlx)?;
        Ok(())
    }

    /// Removes one tag, returning whether it was present.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] if the tag is blank or overlong, or a [`DbError`] if the
    /// delete fails.
    pub async fn remove_tag(&self, video: &VideoId, tag: &str) -> DbResult<bool> {
        let tag = normalize_tag(tag)?;
        let removed = sqlx::query("DELETE FROM bookmark_tags WHERE video_id = ? AND tag = ?")
            .bind(video.as_str())
            .bind(&tag)
            .execute(self.db.writer())
            .await
            .map_err(DbError::from_sqlx)?
            .rows_affected();
        Ok(removed > 0)
    }

    /// Tags on one bookmark, in stable alphabetical order.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails.
    pub async fn tags(&self, video: &VideoId) -> DbResult<Vec<String>> {
        sqlx::query_scalar("SELECT tag FROM bookmark_tags WHERE video_id = ? ORDER BY tag")
            .bind(video.as_str())
            .fetch_all(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)
    }

    /// Every tag in use, with how many bookmarks carry it, most used first.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails.
    pub async fn tag_counts(&self) -> DbResult<Vec<(String, u64)>> {
        let rows = sqlx::query(
            "SELECT tag, COUNT(*) AS uses FROM bookmark_tags
             GROUP BY tag ORDER BY uses DESC, tag ASC",
        )
        .fetch_all(self.db.reader())
        .await
        .map_err(DbError::from_sqlx)?;

        rows.iter()
            .map(|row| {
                Ok((
                    column::<String>(row, "tag")?,
                    from_db_count(column::<i64>(row, "uses")?),
                ))
            })
            .collect()
    }

    /// Replaces the note on an existing bookmark; `None` clears it.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] if the note is overlong, [`DbError::NotFound`] if the video is
    /// not bookmarked, or a [`DbError`] if the update fails.
    pub async fn set_note(&self, video: &VideoId, note: Option<&str>) -> DbResult<()> {
        if let Some(note) = note {
            require_max_len("note", note, MAX_NOTE_LEN)?;
        }
        let updated = sqlx::query("UPDATE bookmarks SET note = ? WHERE video_id = ?")
            .bind(note)
            .bind(video.as_str())
            .execute(self.db.writer())
            .await
            .map_err(DbError::from_sqlx)?
            .rows_affected();
        if updated == 0 {
            return Err(DbError::NotFound {
                entity: "bookmark",
                id: video.as_str().to_owned(),
            });
        }
        Ok(())
    }

    /// Number of bookmarks.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails.
    pub async fn count(&self) -> DbResult<u64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bookmarks")
            .fetch_one(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(from_db_count(count))
    }

    /// Deletes every bookmark and its tags, returning how many were removed.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the delete fails.
    pub async fn clear(&self) -> DbResult<u64> {
        let removed = sqlx::query("DELETE FROM bookmarks")
            .execute(self.db.writer())
            .await
            .map_err(DbError::from_sqlx)?
            .rows_affected();
        Ok(removed)
    }

    /// Maps a page of rows and attaches each bookmark's tags in one batched lookup.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the tag lookup fails.
    async fn with_tags(&self, rows: &[SqliteRow]) -> DbResult<Vec<Bookmark>> {
        let mut bookmarks: Vec<Bookmark> = rows
            .iter()
            .filter_map(|row| degrade(bookmark_from_row(row), "bookmarks"))
            .collect();

        let ids: Vec<String> = bookmarks
            .iter()
            .map(|b| b.video_id.as_str().to_owned())
            .collect();
        let mut tags = self.tags_for(&ids).await?;
        for bookmark in &mut bookmarks {
            bookmark.tags = tags.remove(bookmark.video_id.as_str()).unwrap_or_default();
        }
        Ok(bookmarks)
    }

    /// Fetches the tags of many bookmarks, chunked so the parameter list stays bounded.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if a query fails.
    async fn tags_for(&self, ids: &[String]) -> DbResult<BTreeMap<String, Vec<String>>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for chunk in ids.chunks(TAG_LOOKUP_CHUNK) {
            // The one place in this crate where the statement text genuinely varies, because the
            // `IN` list length follows the chunk. `QueryBuilder` is sqlx's sanctioned answer: it
            // emits a bind placeholder per value, so the identifiers are still parameters and never
            // interpolated text. Chunking bounds the placeholder count, which SQLite caps.
            let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                "SELECT video_id, tag FROM bookmark_tags WHERE video_id IN (",
            );
            let mut separated = builder.separated(", ");
            for id in chunk {
                separated.push_bind(id);
            }
            separated.push_unseparated(") ORDER BY tag");
            let rows = builder
                .build()
                .fetch_all(self.db.reader())
                .await
                .map_err(DbError::from_sqlx)?;
            for row in &rows {
                let video_id: String = column(row, "video_id")?;
                let tag: String = column(row, "tag")?;
                out.entry(video_id).or_default().push(tag);
            }
        }
        Ok(out)
    }
}

/// Fails unless `video` is bookmarked, inside the caller's transaction.
///
/// # Errors
///
/// Returns [`DbError::NotFound`] if there is no such bookmark.
async fn require_bookmark(tx: &mut Transaction<'_, Sqlite>, video: &VideoId) -> DbResult<()> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM bookmarks WHERE video_id = ?")
        .bind(video.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(DbError::from_sqlx)?;
    if exists.is_none() {
        return Err(DbError::NotFound {
            entity: "bookmark",
            id: video.as_str().to_owned(),
        });
    }
    Ok(())
}

/// Inserts one already-normalized tag, ignoring a duplicate.
///
/// # Errors
///
/// Returns a [`DbError`] if the insert fails.
async fn insert_tag(tx: &mut Transaction<'_, Sqlite>, video: &str, tag: &str) -> DbResult<()> {
    sqlx::query("INSERT OR IGNORE INTO bookmark_tags (video_id, tag) VALUES (?, ?)")
        .bind(video)
        .bind(tag)
        .execute(&mut **tx)
        .await
        .map_err(DbError::from_sqlx)?;
    Ok(())
}

/// Trims, lowercases and validates one tag.
///
/// # Errors
///
/// Returns [`DbError::Invalid`] if the tag is blank or longer than [`MAX_TAG_LEN`].
fn normalize_tag(raw: &str) -> DbResult<String> {
    let tag = raw.trim().to_lowercase();
    require_non_blank("tag", &tag)?;
    require_max_len("tag", &tag, MAX_TAG_LEN)?;
    Ok(tag)
}

/// Normalizes a tag list, deduplicating and ordering it.
///
/// Ordering is a side effect of deduplicating through a [`BTreeSet`], and a welcome one: the tags
/// stored for a bookmark are then independent of the order the user typed them.
///
/// # Errors
///
/// Returns [`DbError::Invalid`] if any tag is blank or overlong.
fn normalize_tags(raw: &[String]) -> DbResult<Vec<String>> {
    let mut set = BTreeSet::new();
    for tag in raw {
        set.insert(normalize_tag(tag)?);
    }
    Ok(set.into_iter().collect())
}


/// Builds a [`Bookmark`] with an empty tag list, which the caller fills in.
///
/// # Errors
///
/// Returns [`DbError::Corrupt`] if the stored identifier no longer validates, or a [`DbError`] if
/// a column cannot be read.
fn bookmark_from_row(row: &SqliteRow) -> DbResult<Bookmark> {
    let raw_id: String = column(row, "video_id")?;
    Ok(Bookmark {
        video_id: decode_video_id("bookmarks", &raw_id)?,
        title: column(row, "title")?,
        channel_id: decode_channel_id(
            "bookmarks",
            column::<Option<String>>(row, "channel_id")?.as_deref(),
        ),
        channel_name: column(row, "channel_name")?,
        thumbnails: decode_thumbnails(
            "bookmarks",
            column::<Option<String>>(row, "thumbnails_json")?.as_deref(),
        ),
        note: column(row, "note")?,
        tags: Vec::new(),
        timestamp_ms: from_db_millis_opt(column::<Option<i64>>(row, "timestamp_ms")?),
        created_at: from_db_timestamp(column::<i64>(row, "created_at")?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(id: &str) -> VideoId {
        VideoId::new(id).expect("valid test identifier")
    }

    fn bookmark(id: &str, title: &str) -> NewBookmark {
        NewBookmark {
            channel_id: ChannelId::new("UCtestchannel").ok(),
            channel_name: Some("Test Channel".to_owned()),
            ..NewBookmark::of(video(id), title)
        }
    }

    async fn repo() -> (Database, BookmarksRepo) {
        let db = Database::open_in_memory()
            .await
            .expect("in-memory database");
        let repo = BookmarksRepo::new(db.clone());
        (db, repo)
    }

    #[tokio::test]
    async fn a_bookmark_round_trips_with_its_note_and_timestamp() {
        let (_db, repo) = repo().await;
        let saved = NewBookmark {
            note: Some("the good bit".to_owned()),
            timestamp_ms: Some(90_000),
            tags: vec!["Rust".to_owned(), "  rust ".to_owned(), "SQL".to_owned()],
            ..bookmark("aaaaaaaaaaa", "Deep dive")
        };
        repo.add(&saved, Timestamp::from_millis(42)).await.unwrap();

        let loaded = repo
            .get(&video("aaaaaaaaaaa"))
            .await
            .unwrap()
            .expect("bookmark");
        assert_eq!(loaded.title, "Deep dive");
        assert_eq!(loaded.note.as_deref(), Some("the good bit"));
        assert_eq!(loaded.timestamp_ms, Some(90_000));
        assert_eq!(loaded.created_at, Timestamp::from_millis(42));
        assert_eq!(
            loaded.tags,
            vec!["rust".to_owned(), "sql".to_owned()],
            "tags are lowercased, deduplicated and ordered"
        );
    }

    #[tokio::test]
    async fn re_saving_updates_in_place_and_keeps_the_original_creation_time() {
        let (_db, repo) = repo().await;
        repo.add(
            &bookmark("aaaaaaaaaaa", "First"),
            Timestamp::from_millis(10),
        )
        .await
        .unwrap();
        repo.add(
            &NewBookmark {
                note: Some("added later".to_owned()),
                ..bookmark("aaaaaaaaaaa", "Second")
            },
            Timestamp::from_millis(20),
        )
        .await
        .unwrap();

        assert_eq!(repo.count().await.unwrap(), 1);
        let loaded = repo
            .get(&video("aaaaaaaaaaa"))
            .await
            .unwrap()
            .expect("bookmark");
        assert_eq!(loaded.title, "Second");
        assert_eq!(loaded.note.as_deref(), Some("added later"));
        assert_eq!(loaded.created_at, Timestamp::from_millis(10));
    }

    #[tokio::test]
    async fn re_saving_replaces_the_tag_set_rather_than_merging_it() {
        let (_db, repo) = repo().await;
        repo.add(
            &NewBookmark {
                tags: vec!["a".to_owned(), "b".to_owned()],
                ..bookmark("aaaaaaaaaaa", "T")
            },
            Timestamp::EPOCH,
        )
        .await
        .unwrap();
        repo.add(
            &NewBookmark {
                tags: vec!["c".to_owned()],
                ..bookmark("aaaaaaaaaaa", "T")
            },
            Timestamp::EPOCH,
        )
        .await
        .unwrap();

        assert_eq!(repo.tags(&video("aaaaaaaaaaa")).await.unwrap(), vec!["c"]);
    }

    #[tokio::test]
    async fn removing_a_bookmark_cascades_to_its_tags() {
        let (db, repo) = repo().await;
        repo.add(
            &NewBookmark {
                tags: vec!["keep".to_owned()],
                ..bookmark("aaaaaaaaaaa", "T")
            },
            Timestamp::EPOCH,
        )
        .await
        .unwrap();

        assert!(repo.remove(&video("aaaaaaaaaaa")).await.unwrap());
        let tags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bookmark_tags")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(tags, 0, "orphaned tags must not survive their bookmark");
        assert!(!repo.remove(&video("aaaaaaaaaaa")).await.unwrap());
    }

    #[tokio::test]
    async fn tagging_a_video_that_is_not_bookmarked_is_actionable_not_opaque() {
        let (_db, repo) = repo().await;
        let err = repo
            .add_tag(&video("aaaaaaaaaaa"), "rust")
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            DbError::NotFound {
                entity: "bookmark",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn tags_can_be_added_and_removed_individually() {
        let (_db, repo) = repo().await;
        let id = video("aaaaaaaaaaa");
        repo.add(&bookmark("aaaaaaaaaaa", "T"), Timestamp::EPOCH)
            .await
            .unwrap();

        repo.add_tag(&id, " Rust ").await.unwrap();
        repo.add_tag(&id, "RUST").await.unwrap();
        assert_eq!(
            repo.tags(&id).await.unwrap(),
            vec!["rust"],
            "the same tag in different casing must not appear twice"
        );

        assert!(repo.remove_tag(&id, "RUST").await.unwrap());
        assert!(repo.tags(&id).await.unwrap().is_empty());
        assert!(!repo.remove_tag(&id, "rust").await.unwrap());
    }

    #[tokio::test]
    async fn blank_and_overlong_tags_are_rejected() {
        let (_db, repo) = repo().await;
        let id = video("aaaaaaaaaaa");
        repo.add(&bookmark("aaaaaaaaaaa", "T"), Timestamp::EPOCH)
            .await
            .unwrap();

        assert!(matches!(
            repo.add_tag(&id, "   ").await.unwrap_err(),
            DbError::Invalid { field: "tag", .. }
        ));
        let long = "x".repeat(MAX_TAG_LEN + 1);
        assert!(matches!(
            repo.add_tag(&id, &long).await.unwrap_err(),
            DbError::Invalid { field: "tag", .. }
        ));
        assert!(repo.add_tag(&id, &"x".repeat(MAX_TAG_LEN)).await.is_ok());
    }

    #[tokio::test]
    async fn a_bookmark_with_an_invalid_tag_is_not_partially_written() {
        let (_db, repo) = repo().await;
        let doomed = NewBookmark {
            tags: vec!["fine".to_owned(), String::new()],
            ..bookmark("aaaaaaaaaaa", "T")
        };
        assert!(repo.add(&doomed, Timestamp::EPOCH).await.is_err());
        assert_eq!(
            repo.count().await.unwrap(),
            0,
            "validation must happen before the transaction opens"
        );
    }

    #[tokio::test]
    async fn listing_by_tag_finds_only_tagged_bookmarks() {
        let (_db, repo) = repo().await;
        repo.add(
            &NewBookmark {
                tags: vec!["rust".to_owned()],
                ..bookmark("aaaaaaaaaaa", "Tagged")
            },
            Timestamp::from_millis(2),
        )
        .await
        .unwrap();
        repo.add(
            &bookmark("bbbbbbbbbbb", "Untagged"),
            Timestamp::from_millis(1),
        )
        .await
        .unwrap();

        let tagged = repo.list_by_tag("RUST", 10, 0).await.unwrap();
        assert_eq!(tagged.len(), 1);
        assert_eq!(tagged[0].title, "Tagged");
        assert_eq!(tagged[0].tags, vec!["rust"]);
        assert!(repo.list_by_tag("absent", 10, 0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn listing_is_newest_first_and_carries_tags() {
        let (_db, repo) = repo().await;
        for (index, id) in ["aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc"]
            .iter()
            .enumerate()
        {
            repo.add(
                &NewBookmark {
                    tags: vec![format!("t{index}")],
                    ..bookmark(id, "T")
                },
                Timestamp::from_millis(i64::try_from(index).unwrap()),
            )
            .await
            .unwrap();
        }

        let page = repo.list(2, 0).await.unwrap();
        let ids: Vec<&str> = page.iter().map(|b| b.video_id.as_str()).collect();
        assert_eq!(ids, vec!["ccccccccccc", "bbbbbbbbbbb"]);
        assert_eq!(page[0].tags, vec!["t2"]);
        assert_eq!(repo.list(2, 2).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_empty_collection_lists_and_counts_without_error() {
        let (_db, repo) = repo().await;
        assert!(repo.list(10, 0).await.unwrap().is_empty());
        assert!(repo.search("anything", 10).await.unwrap().is_empty());
        assert!(repo.tag_counts().await.unwrap().is_empty());
        assert_eq!(repo.count().await.unwrap(), 0);
        assert!(repo.get(&video("aaaaaaaaaaa")).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn search_matches_title_and_channel_and_ignores_wildcards() {
        let (_db, repo) = repo().await;
        repo.add(&bookmark("aaaaaaaaaaa", "50% off"), Timestamp::EPOCH)
            .await
            .unwrap();
        repo.add(&bookmark("bbbbbbbbbbb", "500 things"), Timestamp::EPOCH)
            .await
            .unwrap();

        assert_eq!(repo.search("test channel", 10).await.unwrap().len(), 2);
        assert_eq!(repo.search("50%", 10).await.unwrap().len(), 1);
        assert!(repo.search("  ", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn tag_counts_rank_by_use() {
        let (_db, repo) = repo().await;
        for (id, tags) in [
            ("aaaaaaaaaaa", vec!["shared", "only-a"]),
            ("bbbbbbbbbbb", vec!["shared"]),
        ] {
            repo.add(
                &NewBookmark {
                    tags: tags.into_iter().map(str::to_owned).collect(),
                    ..bookmark(id, "T")
                },
                Timestamp::EPOCH,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            repo.tag_counts().await.unwrap(),
            vec![("shared".to_owned(), 2), ("only-a".to_owned(), 1)]
        );
    }

    #[tokio::test]
    async fn a_note_can_be_replaced_and_cleared() {
        let (_db, repo) = repo().await;
        let id = video("aaaaaaaaaaa");
        repo.add(&bookmark("aaaaaaaaaaa", "T"), Timestamp::EPOCH)
            .await
            .unwrap();

        repo.set_note(&id, Some("first")).await.unwrap();
        assert_eq!(
            repo.get(&id)
                .await
                .unwrap()
                .expect("bookmark")
                .note
                .as_deref(),
            Some("first")
        );
        repo.set_note(&id, None).await.unwrap();
        assert!(
            repo.get(&id)
                .await
                .unwrap()
                .expect("bookmark")
                .note
                .is_none()
        );

        let err = repo
            .set_note(&video("bbbbbbbbbbb"), Some("x"))
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::NotFound { .. }));
    }

    #[tokio::test]
    async fn an_overlong_note_is_rejected_rather_than_silently_cut() {
        let (_db, repo) = repo().await;
        let id = video("aaaaaaaaaaa");
        repo.add(&bookmark("aaaaaaaaaaa", "T"), Timestamp::EPOCH)
            .await
            .unwrap();
        let long = "x".repeat(MAX_NOTE_LEN + 1);
        assert!(matches!(
            repo.set_note(&id, Some(&long)).await.unwrap_err(),
            DbError::Invalid { field: "note", .. }
        ));
    }

    #[tokio::test]
    async fn a_corrupt_row_costs_its_own_entry_only() {
        let (db, repo) = repo().await;
        repo.add(&bookmark("aaaaaaaaaaa", "Good"), Timestamp::from_millis(2))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO bookmarks (video_id, title, created_at, search_text)
             VALUES ('../hostile', 'Bad', 1, 'bad\n')",
        )
        .execute(db.writer())
        .await
        .unwrap();

        let listed = repo.list(10, 0).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "Good");
    }

    #[tokio::test]
    async fn tags_are_batched_across_a_page_larger_than_one_chunk() {
        let (_db, repo) = repo().await;
        let count = TAG_LOOKUP_CHUNK + 3;
        for index in 0..count {
            repo.add(
                &NewBookmark {
                    tags: vec!["all".to_owned()],
                    ..bookmark(&format!("bkm{index:08}"), "T")
                },
                Timestamp::from_millis(i64::try_from(index).unwrap()),
            )
            .await
            .unwrap();
        }

        let page = repo.list(u32::try_from(count).unwrap(), 0).await.unwrap();
        assert_eq!(page.len(), count);
        assert!(
            page.iter().all(|b| b.tags == vec!["all".to_owned()]),
            "chunking must not drop tags for the rows past the first chunk"
        );
    }

    #[tokio::test]
    async fn clearing_removes_everything() {
        let (db, repo) = repo().await;
        repo.add(
            &NewBookmark {
                tags: vec!["t".to_owned()],
                ..bookmark("aaaaaaaaaaa", "T")
            },
            Timestamp::EPOCH,
        )
        .await
        .unwrap();
        assert_eq!(repo.clear().await.unwrap(), 1);
        assert_eq!(repo.count().await.unwrap(), 0);
        let tags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bookmark_tags")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(tags, 0);
    }
}
