//! BEASTUBE content-filtering subsystem: rule engine, adapters, rollback and diagnostics.
//!
//! Scope is set by `docs/architecture-decisions/0001-playback-architecture.md`: this subsystem
//! filters what is legitimately filterable within the active playback architecture — creator-marked
//! segments offered for user-controlled skipping, content the user has chosen to hide, and
//! third-party tracking hosts. It does not suppress the provider's own advertising.
//!
//! The engine is generic over rule kinds; nothing here is hardcoded to one use.
//!
//! ## The safety rule
//!
//! Filtering must never break ordinary playback (§10). Three invariants enforce that, and each is
//! covered by tests:
//!
//! 1. An allow rule always beats a block rule, whatever the priorities say.
//! 2. An unmatched request is **allowed**. Unknown is never treated as hostile.
//! 3. Playback-critical hosts are never blocked, by any rule, in any mode.

// Lint policy is set workspace-wide in Cargo.toml. Pure logic with no FFI.
#![forbid(unsafe_code)]

pub mod adapter;
pub mod diagnostics;
pub mod engine;
pub mod error;
pub mod rule;
pub mod ruleset;
pub mod segment;

pub use adapter::{ContentFilterAdapter, FilteringProvider, RequestFilterAdapter};
pub use diagnostics::{FilteringDiagnostics, FilteringSnapshot, RuleCounts};
pub use engine::{Decision, EngineConfig, FilterEngine, NeverBlockList};
pub use error::{FilterError, FilterResult};
pub use rule::{Rule, RuleKind, RuleMode};
pub use ruleset::{RuleSet, RuleSetManager, RuleSetSource};
pub use segment::{Segment, SegmentAction, SegmentSkipper, SkipTarget};
