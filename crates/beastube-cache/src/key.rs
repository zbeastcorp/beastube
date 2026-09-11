//! Namespaces and cache keys.
//!
//! ## Why keys are hashed rather than sanitized
//!
//! A cache key is whatever identifies the thing being cached — most often a provider URL. Turning
//! that into a filename by escaping it invites two failures that are hard to notice: two different
//! keys can sanitize to the same name (silently serving the wrong image), and a long URL can exceed
//! the 255-byte NTFS component limit (so the entry can never be written). Hashing with BLAKE3
//! removes both: every key maps to exactly 64 hex characters, collisions are cryptographically
//! improbable, and the resulting name contains nothing a filesystem cares about.
//!
//! The key itself is still stored *inside* the entry and compared on read, so even a collision
//! becomes a miss rather than a wrong answer.
//!
//! ## Why the path is sharded
//!
//! A long-lived install accumulates tens of thousands of thumbnails. A single directory with
//! 100 000 entries makes every `FindFirstFile`/`ReadDir` on Windows expensive, and makes the
//! directory itself slow to delete. The first two hex characters of the digest become a
//! subdirectory, spreading entries over 256 directories with an even distribution for free — the
//! digest is uniform, so no rebalancing is ever needed.
//!
//! ## Why namespaces are `&'static str`
//!
//! Namespaces are directory names, and they are always chosen by code, never by a provider or a
//! user. Binding them to `'static` makes [`Namespace`] `Copy` and free to embed in hot keys, and
//! makes it structurally impossible for untrusted input to become a directory name.

use std::fmt;
use std::sync::Arc;

use beastube_core::security::validate_file_component;

use crate::error::{CacheError, CacheResult};

/// Length of the hex-encoded digest that names an entry on disk.
pub const DIGEST_HEX_LEN: usize = 64;

/// Number of leading hex characters used as the shard directory name.
///
/// Two characters give 256 shards. Three would give 4096, which spreads better but costs a
/// directory-per-shard even on a small cache; two keeps a 100 000 entry cache at ~390 files per
/// directory, comfortably inside the range where directory enumeration stays cheap.
pub const SHARD_PREFIX_LEN: usize = 2;

/// Filename extension of a stored entry.
///
/// Distinctive on purpose: the disk layer indexes only files carrying it, so nothing the cache did
/// not write is ever considered — or deleted — by eviction.
pub const ENTRY_EXTENSION: &str = "bcx";

/// A cache partition, which is also a directory under the cache root.
///
/// Partitioning exists so that clearing thumbnails does not discard cached metadata, and so that
/// each partition can carry its own lifetime (thumbnails live for weeks, metadata for hours).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Namespace(&'static str);

impl Namespace {
    /// Maximum length of a namespace name.
    pub const MAX_LEN: usize = 32;

    /// Rendered thumbnail and avatar images.
    pub const THUMBNAILS: Self = Self("thumbnails");
    /// Serialized video, channel and playlist metadata.
    pub const METADATA: Self = Self("metadata");
    /// Serialized search result pages.
    pub const SEARCH: Self = Self("search");

    /// Every namespace this crate defines, used by maintenance and by the storage panel.
    pub const BUILT_IN: [Self; 3] = [Self::THUMBNAILS, Self::METADATA, Self::SEARCH];

    /// Validates a namespace name.
    ///
    /// The alphabet is deliberately narrower than the filesystem allows: lowercase ASCII letters,
    /// digits and `_` only. Windows filesystems are case-insensitive, so permitting uppercase would
    /// let `Metadata` and `metadata` name one directory while comparing as two namespaces — a bug
    /// that only appears on the platform this application ships on.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::InvalidNamespace`] if the name is empty, too long, outside the
    /// alphabet, or rejected as a path component by [`validate_file_component`].
    pub fn new(name: &'static str) -> CacheResult<Self> {
        if is_valid_namespace(name) {
            Ok(Self(name))
        } else {
            Err(CacheError::InvalidNamespace {
                name: name.to_owned(),
            })
        }
    }

