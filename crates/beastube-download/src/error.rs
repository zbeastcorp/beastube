//! Download failures, classified into the shared error contract.
//!
//! `yt-dlp` reports failure as prose on stderr. That prose is the *diagnostic*; what the user sees
//! is a message key chosen here, so "Sign in to confirm you're not a bot" becomes a sentence the
//! localization layer owns and the raw line stays available on the diagnostics screen (§74).

use std::collections::BTreeMap;

use beastube_core::error::{DomainError, ErrorKind, Recovery};

/// Result alias for this crate.
pub type DownloadResult<T> = Result<T, DownloadError>;

/// Why a download did not produce a file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DownloadError {
    /// No downloader executable could be found: not beside the application, not at the configured
    /// path, not on `PATH`.
    #[error("no downloader executable (yt-dlp) was found")]
    ToolMissing,

    /// No `ffmpeg` could be found.
    ///
    /// Fatal rather than a quality limit. YouTube no longer offers a combined audio-and-video
    /// file to the clients the downloader reaches — measured against live responses on
    /// 2026-09-04, every format was video-only or audio-only — so without something to join them
    /// there is nothing to download.
    #[error("no muxer (ffmpeg) was found, and YouTube serves video and audio separately")]
    MuxerMissing,

    /// The executable exists but the operating system refused to start it.
    #[error("the downloader could not be started: {detail}")]
    Spawn {
        /// The OS error.
        detail: String,
    },

    /// The download directory could not be created or written.
    #[error("the download directory is unusable: {detail}")]
    Directory {
        /// The OS error.
        detail: String,
    },

    /// The disk filled up mid-download.
    #[error("there is not enough free disk space")]
    DiskFull,

    /// The provider answered but would not serve the video to this computer: a bot check, a rate
    /// limit, or a client version it no longer recognises. Usually temporary, or cured by updating
    /// the downloader.
    #[error("the provider refused the download: {detail}")]
    Refused {
        /// The tool's own explanation.
        detail: String,
    },

    /// The video cannot be downloaded by anyone: private, removed, restricted.
    #[error("the video is unavailable ({reason}): {detail}")]
    Unavailable {
        /// Stable reason code matching the provider taxonomy, e.g. `private`.
        reason: &'static str,
        /// The tool's own explanation.
        detail: String,
    },

    /// The tool exited unsuccessfully for a reason not recognised above.
    #[error("the downloader exited with status {exit_code:?}: {detail}")]
    Failed {
        /// Process exit code, when the process exited normally.
        exit_code: Option<i32>,
        /// The last lines the tool wrote to stderr.
        detail: String,
    },

    /// The download was cancelled by the user. A normal outcome, never shown as a fault.
    #[error("the download was cancelled")]
    Cancelled,
}

impl DomainError for DownloadError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::ToolMissing | Self::MuxerMissing | Self::Spawn { .. } => ErrorKind::Configuration,
            Self::Directory { .. } | Self::DiskFull => ErrorKind::FileSystem,
            Self::Refused { .. } | Self::Unavailable { .. } | Self::Failed { .. } => {
                ErrorKind::Provider
            }
            // The same code the IPC layer uses for an abandoned request, so the frontend's
            // existing "this was cancelled, say nothing" rule applies unchanged.
            Self::Cancelled => ErrorKind::Network,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::ToolMissing => "downloader_missing",
            Self::MuxerMissing => "muxer_missing",
            Self::Spawn { .. } => "downloader_failed_to_start",
            Self::Directory { .. } => "invalid_path",
            Self::DiskFull => "disk_full",
            Self::Refused { .. } => "download_refused",
            Self::Unavailable { reason, .. } => reason,
            Self::Failed { .. } => "download_failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn recovery(&self) -> Recovery {
        match self {
            Self::ToolMissing => Recovery::AdjustSettings {
                settings_path: "downloads.tool_path".to_owned(),
            },
            Self::MuxerMissing => Recovery::AdjustSettings {
                settings_path: "downloads.ffmpeg_path".to_owned(),
            },
            Self::Directory { .. } => Recovery::AdjustSettings {
                settings_path: "downloads.directory".to_owned(),
            },
            Self::Spawn { .. } | Self::DiskFull | Self::Refused { .. } | Self::Failed { .. } => {
                Recovery::RetryManual
            }
            Self::Unavailable { .. } | Self::Cancelled => Recovery::Unrecoverable,
        }
    }

    fn params(&self) -> BTreeMap<String, String> {
        let mut params = BTreeMap::new();
        if let Self::Failed {
            exit_code: Some(code),
            ..
        } = self
        {
            params.insert("exit_code".to_owned(), code.to_string());
        }
        params
    }
}

