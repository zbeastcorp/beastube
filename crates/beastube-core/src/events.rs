//! Typed events emitted from the native side to the UI.
//!
//! Events exist so the UI does not poll. Each one has a declared payload, a stable name, and
//! a documented producer, which is what keeps this from degenerating into an untyped bus where any
//! module emits anything.
//!
//! High-frequency playback position is deliberately **not** an event here. Position updates arrive
//! many times a second, and routing them through the global event channel into shared state would
//! re-render the application on every tick. Position stays local to the player component; the
//! native side learns about it only at checkpoints.

use serde::{Deserialize, Serialize};

use crate::error::ErrorPayload;
use crate::ids::VideoId;
use crate::playback_state::PlaybackState;
use crate::time_util::Timestamp;

/// Connectivity as observed by the native side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkStatus {
    /// Reachable.
    #[default]
    Online,
    /// No usable route. The UI switches to cached and local content rather than showing failures.
    Offline,
    /// Reachable but metered, so prefetch and high bitrates are curtailed.
    Metered,
}

impl NetworkStatus {
    /// Whether network requests should be attempted at all.
    #[must_use]
    pub const fn is_usable(self) -> bool {
        !matches!(self, Self::Offline)
    }

    /// Whether speculative work should be suppressed.
    #[must_use]
    pub const fn should_suppress_prefetch(self) -> bool {
        matches!(self, Self::Offline | Self::Metered)
    }
}

/// Why the content-filtering rule set changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterUpdateOutcome {
    /// A newer rule set was validated and activated.
    Applied,
    /// The check completed and the existing rule set is current.
    AlreadyCurrent,
    /// A candidate rule set failed validation and was discarded; the previous set remains active.
    RejectedInvalid,
    /// An activated rule set was rolled back after it correlated with playback failures.
    RolledBack,
}

/// Playback lifecycle transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackStateChanged {
    /// Session this transition belongs to. Late events from a superseded session are discarded by
    /// comparing this against the active session.
    pub session_id: String,
    /// The video being played.
    pub video_id: VideoId,
    /// State before the transition.
    pub previous: PlaybackState,
    /// State after the transition.
    pub current: PlaybackState,
    /// Playhead position at the transition, in milliseconds.
    pub position_ms: u64,
}

/// A playback failure, carrying the recovery decision already made by the producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackFailed {
    /// Session that failed.
    pub session_id: String,
    /// The video being played.
    pub video_id: VideoId,
    /// Position at failure, so recovery can resume rather than restart.
    pub position_ms: u64,
    /// The failure.
    pub error: ErrorPayload,
}

/// A search finished, succeeded or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchCompleted {
    /// The query this result answers, so a stale response can be discarded.
    pub query: String,
    /// Number of results returned.
    pub result_count: usize,
    /// Wall-clock duration of the search, for the performance panel.
    pub elapsed_ms: u64,
    /// Whether the results came from cache rather than the network.
    pub from_cache: bool,
}

/// Connectivity changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkChanged {
    /// Previous status.
    pub previous: NetworkStatus,
    /// Current status.
    pub current: NetworkStatus,
}

/// Cache contents or size changed materially.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheChanged {
    /// Bytes currently held on disk.
    pub disk_bytes: u64,
    /// Bytes currently held in memory.
    pub memory_bytes: u64,
    /// Entries evicted since the last event.
    pub evicted_entries: u64,
}

/// Filtering rule set changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterUpdated {
    /// What happened.
    pub outcome: FilterUpdateOutcome,
    /// Version of the rule set now active.
    pub active_version: String,
    /// Number of rules now active.
    pub rule_count: usize,
    /// Why a candidate was rejected or rolled back, as an i18n key. Absent on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_key: Option<String>,
}

/// An application update is available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateAvailable {
    /// Version offered.
    pub version: String,
    /// Release notes, when the endpoint supplies them. Rendered as plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Publication time, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<Timestamp>,
}

/// A long-running background task changed state, for the diagnostics screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaintenanceProgress {
    /// Stable task name, e.g. `cache.cleanup`, `database.vacuum`.
    pub task: String,
    /// Completion in `0.0..=1.0`, or `None` when the total is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f32>,
    /// Whether the task has finished.
    pub finished: bool,
}

