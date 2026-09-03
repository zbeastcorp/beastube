//! Repositories: the only supported way to read and write the local database.
//!
//! Every repository is a thin struct holding a cloned [`Database`]. That shape is deliberate:
//!
//! * **Cloning is free.** [`Database`] holds two reference-counted pools, so a repository can be
//!   constructed per call, moved into a task, or stored in application state without an `Arc`
//!   wrapper or a lifetime parameter leaking into every signature.
//! * **No shared mutable state.** A repository owns nothing but the handle, so two repositories
//!   operating concurrently cannot disagree about anything except what SQLite itself arbitrates.
//! * **Reads and writes are routed explicitly.** Every method picks [`Database::reader`] or
//!   [`Database::writer`]; a read never occupies the single writer connection, which is what keeps
//!   a playback checkpoint from queueing behind a history scan.
//!
//! ## Policies that hold across every repository
//!
//! 1. **The caller supplies the clock.** Methods that write a timestamp take one rather than
//!    calling [`Timestamp::now`] internally. A batch import then stamps one consistent instant,
//!    and tests assert on exact values instead of tolerances.
//! 2. **The repository does not second-guess the caller.** Whether history should be recorded is a
//!    privacy decision made from [`beastube_core::Settings`] by the caller; the repository records
//!    what it is told to record. Putting the policy here would mean two places to change it and
//!    one of them silently wrong.
//! 3. **One damaged row costs one row.** Thumbnail blobs degrade to an empty set
//!    ([`decode_thumbnails`]), and a row whose identifier cannot be revalidated is dropped from a
//!    listing ([`degrade`]) rather than failing the whole query. A local library that refuses to
//!    open because one row is bad is worse than a library missing one row (§81).
//! 4. **Driver errors are classified.** Everything goes through [`DbError::from_sqlx`], so
//!    "retry in a moment" (`SQLITE_BUSY`) never reaches the UI as "your database is damaged".
//! 5. **No English reaches the caller.** Rejections are [`DbError::Invalid`] or
//!    [`DbError::NotFound`], which carry i18n keys and parameters, never a sentence.

pub mod bookmarks;
pub mod history;
pub mod playlists;
pub mod positions;
pub mod searches;
pub mod settings;

pub use bookmarks::{BookmarksRepo, NewBookmark};
pub use history::{HistoryRepo, WatchRecord};
pub use playlists::PlaylistsRepo;
pub use positions::PositionsRepo;
pub use searches::{SearchEntry, SearchesRepo};
pub use settings::SettingsRepo;

use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite};

use beastube_core::ids::{ChannelId, VideoId};
use beastube_core::model::thumbnail::ThumbnailSet;
use beastube_core::time_util::Timestamp;

use crate::connection::Database;
use crate::error::{DbError, DbResult};

/// Every repository, constructed once and shared.
///
/// Bundling them is convenience, not coupling: each repository is independently constructible from
/// a [`Database`], and nothing here mediates between them. The application state holds one of these
/// so that a command handler names the repository it needs rather than threading a `Database`
/// through and constructing repositories ad hoc at each call site.
#[derive(Debug, Clone)]
pub struct Repositories {
    /// The settings document.
    pub settings: SettingsRepo,
    /// Watch history.
    pub history: HistoryRepo,
    /// Resume positions.
    pub positions: PositionsRepo,
    /// The user's own playlists.
    pub playlists: PlaylistsRepo,
    /// Bookmarks and their tags.
    pub bookmarks: BookmarksRepo,
    /// Remembered search queries.
    pub searches: SearchesRepo,
}

impl Repositories {
    /// Binds every repository to one database handle.
    #[must_use]
    pub fn new(db: &Database) -> Self {
        Self {
            settings: SettingsRepo::new(db.clone()),
            history: HistoryRepo::new(db.clone()),
            positions: PositionsRepo::new(db.clone()),
            playlists: PlaylistsRepo::new(db.clone()),
            bookmarks: BookmarksRepo::new(db.clone()),
            searches: SearchesRepo::new(db.clone()),
        }
    }
}

/// Reads one column out of a row, classifying a driver failure.
///
/// Every row mapper in this module goes through it so that a schema drift (a renamed column, a
/// type that no longer decodes) surfaces as a classified [`DbError`] rather than as a panic at the
/// call site.
///
/// # Errors
///
/// Returns a [`DbError`] if the column is absent or cannot be decoded into `T`.
pub(crate) fn column<'r, T>(row: &'r SqliteRow, name: &str) -> DbResult<T>
where
    T: sqlx::Decode<'r, Sqlite> + sqlx::Type<Sqlite>,
{
    row.try_get(name).map_err(DbError::from_sqlx)
}

