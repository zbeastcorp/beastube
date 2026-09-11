//! The playback state machine.
//!
//! The media element and the adaptive-streaming engine emit a stream of low-level events
//! (`waiting`, `playing`, `seeking`, `seeked`, `stalled`, `ended`, `error`) whose ordering is not
//! guaranteed and which fire redundantly. Translating them into a small set of explicit states with
//! declared transitions is what keeps impossible combinations from arising — the alternative,
//! several independent booleans (`isPlaying`, `isBuffering`, `isSeeking`), permits states like
//! "playing and ended" that then have to be defended against at every read site.
//!
//! This type lives in `core` rather than in the playback crate because both sides of the IPC
//! boundary need it: the frontend derives it from media events, and the native side consumes it to
//! drive the tray, the mini-player and the OS media controls. One definition means they cannot
//! disagree about what "buffering" means.

use serde::{Deserialize, Serialize};

/// A state in the playback lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackState {
    /// Nothing is loaded. The initial and post-teardown state.
    #[default]
    Idle,
    /// A video is being resolved and the engine is being prepared. No frame yet.
    Loading,
    /// Media is loaded and the first frame is decodable, but playback has not begun.
    Ready,
    /// Advancing.
    Playing,
    /// Deliberately halted by the user or by the application.
    Paused,
    /// Halted involuntarily because the buffer ran dry. Distinct from [`PlaybackState::Paused`]
    /// because the UI must show progress rather than a play affordance, and because rebuffer counts
    /// are a performance metric.
    Buffering,
    /// A seek is in flight. Position reporting is suppressed so the scrub bar does not jump back to
    /// the old position between the request and its completion.
    Seeking,
    /// Reached the end of the media.
    Ended,
    /// Halted by a failure. Carries no payload here; the failure itself travels as an
    /// [`crate::error::ErrorPayload`].
    Error,
}

impl PlaybackState {
    /// Stable identifier for logs, metrics and the diagnostics screen.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Playing => "playing",
            Self::Paused => "paused",
            Self::Buffering => "buffering",
            Self::Seeking => "seeking",
            Self::Ended => "ended",
            Self::Error => "error",
        }
    }

    /// Whether media is loaded, so position, duration and track selection are meaningful.
    #[must_use]
    pub const fn has_media(self) -> bool {
        matches!(
            self,
            Self::Ready
                | Self::Playing
                | Self::Paused
                | Self::Buffering
                | Self::Seeking
                | Self::Ended
        )
    }

    /// Whether the playhead is advancing, or is trying to.
    ///
    /// True while buffering and seeking as well as while playing, because in all three the user's
    /// intent is "play" — this is what the OS media controls and the tray should reflect, so that a
    /// momentary stall does not flicker the play/pause icon.
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Playing | Self::Buffering | Self::Seeking)
    }

    /// Whether the UI should show a busy indicator.
    #[must_use]
    pub const fn is_busy(self) -> bool {
        matches!(self, Self::Loading | Self::Buffering | Self::Seeking)
    }

    /// Whether position checkpoints should be written in this state.
    ///
    /// Suppressed while seeking (the position is in flux and would checkpoint a stale value) and
    /// while erroring or idle (nothing meaningful to record).
    #[must_use]
    pub const fn should_checkpoint_position(self) -> bool {
        matches!(
            self,
            Self::Playing | Self::Paused | Self::Buffering | Self::Ended
        )
    }

    /// Whether `next` is a legal successor of `self`.
    ///
    /// Transitions to [`PlaybackState::Error`] and [`PlaybackState::Idle`] are legal from anywhere:
    /// a failure or a teardown can occur at any moment, and refusing them would strand the machine.
    ///
    /// Staying in the same state is always legal. It is not really a transition, and it happens
    /// routinely: requesting a load while one is already in flight keeps the machine in
    /// [`PlaybackState::Loading`], and a redundant media event resolves to the current state.
    // Two arms currently share a successor set by coincidence rather than by meaning; merging
    // them would couple rules that are expected to diverge.
    #[allow(clippy::match_same_arms)]
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        // Fieldless enums cast to their discriminant, which `derive(PartialEq)` cannot do in a
        // const context.
        if self as u8 == next as u8 {
            return true;
        }
        // A failure, a teardown, or a fresh load can arrive at any moment. The first two would
        // otherwise strand the machine; the third is the ordinary case of the user opening another
        // video from the watch page, whatever the current player is doing.
        if matches!(next, Self::Error | Self::Idle | Self::Loading) {
            return true;
        }
        match self {
            Self::Idle => matches!(next, Self::Loading),
            // Loading resolves to Ready; it cannot jump straight to Playing, because "playing"
            // without a decodable first frame is exactly the lie this machine exists to prevent.
            Self::Loading => matches!(next, Self::Ready),
            // Ended is reachable from Ready for zero-length or already-exhausted media, where the
            // engine reports end-of-stream before playback ever starts.
            Self::Ready => matches!(
                next,
                Self::Playing | Self::Paused | Self::Seeking | Self::Ended
            ),
            // Loading is covered by the universal rule above, so these arms describe only the
            // successors reachable without starting a new load.
            Self::Playing => matches!(
                next,
                Self::Paused | Self::Buffering | Self::Seeking | Self::Ended
            ),
            // A paused player does not enter Buffering: it prebuffers without a user-visible stall.
            // It can still reach Ended, by being paused at the tail or seeking to the end.
            Self::Paused => matches!(next, Self::Playing | Self::Seeking | Self::Ended),
            Self::Buffering => matches!(
                next,
                Self::Playing | Self::Paused | Self::Seeking | Self::Ended
            ),
            Self::Seeking => matches!(
                next,
                Self::Playing | Self::Paused | Self::Buffering | Self::Ended | Self::Ready
            ),
            // Ended is not terminal: replaying, scrubbing back and loading the next item all
            // proceed from it.
            Self::Ended => matches!(next, Self::Playing | Self::Seeking | Self::Loading),
            // Recovery from Error goes through a fresh load, never straight back to Playing.
            Self::Error => matches!(next, Self::Loading),
        }
    }
}