/// Where a download is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    /// Accepted and waiting for a free slot.
    Queued,
    /// The downloader process is starting and has not reported a byte yet.
    Starting,
    /// Bytes are arriving.
    Downloading,
    /// Separate video and audio tracks are being joined into one file.
    Merging,
    /// The file is in its final place.
    Finished,
    /// No file was produced; [`DownloadProgress::error`] says why.
    Failed,
    /// Stopped by the user; partial files were removed.
    Cancelled,
}

impl DownloadStatus {
    /// Whether the download is over, one way or another.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Finished | Self::Failed | Self::Cancelled)
    }
}

/// The state of one download. Producer: the download manager, on every change.
///
/// The whole record is sent each time rather than a delta, so a listener that missed an event —
/// a screen mounted mid-download — is correct as soon as the next one arrives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadProgress {
    /// Identifier of this download, for cancelling and revealing it.
    pub id: String,
    /// The video being downloaded.
    pub video_id: VideoId,
    /// Its title, so a notification can name it without a lookup.
    pub title: String,
    /// Where it is.
    pub status: DownloadStatus,
    /// Bytes received for the current file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloaded_bytes: Option<u64>,
    /// Size of the current file, when the server said. Absent means unknown, not zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    /// Completion in `0.0..=1.0`, or `None` when the total is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f32>,
    /// Current transfer rate in bytes per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_bps: Option<u64>,
    /// Estimated seconds remaining for the current file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_seconds: Option<u64>,
    /// The finished file, once there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Why it failed, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorPayload>,
    /// When this record last changed.
    pub updated_at: Timestamp,
}

/// Every event the native side can emit.
///
/// Serialized with an external tag so the payload shape is unambiguous on the wire and a new
/// variant cannot be mistaken for an existing one by a build that predates it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", content = "payload", rename_all = "snake_case")]
pub enum AppEvent {
    /// Playback moved between states. Producer: the playback session in the app shell.
    PlaybackStateChanged(PlaybackStateChanged),
    /// Playback failed. Producer: the playback session.
    PlaybackFailed(PlaybackFailed),
    /// A search finished. Producer: the search orchestrator.
    SearchCompleted(SearchCompleted),
    /// Connectivity changed. Producer: the network monitor.
    NetworkChanged(NetworkChanged),
    /// Cache size or contents changed. Producer: the cache maintenance task.
    CacheChanged(CacheChanged),
    /// Filtering rules changed. Producer: the filtering rule updater.
    FilterUpdated(FilterUpdated),
    /// An update is available. Producer: the updater task.
    UpdateAvailable(UpdateAvailable),
    /// Maintenance progressed. Producer: the task scheduler.
    MaintenanceProgress(MaintenanceProgress),
    /// A download changed state. Producer: the download manager.
    DownloadProgress(DownloadProgress),
}

