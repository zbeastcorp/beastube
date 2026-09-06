//! The provider-neutral domain model.
//!
//! These types are what the UI, the database and the provider adapters all agree on. They are
//! deliberately *not* a mirror of any provider's response shape: an adapter's job is to translate
//! into these types, absorbing upstream schema drift so that a provider change never propagates
//! past the adapter boundary (§118).
//!
//! Two conventions run throughout:
//!
//! * **Durations and positions are milliseconds** (`u64`), matching [`crate::time_util`].
//! * **Absent means unknown, not zero.** A provider that omits a view count yields `None`, so the
//!   UI can hide the field instead of rendering a confident and wrong "0 views".

pub mod channel;
pub mod library;
pub mod playlist;
pub mod search;
pub mod stream;
pub mod thumbnail;
pub mod video;

pub use channel::{ChannelDetails, ChannelSummary};
pub use library::{
    Bookmark, COMPLETION_THRESHOLD_FRACTION, HistoryEntry, PlaybackPosition, WatchState,
};
pub use playlist::{
    LocalPlaylist, LocalPlaylistId, PlaylistDetails, PlaylistItem, PlaylistSummary,
};
pub use search::{
    SearchFilters, SearchItem, SearchResultKind, SearchResults, SearchSortOrder, Suggestion,
    UploadDateFilter, VideoDurationFilter, VideoFeatureFilter,
};
pub use stream::{AudioCodec, AudioStream, ByteRange, Quality, StreamSet, VideoCodec, VideoStream};
pub use thumbnail::{Thumbnail, ThumbnailSet};
pub use video::{
    AudioTrack, CaptionTrack, Chapter, Cue, LiveStatus, VideoDetails, VideoSummary,
};

use serde::{Deserialize, Deserializer, Serialize};

/// Maximum accepted length of a continuation token.
///
/// Provider continuation tokens are opaque and can be large; the bound exists so a drifted or
/// hostile response cannot make us hold an unbounded string in memory or in the database.
pub const MAX_CONTINUATION_LEN: usize = 8192;

/// An opaque, provider-specific cursor for fetching the next page of a paginated result.
///
/// The application never interprets the contents. It is validated only for length and for the
/// absence of control characters, since it is round-tripped through JSON and the database.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ContinuationToken(String);

impl ContinuationToken {
    /// Wraps a provider cursor.
    ///
    /// # Errors
    ///
    /// Returns `Err` with a static reason if the token is empty, exceeds
    /// [`MAX_CONTINUATION_LEN`], or contains a control character.
    pub fn new(raw: impl Into<String>) -> Result<Self, &'static str> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err("continuation token is empty");
        }
        if raw.len() > MAX_CONTINUATION_LEN {
            return Err("continuation token exceeds maximum length");
        }
        if raw.chars().any(char::is_control) {
            return Err("continuation token contains a control character");
        }
        Ok(Self(raw))
    }

    /// Borrows the token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ContinuationToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

/// One page of a paginated collection.
///
/// Pagination is expressed as an explicit cursor rather than an offset because provider feeds are
/// not stable under insertion: an offset re-reads or skips items when the upstream list changes
/// between requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    /// The items in this page, in provider order.
    pub items: Vec<T>,
    /// Cursor for the next page, or `None` when the collection is exhausted.
    #[serde(default = "none", skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ContinuationToken>,
    /// Total item count, when the provider reports one. Frequently absent or approximate.
    #[serde(default = "none", skip_serializing_if = "Option::is_none")]
    pub total_estimate: Option<u64>,
}

/// serde `default` helper. A plain `Option::default` cannot be named in a `default = "..."` path
/// for a generic field without a turbofish, so this monomorphic helper stands in.
fn none<T>() -> Option<T> {
    None
}

impl<T> Page<T> {
    /// A page with no items and no continuation.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            items: Vec::new(),
            continuation: None,
            total_estimate: None,
        }
    }

    /// A terminal page containing `items` and no continuation.
    #[must_use]
    pub const fn final_page(items: Vec<T>) -> Self {
        Self {
            items,
            continuation: None,
            total_estimate: None,
        }
    }

    /// Whether another page can be requested.
    #[must_use]
    pub const fn has_more(&self) -> bool {
        self.continuation.is_some()
    }

    /// Whether this page carries no items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Number of items in this page.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Applies `f` to every item, preserving the cursor.
    #[must_use]
    pub fn map<U, F: FnMut(T) -> U>(self, f: F) -> Page<U> {
        Page {
            items: self.items.into_iter().map(f).collect(),
            continuation: self.continuation,
            total_estimate: self.total_estimate,
        }
    }

    /// Keeps only items satisfying `predicate`, preserving the cursor.
    ///
    /// Used by the filtering layer, which must be able to drop items without terminating
    /// pagination — dropping the cursor would silently truncate the feed.
    #[must_use]
    pub fn retain<F: FnMut(&T) -> bool>(mut self, mut predicate: F) -> Self {
        self.items.retain(|item| predicate(item));
        self
    }
}

impl<T> Default for Page<T> {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_rejects_hostile_input() {
        assert!(ContinuationToken::new("").is_err());
        assert!(ContinuationToken::new("a\u{0}b").is_err());
        assert!(ContinuationToken::new("a\nb").is_err());
        assert!(ContinuationToken::new("x".repeat(MAX_CONTINUATION_LEN + 1)).is_err());
        assert!(ContinuationToken::new("4qmFsgKPARIYVUN1QVhGa2dzdzFMN3hhQ2Zu").is_ok());
    }

    #[test]
    fn continuation_deserialization_validates() {
        let bad: Result<ContinuationToken, _> = serde_json::from_str("\"\"");
        assert!(bad.is_err());
        let ok: Result<ContinuationToken, _> = serde_json::from_str("\"abc123==\"");
        assert!(ok.is_ok());
    }

    #[test]
    fn retain_preserves_the_cursor() {
        let page = Page {
            items: vec![1, 2, 3, 4],
            continuation: Some(ContinuationToken::new("next").unwrap()),
            total_estimate: Some(100),
        };
        let filtered = page.retain(|n| n % 2 == 0);
        assert_eq!(filtered.items, vec![2, 4]);
        assert!(
            filtered.has_more(),
            "filtering must not terminate pagination"
        );
    }

    #[test]
    fn filtering_every_item_still_allows_the_next_page() {
        let page = Page {
            items: vec![1, 3, 5],
            continuation: Some(ContinuationToken::new("next").unwrap()),
            total_estimate: None,
        };
        let filtered = page.retain(|n| n % 2 == 0);
        assert!(filtered.is_empty());
        assert!(
            filtered.has_more(),
            "an empty page with a cursor must still be pageable, or aggressive filtering ends the feed"
        );
    }

    #[test]
    fn empty_page_omits_optional_fields_on_the_wire() {
        let json = serde_json::to_string(&Page::<u32>::empty()).unwrap();
        assert_eq!(json, r#"{"items":[]}"#);
    }

    #[test]
    fn page_round_trips() {
        let page = Page::final_page(vec!["a".to_owned(), "b".to_owned()]);
        let json = serde_json::to_string(&page).unwrap();
        let back: Page<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(page, back);
        assert!(!back.has_more());
    }
}
