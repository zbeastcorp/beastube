//! BEASTUBE local SQLite storage: connection policy, migrations and repositories.
//!
//! Everything the user creates — history, playlists, bookmarks, settings — lives here and nowhere
//! else. There is no server and no synchronization (§13), so this crate is the whole persistence
//! story.
//!
//! Two rules shape the design:
//!
//! 1. **User data and cache data are separated by table, not by convention.** Cache tables carry
//!    `expires_at` and may be truncated wholesale; library tables never are. "Clear cache" and
//!    "reset application data" are therefore different operations that cannot be confused (§101).
//! 2. **Library rows denormalize what they display.** History and playlists keep their own copy of
//!    the title, channel and thumbnails, so they render offline and after a cache clear (§72).

// Lint policy is set workspace-wide in Cargo.toml. Storage is pure SQL and serde: no FFI, so
// unsafe here would always be a mistake.
#![forbid(unsafe_code)]

pub mod connection;
pub mod error;
pub mod repo;

pub use connection::{Database, MIGRATOR};
pub use error::{DbError, DbResult};
pub use repo::Repositories;