impl AppEvent {
    /// The channel name this event is emitted on.
    ///
    /// Kept in sync with the `AppEventName` union on the TypeScript side; the round-trip test below
    /// fails if a variant is added without a name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::PlaybackStateChanged(_) => "playback:state-changed",
            Self::PlaybackFailed(_) => "playback:failed",
            Self::SearchCompleted(_) => "search:completed",
            Self::NetworkChanged(_) => "network:changed",
            Self::CacheChanged(_) => "cache:changed",
            Self::FilterUpdated(_) => "filter:updated",
            Self::UpdateAvailable(_) => "update:available",
            Self::MaintenanceProgress(_) => "maintenance:progress",
            Self::DownloadProgress(_) => "download:progress",
        }
    }

    /// Every event channel name, for the frontend's listener registry and for tests.
    pub const ALL_NAMES: [&'static str; 9] = [
        "playback:state-changed",
        "playback:failed",
        "search:completed",
        "network:changed",
        "cache:changed",
        "filter:updated",
        "update:available",
        "maintenance:progress",
        "download:progress",
    ];

    /// Whether this event may be emitted while the user is in incognito mode.
    ///
    /// Incognito suppresses events that would cause persistent state to be written or that reveal
    /// viewing activity to surfaces which outlive the session. Infrastructure events are
    /// unaffected because they carry no viewing information.
    #[must_use]
    pub const fn allowed_in_incognito(&self) -> bool {
        !matches!(
            self,
            Self::PlaybackStateChanged(_) | Self::PlaybackFailed(_) | Self::SearchCompleted(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn sample_state_change() -> AppEvent {
        AppEvent::PlaybackStateChanged(PlaybackStateChanged {
            session_id: "s1".to_owned(),
            video_id: VideoId::new("dQw4w9WgXcQ").unwrap(),
            previous: PlaybackState::Loading,
            current: PlaybackState::Ready,
            position_ms: 0,
        })
    }

    #[test]
    fn every_variant_has_a_unique_name() {
        let mut names = AppEvent::ALL_NAMES.to_vec();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "event names must be unique");
    }

    #[test]
    fn variant_names_are_all_registered() {
        let events = [
            sample_state_change(),
            AppEvent::PlaybackFailed(PlaybackFailed {
                session_id: "s1".to_owned(),
                video_id: VideoId::new("dQw4w9WgXcQ").unwrap(),
                position_ms: 1000,
                error: ErrorPayload {
                    kind: crate::error::ErrorKind::Playback,
                    code: "playback.decode".to_owned(),
                    message_key: "error.playback.decode".to_owned(),
                    params: BTreeMap::new(),
                    recovery: crate::error::Recovery::RetryManual,
                    diagnostic: None,
                    correlation_id: None,
                },
            }),
            AppEvent::SearchCompleted(SearchCompleted {
                query: "q".to_owned(),
                result_count: 0,
                elapsed_ms: 1,
                from_cache: false,
            }),
            AppEvent::NetworkChanged(NetworkChanged {
                previous: NetworkStatus::Online,
                current: NetworkStatus::Offline,
            }),
            AppEvent::CacheChanged(CacheChanged {
                disk_bytes: 0,
                memory_bytes: 0,
                evicted_entries: 0,
            }),
            AppEvent::FilterUpdated(FilterUpdated {
                outcome: FilterUpdateOutcome::Applied,
                active_version: "1".to_owned(),
                rule_count: 0,
                reason_key: None,
            }),
            AppEvent::UpdateAvailable(UpdateAvailable {
                version: "0.2.0".to_owned(),
                notes: None,
                published_at: None,
            }),
            AppEvent::MaintenanceProgress(MaintenanceProgress {
                task: "cache.cleanup".to_owned(),
                fraction: None,
                finished: true,
            }),
            AppEvent::DownloadProgress(DownloadProgress {
                id: "d1".to_owned(),
                video_id: VideoId::new("dQw4w9WgXcQ").unwrap(),
                title: "Clip".to_owned(),
                status: DownloadStatus::Downloading,
                downloaded_bytes: Some(1),
                total_bytes: Some(2),
                fraction: Some(0.5),
                speed_bps: None,
                eta_seconds: None,
                path: None,
                error: None,
                updated_at: Timestamp::from_millis(0),
            }),
        ];

        assert_eq!(
            events.len(),
            AppEvent::ALL_NAMES.len(),
            "ALL_NAMES must list every variant"
        );
        for event in &events {
            assert!(
                AppEvent::ALL_NAMES.contains(&event.name()),
                "{} is not in ALL_NAMES",
                event.name()
            );
        }
    }

    #[test]
    fn events_are_externally_tagged_on_the_wire() {
        let json = serde_json::to_string(&sample_state_change()).unwrap();
        assert!(
            json.contains(r#""event":"playback_state_changed""#),
            "{json}"
        );
        assert!(json.contains(r#""payload""#), "{json}");
        let back: AppEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, sample_state_change());
    }

    #[test]
    fn incognito_suppresses_viewing_activity_but_not_infrastructure() {
        assert!(!sample_state_change().allowed_in_incognito());
        assert!(
            AppEvent::NetworkChanged(NetworkChanged {
                previous: NetworkStatus::Online,
                current: NetworkStatus::Offline,
            })
            .allowed_in_incognito()
        );
        assert!(
            AppEvent::CacheChanged(CacheChanged {
                disk_bytes: 0,
                memory_bytes: 0,
                evicted_entries: 0,
            })
            .allowed_in_incognito()
        );
    }

    #[test]
    fn offline_suppresses_requests_and_prefetch() {
        assert!(!NetworkStatus::Offline.is_usable());
        assert!(NetworkStatus::Offline.should_suppress_prefetch());
        assert!(NetworkStatus::Metered.is_usable());
        assert!(
            NetworkStatus::Metered.should_suppress_prefetch(),
            "a metered connection is usable but speculative work is not free"
        );
        assert!(!NetworkStatus::Online.should_suppress_prefetch());
    }
}
