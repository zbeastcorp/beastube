//! Search history.
//!
//! What the user typed into the search box, stored so the suggestion list can lead with their own
//! previous queries the way YouTube's does — except that here the list is built on this device from
//! this device's rows, and nothing about it is sent anywhere.
//!
//! ## Why the query is the primary key
//!
//! Searching the same thing twice is the normal case, not a duplicate to be tolerated. Keying on
//! the query text collapses repeats into one row with a count, which is what makes "most-used
//! first" cheap and keeps the table from growing once per keystroke-completed search.
//!
//! ## Normalization, and its limit
//!
//! Queries are trimmed and inner whitespace is collapsed before storage, so `  cats   video ` and
//! `cats video` are the same row. Case is **not** folded: `NASA` and `nasa` stay distinct, because
//! the stored text is displayed back to the user and lowercasing their own query would look like a
//! bug. Prefix matching compensates by comparing case-insensitively in SQL.
//!
//! ## Bounding
//!
//! [`SearchesRepo::prune`] enforces the user's configured ceiling. Without it the table is the one
//! store in the application that grows without limit in ordinary use, and an unbounded local record
//! of every search is precisely the artefact this application exists to not create.

use beastube_core::model::Suggestion;
use beastube_core::time_util::Timestamp;
use sqlx::sqlite::SqliteRow;

use crate::connection::Database;
use crate::error::{DbError, DbResult};

use super::{
    LIKE_ESCAPE, column, degrade, from_db_count, from_db_timestamp, like_prefix, require_max_len,
    require_non_blank, to_db_limit,
};

/// Longest query stored, in characters.
///
/// Far beyond any real search, and short enough that a scripted caller cannot grow a row without
/// bound. A longer query is rejected rather than truncated: a truncated query re-run from the
/// suggestion list would search for something the user never typed.
const MAX_QUERY_LEN: usize = 256;

/// One remembered query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEntry {
    /// The query as the user typed it, trimmed. Untrusted text.
    pub query: String,
    /// When it was last searched.
    pub last_searched_at: Timestamp,
    /// How many times it has been searched.
    pub search_count: u64,
}

impl SearchEntry {
    /// Renders the entry as a suggestion, marked as coming from the local history.
    #[must_use]
    pub fn into_suggestion(self) -> Suggestion {
        Suggestion {
            text: self.query,
            from_history: true,
        }
    }
}

/// Reads and writes the local search history.
#[derive(Debug, Clone)]
pub struct SearchesRepo {
    db: Database,
}

impl SearchesRepo {
    /// Binds the repository to a database handle.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Records a search, or bumps the existing row for the same query.
    ///
    /// Whether a search *should* be recorded is a privacy decision the caller makes; this records
    /// what it is told to record.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] if the query is blank or longer than [`MAX_QUERY_LEN`], or a
    /// [`DbError`] if the write fails.
    pub async fn record(&self, query: &str, at: Timestamp) -> DbResult<()> {
        let normalized = normalize(query);
        require_non_blank("query", &normalized)?;
        require_max_len("query", &normalized, MAX_QUERY_LEN)?;

        sqlx::query(
            "INSERT INTO search_history (query, last_searched_at, search_count)
             VALUES (?, ?, 1)
             ON CONFLICT(query) DO UPDATE SET
                 last_searched_at = excluded.last_searched_at,
                 search_count     = search_history.search_count + 1",
        )
        .bind(&normalized)
        .bind(at.as_millis())
        .execute(self.db.writer())
        .await
        .map_err(DbError::from_sqlx)?;
        Ok(())
    }

    /// The most recently searched queries, newest first.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails.
    pub async fn recent(&self, limit: u32) -> DbResult<Vec<SearchEntry>> {
        let rows = sqlx::query(
            "SELECT query, last_searched_at, search_count
             FROM search_history
             ORDER BY last_searched_at DESC
             LIMIT ?",
        )
        .bind(to_db_limit(limit))
        .fetch_all(self.db.reader())
        .await
        .map_err(DbError::from_sqlx)?;

        Ok(map_entries(&rows))
    }