/// Escape character used with every `LIKE` pattern this crate builds.
///
/// `LIKE` treats `%` and `_` as wildcards. Without an escape, searching for `100_000` would match
/// `1000000`, and a query of `%` would match the entire library — which is both wrong and a way to
/// make a search box scan every row.
pub(crate) const LIKE_ESCAPE: char = '\\';

/// Escapes `LIKE` metacharacters in `raw`.
///
/// The escape character itself must be escaped first, or `\%` would be produced from an input of
/// `\` followed by a literal `%` and the two would be indistinguishable.
pub(crate) fn escape_like(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch == LIKE_ESCAPE || ch == '%' || ch == '_' {
            out.push(LIKE_ESCAPE);
        }
        out.push(ch);
    }
    out
}

/// Builds a `LIKE` pattern matching rows containing `needle` anywhere.
pub(crate) fn like_contains(needle: &str) -> String {
    format!("%{}%", escape_like(needle))
}

/// Builds a `LIKE` pattern matching rows starting with `needle`.
pub(crate) fn like_prefix(needle: &str) -> String {
    format!("{}%", escape_like(needle))
}

/// Builds the denormalized `search_text` column: lowercased title, a newline, lowercased channel.
///
/// Maintained on every write so that searching is one scan over a narrow column rather than a scan
/// plus per-row case folding of two wide ones. The newline separator keeps a query from matching
/// across the title/channel boundary, so searching for `blender guru` does not match a video
/// titled "blender" by a channel named "guru's kitchen" purely by adjacency.
pub(crate) fn search_text(title: &str, channel: Option<&str>) -> String {
    let mut out = title.to_lowercase();
    out.push('\n');
    if let Some(channel) = channel {
        out.push_str(&channel.to_lowercase());
    }
    out
}

/// Serializes a value for a JSON column.
///
/// # Errors
///
/// Returns [`DbError::Invalid`] if the value cannot be serialized. That is not reachable for the
/// domain types stored here, but returning an error rather than substituting a placeholder means a
/// future type with a fallible `Serialize` cannot silently write an empty document over user data.
pub(crate) fn encode_json<T: Serialize>(field: &'static str, value: &T) -> DbResult<String> {
    serde_json::to_string(value).map_err(|source| DbError::Invalid {
        field,
        reason: source.to_string(),
    })
}

/// Deserializes a JSON column, reporting failure as [`DbError::Decode`].
///
/// # Errors
///
/// Returns [`DbError::Decode`] identifying the table and column, whose recovery strategy is to
/// rebuild that store rather than to quarantine the database file.
pub(crate) fn decode_json<T: DeserializeOwned>(
    table: &'static str,
    column: &'static str,
    raw: &str,
) -> DbResult<T> {
    serde_json::from_str(raw).map_err(|source| DbError::Decode {
        table,
        column,
        source,
    })
}

/// Decodes a `thumbnails_json` column, degrading a damaged blob to an empty set.
///
/// Deliberately infallible. Thumbnails are decoration over a row that is otherwise intact: losing
/// them costs the user an image placeholder, whereas failing the row would remove a video from
/// their history entirely. The failure is still logged, so a systematic encoding bug is visible in
/// the diagnostics log rather than silent.
pub(crate) fn decode_thumbnails(table: &'static str, raw: Option<&str>) -> ThumbnailSet {
    let Some(raw) = raw else {
        return ThumbnailSet::empty();
    };
    match decode_json::<ThumbnailSet>(table, "thumbnails_json", raw) {
        Ok(set) => set,
        Err(error) => {
            tracing::warn!(table, %error, "thumbnail blob is undecodable; rendering without it");
            ThumbnailSet::empty()
        }
    }
}

/// Serializes a thumbnail set, storing `NULL` for an empty one.
///
/// An empty set is far more common than a populated one for library rows written from a minimal
/// summary, and `NULL` keeps those rows narrow.
///
/// # Errors
///
/// Returns [`DbError::Invalid`] if the set cannot be serialized.
pub(crate) fn encode_thumbnails(thumbnails: &ThumbnailSet) -> DbResult<Option<String>> {
    if thumbnails.is_empty() {
        return Ok(None);
    }
    encode_json("thumbnails", thumbnails).map(Some)
}