    /// The namespace name, which is also its directory name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Whether `name` is usable as a namespace directory.
///
/// Shared with the disk scanner, which meets namespace names as runtime strings read from the
/// filesystem and must apply exactly the same rule the constructor does.
pub(crate) fn is_valid_namespace(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= Namespace::MAX_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && validate_file_component(name).is_ok()
}

/// A namespace plus the opaque key identifying one entry within it.
///
/// Cloning is cheap by design — a key is cloned into the single-flight map, into the memory cache
/// and into every waiter — so the namespace is `Copy` and the key body is reference counted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    namespace: Namespace,
    key: Arc<str>,
}

impl CacheKey {
    /// Maximum key length in bytes.
    ///
    /// Matches [`beastube_core::security::MAX_URL_LEN`] because the keys that reach this cache are
    /// overwhelmingly URLs, and a key longer than the longest URL we would ever fetch is a bug or
    /// an attempt to inflate the entry headers from outside.
    pub const MAX_LEN: usize = beastube_core::security::MAX_URL_LEN;

    /// Builds a key.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::KeyTooLong`] if `key` exceeds [`CacheKey::MAX_LEN`]. An empty key is
    /// accepted: it is a legitimate identifier for a namespace's single singleton entry, and it
    /// hashes like any other.
    pub fn new(namespace: Namespace, key: impl AsRef<str>) -> CacheResult<Self> {
        let key = key.as_ref();
        if key.len() > Self::MAX_LEN {
            return Err(CacheError::KeyTooLong {
                len: key.len(),
                max: Self::MAX_LEN,
            });
        }
        Ok(Self {
            namespace,
            key: Arc::from(key),
        })
    }

    /// The namespace this key lives in.
    #[must_use]
    pub const fn namespace(&self) -> Namespace {
        self.namespace
    }

    /// The opaque key body.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The BLAKE3 digest that names this key's entry on disk.
    ///
    /// The namespace is folded in, length-prefixed, so that `("ab", "c")` and `("a", "bc")` cannot
    /// produce the same digest. Without the length prefix a namespace boundary could be forged by a
    /// key that begins with another namespace's name.
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        let name = self.namespace.as_str();
        // `u32` rather than `usize` so the digest does not depend on the pointer width.
        let len = u32::try_from(name.len()).unwrap_or(u32::MAX);
        hasher.update(&len.to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update(self.key.as_bytes());
        *hasher.finalize().as_bytes()
    }

    /// The entry's path relative to the cache root, e.g. `thumbnails/a3/a3f1….bcx`.
    ///
    /// Built from the digest alone, so a hostile key (`../../etc/passwd`, `CON`, `a/b`) cannot
    /// influence it. The result is still passed through
    /// [`resolve_within`](beastube_core::security::resolve_within) by the disk layer rather than
    /// joined directly: the guarantee should hold because it is checked, not because this function
    /// is believed.
    #[must_use]
    pub fn relative_path(&self) -> String {
        let hex = hex_digest(&self.digest());
        let mut path = String::with_capacity(
            self.namespace.as_str().len()
                + SHARD_PREFIX_LEN
                + DIGEST_HEX_LEN
                + ENTRY_EXTENSION.len()
                + 3,
        );
        path.push_str(self.namespace.as_str());
        path.push('/');
        path.push_str(&hex[..SHARD_PREFIX_LEN]);
        path.push('/');
        path.push_str(&hex);
        path.push('.');
        path.push_str(ENTRY_EXTENSION);
        path
    }

    /// Approximate heap footprint of this key, used by the memory cache's weigher.
    #[must_use]
    pub fn weight(&self) -> usize {
        self.key.len() + size_of::<Self>()
    }
}

impl fmt::Display for CacheKey {
    /// Renders as `namespace:key`, for log lines and diagnostics only.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.namespace, self.key)
    }
}

