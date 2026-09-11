//! BEASTUBE video downloads.
//!
//! The application does not extract media itself: obtaining a file from YouTube means keeping up
//! with a signature cipher, an `n`-parameter transform and a client allow-list that change every
//! few weeks, and the only software that keeps up is `yt-dlp`. So this crate is a *driver*. It
//! finds a `yt-dlp` executable (bundled beside the application, chosen by the user, or on `PATH`),
//! runs it as a child process with output templates it controls, and turns the lines the process
//! prints into typed [`DownloadProgress`] updates. ADR-0003 records the decision.
//!
//! ## What this crate guarantees
//!
//! * **The page cannot reach the command line.** Arguments are built from a validated
//!   [`beastube_core::ids::VideoId`] and paths the application resolved; no string from the
//!   frontend is passed through unchecked.
//! * **Progress is honest.** A percentage is reported only when the tool reports a total; a
//!   download whose size is unknown says so rather than inventing a number.
//! * **Failure is classified, not pasted.** The tool's stderr is read and mapped to the same error
//!   contract the rest of the application uses, so the UI can say "YouTube refused" or "the disk is
//!   full" and keep the raw text for the diagnostics screen.
//! * **Cancellation is real.** Cancelling kills the child process and removes the partial files it
//!   left behind, instead of merely forgetting about it.
//!
//! ## What it deliberately does not do
//!
//! It does not download `yt-dlp` itself, does not update it, and does not read cookies from any
//! browser. Whether a downloader exists on this computer is the user's decision; the settings
//! screen reports what was found rather than fetching one behind their back.

// Lint policy is set workspace-wide in Cargo.toml. This crate is process orchestration over safe
// std and tokio primitives; an unsafe block here would always be a mistake.
#![forbid(unsafe_code)]

pub mod command;
pub mod diagnose;
pub mod error;
pub mod locate;
pub mod manager;
pub mod progress;

pub use beastube_core::events::{DownloadProgress, DownloadStatus};
pub use command::DownloadPlan;
pub use diagnose::{Playability, playability};
pub use error::{DownloadError, DownloadResult, classify_failure};
pub use locate::{JsRuntime, JsRuntimeKind, LocateOptions, Tools, locate, simplified, version_of};
pub use manager::{DownloadManager, DownloadRequest, ProgressSink};
