//! BEASTUBE in-memory cache.
//!
//! Caches are disposable: nothing here holds data that cannot be rebuilt from its source, so an
//! entry that fails verification is dropped and refetched rather than surfaced as an error.
//!
//! A disk layer is planned and not present. [`stats::CacheStats`] keeps disk counters because
//! they are part of the shape a second layer will report through; nothing increments them yet.

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
