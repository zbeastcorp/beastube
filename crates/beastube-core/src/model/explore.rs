//! The provider's browsable categories.
//!
//! These are the destinations the site groups under *Explore*: a fixed set of editorial hubs, each
//! collecting videos across many channels. They are not search results and not a personalised feed
//! — no account is involved in reading any of them, which is what makes them usable here.
//!
//! The set is deliberately closed. Every member is one this build has confirmed returns videos
//! without signing in, and the hub each one reads is recorded in the adapter rather than here, so
//! this stays a vocabulary rather than a piece of provider knowledge.

use serde::{Deserialize, Serialize};

/// A category the viewer can browse.
///
/// ## Why the site's own Explore list is not reproduced whole
///
/// Three of its entries cannot be served by this build, and each is absent rather than present and
/// failing:
///
/// - **Trending** was retired by the provider itself; its browse id now answers `400` for every
///   variant, so there is nothing left to show and nothing to restore.
/// - **Movies & TV** and **Podcasts** still exist on the site, but their feeds return no videos to
///   a signed-out reader — they are storefronts rather than lists of watchable things.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExploreCategory {
    /// Music videos and performances.
    Music,
    /// Gaming.
    Gaming,
    /// Streams that are live now.
    Live,
    /// News reporting.
    News,
    /// Sport.
    Sport,
    /// Teaching and explanatory material.
    Learning,
    /// Fashion and beauty.
    Fashion,
}

impl ExploreCategory {
    /// Every category, in the order the surface lists them.
    ///
    /// Music first because it is the largest of them, then the site's own ordering.
    pub const ALL: [Self; 7] = [
        Self::Music,
        Self::Gaming,
        Self::Live,
        Self::News,
        Self::Sport,
        Self::Learning,
        Self::Fashion,
    ];

    /// Stable identifier for routing, settings and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Music => "music",
            Self::Gaming => "gaming",
            Self::Live => "live",
            Self::News => "news",
            Self::Sport => "sport",
            Self::Learning => "learning",
            Self::Fashion => "fashion",
        }
    }

    /// Parses the identifier [`Self::as_str`] produces, or `None` for anything else.
    ///
    /// Deliberately not an inherent `from_str`: that name shadows [`std::str::FromStr`], and this
    /// is the same operation, so the trait is implemented below instead.
    #[must_use]
    pub fn from_id(raw: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|category| category.as_str() == raw)
    }
}

/// Rejects an unknown identifier rather than resolving it to anything.
///
/// A retired or mistyped category must not quietly open a different one.
impl std::str::FromStr for ExploreCategory {
    type Err = ();

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::from_id(raw).ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_identifier_round_trips() {
        for category in ExploreCategory::ALL {
            assert_eq!(ExploreCategory::from_id(category.as_str()), Some(category));
        }
    }

    #[test]
    fn an_unknown_identifier_is_refused_rather_than_guessed() {
        assert_eq!(ExploreCategory::from_id("trending"), None);
        assert_eq!(ExploreCategory::from_id(""), None);
        assert_eq!(ExploreCategory::from_id("MUSIC"), None);
    }

    #[test]
    fn identifiers_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for category in ExploreCategory::ALL {
            assert!(seen.insert(category.as_str()), "{category:?} repeats an id");
        }
    }

    #[test]
    fn the_wire_form_is_the_routing_form() {
        // The surface routes on `as_str` and deserialises on serde; they must not drift apart.
        for category in ExploreCategory::ALL {
            let json = serde_json::to_string(&category).unwrap();
            assert_eq!(json, format!("\"{}\"", category.as_str()));
        }
    }
}