/// Drops a row that could not be decoded, keeping the rest of the result set.
///
/// Used by listing methods. A single corrupt row must not empty a user's history view; it is
/// logged and skipped so the remaining rows still render (§81).
pub(crate) fn degrade<T>(decoded: DbResult<T>, table: &'static str) -> Option<T> {
    match decoded {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(table, %error, "discarding an undecodable row");
            None
        }
    }
}

/// Revalidates an identifier read back from the database.
///
/// The database is untrusted input like any other: it may have been written by an older build,
/// hand-edited, or partially overwritten by a crash. A row whose identifier no longer validates
/// cannot be used to build a URL or a cache path, so it is reported as corruption of that row.
///
/// # Errors
///
/// Returns [`DbError::Corrupt`] naming the table and the rejected value.
pub(crate) fn decode_video_id(table: &'static str, raw: &str) -> DbResult<VideoId> {
    VideoId::new(raw).map_err(|source| DbError::Corrupt {
        detail: format!("{table}.video_id: {source}"),
    })
}

/// Revalidates an optional channel identifier, degrading an unusable one to `None`.
///
/// Unlike a video identifier, the channel is not what the row is keyed by: a history entry with an
/// unusable channel still plays, it just cannot link to the channel page. Dropping the link is a
/// better outcome than dropping the entry.
pub(crate) fn decode_channel_id(table: &'static str, raw: Option<&str>) -> Option<ChannelId> {
    let raw = raw?;
    match ChannelId::new(raw) {
        Ok(id) => Some(id),
        Err(error) => {
            tracing::warn!(table, %error, "channel identifier is unusable; dropping the link");
            None
        }
    }
}