/// Lowercase hex encoding of a digest.
///
/// Hand-rolled rather than pulled from a dependency: it is eight lines, it is on the path of every
/// cache lookup, and adding a crate for it would be the more surprising choice.
#[must_use]
pub(crate) fn hex_digest(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(DIGEST_HEX_LEN);
    for byte in digest {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

/// Whether `name` looks like a digest-named entry file this crate wrote.
///
/// Used by the scanner so that a foreign file dropped into the cache tree is indexed by nothing,
/// evicted by nothing and deleted by nothing.
// Consumed by `disk.rs` when sweeping the cache directory for orphans.
#[allow(dead_code)]
pub(crate) fn is_entry_file_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(ENTRY_EXTENSION) else {
        return false;
    };
    let Some(stem) = stem.strip_suffix('.') else {
        return false;
    };
    stem.len() == DIGEST_HEX_LEN
        && stem
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Whether `name` is a shard directory this crate created.
// Consumed by `disk.rs` when sweeping the cache directory for orphans.
#[allow(dead_code)]
pub(crate) fn is_shard_dir_name(name: &str) -> bool {
    name.len() == SHARD_PREFIX_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use beastube_core::security::resolve_within;

    use super::*;

    #[test]
    fn every_built_in_namespace_is_a_valid_directory_name() {
        // The constants bypass `new`, so this test is what keeps them honest.
        for namespace in Namespace::BUILT_IN {
            assert!(
                is_valid_namespace(namespace.as_str()),
                "{namespace} is not a usable directory name"
            );
            assert!(validate_file_component(namespace.as_str()).is_ok());
        }
    }

    #[test]
    fn namespaces_reject_names_that_collide_on_a_case_insensitive_filesystem() {
        assert!(Namespace::new("Metadata").is_err());
        assert!(Namespace::new("META").is_err());
        assert!(Namespace::new("metadata").is_ok());
    }

    #[test]
    fn namespaces_reject_traversal_and_reserved_names() {
        for hostile in ["..", ".", "a/b", "a\\b", "con", "nul", "aux", "com1"] {
            assert!(
                Namespace::new(hostile).is_err(),
                "{hostile} must be rejected as a namespace"
            );
        }
    }

    #[test]
    fn namespaces_reject_empty_and_overlong_names() {
        assert!(Namespace::new("").is_err());
        assert!(!is_valid_namespace(&"a".repeat(Namespace::MAX_LEN + 1)));
        assert!(is_valid_namespace(&"a".repeat(Namespace::MAX_LEN)));
    }

    #[test]
    fn a_hostile_key_cannot_escape_the_cache_root() {
        let root = Path::new("C:\\cache");
        for hostile in [
            "../../etc/passwd",
            "..\\..\\Windows\\System32\\config\\SAM",
            "CON",
            "NUL.jpg",
            "a/b",
            "..",
            ".",
            "",
            "C:\\absolute",
            "\\\\server\\share\\x",
            "trailing.   ",
            "\u{0}\u{1}",
            "🎬",
        ] {
            let key = CacheKey::new(Namespace::THUMBNAILS, hostile)
                .expect("any key under the length bound is accepted");
            let relative = key.relative_path();
            let resolved =
                resolve_within(root, &relative).expect("a digest path always resolves safely");
            assert!(
                resolved.starts_with(root),
                "{hostile:?} escaped to {resolved:?}"
            );
            assert!(
                !relative.contains(".."),
                "{hostile:?} produced {relative} which contains a traversal segment"
            );
        }
    }

    #[test]
    fn the_path_is_sharded_by_the_first_digest_bytes() {
        let key = CacheKey::new(Namespace::THUMBNAILS, "https://i.ytimg.com/vi/x/hq.jpg").unwrap();
        let relative = key.relative_path();
        let parts: Vec<_> = relative.split('/').collect();
        assert_eq!(parts.len(), 3, "namespace/shard/file, got {relative}");
        assert_eq!(parts[0], "thumbnails");
        assert_eq!(parts[1].len(), SHARD_PREFIX_LEN);
        assert!(
            parts[2].starts_with(parts[1]),
            "the shard must be the digest prefix so distribution is uniform"
        );
        assert!(is_entry_file_name(parts[2]), "{}", parts[2]);
        assert!(is_shard_dir_name(parts[1]));
    }

    #[test]
    fn the_same_key_always_produces_the_same_path() {
        let a = CacheKey::new(Namespace::METADATA, "video:dQw4w9WgXcQ").unwrap();
        let b = CacheKey::new(Namespace::METADATA, "video:dQw4w9WgXcQ").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.relative_path(), b.relative_path());
        assert_eq!(a.digest(), b.digest());
    }

    #[test]
    fn namespaces_partition_the_key_space() {
        let a = CacheKey::new(Namespace::METADATA, "same").unwrap();
        let b = CacheKey::new(Namespace::THUMBNAILS, "same").unwrap();
        assert_ne!(a, b);
        assert_ne!(a.digest(), b.digest());
        assert_ne!(a.relative_path(), b.relative_path());
    }

    #[test]
    fn the_namespace_boundary_cannot_be_forged_by_a_crafted_key() {
        // Without the length prefix, ("meta", "data:x") and ("metadata", ":x") would hash the same
        // bytes and share an entry across a partition boundary.
        let a = CacheKey::new(Namespace::new("meta").unwrap(), "data:x").unwrap();
        let b = CacheKey::new(Namespace::new("metadata").unwrap(), ":x").unwrap();
        assert_ne!(a.digest(), b.digest());
    }

    #[test]
    fn keys_are_bounded() {
        let at_limit = "a".repeat(CacheKey::MAX_LEN);
        assert!(CacheKey::new(Namespace::SEARCH, &at_limit).is_ok());
        assert!(matches!(
            CacheKey::new(Namespace::SEARCH, "a".repeat(CacheKey::MAX_LEN + 1)),
            Err(CacheError::KeyTooLong { .. })
        ));
    }

    #[test]
    fn an_empty_key_is_a_usable_singleton_identifier() {
        let key = CacheKey::new(Namespace::METADATA, "").unwrap();
        assert_eq!(key.key(), "");
        assert!(is_entry_file_name(
            key.relative_path().rsplit('/').next().unwrap()
        ));
    }

    #[test]
    fn hex_encoding_is_lowercase_and_fixed_width() {
        let hex = hex_digest(&[
            0x00, 0xff, 0x0a, 0xb3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0,
        ]);
        assert_eq!(hex.len(), DIGEST_HEX_LEN);
        assert!(hex.starts_with("00ff0ab3"));
        assert!(hex.bytes().all(|b| !b.is_ascii_uppercase()));
    }

    #[test]
    fn foreign_file_names_are_not_mistaken_for_entries() {
        assert!(!is_entry_file_name("readme.txt"));
        assert!(!is_entry_file_name(&format!("{}.tmp", "a".repeat(64))));
        assert!(!is_entry_file_name(&format!("{}.bcx", "a".repeat(63))));
        assert!(
            !is_entry_file_name(&format!("{}.bcx", "A".repeat(64))),
            "uppercase hex would let one entry occupy two names on a case-insensitive filesystem"
        );
        assert!(is_entry_file_name(&format!("{}.bcx", "0f".repeat(32))));
        assert!(!is_shard_dir_name("abc"));
        assert!(!is_shard_dir_name("zz"));
        assert!(is_shard_dir_name("0f"));
    }

    #[test]
    fn weight_grows_with_the_key_body() {
        let short = CacheKey::new(Namespace::SEARCH, "a").unwrap();
        let long = CacheKey::new(Namespace::SEARCH, "a".repeat(1000)).unwrap();
        assert!(long.weight() > short.weight() + 900);
    }

    #[test]
    fn display_is_stable_for_logs() {
        let key = CacheKey::new(Namespace::SEARCH, "query=rust").unwrap();
        assert_eq!(key.to_string(), "search:query=rust");
    }
}