/// Classifies a failed run from its exit code and the tail of its stderr.
///
/// Matching is on substrings of `yt-dlp`'s own messages, lower-cased. They are stable enough to
/// key on — several have been unchanged for years — and a miss degrades to the generic
/// [`DownloadError::Failed`] with the text preserved, never to a wrong classification.
#[must_use]
pub fn classify_failure(exit_code: Option<i32>, stderr_tail: &str) -> DownloadError {
    let lower = stderr_tail.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));

    if has(&[
        "no space left",
        "not enough space",
        "disk full",
        "errno 28",
        "winerror 112",
    ]) {
        return DownloadError::DiskFull;
    }
    if has(&["permission denied", "errno 13", "access is denied", "winerror 5]"]) {
        return DownloadError::Directory {
            detail: stderr_tail.to_owned(),
        };
    }

    let unavailable = |reason: &'static str| DownloadError::Unavailable {
        reason,
        detail: stderr_tail.to_owned(),
    };
    if has(&["private video"]) {
        return unavailable("private");
    }
    if has(&["members-only", "members only", "join this channel"]) {
        return unavailable("members_only");
    }
    if has(&["not available in your country", "geo restricted", "geo-restricted"]) {
        return unavailable("geo_blocked");
    }
    if has(&["confirm your age", "age-restricted", "age restricted"]) {
        return unavailable("age_restricted");
    }
    if has(&["has been removed", "does not exist", "no longer available"]) {
        return unavailable("not_found");
    }
    if has(&["video unavailable"]) {
        return unavailable("unavailable");
    }

    // The provider is up and the video exists; it simply will not serve it to this client. The
    // format message is included because that is how a player response with no streams surfaces.
    if has(&[
        "sign in to confirm",
        "not a bot",
        "http error 429",
        "http error 403",
        "only images are available",
        "requested format is not available",
        "failed to extract any player response",
    ]) {
        return DownloadError::Refused {
            detail: stderr_tail.to_owned(),
        };
    }

    DownloadError::Failed {
        exit_code,
        detail: stderr_tail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bot_check_is_a_refusal_not_a_generic_failure() {
        let error = classify_failure(
            Some(1),
            "ERROR: [youtube] abc: Sign in to confirm you're not a bot.",
        );
        assert!(matches!(error, DownloadError::Refused { .. }));
        assert_eq!(error.full_code(), "provider.download_refused");
        assert_eq!(error.recovery(), Recovery::RetryManual);
    }

    #[test]
    fn unavailability_reuses_the_provider_taxonomy_codes() {
        let private = classify_failure(Some(1), "ERROR: [youtube] abc: Private video.");
        assert_eq!(private.full_code(), "provider.private");
        assert_eq!(private.recovery(), Recovery::Unrecoverable);

        let removed = classify_failure(
            Some(1),
            "ERROR: This video has been removed by the uploader",
        );
        assert_eq!(removed.full_code(), "provider.not_found");
    }

    #[test]
    fn a_full_disk_is_a_filesystem_failure() {
        let error = classify_failure(Some(1), "OSError: [Errno 28] No space left on device");
        assert_eq!(error, DownloadError::DiskFull);
        assert_eq!(error.kind(), ErrorKind::FileSystem);
        assert_eq!(error.message_key(), "error.filesystem.disk_full");
    }

    #[test]
    fn an_unrecognised_failure_keeps_the_text_and_the_exit_code() {
        let error = classify_failure(Some(7), "ERROR: something new");
        assert_eq!(
            error,
            DownloadError::Failed {
                exit_code: Some(7),
                detail: "ERROR: something new".to_owned(),
            }
        );
        assert_eq!(
            error.params().get("exit_code").map(String::as_str),
            Some("7")
        );
    }

    #[test]
    fn a_missing_tool_points_at_the_setting_that_fixes_it() {
        let error = DownloadError::ToolMissing;
        assert_eq!(error.full_code(), "configuration.downloader_missing");
        assert_eq!(
            error.recovery(),
            Recovery::AdjustSettings {
                settings_path: "downloads.tool_path".to_owned()
            }
        );
    }

    #[test]
    fn a_missing_muxer_points_at_its_own_setting_not_the_downloader_one() {
        let error = DownloadError::MuxerMissing;
        assert_eq!(error.full_code(), "configuration.muxer_missing");
        assert_eq!(
            error.recovery(),
            Recovery::AdjustSettings {
                settings_path: "downloads.ffmpeg_path".to_owned()
            }
        );
    }

    #[test]
    fn cancellation_shares_the_ipc_cancellation_code() {
        assert_eq!(DownloadError::Cancelled.full_code(), "network.cancelled");
    }
}