    /// Remembered queries starting with `prefix`, best first.
    ///
    /// Ordering is by how often the query was searched and then by recency, which is what makes a
    /// long-standing habit outrank a query typed once yesterday. Matching is case-insensitive so
    /// typing `na` offers back a stored `NASA`; SQLite's `LIKE` is ASCII-case-insensitive by
    /// default, and the arguments are lowercased so a non-ASCII prefix still matches consistently.
    ///
    /// An empty or blank prefix returns nothing rather than everything: the caller asking for
    /// completions of nothing wants [`SearchesRepo::recent`], and silently substituting it would
    /// put the whole history on screen the moment the field is focused.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails.
    pub async fn matching(&self, prefix: &str, limit: u32) -> DbResult<Vec<SearchEntry>> {
        let normalized = normalize(prefix);
        if normalized.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = like_prefix(&normalized.to_lowercase());

        let rows = sqlx::query(
            "SELECT query, last_searched_at, search_count
             FROM search_history
             WHERE LOWER(query) LIKE ? ESCAPE ?
             ORDER BY search_count DESC, last_searched_at DESC
             LIMIT ?",
        )
        .bind(pattern)
        .bind(LIKE_ESCAPE.to_string())
        .bind(to_db_limit(limit))
        .fetch_all(self.db.reader())
        .await
        .map_err(DbError::from_sqlx)?;

        Ok(map_entries(&rows))
    }

    /// Forgets one query. Returns whether a row was removed.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the delete fails.
    pub async fn delete(&self, query: &str) -> DbResult<bool> {
        let normalized = normalize(query);
        let result = sqlx::query("DELETE FROM search_history WHERE query = ?")
            .bind(&normalized)
            .execute(self.db.writer())
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(result.rows_affected() > 0)
    }

    /// Forgets everything. Returns how many queries were removed.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the delete fails.
    pub async fn clear(&self) -> DbResult<u64> {
        let result = sqlx::query("DELETE FROM search_history")
            .execute(self.db.writer())
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(result.rows_affected())
    }

    /// How many queries are remembered.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the query fails.
    pub async fn count(&self) -> DbResult<u64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM search_history")
            .fetch_one(self.db.reader())
            .await
            .map_err(DbError::from_sqlx)?;
        Ok(from_db_count(count))
    }

    /// Drops all but the `keep` most recent queries. Returns how many were removed.
    ///
    /// A `keep` of zero clears the table, which is the honest reading of a ceiling of zero.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the delete fails.
    pub async fn prune(&self, keep: u32) -> DbResult<u64> {
        // Deleting by "not in the newest N" rather than by age: the setting is a count, and an
        // age-based rule would leave a heavy user with far more rows than they asked to keep.
        let result = sqlx::query(
            "DELETE FROM search_history
             WHERE query NOT IN (
                 SELECT query FROM search_history ORDER BY last_searched_at DESC LIMIT ?
             )",
        )
        .bind(to_db_limit(keep))
        .execute(self.db.writer())
        .await
        .map_err(DbError::from_sqlx)?;
        Ok(result.rows_affected())
    }
}

