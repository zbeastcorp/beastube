//! Validated identifier newtypes.
//!
//! Identifiers arrive from three untrusted directions: the provider (parsed from remote JSON), the
//! frontend (IPC arguments) and the local database (which may have been written by an older or
//! corrupted build). They are then used as URL path segments, cache filenames and SQL parameters.
//!
//! Rather than validating at each of those call sites, validation happens once, at construction.
//! The charset is restricted to the RFC 4648 "URL and filename safe" alphabet, which makes every
//! downstream use safe without further escaping:
//!
//! * no `/`, `\`, `:` or `..`, so path traversal is impossible,
//! * no characters requiring percent-encoding, so URL building is lossless,
//! * no Windows-reserved characters (`<>:"/\|?*`), so cache files always open.
//!
//! Deserialization goes through the same validation, so an identifier read from IPC or from a
//! corrupted cache file is rejected instead of being trusted.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

/// Reasons an identifier failed validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// The identifier was empty.
    #[error("{kind} identifier is empty")]
    Empty {
        /// Which identifier type was being constructed.
        kind: &'static str,
    },
    /// The identifier exceeded the maximum length for its kind.
    #[error("{kind} identifier is {len} characters, maximum is {max}")]
    TooLong {
        /// Which identifier type was being constructed.
        kind: &'static str,
        /// Actual length in characters.
        len: usize,
        /// Maximum permitted length.
        max: usize,
    },
    /// The identifier contained a character outside the URL/filename-safe alphabet.
    #[error("{kind} identifier contains an invalid character at byte {index}")]
    InvalidCharacter {
        /// Which identifier type was being constructed.
        kind: &'static str,
        /// Byte offset of the first offending character.
        index: usize,
    },
}

/// Returns `true` for characters in the URL and filename safe alphabet (RFC 4648 §5).
#[must_use]
#[inline]
pub const fn is_safe_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

fn validate(kind: &'static str, raw: &str, max: usize) -> Result<(), IdError> {
    if raw.is_empty() {
        return Err(IdError::Empty { kind });
    }
    // Count characters rather than bytes: the alphabet is ASCII, so a multi-byte character is
    // always a validation failure, but reporting a character count is clearer in diagnostics.
    let len = raw.chars().count();
    if len > max {
        return Err(IdError::TooLong { kind, len, max });
    }
    if let Some((index, _)) = raw.char_indices().find(|&(_, c)| !is_safe_id_char(c)) {
        return Err(IdError::InvalidCharacter { kind, index });
    }
    Ok(())
}

macro_rules! define_id {
    (
        $(#[$meta:meta])*
        $name:ident, kind = $kind:literal, max = $max:literal
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Maximum permitted length in characters.
            pub const MAX_LEN: usize = $max;

            /// Human-readable name of this identifier kind, used in error messages.
            pub const KIND: &'static str = $kind;

            /// Validates `raw` and wraps it.
            ///
            /// # Errors
            ///
            /// Returns [`IdError`] if `raw` is empty, too long, or contains a character outside
            /// the URL and filename safe alphabet.
            pub fn new(raw: impl Into<String>) -> Result<Self, IdError> {
                let raw = raw.into();
                validate($kind, &raw, $max)?;
                Ok(Self(raw))
            }

            /// Borrows the identifier as a string slice.
            #[must_use]
            #[inline]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consumes the identifier, returning the inner string.
            #[must_use]
            #[inline]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            #[inline]
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        // Deserialization re-runs validation: identifiers reaching us over IPC or out of a
        // possibly-corrupted cache file are untrusted input, exactly like provider responses.
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let raw = String::deserialize(deserializer)?;
                Self::new(raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

define_id! {
    /// Provider-scoped identifier of a single video.
    ///
    /// YouTube video identifiers are 11 characters, but the bound is deliberately looser so that a
    /// future provider adapter does not require a change to the core contract.
    VideoId, kind = "video", max = 64
}

define_id! {
    /// Provider-scoped identifier of a channel.
    ///
    /// YouTube channel identifiers are 24 characters (`UC` + 22).
    ChannelId, kind = "channel", max = 64
}

define_id! {
    /// Provider-scoped identifier of a remote playlist.
    ///
    /// Local playlists created by the user are identified by
    /// [`LocalPlaylistId`](crate::model::LocalPlaylistId) instead, because they have no
    /// provider-side existence.
    PlaylistId, kind = "playlist", max = 128
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_typical_youtube_ids() {
        assert!(VideoId::new("dQw4w9WgXcQ").is_ok());
        assert!(ChannelId::new("UCuAXFkgsw1L7xaCfnd5JJOw").is_ok());
        assert!(PlaylistId::new("PLrAXtmRdnEQy6nuLMfO6uKk3-lI0ZC2Vs").is_ok());
        assert!(VideoId::new("_-aB9Zx0Y1w").is_ok());
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(VideoId::new(""), Err(IdError::Empty { kind: "video" }));
    }

    #[test]
    fn rejects_path_traversal() {
        for hostile in ["..", "../..", "a/../b", "a\\b", "C:", "a/b"] {
            assert!(
                VideoId::new(hostile).is_err(),
                "{hostile} should have been rejected"
            );
        }
    }

    #[test]
    fn rejects_windows_reserved_and_shell_characters() {
        for hostile in [
            "a<b", "a>b", "a:b", "a\"b", "a|b", "a?b", "a*b", "a b", "a\0b",
        ] {
            assert!(
                VideoId::new(hostile).is_err(),
                "{hostile:?} should have been rejected"
            );
        }
    }

    #[test]
    fn rejects_percent_and_query_injection() {
        for hostile in ["a%2Fb", "a&b=c", "a#frag", "a+b"] {
            assert!(
                VideoId::new(hostile).is_err(),
                "{hostile} should have been rejected"
            );
        }
    }

    #[test]
    fn rejects_overlong() {
        let long = "a".repeat(VideoId::MAX_LEN + 1);
        assert!(matches!(
            VideoId::new(long),
            Err(IdError::TooLong { max: 64, .. })
        ));
    }

    #[test]
    fn rejects_non_ascii() {
        // A multi-byte character must fail on the character check, not panic on byte indexing.
        assert!(VideoId::new("vidéo123456").is_err());
        assert!(VideoId::new("视频").is_err());
    }

    #[test]
    fn deserialization_validates() {
        let ok: Result<VideoId, _> = serde_json::from_str("\"dQw4w9WgXcQ\"");
        assert!(ok.is_ok());

        let traversal: Result<VideoId, _> = serde_json::from_str("\"../../secret\"");
        assert!(
            traversal.is_err(),
            "deserialization must not bypass validation"
        );
    }

    #[test]
    fn serializes_transparently() {
        let id = VideoId::new("dQw4w9WgXcQ").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"dQw4w9WgXcQ\"");
    }

    #[test]
    fn round_trips_through_string() {
        let id = VideoId::new("dQw4w9WgXcQ").unwrap();
        let reparsed: VideoId = id.to_string().parse().unwrap();
        assert_eq!(id, reparsed);
    }
}
