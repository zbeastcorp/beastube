//! BEASTUBE multi-layer cache: memory (L1), disk (L2) and the thumbnail manager built on them.
//!
//! Caches are disposable by design (§71). Every entry is content-addressed and checksummed, so a
//! truncated or corrupted file is detected on read, deleted and refetched rather than surfacing as
//! an error the user has to act on. Nothing here holds data that cannot be rebuilt.

// Lint policy is set workspace-wide in Cargo.toml. Pure async Rust with no FFI.
#![forbid(unsafe_code)]

pub mod error;
pub mod key;
pub mod memory;
pub mod stats;

pub use error::{CacheError, CacheResult};
pub use key::{CacheKey, Namespace};
pub use memory::MemoryCache;
pub use stats::{CacheStats, CacheStatsSnapshot};
