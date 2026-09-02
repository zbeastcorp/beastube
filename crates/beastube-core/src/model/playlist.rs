//! Playlists, both provider-hosted and locally created.
//!
//! The two are separate types on purpose. A provider playlist is read-only and identified by a
//! [`PlaylistId`]; a local playlist is user-owned, mutable, ordered, and identified by a database
//! row. Merging them into one type would invite a UI that offers "rename" on something it cannot
//! rename, or that implies a local playlist is synchronized with an external account — which it
//! never is (§42).

use serde::{Deserialize, Serialize};

use crate::ids::{ChannelId, PlaylistId};
use crate::model::thumbnail::ThumbnailSet;
use crate::model::video::VideoSummary;
use crate::time_util::Timestamp;

/// The compact shape used by playlist cards in search and channel views.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistSummary {
    /// Provider identifier.
    pub id: PlaylistId,
    /// Playlist title. Untrusted text.
    pub title: String,
    /// Owning channel, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<ChannelId>,
    /// Owning channel name. Untrusted text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_name: Option<String>,
    /// Cover art, usually the first video's thumbnail.
    #[serde(default, skip_serializing_if = "ThumbnailSet::is_empty")]
    pub thumbnails: ThumbnailSet,
    /// Number of videos, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_count: Option<u64>,
}

/// The full shape used by the playlist page, carrying the first page of items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistDetails {
    /// Everything a card shows.
    #[serde(flatten)]
    pub summary: PlaylistSummary,
    /// Description. Untrusted text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// First page of videos, in playlist order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub videos: Vec<VideoSummary>,
}

/// Identifier of a playlist the user created locally.
///
/// A distinct type from [`PlaylistId`] so that a local identifier can never be passed to a provider
/// call, and a provider identifier can never be used as a database key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LocalPlaylistId(i64);

impl LocalPlaylistId {
    /// Wraps a database row identifier.
    #[must_use]
    pub const fn new(id: i64) -> Self {
        Self(id)
    }

    /// The underlying row identifier.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

impl std::fmt::Display for LocalPlaylistId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One entry in a local playlist.
///
/// Carries a denormalized [`VideoSummary`] so the playlist renders without a network round trip,
/// and remains readable offline (§72) even after the metadata cache is cleared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistItem {
    /// The video.
    pub video: VideoSummary,
    /// Sort key within the playlist. Sparse, so a reorder rewrites few rows.
    pub position: i64,
    /// When the user added this item.
    pub added_at: Timestamp,
}

/// A playlist the user created and owns, stored only on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalPlaylist {
    /// Database identifier.
    pub id: LocalPlaylistId,
    /// User-chosen name.
    pub name: String,
    /// Optional user-written description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Number of items, maintained by the storage layer so list views need no join.
    pub item_count: u64,
    /// Creation time.
    pub created_at: Timestamp,
    /// Last modification time, used for "recently updated" ordering.
    pub updated_at: Timestamp,
    /// Cover art, taken from the first item.
    #[serde(default, skip_serializing_if = "ThumbnailSet::is_empty")]
    pub thumbnails: ThumbnailSet,
    /// Whether this is a built-in list (Watch Later, Favorites) rather than a user-created one.
    ///
    /// Built-in lists cannot be renamed or deleted; the UI reads this rather than comparing names,
    /// which would break under localization.
    #[serde(default)]
    pub is_system: bool,
}

/// The well-known local playlists created at first launch.
///
/// Identified by a stable slug rather than by name so that renaming the display string in a
/// translation never orphans the underlying rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemPlaylist {
    /// Queue of videos the user intends to watch.
    WatchLater,
    /// Videos the user marked as favourites.
    Favorites,
}

impl SystemPlaylist {
    /// Every system playlist, in display order.
    pub const ALL: [Self; 2] = [Self::WatchLater, Self::Favorites];

    /// Stable slug, used as the database key and as the i18n suffix for the display name.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::WatchLater => "watch_later",
            Self::Favorites => "favorites",
        }
    }

    /// i18n key for the display name, e.g. `library.playlist.watch_later`.
    #[must_use]
    pub fn name_key(self) -> String {
        format!("library.playlist.{}", self.slug())
    }
}

