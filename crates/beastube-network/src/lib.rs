//! BEASTUBE network manager: priorities, deduplication, retry, cancellation and concurrency.
//!
//! One [`reqwest::Client`] serves the whole application, so connections are pooled and TLS
//! handshakes are amortized across every request. Everything else here exists to keep that single
//! client from being abused: a per-host semaphore bounds concurrency, an in-flight map collapses
//! duplicate requests, and a retry policy distinguishes failures worth repeating from those that
//! will fail identically the second time.

// Lint policy is set workspace-wide in Cargo.toml. This crate is pure async Rust over reqwest,
// with no FFI, so an unsafe block would always be a mistake.
#![forbid(unsafe_code)]

pub mod error;
pub mod limits;
pub mod retry;

pub use error::{NetworkError, NetworkResult};
pub use limits::{DEFAULT_MAX_CONCURRENT_PER_HOST, HostLimiter, HostPermit};
pub use retry::{RetryDecision, RetryPolicy};
