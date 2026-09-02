//! Channel metadata.
//!
//! Follows the same summary/details split as [`crate::model::video`], for the same reason: channel
//! cards appear in search results and subscription lists where the heavy shape would be wasteful.

use serde::{Deserialize, Serialize};

use crate::ids::ChannelId;
use crate::model::thumbnail::ThumbnailSet;

/// The compact shape used by search results and follow lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSummary {
    /// Provider identifier.
    pub id: ChannelId,
    /// Display name. Untrusted text.
    pub name: String,
    /// Avatar renditions.
    #[serde(default, skip_serializing_if = "ThumbnailSet::is_empty")]
    pub avatar: ThumbnailSet,
    /// Subscriber count, when reported. Frequently rounded by the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscriber_count: Option<u64>,
    /// Handle without the leading `@`, when the provider exposes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// Whether the provider marks the channel as verified.
    #[serde(default)]
    pub is_verified: bool,
}

impl ChannelSummary {
    /// Minimal summary for a channel whose metadata has not been fetched.
    #[must_use]
    pub fn placeholder(id: ChannelId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            avatar: ThumbnailSet::empty(),
            subscriber_count: None,
            handle: None,
            is_verified: false,
        }
    }

    /// The handle rendered with its `@` prefix, when present.
    #[must_use]
    pub fn display_handle(&self) -> Option<String> {
        self.handle.as_ref().map(|h| format!("@{h}"))
    }
}

/// Content tabs a channel may expose.
///
/// Reported per channel rather than assumed, so the UI renders only tabs that exist: a channel with
/// no Shorts must not show an empty Shorts tab (§131).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelTab {
    /// Long-form uploads.
    Videos,
    /// Short-form vertical videos.
    Shorts,
    /// Live and past-live broadcasts.
    Live,
    /// Playlists curated by the channel.
    Playlists,
}

impl ChannelTab {
    /// Stable identifier for routing and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Videos => "videos",
            Self::Shorts => "shorts",
            Self::Live => "live",
            Self::Playlists => "playlists",
        }
    }
}

/// The full shape used by the channel page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelDetails {
    /// Everything a card shows.
    #[serde(flatten)]
    pub summary: ChannelSummary,
    /// Channel description. Untrusted text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Banner renditions for the page header.
    #[serde(default, skip_serializing_if = "ThumbnailSet::is_empty")]
    pub banner: ThumbnailSet,
    /// Tabs this channel actually has content for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_tabs: Vec<ChannelTab>,
    /// Total video count, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_count: Option<u64>,
    /// Canonical provider URL, for the "open externally" action. Validated before use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
}

impl ChannelDetails {
    /// Whether the channel exposes `tab`.
    #[must_use]
    pub fn has_tab(&self, tab: ChannelTab) -> bool {
        self.available_tabs.contains(&tab)
    }

    /// The tab to open by default: `Videos` when present, otherwise the first available.
    #[must_use]
    pub fn default_tab(&self) -> Option<ChannelTab> {
        if self.has_tab(ChannelTab::Videos) {
            Some(ChannelTab::Videos)
        } else {
            self.available_tabs.first().copied()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details(tabs: Vec<ChannelTab>) -> ChannelDetails {
        ChannelDetails {
            summary: ChannelSummary::placeholder(
                ChannelId::new("UCuAXFkgsw1L7xaCfnd5JJOw").unwrap(),
                "Rick Astley",
            ),
            description: None,
            banner: ThumbnailSet::empty(),
            available_tabs: tabs,
            video_count: None,
            canonical_url: None,
        }
    }

    #[test]
    fn default_tab_prefers_videos_regardless_of_order() {
        let d = details(vec![ChannelTab::Shorts, ChannelTab::Videos]);
        assert_eq!(d.default_tab(), Some(ChannelTab::Videos));
    }

    #[test]
    fn default_tab_falls_back_to_the_first_available() {
        let d = details(vec![ChannelTab::Playlists, ChannelTab::Shorts]);
        assert_eq!(d.default_tab(), Some(ChannelTab::Playlists));
    }

    #[test]
    fn a_channel_with_no_tabs_offers_none() {
        let d = details(vec![]);
        assert_eq!(d.default_tab(), None);
        assert!(!d.has_tab(ChannelTab::Shorts));
    }

    #[test]
    fn handle_is_rendered_with_its_prefix() {
        let mut summary = ChannelSummary::placeholder(ChannelId::new("UCabc").unwrap(), "Example");
        assert_eq!(summary.display_handle(), None);
        summary.handle = Some("example".to_owned());
        assert_eq!(summary.display_handle().as_deref(), Some("@example"));
    }

    #[test]
    fn details_flattens_the_summary_on_the_wire() {
        let json = serde_json::to_string(&details(vec![ChannelTab::Videos])).unwrap();
        assert!(json.contains("\"name\":\"Rick Astley\""), "{json}");
        assert!(!json.contains("\"summary\""), "{json}");
        let back: ChannelDetails = serde_json::from_str(&json).unwrap();
        assert!(back.has_tab(ChannelTab::Videos));
    }
}