/// Spacing between adjacent playlist positions.
///
/// Positions are sparse so that inserting between two items usually needs a single row update
/// instead of renumbering the tail. With a gap of 1024, roughly ten insertions can occur at the
/// same point before the storage layer must renumber that region.
pub const POSITION_GAP: i64 = 1024;

/// Computes a position between `before` and `after`.
///
/// Returns `None` when there is no integer strictly between them, which signals the storage layer
/// to renumber the affected range and retry. Callers must handle that case: silently reusing a
/// position would produce a nondeterministic playlist order.
#[must_use]
pub const fn position_between(before: Option<i64>, after: Option<i64>) -> Option<i64> {
    match (before, after) {
        (None, None) => Some(0),
        (Some(b), None) => b.checked_add(POSITION_GAP),
        (None, Some(a)) => a.checked_sub(POSITION_GAP),
        (Some(b), Some(a)) => {
            // `a - b` overflows when the endpoints straddle the i64 range, so the gap is probed
            // with a checked subtraction: `None` means the span exceeded i64, which is
            // unambiguously wide enough to insert into.
            match a.checked_sub(b) {
                // Adjacent, equal, or out of order: no integer strictly between them.
                Some(gap) if gap < 2 => None,
                // Floor midpoint computed without ever forming `a + b` or `a - b`, so it is exact
                // even at i64::MIN..=i64::MAX.
                _ => Some((a & b) + ((a ^ b) >> 1)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appending_to_an_empty_list_starts_at_zero() {
        assert_eq!(position_between(None, None), Some(0));
    }

    #[test]
    fn appending_advances_by_the_gap() {
        assert_eq!(position_between(Some(0), None), Some(POSITION_GAP));
        assert_eq!(position_between(Some(5120), None), Some(6144));
    }

    #[test]
    fn prepending_moves_back_by_the_gap() {
        assert_eq!(position_between(None, Some(0)), Some(-POSITION_GAP));
    }

    #[test]
    fn inserting_between_takes_the_midpoint() {
        assert_eq!(position_between(Some(0), Some(1024)), Some(512));
        assert_eq!(position_between(Some(0), Some(2)), Some(1));
    }

    #[test]
    fn exhausted_gaps_signal_a_renumber_instead_of_colliding() {
        assert_eq!(position_between(Some(5), Some(6)), None);
        assert_eq!(position_between(Some(5), Some(5)), None);
        // Out-of-order input must not silently produce a position.
        assert_eq!(position_between(Some(9), Some(3)), None);
    }

    #[test]
    fn positions_never_overflow_at_the_extremes() {
        assert_eq!(position_between(Some(i64::MAX), None), None);
        assert_eq!(position_between(None, Some(i64::MIN)), None);
        // A span covering the whole i64 range still yields an exact floor midpoint rather
        // than overflowing: floor((MIN + MAX) / 2) == floor(-1 / 2) == -1.
        assert_eq!(position_between(Some(i64::MIN), Some(i64::MAX)), Some(-1));
        assert_eq!(position_between(Some(-9), Some(9)), Some(0));
        assert_eq!(position_between(Some(-5), Some(-3)), Some(-4));
    }

    #[test]
    fn repeated_midpoint_insertion_converges_rather_than_looping() {
        // Simulates dragging an item to the same slot repeatedly: eventually the gap is exhausted
        // and the storage layer is told to renumber, instead of the loop never terminating.
        let (mut before, after) = (0_i64, POSITION_GAP);
        let mut inserts = 0;
        while let Some(position) = position_between(Some(before), Some(after)) {
            before = position;
            inserts += 1;
            assert!(inserts < 64, "insertion must terminate");
        }
        assert!(
            inserts >= 9,
            "a 1024 gap should absorb ~10 insertions, got {inserts}"
        );
    }

    #[test]
    fn system_playlists_use_stable_slugs_not_display_names() {
        assert_eq!(SystemPlaylist::WatchLater.slug(), "watch_later");
        assert_eq!(
            SystemPlaylist::Favorites.name_key(),
            "library.playlist.favorites"
        );
        assert_eq!(SystemPlaylist::ALL.len(), 2);
    }

    #[test]
    fn local_and_provider_identifiers_are_distinct_types() {
        let local = LocalPlaylistId::new(7);
        assert_eq!(local.get(), 7);
        assert_eq!(local.to_string(), "7");
        assert_eq!(serde_json::to_string(&local).unwrap(), "7");
    }
}