/// Trims and collapses inner whitespace, leaving case alone.
fn normalize(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Maps a result set, dropping rows that cannot be decoded.
fn map_entries(rows: &[SqliteRow]) -> Vec<SearchEntry> {
    rows.iter()
        .filter_map(|row| degrade(entry_from_row(row), "search_history"))
        .collect()
}

fn entry_from_row(row: &SqliteRow) -> DbResult<SearchEntry> {
    Ok(SearchEntry {
        query: column(row, "query")?,
        last_searched_at: from_db_timestamp(column(row, "last_searched_at")?),
        search_count: from_db_count(column(row, "search_count")?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::Database;

    async fn repo() -> SearchesRepo {
        let db = Database::open_in_memory()
            .await
            .expect("in-memory database opens");
        SearchesRepo::new(db)
    }

    fn at(millis: i64) -> Timestamp {
        Timestamp::from_millis(millis)
    }

    #[tokio::test]
    async fn searching_the_same_thing_twice_bumps_one_row() {
        let repo = repo().await;
        repo.record("rust async", at(1_000)).await.unwrap();
        repo.record("rust async", at(2_000)).await.unwrap();

        let entries = repo.recent(10).await.unwrap();
        assert_eq!(entries.len(), 1, "a repeat is not a second row");
        assert_eq!(entries[0].search_count, 2);
        assert_eq!(entries[0].last_searched_at, at(2_000));
    }

    #[tokio::test]
    async fn whitespace_differences_are_the_same_query() {
        let repo = repo().await;
        repo.record("  lofi   beats ", at(1_000)).await.unwrap();
        repo.record("lofi beats", at(2_000)).await.unwrap();

        let entries = repo.recent(10).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].query, "lofi beats");
    }

    #[tokio::test]
    async fn case_is_preserved_but_matching_ignores_it() {
        // Lowercasing what is displayed back would look like a bug; matching must not care.
        let repo = repo().await;
        repo.record("NASA launch", at(1_000)).await.unwrap();

        let entries = repo.matching("nasa", 10).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].query, "NASA launch");
    }

    #[tokio::test]
    async fn matches_are_prefixes_not_substrings() {
        let repo = repo().await;
        repo.record("blender tutorial", at(1_000)).await.unwrap();

        assert_eq!(repo.matching("blender", 10).await.unwrap().len(), 1);
        assert!(
            repo.matching("tutorial", 10).await.unwrap().is_empty(),
            "a mid-query word must not complete the whole query"
        );
    }

    #[tokio::test]
    async fn a_wildcard_in_the_prefix_is_matched_literally() {
        // Without escaping, typing `%` would offer back the entire search history.
        let repo = repo().await;
        repo.record("100% wool", at(1_000)).await.unwrap();
        repo.record("something else", at(2_000)).await.unwrap();

        let entries = repo.matching("100%", 10).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].query, "100% wool");
    }

    #[tokio::test]
    async fn habitual_queries_outrank_recent_one_offs() {
        let repo = repo().await;
        repo.record("cats", at(1_000)).await.unwrap();
        repo.record("cats", at(1_100)).await.unwrap();
        repo.record("cats", at(1_200)).await.unwrap();
        repo.record("cathedral", at(9_000)).await.unwrap();

        let entries = repo.matching("cat", 10).await.unwrap();
        assert_eq!(entries[0].query, "cats", "count wins over recency");
        assert_eq!(entries[1].query, "cathedral");
    }

    #[tokio::test]
    async fn a_blank_prefix_offers_nothing_rather_than_everything() {
        let repo = repo().await;
        repo.record("anything", at(1_000)).await.unwrap();
        assert!(repo.matching("   ", 10).await.unwrap().is_empty());
        assert!(repo.matching("", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn blank_and_overlong_queries_are_refused() {
        let repo = repo().await;
        assert!(matches!(
            repo.record("   ", at(1_000)).await,
            Err(DbError::Invalid { field: "query", .. })
        ));
        let long = "x".repeat(MAX_QUERY_LEN + 1);
        assert!(matches!(
            repo.record(&long, at(1_000)).await,
            Err(DbError::Invalid { field: "query", .. })
        ));
        assert_eq!(repo.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn one_query_can_be_forgotten_without_touching_the_rest() {
        let repo = repo().await;
        repo.record("keep me", at(1_000)).await.unwrap();
        repo.record("forget me", at(2_000)).await.unwrap();

        assert!(repo.delete("forget me").await.unwrap());
        assert!(!repo.delete("forget me").await.unwrap(), "already gone");

        let remaining = repo.recent(10).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].query, "keep me");
    }

    #[tokio::test]
    async fn clearing_reports_how_much_was_removed() {
        let repo = repo().await;
        repo.record("a", at(1_000)).await.unwrap();
        repo.record("b", at(2_000)).await.unwrap();
        assert_eq!(repo.clear().await.unwrap(), 2);
        assert_eq!(repo.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn pruning_keeps_the_newest_and_nothing_more() {
        let repo = repo().await;
        for (index, query) in ["oldest", "middle", "newest"].iter().enumerate() {
            let ordinal = i64::try_from(index).expect("three elements fit");
            repo.record(query, at(1_000 * (ordinal + 1))).await.unwrap();
        }

        assert_eq!(repo.prune(2).await.unwrap(), 1);
        let remaining = repo.recent(10).await.unwrap();
        assert_eq!(
            remaining.iter().map(|e| e.query.as_str()).collect::<Vec<_>>(),
            ["newest", "middle"]
        );

        assert_eq!(repo.prune(0).await.unwrap(), 2, "a ceiling of zero clears");
        assert_eq!(repo.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn an_entry_becomes_a_suggestion_marked_as_local() {
        let repo = repo().await;
        repo.record("local thing", at(1_000)).await.unwrap();
        let suggestion = repo.recent(1).await.unwrap().remove(0).into_suggestion();
        assert_eq!(suggestion.text, "local thing");
        assert!(
            suggestion.from_history,
            "the UI renders local suggestions differently and must be able to tell"
        );
    }
}
