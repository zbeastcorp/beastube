//! BEASTUBE core domain types, identifiers, errors, settings and event definitions.
//!
//! This crate is the shared contract between every other crate in the workspace. It deliberately
//! has **no** I/O, no database, no network and no provider-specific knowledge, so that:
//!
//! * the provider layer can be swapped without touching the UI contract,
//! * the storage layer can be swapped without touching the domain model,
//! * every crate agrees on one set of identifiers, errors and events.
//!
//! ## Design rules enforced here
//!
//! 1. **Identifiers are validated newtypes.** A [`VideoId`] cannot hold `../../etc/passwd`, so
//!    every downstream use as a cache filename or URL path segment is safe by construction.
//! 2. **Errors carry machine-readable recovery information.** The frontend never parses English
//!    prose; it receives a stable code plus an i18n message key (see [`error`]).
//! 3. **No user-facing English lives in Rust.** Rust emits message *keys*; the i18n layer in the
//!    UI renders them.

// Lint policy is set workspace-wide in Cargo.toml. This crate additionally forbids unsafe: it is
// pure domain logic with no FFI, so an unsafe block here would always indicate a mistake.
#![forbid(unsafe_code)]

pub mod error;
pub mod events;
pub mod ids;
pub mod model;
pub mod playback_state;
pub mod priority;
pub mod security;
pub mod settings;
pub mod time_util;

pub use error::{ErrorKind, ErrorPayload, Recovery};
pub use ids::{ChannelId, IdError, PlaylistId, VideoId};
pub use playback_state::{InvalidTransition, PlaybackSignal, PlaybackState};
pub use priority::Priority;
pub use settings::Settings;

/// Schema version of the settings document and of the IPC contract.
///
/// Bump this whenever a breaking change is made to [`Settings`] so that the migration path in
/// `beastube-db` can upgrade persisted documents deterministically.
pub const CONTRACT_VERSION: u32 = 1;