/// A low-level occurrence that may drive a state change.
///
/// Named for the *cause* rather than the resulting state, so that the mapping from media events to
/// states lives in one table instead of being decided at each call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackSignal {
    /// A load was requested.
    LoadRequested,
    /// Media is loaded and decodable.
    LoadSucceeded,
    /// Playback was requested or resumed.
    PlayRequested,
    /// A pause was requested.
    PauseRequested,
    /// The buffer ran dry while playing.
    BufferUnderrun,
    /// The buffer refilled enough to resume.
    BufferSatisfied,
    /// A seek was requested.
    SeekRequested,
    /// The seek completed and decoding resumed.
    SeekCompleted,
    /// The end of the media was reached.
    EndOfStream,
    /// A failure occurred.
    Failed,
    /// The session was torn down.
    Reset,
}

/// A rejected transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("playback signal {signal:?} is not valid in state {state:?}")]
pub struct InvalidTransition {
    /// The state the machine was in.
    pub state: PlaybackState,
    /// The signal that was rejected.
    pub signal: PlaybackSignal,
}

impl PlaybackState {
    /// Applies `signal`, returning the next state.
    ///
    /// Redundant signals are absorbed rather than rejected: media elements routinely fire `playing`
    /// twice, and treating the second as an error would produce spurious diagnostics. Genuinely
    /// contradictory signals — resuming from `Idle`, seeking with nothing loaded — are rejected, so
    /// a real ordering bug is visible instead of silently producing an impossible state.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidTransition`] when `signal` has no meaning in the current state.
    pub fn apply(self, signal: PlaybackSignal) -> Result<Self, InvalidTransition> {
        use PlaybackSignal as S;
        use PlaybackState as P;

        let reject = || {
            Err(InvalidTransition {
                state: self,
                signal,
            })
        };

        match signal {
            // A failure or teardown can arrive at any time.
            S::Failed => Ok(P::Error),
            S::Reset => Ok(P::Idle),
            S::LoadRequested => Ok(P::Loading),
            S::LoadSucceeded => match self {
                P::Loading => Ok(P::Ready),
                // Absorbed: a second `canplay` after we are already running changes nothing.
                P::Ready | P::Playing | P::Paused | P::Buffering | P::Seeking => Ok(self),
                _ => reject(),
            },
            S::PlayRequested => match self {
                P::Ready | P::Paused | P::Ended | P::Playing => Ok(P::Playing),
                // Play during a stall or a seek expresses intent; the state is owned by the
                // in-flight operation and resolves on its own.
                P::Buffering | P::Seeking => Ok(self),
                P::Idle | P::Loading | P::Error => reject(),
            },
            S::PauseRequested => match self {
                P::Playing | P::Buffering | P::Ready | P::Paused => Ok(P::Paused),
                P::Seeking => Ok(P::Seeking),
                P::Idle | P::Loading | P::Ended | P::Error => reject(),
            },
            S::BufferUnderrun => match self {
                P::Playing | P::Buffering => Ok(P::Buffering),
                // A stall reported while paused or seeking is not a user-visible rebuffer.
                P::Paused | P::Seeking | P::Ready => Ok(self),
                P::Idle | P::Loading | P::Ended | P::Error => reject(),
            },
            S::BufferSatisfied => match self {
                P::Buffering => Ok(P::Playing),
                P::Playing | P::Paused | P::Seeking | P::Ready => Ok(self),
                P::Idle | P::Loading | P::Ended | P::Error => reject(),
            },
            S::SeekRequested => match self {
                P::Ready | P::Playing | P::Paused | P::Buffering | P::Ended | P::Seeking => {
                    Ok(P::Seeking)
                }
                P::Idle | P::Loading | P::Error => reject(),
            },
            S::SeekCompleted => match self {
                // The engine reports where it landed; resuming to Playing is the common case and
                // a subsequent pause signal corrects it if the user had paused mid-seek.
                P::Seeking => Ok(P::Playing),
                P::Playing | P::Paused | P::Buffering | P::Ready => Ok(self),
                P::Idle | P::Loading | P::Ended | P::Error => reject(),
            },
            S::EndOfStream => match self {
                P::Playing | P::Buffering | P::Seeking | P::Paused | P::Ready | P::Ended => {
                    Ok(P::Ended)
                }
                P::Idle | P::Loading | P::Error => reject(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PlaybackSignal as S;
    use super::PlaybackState as P;

    const ALL_STATES: [P; 9] = [
        P::Idle,
        P::Loading,
        P::Ready,
        P::Playing,
        P::Paused,
        P::Buffering,
        P::Seeking,
        P::Ended,
        P::Error,
    ];

    const ALL_SIGNALS: [S; 11] = [
        S::LoadRequested,
        S::LoadSucceeded,
        S::PlayRequested,
        S::PauseRequested,
        S::BufferUnderrun,
        S::BufferSatisfied,
        S::SeekRequested,
        S::SeekCompleted,
        S::EndOfStream,
        S::Failed,
        S::Reset,
    ];

    #[test]
    fn the_happy_path_runs_end_to_end() {
        let state = P::Idle
            .apply(S::LoadRequested)
            .and_then(|s| s.apply(S::LoadSucceeded))
            .and_then(|s| s.apply(S::PlayRequested))
            .unwrap();
        assert_eq!(state, P::Playing);

        let state = state.apply(S::EndOfStream).unwrap();
        assert_eq!(state, P::Ended);
    }

    #[test]
    fn loading_cannot_skip_straight_to_playing() {
        assert!(P::Loading.apply(S::PlayRequested).is_err());
        assert!(
            !P::Loading.can_transition_to(P::Playing),
            "claiming playback before a decodable frame is exactly the lie to prevent"
        );
    }

    #[test]
    fn a_stall_is_distinguishable_from_a_user_pause() {
        let buffering = P::Playing.apply(S::BufferUnderrun).unwrap();
        assert_eq!(buffering, P::Buffering);
        assert!(buffering.is_active(), "intent is still to play");
        assert!(buffering.is_busy());

        let paused = P::Playing.apply(S::PauseRequested).unwrap();
        assert_eq!(paused, P::Paused);
        assert!(!paused.is_active());
        assert!(!paused.is_busy());
    }

    #[test]
    fn a_stall_while_paused_is_not_a_rebuffer() {
        assert_eq!(P::Paused.apply(S::BufferUnderrun).unwrap(), P::Paused);
    }

    #[test]
    fn redundant_signals_are_absorbed_rather_than_rejected() {
        // Media elements fire these repeatedly; a second one must not raise a diagnostic.
        assert_eq!(P::Playing.apply(S::PlayRequested).unwrap(), P::Playing);
        assert_eq!(P::Paused.apply(S::PauseRequested).unwrap(), P::Paused);
        assert_eq!(P::Playing.apply(S::LoadSucceeded).unwrap(), P::Playing);
        assert_eq!(P::Buffering.apply(S::BufferUnderrun).unwrap(), P::Buffering);
        assert_eq!(P::Ended.apply(S::EndOfStream).unwrap(), P::Ended);
    }

    #[test]
    fn contradictory_signals_are_rejected_so_ordering_bugs_stay_visible() {
        assert!(P::Idle.apply(S::PlayRequested).is_err());
        assert!(P::Idle.apply(S::SeekRequested).is_err());
        assert!(P::Idle.apply(S::EndOfStream).is_err());
        assert!(P::Error.apply(S::PlayRequested).is_err());
        assert!(P::Loading.apply(S::SeekCompleted).is_err());
    }

    #[test]
    fn failure_and_teardown_are_accepted_from_every_state() {
        for state in ALL_STATES {
            assert_eq!(state.apply(S::Failed).unwrap(), P::Error, "from {state:?}");
            assert_eq!(state.apply(S::Reset).unwrap(), P::Idle, "from {state:?}");
            assert!(state.can_transition_to(P::Error));
            assert!(state.can_transition_to(P::Idle));
        }
    }

    #[test]
    fn a_new_load_can_start_from_any_state() {
        // Clicking a different video must work whatever the player is doing.
        for state in ALL_STATES {
            assert!(
                state.can_transition_to(P::Loading),
                "{state:?} must permit starting a new load"
            );
            assert_eq!(
                state.apply(S::LoadRequested).unwrap(),
                P::Loading,
                "from {state:?}"
            );
        }
    }

    #[test]
    fn staying_in_the_same_state_is_always_legal() {
        for state in ALL_STATES {
            assert!(
                state.can_transition_to(state),
                "{state:?} must permit remaining in {state:?}"
            );
        }
        // The case that motivated it: opening a second video while the first is still resolving.
        assert_eq!(P::Loading.apply(S::LoadRequested).unwrap(), P::Loading);
    }

    #[test]
    fn recovery_from_error_goes_through_a_fresh_load() {
        assert!(P::Error.can_transition_to(P::Loading));
        assert!(
            !P::Error.can_transition_to(P::Playing),
            "resuming a failed session without reloading would replay the failure"
        );
        assert_eq!(P::Error.apply(S::LoadRequested).unwrap(), P::Loading);
    }

    #[test]
    fn end_of_stream_is_reachable_from_every_media_bearing_state() {
        for state in [P::Ready, P::Playing, P::Paused, P::Buffering, P::Seeking] {
            assert_eq!(
                state.apply(S::EndOfStream).unwrap(),
                P::Ended,
                "from {state:?}"
            );
            assert!(state.can_transition_to(P::Ended), "from {state:?}");
        }
        // Nothing is loaded, so there is no stream to end.
        assert!(P::Idle.apply(S::EndOfStream).is_err());
        assert!(P::Loading.apply(S::EndOfStream).is_err());
        assert!(P::Error.apply(S::EndOfStream).is_err());
    }

    #[test]
    fn ended_is_not_terminal() {
        assert_eq!(P::Ended.apply(S::PlayRequested).unwrap(), P::Playing);
        assert_eq!(P::Ended.apply(S::SeekRequested).unwrap(), P::Seeking);
        assert_eq!(P::Ended.apply(S::LoadRequested).unwrap(), P::Loading);
    }

    #[test]
    fn every_reachable_transition_is_permitted_by_the_guard() {
        // The signal table and the transition guard are two descriptions of the same machine;
        // this is what keeps them from drifting apart.
        for state in ALL_STATES {
            for signal in ALL_SIGNALS {
                if let Ok(next) = state.apply(signal) {
                    assert!(
                        state.can_transition_to(next),
                        "apply({state:?}, {signal:?}) = {next:?}, which can_transition_to rejects"
                    );
                }
            }
        }
    }

    #[test]
    fn checkpoints_are_suppressed_while_seeking_and_idle() {
        assert!(!P::Seeking.should_checkpoint_position());
        assert!(!P::Idle.should_checkpoint_position());
        assert!(!P::Loading.should_checkpoint_position());
        assert!(!P::Error.should_checkpoint_position());
        assert!(P::Playing.should_checkpoint_position());
        assert!(P::Paused.should_checkpoint_position());
        assert!(P::Ended.should_checkpoint_position());
    }

    #[test]
    fn media_bearing_states_are_exactly_the_loaded_ones() {
        assert!(!P::Idle.has_media());
        assert!(!P::Loading.has_media());
        assert!(!P::Error.has_media());
        for state in [
            P::Ready,
            P::Playing,
            P::Paused,
            P::Buffering,
            P::Seeking,
            P::Ended,
        ] {
            assert!(state.has_media(), "{state:?}");
        }
    }

    #[test]
    fn seeking_does_not_flicker_the_play_indicator() {
        let seeking = P::Playing.apply(S::SeekRequested).unwrap();
        assert_eq!(seeking, P::Seeking);
        assert!(
            seeking.is_active(),
            "OS media controls must keep showing pause while a seek is in flight"
        );
    }

    #[test]
    fn state_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&P::Buffering).unwrap(),
            "\"buffering\""
        );
        let parsed: P = serde_json::from_str("\"seeking\"").unwrap();
        assert_eq!(parsed, P::Seeking);
        assert_eq!(P::default(), P::Idle);
    }
}