/// Converts a domain millisecond count into the signed integer SQLite stores.
///
/// Saturates rather than failing. A `u64` position beyond [`i64::MAX`] milliseconds is roughly 292
/// million years and can only come from a drifted provider value or a corrupted decode; refusing
/// the write would lose the whole checkpoint over a field the UI would not have rendered anyway.
pub(crate) fn to_db_millis(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Converts a stored millisecond count back into the domain's unsigned form.
///
/// A negative value is impossible through the `CHECK` constraints, so encountering one means the
/// file was written by something other than this code; clamping to zero degrades that field rather
/// than the row.
pub(crate) fn from_db_millis(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Reads an optional millisecond column.
pub(crate) fn from_db_millis_opt(value: Option<i64>) -> Option<u64> {
    value.map(from_db_millis)
}

/// Converts an optional domain millisecond count for binding.
pub(crate) fn to_db_millis_opt(value: Option<u64>) -> Option<i64> {
    value.map(to_db_millis)
}

/// Reads a stored `0`/`1` column as a boolean.
///
/// Any non-zero value counts as true, matching how every `CHECK (x IN (0, 1))` column can only
/// hold `0` or `1` while still giving a defined answer if one somehow does not.
// Consumed by the filtering repository, which stores enabled flags as 0/1.
#[allow(dead_code)]
pub(crate) fn from_db_bool(value: i64) -> bool {
    value != 0
}

/// Converts a boolean for binding into a `0`/`1` column.
// Consumed by the filtering repository, which stores enabled flags as 0/1.
#[allow(dead_code)]
pub(crate) fn to_db_bool(value: bool) -> i64 {
    i64::from(value)
}

/// Reads a stored count column as an unsigned count.
pub(crate) fn from_db_count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Reads a stored timestamp column.
pub(crate) fn from_db_timestamp(value: i64) -> Timestamp {
    Timestamp::from_millis(value)
}

/// Reads an optional stored timestamp column.
pub(crate) fn from_db_timestamp_opt(value: Option<i64>) -> Option<Timestamp> {
    value.map(Timestamp::from_millis)
}

/// Widens a page size or limit for binding.
///
/// `u32` in the public signature and `i64` at the driver keeps a caller from ever expressing a
/// negative limit, which SQLite reads as "unbounded".
pub(crate) fn to_db_limit(value: u32) -> i64 {
    i64::from(value)
}

/// Rejects a caller-supplied string that is empty or blank after trimming.
///
/// # Errors
///
/// Returns [`DbError::Invalid`] naming the field.
pub(crate) fn require_non_blank(field: &'static str, value: &str) -> DbResult<()> {
    if value.trim().is_empty() {
        return Err(DbError::Invalid {
            field,
            reason: "blank".to_owned(),
        });
    }
    Ok(())
}

/// Rejects a caller-supplied string longer than `max` characters.
///
/// Bounds exist so that a scripted or hostile caller cannot grow a row without limit; the local
/// database has no other quota.
///
/// # Errors
///
/// Returns [`DbError::Invalid`] naming the field and the limit.
pub(crate) fn require_max_len(field: &'static str, value: &str, max: usize) -> DbResult<()> {
    let len = value.chars().count();
    if len > max {
        return Err(DbError::Invalid {
            field,
            reason: format!("length {len} exceeds {max}"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use beastube_core::model::thumbnail::Thumbnail;

    #[test]
    fn like_wildcards_in_a_query_are_matched_literally() {
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("c:\\path"), "c:\\\\path");
        assert_eq!(like_contains("%"), "%\\%%");
        assert_eq!(like_prefix("_"), "\\_%");
    }

    #[test]
    fn search_text_separates_title_from_channel() {
        assert_eq!(search_text("Blender", Some("Guru")), "blender\nguru");
        assert_eq!(search_text("Blender", None), "blender\n");
        assert_eq!(search_text("ÉCLAIR", None), "éclair\n");
    }

    #[test]
    fn a_damaged_thumbnail_blob_costs_the_images_not_the_row() {
        let set = decode_thumbnails("history", Some("{not json"));
        assert!(set.is_empty(), "a bad blob must not propagate an error");
        assert!(decode_thumbnails("history", None).is_empty());
    }

    #[test]
    fn thumbnails_round_trip_and_empty_sets_store_null() {
        let set = ThumbnailSet::new(vec![Thumbnail::sized("https://example.com/a.jpg", 120, 90)]);
        let encoded = encode_thumbnails(&set).unwrap().expect("non-empty");
        assert_eq!(decode_thumbnails("history", Some(&encoded)).len(), 1);
        assert_eq!(encode_thumbnails(&ThumbnailSet::empty()).unwrap(), None);
    }

    #[test]
    fn millisecond_conversion_saturates_instead_of_wrapping() {
        assert_eq!(to_db_millis(u64::MAX), i64::MAX);
        assert_eq!(to_db_millis(1_500), 1_500);
        assert_eq!(
            from_db_millis(-1),
            0,
            "a negative duration degrades to zero"
        );
        assert_eq!(from_db_millis(i64::MAX), 9_223_372_036_854_775_807);
    }

    #[test]
    fn an_unusable_identifier_is_corruption_of_that_row_only() {
        let err = decode_video_id("history", "../../etc/passwd").unwrap_err();
        assert!(matches!(err, DbError::Corrupt { .. }));
        assert!(decode_video_id("history", "dQw4w9WgXcQ").is_ok());
    }

    #[test]
    fn an_unusable_channel_link_is_dropped_not_fatal() {
        assert!(decode_channel_id("history", Some("a/b")).is_none());
        assert!(decode_channel_id("history", None).is_none());
        assert!(decode_channel_id("history", Some("UCabc")).is_some());
    }

    #[test]
    fn degrading_a_row_yields_none_rather_than_propagating() {
        let bad: DbResult<u8> = Err(DbError::Corrupt {
            detail: "x".to_owned(),
        });
        assert_eq!(degrade(bad, "history"), None);
        assert_eq!(degrade(Ok(7_u8), "history"), Some(7));
    }

    #[test]
    fn blank_and_overlong_inputs_are_rejected_with_a_field_name() {
        assert!(require_non_blank("name", "   ").is_err());
        assert!(require_non_blank("name", " x ").is_ok());
        assert!(require_max_len("name", "abc", 3).is_ok());
        let err = require_max_len("name", "abcd", 3).unwrap_err();
        assert!(matches!(err, DbError::Invalid { field: "name", .. }));
    }

    #[test]
    fn boolean_and_count_columns_decode_defensively() {
        assert!(from_db_bool(1));
        assert!(!from_db_bool(0));
        assert!(from_db_bool(-1), "any non-zero value reads as true");
        assert_eq!(to_db_bool(true), 1);
        assert_eq!(from_db_count(-5), 0);
        assert_eq!(to_db_limit(50), 50);
    }
}
