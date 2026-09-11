//! BEASTUBE provider abstraction.
//!
//! Everything the application knows about a content source goes through the traits here. No crate
//! above this one names YouTube, `rustypipe`, or any wire format — which is what makes the source
//! replaceable when the external service changes.
//!
//! ## Capabilities are reported, not assumed
//!
//! Real providers are partial and get more partial over time. As of this build, the YouTube adapter
//! can search, resolve video details and list channel content, but **cannot** read remote playlists
//! — upstream parsing broke against a response-schema change — and **cannot** produce video stream
//! URLs, because the provider now serves them only over a transport we do not implement.
//!
//! Rather than letting those surface as mysterious runtime errors, a provider declares
//! [`ProviderCapabilities`], and the UI renders only what is actually backed by an implementation
//!. A capability that turns off is a control that disappears, not a button that fails.
//!
//! ## Traits are split by concern
//!
//! [`SearchProvider`], [`VideoProvider`], [`ChannelProvider`] and [`PlaylistProvider`] are separate
//! so an adapter can implement what it supports and a composite can source each from a different
//! backend. [`MetadataProvider`] is the convenience supertrait the application actually holds.

// Lint policy is set workspace-wide in Cargo.toml. This crate is trait definitions and plain data;
// unsafe here would always be a mistake.
#![forbid(unsafe_code)]

pub mod capabilities;
pub mod error;
pub mod traits;

pub use capabilities::{PlaybackCapabilities, ProviderCapabilities};
pub use error::{ProviderError, ProviderResult, Unavailability};
pub use traits::{
    ChannelProvider, MetadataProvider, PlaylistProvider, SearchProvider, VideoProvider,
};
