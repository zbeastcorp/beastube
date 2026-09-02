//! Thumbnail descriptors and size selection.
//!
//! A provider returns several renditions of the same image. Choosing among them is a performance
//! decision, not a cosmetic one: fetching a 1280×720 thumbnail for a 210 px card wastes bandwidth,
//! decode time and cache space, multiplied by every card in a virtualized grid. Selection therefore
//! lives here, next to the data, rather than being re-implemented per call site.

use serde::{Deserialize, Serialize};

/// A single thumbnail rendition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thumbnail {
    /// Absolute URL of the image.
    pub url: String,
    /// Pixel width, when the provider reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Pixel height, when the provider reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

impl Thumbnail {
    /// Creates a rendition with known dimensions.
    #[must_use]
    pub fn sized(url: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            url: url.into(),
            width: Some(width),
            height: Some(height),
        }
    }

    /// Creates a rendition of unknown size.
    #[must_use]
    pub fn unsized_at(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            width: None,
            height: None,
        }
    }

    /// Aspect ratio (width / height), when both dimensions are known.
    ///
    /// Used to reserve layout space before the image loads, which is what keeps a scrolling grid
    /// free of cumulative layout shift (§90).
    #[must_use]
    pub fn aspect_ratio(&self) -> Option<f32> {
        match (self.width, self.height) {
            #[allow(clippy::cast_precision_loss)]
            (Some(w), Some(h)) if h > 0 => Some(w as f32 / h as f32),
            _ => None,
        }
    }
}

/// The set of renditions a provider offered for one image, kept in ascending width order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThumbnailSet {
    renditions: Vec<Thumbnail>,
}

impl ThumbnailSet {
    /// Builds a set, sorting by ascending width so selection is a single scan.
    ///
    /// Renditions of unknown width sort last: they are usable as a fallback but cannot participate
    /// in size-based selection.
    #[must_use]
    pub fn new(mut renditions: Vec<Thumbnail>) -> Self {
        renditions.sort_by_key(|t| t.width.unwrap_or(u32::MAX));
        Self { renditions }
    }

    /// An empty set.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            renditions: Vec::new(),
        }
    }

    /// Whether any rendition is available.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.renditions.is_empty()
    }

    /// Number of renditions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.renditions.len()
    }

    /// All renditions, ascending by width.
    #[must_use]
    pub fn renditions(&self) -> &[Thumbnail] {
        &self.renditions
    }

    /// The smallest rendition at least `target_width` wide, falling back to the largest available.
    ///
    /// Choosing the smallest *sufficient* rendition rather than the largest available is what keeps
    /// a 10 000-item history view from pulling megabytes of oversized JPEGs. Falling back to the
    /// largest when none is big enough is deliberate: an upscaled small image looks broken, whereas
    /// a downscaled large one merely costs bandwidth once.
    #[must_use]
    pub fn best_for_width(&self, target_width: u32) -> Option<&Thumbnail> {
        self.renditions
            .iter()
            .find(|t| t.width.is_some_and(|w| w >= target_width))
            .or_else(|| self.largest())
    }

    /// The largest rendition by width, preferring one with known dimensions.
    #[must_use]
    pub fn largest(&self) -> Option<&Thumbnail> {
        self.renditions
            .iter()
            .rev()
            .find(|t| t.width.is_some())
            .or_else(|| self.renditions.last())
    }

    /// The smallest rendition, used for blurred placeholders and tray/notification art.
    #[must_use]
    pub fn smallest(&self) -> Option<&Thumbnail> {
        self.renditions.first()
    }
}

impl FromIterator<Thumbnail> for ThumbnailSet {
    fn from_iter<I: IntoIterator<Item = Thumbnail>>(iter: I) -> Self {
        Self::new(iter.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ThumbnailSet {
        ThumbnailSet::new(vec![
            Thumbnail::sized("https://i.ytimg.com/vi/x/maxresdefault.jpg", 1280, 720),
            Thumbnail::sized("https://i.ytimg.com/vi/x/default.jpg", 120, 90),
            Thumbnail::sized("https://i.ytimg.com/vi/x/mqdefault.jpg", 320, 180),
            Thumbnail::sized("https://i.ytimg.com/vi/x/hqdefault.jpg", 480, 360),
        ])
    }

    #[test]
    fn sorts_ascending_by_width() {
        let widths: Vec<_> = sample()
            .renditions()
            .iter()
            .filter_map(|t| t.width)
            .collect();
        assert_eq!(widths, vec![120, 320, 480, 1280]);
    }

    #[test]
    fn picks_the_smallest_sufficient_rendition() {
        let set = sample();
        assert_eq!(set.best_for_width(100).unwrap().width, Some(120));
        assert_eq!(set.best_for_width(120).unwrap().width, Some(120));
        assert_eq!(set.best_for_width(121).unwrap().width, Some(320));
        assert_eq!(set.best_for_width(480).unwrap().width, Some(480));
    }

    #[test]
    fn falls_back_to_largest_rather_than_upscaling() {
        let set = sample();
        assert_eq!(
            set.best_for_width(4000).unwrap().width,
            Some(1280),
            "no rendition is large enough, so the largest must be used"
        );
    }

    #[test]
    fn unsized_renditions_sort_last_but_remain_usable() {
        let set = ThumbnailSet::new(vec![
            Thumbnail::unsized_at("https://example.com/unknown.jpg"),
            Thumbnail::sized("https://example.com/small.jpg", 120, 90),
        ]);
        assert_eq!(set.renditions()[0].width, Some(120));
        assert_eq!(set.renditions()[1].width, None);
        // Selection prefers a known-size rendition for the "largest" role.
        assert_eq!(set.largest().unwrap().width, Some(120));
    }

    #[test]
    fn empty_set_selects_nothing_without_panicking() {
        let set = ThumbnailSet::empty();
        assert!(set.is_empty());
        assert!(set.best_for_width(320).is_none());
        assert!(set.largest().is_none());
        assert!(set.smallest().is_none());
    }

    #[test]
    fn only_unsized_renditions_still_yield_a_fallback() {
        let set = ThumbnailSet::new(vec![Thumbnail::unsized_at("https://example.com/a.jpg")]);
        assert!(set.best_for_width(320).is_some());
        assert!(set.largest().is_some());
    }

    #[test]
    fn aspect_ratio_guards_against_division_by_zero() {
        assert!(
            (Thumbnail::sized("u", 1280, 720).aspect_ratio().unwrap() - 16.0 / 9.0).abs() < 1e-6
        );
        assert!(Thumbnail::sized("u", 100, 0).aspect_ratio().is_none());
        assert!(Thumbnail::unsized_at("u").aspect_ratio().is_none());
    }

    #[test]
    fn serializes_transparently_as_an_array() {
        let json = serde_json::to_string(&sample()).unwrap();
        assert!(json.starts_with('['), "{json}");
        let back: ThumbnailSet = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 4);
    }
}
