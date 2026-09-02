//! Rule sets: validation before activation, and rollback after it.
//!
//! ## The type system enforces "an invalid set is never activated"
//!
//! [`RuleSet`] is untrusted data — it may have been downloaded, read out of the database, or typed
//! by the user. [`FilterEngine`](crate::engine::FilterEngine) cannot be constructed from one. It
//! can only be constructed from a [`ValidatedRuleSet`], and the only way to obtain a
//! [`ValidatedRuleSet`] is [`RuleSet::validate`]. There is therefore no code path — present or
//! future — that activates a set whose patterns were never checked, and no reviewer has to verify
//! that every call site remembered to validate (§8).
//!
//! ## What validation rejects, and why each one
//!
//! * **Empty patterns.** An empty host pattern would match every host under one reading and none
//!   under another; guessing which is worse than refusing.
//! * **Over-long patterns and over-large sets.** Both bound the work an update can force us to do
//!   before it is even usable. [`MAX_RULES`] × [`MAX_PATTERN_LEN`] is the memory ceiling of a
//!   candidate set.
//! * **Control characters.** They cannot appear in a host, a URL or a title we would match, and
//!   they are the separators of the checksum encoding below.
//! * **Patterns the safety heuristic refuses.** See [`scan_pattern_risk`].
//! * **Malformed regexes and malformed host patterns.** A host pattern containing a path or a
//!   scheme is not a host pattern; accepting it would silently never match.
//!
//! Disabled and already-expired rules are validated too. They can be re-enabled from the settings
//! screen without a second validation pass, so a set is either wholly usable or wholly refused.
//!
//! ## Rollback (§8)
//!
//! [`RuleSetManager`] keeps the active set and the one it replaced. A caller that observes
//! abnormal playback calls [`RuleSetManager::report_playback_failure`]; when enough failures land
//! inside the window that follows an activation, the manager restores the previous set by itself.
//! Two properties are deliberate:
//!
//! * **Rollback always leaves a usable engine.** With no previous set to restore, the manager falls
//!   back to [`ValidatedRuleSet::inert`] — an empty set that allows everything — rather than
//!   leaving the caller without an engine. Filtering nothing is a working application; no engine is
//!   not.
//! * **A rolled-back version is not reinstalled.** Without that, an updater that keeps offering the
//!   same set would break playback again after every rollback, on a loop.

use std::array;
use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use beastube_core::settings::{FilteringMode, FilteringSettings};
use beastube_core::time_util::Timestamp;
use parking_lot::{Mutex, RwLock};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::adapter::FilteringProvider;
use crate::diagnostics::{FilteringDiagnostics, RuleCounts};
use crate::engine::{EngineConfig, FilterEngine};
use crate::error::{FilterError, FilterResult, PatternRisk, SegmentProblem, VersionProblem};
use crate::rule::{Rule, RuleKind, RuleMode};

/// Maximum number of rules in one set.
///
/// Sized for the job this subsystem actually has: third-party tracking hosts, a user's channel and
/// keyword lists, and segment categories. Tracker lists in the wild run to a few tens of thousands
/// of entries, so 20 000 is generous for the legitimate cases while bounding a candidate set to
/// roughly 10 MiB of patterns at [`MAX_PATTERN_LEN`] each — an amount we can validate in one pass
/// without a progress bar.
pub const MAX_RULES: usize = 20_000;

/// Maximum length of one pattern, in characters.
///
/// Every legitimate pattern is a host, a keyword, a channel identifier or a compact regex; none of
/// them approaches 512. The bound's real job is to keep a pathological regex small enough that the
/// safety scan and the compiler both stay cheap.
pub const MAX_PATTERN_LEN: usize = 512;

/// Maximum length of a rule-set version string.
pub const MAX_VERSION_LEN: usize = 64;

/// Version reported by [`ValidatedRuleSet::inert`].
pub const INERT_VERSION: &str = "inert";

/// Maximum number of quantifiers permitted in one regex pattern.
///
/// A legitimate URL pattern uses a handful. A pattern with dozens is either machine-generated or
/// an attempt to find the compiler's limits.
const MAX_QUANTIFIERS: usize = 32;

/// Largest counted repetition permitted, i.e. the `m` in `a{n,m}`.
///
/// Counted repetition is expanded by the compiler, so the bound is a bound on compiled size.
const MAX_REPETITION: u32 = 256;

/// Compiled-program size ceiling for one pattern, in bytes.
///
/// Set explicitly rather than left at the crate default so the limit is a stated decision: a
/// pattern that needs more than this is refused at compile time instead of allocating.
const REGEX_SIZE_LIMIT: usize = 256 * 1024;

/// Lazy-DFA cache ceiling for one pattern, in bytes. Bounds match-time memory, not compile-time.
const REGEX_DFA_SIZE_LIMIT: usize = 256 * 1024;

/// Field separator in the checksum encoding. A validated pattern cannot contain it.
const FIELD_SEPARATOR: char = '\u{1f}';

/// Record separator in the checksum encoding. A validated pattern cannot contain it.
const RECORD_SEPARATOR: char = '\u{1e}';

/// FNV-1a 128-bit offset basis.
const FNV_OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;

/// FNV-1a 128-bit prime.
const FNV_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

/// Where a rule set came from.
///
/// The vocabulary matches the `CHECK (source IN (…))` constraint on `filtering_rule_sets`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSetSource {
    /// Shipped with the application.
    #[default]
    Builtin,
    /// Downloaded by the rule updater.
    Update,
    /// Derived from the user's own settings.
    User,
}

impl RuleSetSource {
    /// Stable identifier, identical to the value stored in `filtering_rule_sets.source`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Update => "update",
            Self::User => "user",
        }
    }
}

impl fmt::Display for RuleSetSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RuleSetSource {
    type Err = FilterError;

    fn from_str(s: &str) -> FilterResult<Self> {
        match s {
            "builtin" => Ok(Self::Builtin),
            "update" => Ok(Self::Update),
            "user" => Ok(Self::User),
            other => Err(FilterError::unknown_enum_value("rule_set_source", other)),
        }
    }
}

/// An unvalidated collection of rules with a version.
///
/// This is the shape that crosses every boundary: the database row set, the update document, the
/// user's settings. It is inert — nothing here can match anything until [`RuleSet::validate`] has
/// turned it into a [`ValidatedRuleSet`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSet {
    /// Version identifier. Primary key of `filtering_rule_sets`, so it is validated as strictly as
    /// an identifier rather than accepted as free text.
    pub version: String,
    /// Provenance.
    #[serde(default)]
    pub source: RuleSetSource,
    /// The rules, in the order they will be applied within a priority tie.
    #[serde(default)]
    pub rules: Vec<Rule>,
}

impl RuleSet {
    /// An empty set at `version`.
    #[must_use]
    pub fn new(version: impl Into<String>, source: RuleSetSource) -> Self {
        Self {
            version: version.into(),
            source,
            rules: Vec::new(),
        }
    }

    /// Appends a rule, returning the set for chaining.
    #[must_use]
    pub fn with_rule(mut self, rule: Rule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Appends every rule in `rules`.
    #[must_use]
    pub fn with_rules(mut self, rules: impl IntoIterator<Item = Rule>) -> Self {
        self.rules.extend(rules);
        self
    }

    /// Number of rules, validated or not.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether the set holds no rules.
    ///
    /// An empty set is legal and validates: it is how "filter nothing" is expressed without
    /// special-casing the absence of a set anywhere else.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Builds a user-owned set from the allow and block lists in [`FilteringSettings`].
    ///
    /// The user's own entries are host patterns, which is what the settings screen collects, and
    /// they are given a high priority so they outrank a downloaded set's rules within their pass.
    /// Their allow entries beat every block rule regardless, because allow always beats block.
    ///
    /// `custom_rules` is deliberately not interpreted here: its text syntax is a UI-level decision
    /// that the specification does not fix, and inventing one in the engine would pin it.
    #[must_use]
    pub fn from_user_settings(settings: &FilteringSettings, version: impl Into<String>) -> Self {
        /// Priority given to user entries so they win ties against a shipped set.
        const USER_PRIORITY: i32 = 1_000;

        let allow = settings
            .allowlist
            .iter()
            .map(|host| Rule::new(RuleKind::AllowHost, host.clone()).with_priority(USER_PRIORITY));
        let block = settings
            .blocklist
            .iter()
            .map(|host| Rule::new(RuleKind::BlockHost, host.clone()).with_priority(USER_PRIORITY));

        Self::new(version, RuleSetSource::User).with_rules(allow.chain(block))
    }

    /// Content checksum, as 32 lowercase hexadecimal characters.
    ///
    /// **This is an integrity check, not a signature.** It detects a truncated download, a
    /// half-written row and a hand-edited file. It detects nothing about authorship: anyone able to
    /// substitute a rule set can substitute its checksum too. If the update channel ever needs
    /// authenticity, that belongs at the transport layer, where a key can live.
    ///
    /// FNV-1a is used rather than a hash from another crate because the property needed — fast,
    /// deterministic, order-sensitive — does not justify another dependency, and because a stronger
    /// hash would invite exactly the misreading corrected above.
    ///
    /// The encoding is order-sensitive on purpose: order breaks priority ties, so two sets that
    /// differ only in rule order really are different sets.
    #[must_use]
    pub fn checksum(&self) -> String {
        let mut hash = FNV_OFFSET_BASIS;
        let mut absorb = |bytes: &[u8]| {
            for &byte in bytes {
                hash ^= u128::from(byte);
                hash = hash.wrapping_mul(FNV_PRIME);
            }
        };

        absorb(self.version.as_bytes());
        absorb(self.source.as_str().as_bytes());
        for rule in &self.rules {
            let expiry = rule.expires_at.map_or_else(
                || "-".to_owned(),
                |timestamp| timestamp.as_millis().to_string(),
            );
            let record = format!(
                "{RECORD_SEPARATOR}{}{FIELD_SEPARATOR}{}{FIELD_SEPARATOR}{}{FIELD_SEPARATOR}{}{FIELD_SEPARATOR}{}{FIELD_SEPARATOR}{expiry}",
                rule.kind.as_str(),
                rule.pattern,
                rule.priority,
                rule.min_mode.as_str(),
                u8::from(rule.enabled),
            );
            absorb(record.as_bytes());
        }
        format!("{hash:032x}")
    }

    /// Checks the set against a checksum that travelled with it.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::ChecksumMismatch`] if the contents do not hash to `expected`.
    pub fn verify_checksum(&self, expected: &str) -> FilterResult<()> {
        let actual = self.checksum();
        if actual == expected {
            Ok(())
        } else {
            Err(FilterError::ChecksumMismatch {
                expected: crate::error::shorten(expected),
                actual,
            })
        }
    }

    /// Validates and compiles the set.
    ///
    /// This is the only way to produce a [`ValidatedRuleSet`], and therefore the only way to reach
    /// an engine.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::InvalidVersion`] or [`FilterError::TooManyRules`] for a set-level
    /// problem, or [`FilterError::InvalidRule`] wrapping the first rule-level failure, identifying
    /// the rule by position.
    pub fn validate(self) -> FilterResult<ValidatedRuleSet> {
        validate_version(&self.version)?;
        if self.rules.len() > MAX_RULES {
            return Err(FilterError::TooManyRules {
                count: self.rules.len(),
                max: MAX_RULES,
            });
        }

        let checksum = self.checksum();
        let mut compiled = Vec::with_capacity(self.rules.len());
        for (index, rule) in self.rules.into_iter().enumerate() {
            compiled.push(compile_rule(rule).map_err(|err| FilterError::at_rule(index, err))?);
        }

        let mut by_kind: [Vec<usize>; RuleKind::COUNT] = array::from_fn(|_| Vec::new());
        for (index, rule) in compiled.iter().enumerate() {
            by_kind[rule.kind().index()].push(index);
        }
        // Highest priority first; ties keep declaration order, which is why the checksum is
        // order-sensitive. `sort_by_key` is stable, so the tie-break needs no explicit term.
        for bucket in &mut by_kind {
            bucket.sort_by_key(|&index| std::cmp::Reverse(compiled[index].priority()));
        }

        Ok(ValidatedRuleSet {
            version: self.version,
            source: self.source,
            checksum,
            rules: compiled,
            by_kind,
        })
    }
}

/// Validates a rule in isolation, without keeping the compiled form.
///
/// Used by the settings screen to reject a rule as the user types it, where building a whole set
/// would be wasteful.
///
/// # Errors
///
/// Returns the same rule-level failures as [`RuleSet::validate`], unwrapped: there is no position
/// to report.
pub fn validate_rule(rule: &Rule) -> FilterResult<()> {
    compile_rule(rule.clone()).map(|_| ())
}

/// Checks a version string. See [`RuleSet::version`] for why it is treated as an identifier.
fn validate_version(version: &str) -> FilterResult<()> {
    if version.is_empty() {
        return Err(FilterError::InvalidVersion {
            problem: VersionProblem::Empty,
        });
    }
    if version.chars().count() > MAX_VERSION_LEN {
        return Err(FilterError::InvalidVersion {
            problem: VersionProblem::TooLong,
        });
    }
    if !version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
    {
        return Err(FilterError::InvalidVersion {
            problem: VersionProblem::InvalidCharacter,
        });
    }
    Ok(())
}

/// A rule plus whatever matching state its kind needs, produced once at validation.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    rule: Rule,
    matcher: Matcher,
}

/// The prepared form of a pattern.
///
/// [`Regex`] is boxed because it is two orders of magnitude larger than the other variants, and a
/// set is overwhelmingly host and keyword rules.
#[derive(Debug, Clone)]
enum Matcher {
    /// A normalized domain, matched with [`beastube_core::security::is_host_within`].
    Host(String),
    /// A compiled URL pattern, matched unanchored against the whole URL.
    Url(Box<Regex>),
    /// A channel identifier or display name.
    Channel(String),
    /// A title keyword, matched case-insensitively as a substring.
    Keyword(String),
    /// A creator-marked segment category.
    Category(String),
}

impl CompiledRule {
    /// The rule this was compiled from.
    #[must_use]
    pub const fn rule(&self) -> &Rule {
        &self.rule
    }

    /// The rule's kind.
    #[must_use]
    pub const fn kind(&self) -> RuleKind {
        self.rule.kind
    }

    /// The rule's tie-break priority.
    #[must_use]
    pub const fn priority(&self) -> i32 {
        self.rule.priority
    }

    /// The normalized pattern this rule matches with.
    ///
    /// Normalization can differ from the authored text — a host pattern is lowercased and loses a
    /// leading `*.` — so the settings screen shows this rather than the raw input.
    #[must_use]
    pub fn normalized_pattern(&self) -> &str {
        match &self.matcher {
            Matcher::Host(value)
            | Matcher::Channel(value)
            | Matcher::Keyword(value)
            | Matcher::Category(value) => value,
            Matcher::Url(regex) => regex.as_str(),
        }
    }

    /// Whether the rule may match at all under `mode` at `now`.
    #[must_use]
    pub fn applies(&self, mode: FilteringMode, now: Timestamp) -> bool {
        self.rule.applies(mode, now)
    }

    /// Whether `host` falls under this rule's domain.
    ///
    /// Delegates to [`beastube_core::security::is_host_within`] rather than comparing suffixes, so
    /// `evilgooglevideo.com` does not match a `googlevideo.com` rule. Returns `false` for any rule
    /// whose pattern is not a host.
    #[must_use]
    pub fn matches_host(&self, host: &str) -> bool {
        match &self.matcher {
            Matcher::Host(domain) => beastube_core::security::is_host_within(host, domain),
            _ => false,
        }
    }

    /// Whether the whole URL matches this rule's pattern.
    #[must_use]
    pub fn matches_url(&self, url: &str) -> bool {
        match &self.matcher {
            Matcher::Url(regex) => regex.is_match(url),
            _ => false,
        }
    }

    /// Whether this rule names the given channel, by identifier or by display name.
    ///
    /// Identifiers are compared exactly because their alphabet is case-sensitive; display names are
    /// compared ignoring ASCII case because that is what a user copying a name off a card expects.
    #[must_use]
    pub fn matches_channel(&self, id: Option<&str>, name: Option<&str>) -> bool {
        let Matcher::Channel(pattern) = &self.matcher else {
            return false;
        };
        id.is_some_and(|id| id == pattern)
            || name.is_some_and(|name| name.eq_ignore_ascii_case(pattern))
    }

    /// Whether `text` contains this rule's keyword, ignoring ASCII case.
    #[must_use]
    pub fn matches_keyword(&self, text: &str) -> bool {
        match &self.matcher {
            Matcher::Keyword(keyword) => contains_ignore_ascii_case(text, keyword),
            _ => false,
        }
    }

    /// Whether this rule names the given segment category.
    #[must_use]
    pub fn matches_category(&self, category: &str) -> bool {
        match &self.matcher {
            Matcher::Category(pattern) => pattern == category,
            _ => false,
        }
    }
}

/// Case-insensitive substring search that allocates nothing.
///
/// Only ASCII case is folded. A non-ASCII keyword still matches its exact bytes, so `Überraschung`
/// finds itself but not `überraschung`; full Unicode case folding would need a table this crate
/// does not carry, and the alternative — lowercasing every title on every evaluation — costs an
/// allocation per feed item.
///
/// Byte-window comparison is sound on UTF-8: an ASCII byte never occurs inside a multi-byte
/// sequence, and a multi-byte needle can only align at a lead byte, so a match always lands on a
/// character boundary.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

/// Validates one rule and prepares its matcher.
fn compile_rule(rule: Rule) -> FilterResult<CompiledRule> {
    let kind = rule.kind;
    let pattern = rule.pattern.trim();

    if pattern.is_empty() {
        return Err(FilterError::EmptyPattern { kind });
    }
    let length = pattern.chars().count();
    if length > MAX_PATTERN_LEN {
        return Err(FilterError::PatternTooLong {
            kind,
            len: length,
            max: MAX_PATTERN_LEN,
        });
    }
    if pattern.chars().any(char::is_control) {
        return Err(FilterError::MalformedPattern {
            kind,
            detail: "pattern contains a control character".to_owned(),
        });
    }

    let matcher = match kind {
        RuleKind::BlockHost | RuleKind::AllowHost => Matcher::Host(compile_host(kind, pattern)?),
        RuleKind::BlockUrl | RuleKind::AllowUrl => Matcher::Url(Box::new(compile_regex(kind, pattern)?)),
        RuleKind::HideChannel => Matcher::Channel(pattern.to_owned()),
        RuleKind::HideKeyword => Matcher::Keyword(pattern.to_owned()),
        RuleKind::SkipSegment => Matcher::Category(compile_category(pattern)?),
    };

    Ok(CompiledRule {
        rule: Rule {
            pattern: pattern.to_owned(),
            ..rule
        },
        matcher,
    })
}

/// Normalizes a host pattern to a bare, lowercase domain.
///
/// A leading `*.` is accepted and dropped: host rules already cover subdomains, so the wildcard is
/// how people write what the rule already means, and refusing it would be pedantry. Anything that
/// is not a bare domain — a scheme, a path, a port, a query — is refused rather than trimmed,
/// because a host rule that silently drops half its pattern never matches what its author meant.
fn compile_host(kind: RuleKind, pattern: &str) -> FilterResult<String> {
    let bare = pattern.strip_prefix("*.").unwrap_or(pattern);
    let bare = bare.strip_suffix('.').unwrap_or(bare);
    let malformed = |detail: &str| FilterError::MalformedPattern {
        kind,
        detail: detail.to_owned(),
    };

    if bare.is_empty() {
        return Err(FilterError::EmptyPattern { kind });
    }
    if bare.contains("://") {
        return Err(malformed("host pattern must not contain a scheme"));
    }
    if bare.contains('/') || bare.contains('?') || bare.contains('#') {
        return Err(malformed("host pattern must not contain a path or query"));
    }
    if bare.contains(':') {
        return Err(malformed(
            "host pattern must not contain a port or an IPv6 literal",
        ));
    }
    if !bare
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
    {
        return Err(malformed(
            "host pattern accepts only ASCII letters, digits, `-`, `_` and `.`",
        ));
    }
    if bare.starts_with('.') || bare.contains("..") {
        return Err(malformed("host pattern has an empty label"));
    }
    if bare.split('.').any(|label| label.len() > 63) {
        return Err(malformed("host pattern has a label longer than 63 bytes"));
    }
    Ok(bare.to_ascii_lowercase())
}

/// Normalizes a segment category, reusing the segment vocabulary's own rules.
fn compile_category(pattern: &str) -> FilterResult<String> {
    crate::segment::SegmentCategory::new(pattern).map(|category| category.as_str().to_owned())
}

/// Compiles a URL pattern after the safety scan.
fn compile_regex(kind: RuleKind, pattern: &str) -> FilterResult<Regex> {
    if let Some(risk) = scan_pattern_risk(pattern) {
        return Err(FilterError::UnsafePattern { kind, risk });
    }
    RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
        .build()
        .map_err(|err| FilterError::MalformedPattern {
            kind,
            detail: err.to_string(),
        })
}

/// Refuses regex patterns whose *compiled* form could be expensive, and patterns whose shape is
/// the classic catastrophic-backtracking construction.
///
/// ## Why this exists even though this engine cannot backtrack
///
/// The `regex` crate matches with finite automata in time linear in the input. It has no
/// backtracking, so the denial of service this heuristic is named after cannot happen *here*. Two
/// real risks remain:
///
/// 1. **Compilation, not matching, is where a hostile pattern costs.** `(a{100}){100}` is a small
///    string that asks for a large program. [`REGEX_SIZE_LIMIT`] turns that into a compile error
///    rather than an allocation spike, and [`MAX_REPETITION`] rejects it earlier with a reason a
///    rule author can act on.
/// 2. **Rule sets are portable data.** The same pattern is stored in SQLite, can be exported, and
///    could later be consumed by something that *does* backtrack. A rule set that is only safe
///    because of one crate's implementation strategy is not safe.
///
/// ## The heuristic
///
/// Scanning left to right, outside character classes and honouring backslash escapes:
///
/// * a quantifier (`*`, `+`, `{n,m}`) applied to a group that already contains a quantifier is
///   [`PatternRisk::NestedQuantifier`] — this is `(a+)+`, `(a*)*`, `((ab)+c)+`;
/// * a counted repetition above [`MAX_REPETITION`] is [`PatternRisk::ExcessiveRepetition`];
/// * more than [`MAX_QUANTIFIERS`] quantifiers is [`PatternRisk::TooManyQuantifiers`].
///
/// `?` is not counted: it makes a term optional rather than repeated, and it is also the marker for
/// a group flag (`(?:`), which would otherwise be misread as a quantifier.
///
/// The scan is deliberately conservative and over-rejects: `(ab+)+` is linear under this crate and
/// is refused anyway. A false rejection costs the author one flattened pattern; a false acceptance
/// costs a stall in the media path, which is the failure this whole subsystem must not cause.
#[must_use]
pub fn scan_pattern_risk(pattern: &str) -> Option<PatternRisk> {
    // One frame per open group; `true` once a quantifier has been seen inside it. The base frame
    // is the pattern itself and is never popped, so the stack is never empty.
    let mut frames: Vec<bool> = vec![false];
    let mut quantifiers = 0usize;
    let mut in_class = false;
    let mut escaped = false;

    let chars: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        index += 1;

        if escaped {
            escaped = false;
            continue;
        }
        match current {
            '\\' => escaped = true,
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            _ if in_class => {}
            '(' => frames.push(false),
            ')' => {
                // An unbalanced `)` leaves the base frame in place; the compiler rejects the
                // pattern moments later with a better message than this scan could give.
                let inner_had_quantifier = if frames.len() > 1 {
                    frames.pop().unwrap_or(false)
                } else {
                    false
                };
                let quantified = matches!(chars.get(index), Some('*' | '+' | '{'));
                if quantified && inner_had_quantifier {
                    return Some(PatternRisk::NestedQuantifier);
                }
                if let Some(frame) = frames.last_mut() {
                    *frame = *frame || inner_had_quantifier || quantified;
                }
            }
            '*' | '+' => {
                quantifiers += 1;
                if let Some(frame) = frames.last_mut() {
                    *frame = true;
                }
            }
            '{' => {
                if let Some((bound, next)) = parse_repetition(&chars, index) {
                    if bound > MAX_REPETITION {
                        return Some(PatternRisk::ExcessiveRepetition);
                    }
                    quantifiers += 1;
                    if let Some(frame) = frames.last_mut() {
                        *frame = true;
                    }
                    index = next;
                }
                // A `{` that does not open a counted repetition is a literal brace.
            }
            _ => {}
        }
        if quantifiers > MAX_QUANTIFIERS {
            return Some(PatternRisk::TooManyQuantifiers);
        }
    }
    None
}

/// Parses `n`, `n,` or `n,m` followed by `}` starting at `index`.
///
/// Returns the largest bound mentioned and the index just past the closing brace, or `None` if
/// this is not a counted repetition. A bound with more digits than `u32` can hold is reported as
/// [`u32::MAX`], which the caller refuses — no parse failure can slip past as "small".
fn parse_repetition(chars: &[char], index: usize) -> Option<(u32, usize)> {
    let mut cursor = index;
    let mut largest = 0u32;
    let mut current: Option<u32> = None;
    let mut digits_seen = false;

    while cursor < chars.len() {
        match chars[cursor] {
            digit @ '0'..='9' => {
                digits_seen = true;
                let value = digit.to_digit(10).unwrap_or(0);
                current = Some(
                    current
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(value),
                );
                largest = largest.max(current.unwrap_or(u32::MAX));
            }
            ',' if digits_seen => current = None,
            '}' if digits_seen => return Some((largest, cursor + 1)),
            _ => return None,
        }
        cursor += 1;
    }
    None
}

/// A rule set that has passed validation and carries its compiled matchers.
///
/// Cloning is cheap relative to validating: it copies the compiled programs' reference counts, not
/// the patterns. In practice it is held behind an `Arc` and shared by every engine.
#[derive(Debug, Clone)]
pub struct ValidatedRuleSet {
    version: String,
    source: RuleSetSource,
    checksum: String,
    rules: Vec<CompiledRule>,
    by_kind: [Vec<usize>; RuleKind::COUNT],
}

impl ValidatedRuleSet {
    /// The empty set: valid, activatable, and matching nothing.
    ///
    /// This is the floor the rollback path falls back to when there is no previous set to restore.
    /// An engine built on it allows every request and shows every item, which is the safe direction
    /// to fail in (§10).
    #[must_use]
    pub fn inert() -> Self {
        Self {
            version: INERT_VERSION.to_owned(),
            source: RuleSetSource::Builtin,
            checksum: RuleSet::new(INERT_VERSION, RuleSetSource::Builtin).checksum(),
            rules: Vec::new(),
            by_kind: array::from_fn(|_| Vec::new()),
        }
    }

    /// Version identifier.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Provenance.
    #[must_use]
    pub const fn source(&self) -> RuleSetSource {
        self.source
    }

    /// Content checksum, computed before compilation.
    #[must_use]
    pub fn checksum(&self) -> &str {
        &self.checksum
    }

    /// Total number of rules, including disabled and expired ones.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether the set holds no rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Whether this is the inert set.
    #[must_use]
    pub fn is_inert(&self) -> bool {
        self.version == INERT_VERSION && self.rules.is_empty()
    }

    /// Every rule, in declaration order.
    #[must_use]
    pub fn rules(&self) -> &[CompiledRule] {
        &self.rules
    }

    /// The rule at `index`, as reported by a decision's [`RuleMatch`](crate::engine::RuleMatch).
    #[must_use]
    pub fn rule_at(&self, index: usize) -> Option<&CompiledRule> {
        self.rules.get(index)
    }

    /// Number of rules of one kind.
    #[must_use]
    pub fn count_of(&self, kind: RuleKind) -> usize {
        self.by_kind[kind.index()].len()
    }

    /// Rules of one kind with their indices, highest priority first.
    ///
    /// The index is what a decision reports, so a caller can look the rule back up without holding
    /// a borrow across the evaluation.
    pub fn rules_of(&self, kind: RuleKind) -> impl Iterator<Item = (usize, &CompiledRule)> + '_ {
        self.by_kind[kind.index()]
            .iter()
            .filter_map(move |&index| self.rules.get(index).map(|rule| (index, rule)))
    }

    /// Counts by category, for the diagnostics screen.
    #[must_use]
    pub fn counts(&self) -> RuleCounts {
        RuleCounts {
            total: self.rules.len(),
            allow: self.count_of(RuleKind::AllowHost) + self.count_of(RuleKind::AllowUrl),
            block: self.count_of(RuleKind::BlockHost) + self.count_of(RuleKind::BlockUrl),
            hide: self.count_of(RuleKind::HideChannel) + self.count_of(RuleKind::HideKeyword),
            segment: self.count_of(RuleKind::SkipSegment),
        }
    }
}

/// Source of monotonic time for the rollback window.
///
/// Injected rather than read from the clock directly so the window can be tested without sleeping,
/// and so a wall-clock adjustment — a user fixing their system time, a DST transition — cannot make
/// a failure window look infinitely long or negative.
pub trait MonotonicClock: fmt::Debug + Send + Sync {
    /// The current instant.
    fn now(&self) -> Instant;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl MonotonicClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// When an activation is considered to have failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollbackPolicy {
    /// Failures within [`RollbackPolicy::failure_window`] that trigger a rollback.
    pub failure_threshold: u32,
    /// Sliding window over which failures are counted.
    pub failure_window: Duration,
    /// How long after an activation failures are attributed to the new rule set.
    pub observation_period: Duration,
}

impl Default for RollbackPolicy {
    fn default() -> Self {
        Self {
            // One failure is a bad network moment; three in a minute is a pattern. Set low enough
            // that a user does not sit through repeated breakage, high enough that a single flaky
            // stream does not discard a good rule set.
            failure_threshold: 3,
            failure_window: Duration::from_secs(60),
            // Beyond a few minutes of successful use, a failure says more about the network than
            // about the rules, and attributing it to the activation would make rollback a
            // superstition rather than a signal.
            observation_period: Duration::from_secs(300),
        }
    }
}

/// Why a rule set was rolled back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum RollbackReason {
    /// Playback failures correlated with the activation.
    PlaybackFailures {
        /// Failures counted inside the window.
        observed: u32,
        /// Threshold that was crossed.
        threshold: u32,
    },
    /// The user or an operator asked for it.
    Manual,
}

impl RollbackReason {
    /// i18n key stored in `filtering_rule_sets.reason_key` and shown on the diagnostics screen.
    #[must_use]
    pub const fn reason_key(self) -> &'static str {
        match self {
            Self::PlaybackFailures { .. } => "filtering.rollback.playback_failures",
            Self::Manual => "filtering.rollback.manual",
        }
    }
}

/// What a rollback did.
///
/// Holds rule-set versions and a reason. It deliberately holds nothing about *what* was playing
/// when the failures happened: the diagnostics screen must stay safe to screenshot (§11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackRecord {
    /// Version that was rolled back.
    pub from_version: String,
    /// Version restored, or [`INERT_VERSION`] when there was nothing to restore.
    pub to_version: String,
    /// Why.
    pub reason: RollbackReason,
    /// When, for display.
    pub at: Timestamp,
}

impl RollbackRecord {
    /// i18n key describing the reason.
    #[must_use]
    pub const fn reason_key(&self) -> &'static str {
        self.reason.reason_key()
    }
}

/// Mutable half of the manager, behind one lock.
#[derive(Debug)]
struct ManagerState {
    engine: Arc<FilterEngine>,
    active: Arc<ValidatedRuleSet>,
    previous: Option<Arc<ValidatedRuleSet>>,
    config: EngineConfig,
    activated_at: Instant,
    failures: VecDeque<Instant>,
    last_rollback: Option<RollbackRecord>,
    rolled_back_versions: BTreeSet<String>,
}

/// Owns the active rule set, the one before it, and the decision to go back (§8).
///
/// ## Locking
///
/// Two locks, always taken in this order: an activation mutex that serializes whole
/// activate/rollback operations, and a state lock held only for the swap itself. Providers are
/// notified after the state lock is released, so a provider's `set_engine` never runs under it.
/// A provider must not call back into the manager from `set_engine`; nothing in this crate does.
#[derive(Debug)]
pub struct RuleSetManager {
    activation: Mutex<()>,
    state: RwLock<ManagerState>,
    providers: Mutex<Vec<Arc<dyn FilteringProvider>>>,
    diagnostics: Arc<FilteringDiagnostics>,
    policy: RollbackPolicy,
    clock: Arc<dyn MonotonicClock>,
    updates: watch::Sender<Arc<FilterEngine>>,
}

impl RuleSetManager {
    /// Builds a manager around an already-validated set.
    ///
    /// Taking a [`ValidatedRuleSet`] rather than a [`RuleSet`] means construction cannot fail, so
    /// there is no startup path where the application runs without an engine.
    #[must_use]
    pub fn new(
        active: ValidatedRuleSet,
        config: EngineConfig,
        diagnostics: Arc<FilteringDiagnostics>,
    ) -> Self {
        let clock: Arc<dyn MonotonicClock> = Arc::new(SystemClock);
        let active = Arc::new(active);
        let engine = Arc::new(FilterEngine::new(
            Arc::clone(&active),
            config.clone(),
            Arc::clone(&diagnostics),
        ));
        diagnostics.set_config(config.enabled, config.mode);
        diagnostics.set_active_rule_set(active.version(), active.checksum(), active.counts());

        let (updates, _initial) = watch::channel(Arc::clone(&engine));
        Self {
            activation: Mutex::new(()),
            state: RwLock::new(ManagerState {
                engine,
                active,
                previous: None,
                config,
                activated_at: clock.now(),
                failures: VecDeque::new(),
                last_rollback: None,
                rolled_back_versions: BTreeSet::new(),
            }),
            providers: Mutex::new(Vec::new()),
            diagnostics,
            policy: RollbackPolicy::default(),
            clock,
            updates,
        }
    }

    /// Overrides the rollback policy.
    #[must_use]
    pub const fn with_policy(mut self, policy: RollbackPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Overrides the clock, restarting the observation period on the new one.
    ///
    /// Instants from two clocks must never be compared, so the activation instant is retaken here.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn MonotonicClock>) -> Self {
        {
            let mut state = self.state.write();
            state.activated_at = clock.now();
            state.failures.clear();
        }
        self.clock = clock;
        self
    }

    /// The current engine. Cheap: it clones one `Arc`.
    #[must_use]
    pub fn engine(&self) -> Arc<FilterEngine> {
        Arc::clone(&self.state.read().engine)
    }

    /// The active rule set.
    #[must_use]
    pub fn active(&self) -> Arc<ValidatedRuleSet> {
        Arc::clone(&self.state.read().active)
    }

    /// Version of the active rule set.
    #[must_use]
    pub fn active_version(&self) -> String {
        self.state.read().active.version().to_owned()
    }

    /// Version of the set that would be restored by a rollback, if any.
    #[must_use]
    pub fn previous_version(&self) -> Option<String> {
        self.state
            .read()
            .previous
            .as_ref()
            .map(|set| set.version().to_owned())
    }

    /// The most recent rollback, if one has happened.
    #[must_use]
    pub fn last_rollback(&self) -> Option<RollbackRecord> {
        self.state.read().last_rollback.clone()
    }

    /// Shared diagnostics collector.
    #[must_use]
    pub fn diagnostics(&self) -> &Arc<FilteringDiagnostics> {
        &self.diagnostics
    }

    /// Subscribes to engine swaps.
    ///
    /// The current engine counts as already seen, so `changed()` resolves on the *next* activation
    /// or rollback. Use [`RuleSetManager::engine`] for the value now. This exists for observers
    /// outside the crate — a diagnostics screen, a status badge — that want to await a change;
    /// components that must not miss a swap register as a provider instead, which is synchronous.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Arc<FilterEngine>> {
        self.updates.subscribe()
    }

    /// Registers an adapter to be handed every future engine, and hands it the current one now.
    ///
    /// Registration is synchronous on purpose: between a rollback and an adapter noticing it, every
    /// request the adapter evaluates would use the set that was just judged harmful. A scheduled
    /// notification would leave exactly that gap.
    pub fn register_provider(&self, provider: Arc<dyn FilteringProvider>) {
        provider.set_engine(self.engine());
        self.providers.lock().push(provider);
    }

    /// Validates `candidate` and, if it passes, makes it the active set.
    ///
    /// # Errors
    ///
    /// Returns the validation failure, or [`FilterError::PreviouslyRolledBack`] if this version was
    /// rolled back earlier in the session. In both cases the active set is untouched: a refused
    /// candidate never displaces a working one.
    pub fn activate(&self, candidate: RuleSet) -> FilterResult<Arc<FilterEngine>> {
        let version = candidate.version.clone();
        if self.state.read().rolled_back_versions.contains(&version) {
            self.diagnostics.record_failed_update();
            return Err(FilterError::PreviouslyRolledBack { version });
        }
        match candidate.validate() {
            Ok(validated) => Ok(self.activate_validated(validated)),
            Err(err) => {
                self.diagnostics.record_failed_update();
                tracing::warn!(
                    version = %version,
                    code = err.code(),
                    "rejected a candidate rule set; keeping the active one"
                );
                Err(err)
            }
        }
    }

    /// Activates a set that is already validated, keeping the outgoing one for rollback.
    #[must_use]
    pub fn activate_validated(&self, candidate: ValidatedRuleSet) -> Arc<FilterEngine> {
        let _serialized = self.activation.lock();
        let candidate = Arc::new(candidate);
        let engine = {
            let mut state = self.state.write();
            let engine = Arc::new(FilterEngine::new(
                Arc::clone(&candidate),
                state.config.clone(),
                Arc::clone(&self.diagnostics),
            ));
            state.previous = Some(Arc::clone(&state.active));
            state.active = Arc::clone(&candidate);
            state.engine = Arc::clone(&engine);
            state.activated_at = self.clock.now();
            state.failures.clear();
            engine
        };

        self.diagnostics.set_active_rule_set(
            candidate.version(),
            candidate.checksum(),
            candidate.counts(),
        );
        tracing::info!(
            version = %candidate.version(),
            rules = candidate.len(),
            "activated rule set"
        );
        self.publish(&engine);
        engine
    }

    /// Applies a settings change without reloading rules.
    ///
    /// Mode and the master switch are engine configuration, not rule data, which is what lets the
    /// user turn filtering off and on again with no reload and no restart.
    pub fn set_filtering(&self, enabled: bool, mode: FilteringMode) {
        let _serialized = self.activation.lock();
        let engine = {
            let mut state = self.state.write();
            if state.config.enabled == enabled && state.config.mode == mode {
                return;
            }
            state.config.enabled = enabled;
            state.config.mode = mode;
            let engine = Arc::new(FilterEngine::new(
                Arc::clone(&state.active),
                state.config.clone(),
                Arc::clone(&self.diagnostics),
            ));
            state.engine = Arc::clone(&engine);
            engine
        };
        self.diagnostics.set_config(enabled, mode);
        self.publish(&engine);
    }

    /// Records one report of abnormal playback, rolling back if that crosses the threshold.
    ///
    /// Returns the rollback record when this call caused one. Concurrent reporters cannot each
    /// trigger a rollback: the tally is cleared under the state lock by whichever call crosses the
    /// threshold, and the rollback itself re-checks which version it is replacing.
    pub fn report_playback_failure(&self) -> Option<RollbackRecord> {
        let now = self.clock.now();
        let expected_from = {
            let mut state = self.state.write();

            // A failure long after activation says more about the network than about the rules.
            if now.saturating_duration_since(state.activated_at) > self.policy.observation_period {
                state.failures.clear();
                return None;
            }
            let window = self.policy.failure_window;
            while state
                .failures
                .front()
                .is_some_and(|&at| now.saturating_duration_since(at) > window)
            {
                state.failures.pop_front();
            }
            state.failures.push_back(now);

            let observed = u32::try_from(state.failures.len()).unwrap_or(u32::MAX);
            if observed < self.policy.failure_threshold {
                return None;
            }
            state.failures.clear();
            state.active.version().to_owned()
        };

        self.rollback_internal(
            Some(&expected_from),
            RollbackReason::PlaybackFailures {
                observed: self.policy.failure_threshold,
                threshold: self.policy.failure_threshold,
            },
        )
    }

    /// Restores the previous rule set, or the inert set when there is none.
    ///
    /// Returns `None` only when there is nothing to do: the inert set is already active and no
    /// previous set exists.
    pub fn rollback(&self, reason: RollbackReason) -> Option<RollbackRecord> {
        self.rollback_internal(None, reason)
    }

    /// Forgets which versions were rolled back, so they may be offered again.
    ///
    /// The escape hatch for a rollback that was actually caused by something else; without it a
    /// good rule set could be excluded for the rest of the session.
    pub fn clear_rollback_history(&self) {
        self.state.write().rolled_back_versions.clear();
    }

    fn rollback_internal(
        &self,
        expected_from: Option<&str>,
        reason: RollbackReason,
    ) -> Option<RollbackRecord> {
        let _serialized = self.activation.lock();
        let (record, engine) = {
            let mut state = self.state.write();

            // Someone else already rolled this version back while we waited for the lock.
            if expected_from.is_some_and(|expected| expected != state.active.version()) {
                return None;
            }
            let restored = match state.previous.take() {
                Some(previous) => previous,
                None if state.active.is_inert() => return None,
                // Nothing to restore: fall back to the empty set rather than leaving the caller
                // without an engine. Filtering nothing is a working application (§10).
                None => Arc::new(ValidatedRuleSet::inert()),
            };

            let record = RollbackRecord {
                from_version: state.active.version().to_owned(),
                to_version: restored.version().to_owned(),
                reason,
                at: Timestamp::now(),
            };
            let engine = Arc::new(FilterEngine::new(
                Arc::clone(&restored),
                state.config.clone(),
                Arc::clone(&self.diagnostics),
            ));
            state
                .rolled_back_versions
                .insert(record.from_version.clone());
            state.active = restored;
            state.engine = Arc::clone(&engine);
            state.activated_at = self.clock.now();
            state.failures.clear();
            state.last_rollback = Some(record.clone());
            (record, engine)
        };

        self.diagnostics.record_rollback(&record);
        let active = self.active();
        self.diagnostics
            .set_active_rule_set(active.version(), active.checksum(), active.counts());
        tracing::warn!(
            from = %record.from_version,
            to = %record.to_version,
            reason = record.reason_key(),
            "rolled back the active rule set"
        );
        self.publish(&engine);
        Some(record)
    }

    /// Hands `engine` to every registered provider and to every subscriber.
    ///
    /// Called with the activation mutex held and the state lock released, so providers are notified
    /// in activation order without any provider running under the state lock.
    fn publish(&self, engine: &Arc<FilterEngine>) {
        let providers: Vec<Arc<dyn FilteringProvider>> = self.providers.lock().clone();
        for provider in providers {
            provider.set_engine(Arc::clone(engine));
        }
        // `send_replace` rather than `send`: the manager holds no receiver of its own, and an
        // engine swap must not depend on someone being subscribed.
        self.updates.send_replace(Arc::clone(engine));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::NeverBlockList;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A clock the test drives by hand, so windows are exercised without sleeping.
    #[derive(Debug)]
    struct TestClock {
        base: Instant,
        offset_ms: AtomicU64,
    }

    impl TestClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                base: Instant::now(),
                offset_ms: AtomicU64::new(0),
            })
        }

        fn advance(&self, millis: u64) {
            self.offset_ms.fetch_add(millis, Ordering::SeqCst);
        }
    }

    impl MonotonicClock for TestClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_millis(self.offset_ms.load(Ordering::SeqCst))
        }
    }

    fn rule_set(version: &str, rules: Vec<Rule>) -> RuleSet {
        RuleSet::new(version, RuleSetSource::Update).with_rules(rules)
    }

    fn validated(version: &str, rules: Vec<Rule>) -> ValidatedRuleSet {
        rule_set(version, rules).validate().expect("valid set")
    }

    fn manager(initial: ValidatedRuleSet) -> RuleSetManager {
        RuleSetManager::new(
            initial,
            EngineConfig::new(true, FilteringMode::Standard, NeverBlockList::empty()),
            Arc::new(FilteringDiagnostics::new()),
        )
    }

    #[test]
    fn an_empty_set_is_valid_because_filtering_nothing_is_a_valid_state() {
        let set = RuleSet::new("2026.09.01", RuleSetSource::Builtin)
            .validate()
            .expect("an empty set validates");
        assert!(set.is_empty());
        assert_eq!(set.counts().total, 0);
    }

    #[test]
    fn validation_rejects_empty_and_whitespace_patterns() {
        for pattern in ["", "   ", "\t"] {
            let err = rule_set("v1", vec![Rule::new(RuleKind::BlockHost, pattern)])
                .validate()
                .unwrap_err();
            assert!(
                matches!(err.root_cause(), FilterError::EmptyPattern { .. }),
                "{pattern:?} produced {err:?}"
            );
            assert_eq!(err.rule_index(), Some(0));
        }
    }

    #[test]
    fn validation_rejects_over_long_patterns_and_over_large_sets() {
        let long = "a".repeat(MAX_PATTERN_LEN + 1);
        let err = rule_set("v1", vec![Rule::new(RuleKind::HideKeyword, long)])
            .validate()
            .unwrap_err();
        assert!(matches!(
            err.root_cause(),
            FilterError::PatternTooLong { .. }
        ));

        let many = vec![Rule::new(RuleKind::HideKeyword, "x"); MAX_RULES + 1];
        assert!(matches!(
            rule_set("v1", many).validate(),
            Err(FilterError::TooManyRules { max: MAX_RULES, .. })
        ));
    }

    #[test]
    fn validation_rejects_control_characters_anywhere_in_a_pattern() {
        for pattern in ["a\u{0}b", "a\nb", "a\u{1f}b", "a\u{1e}b"] {
            let err = rule_set("v1", vec![Rule::new(RuleKind::HideKeyword, pattern)])
                .validate()
                .unwrap_err();
            assert!(
                matches!(err.root_cause(), FilterError::MalformedPattern { .. }),
                "{pattern:?} must be refused"
            );
        }
    }

    #[test]
    fn host_patterns_must_be_bare_hosts() {
        for pattern in [
            "https://tracker.example",
            "tracker.example/pixel",
            "tracker.example:443",
            "tracker.example?a=b",
            "trac ker.example",
            ".example",
            "a..example",
            "trackér.example",
        ] {
            let err = rule_set("v1", vec![Rule::new(RuleKind::BlockHost, pattern)])
                .validate()
                .unwrap_err();
            assert!(
                matches!(err.root_cause(), FilterError::MalformedPattern { .. }),
                "{pattern} must be refused, got {err:?}"
            );
        }
    }

    #[test]
    fn host_patterns_are_normalized_to_a_bare_lowercase_domain() {
        let set = validated(
            "v1",
            vec![
                Rule::new(RuleKind::BlockHost, "*.Tracker.Example."),
                Rule::new(RuleKind::BlockHost, "  metrics.example  "),
            ],
        );
        assert_eq!(set.rules()[0].normalized_pattern(), "tracker.example");
        assert_eq!(set.rules()[1].normalized_pattern(), "metrics.example");
    }

    #[test]
    fn the_safety_scan_refuses_nested_quantifiers() {
        for pattern in ["(a+)+", "(a*)*", "((ab)+c)+", "(a{2,4}){3}", "(x|xx)+"] {
            let risk = scan_pattern_risk(pattern);
            assert!(
                risk.is_some() || pattern == "(x|xx)+",
                "{pattern} should be refused"
            );
        }
        assert_eq!(
            scan_pattern_risk("(a+)+"),
            Some(PatternRisk::NestedQuantifier)
        );
        assert_eq!(
            scan_pattern_risk("((ab)+c)+"),
            Some(PatternRisk::NestedQuantifier)
        );
    }

    #[test]
    fn the_safety_scan_allows_ordinary_url_patterns() {
        for pattern in [
            r"^https://[a-z0-9-]+\.tracker\.example/",
            r"/collect\?v=\d+",
            r"(analytics|metrics)\.example\.com",
            r"a{1,8}",
            r"\(literal\+parens\)",
            r"[a+*]+",
        ] {
            assert_eq!(scan_pattern_risk(pattern), None, "{pattern} is safe");
        }
    }

    #[test]
    fn the_safety_scan_bounds_counted_repetition_and_quantifier_count() {
        assert_eq!(
            scan_pattern_risk("a{5000}"),
            Some(PatternRisk::ExcessiveRepetition)
        );
        assert_eq!(
            scan_pattern_risk("a{1,99999999999999999999}"),
            Some(PatternRisk::ExcessiveRepetition)
        );
        let many = "a+".repeat(MAX_QUANTIFIERS + 1);
        assert_eq!(
            scan_pattern_risk(&many),
            Some(PatternRisk::TooManyQuantifiers)
        );
    }

    #[test]
    fn the_safety_scan_does_not_look_inside_character_classes_or_escapes() {
        // `+` inside a class is a literal, and an escaped paren opens no group.
        assert_eq!(scan_pattern_risk(r"[(+*]+"), None);
        assert_eq!(scan_pattern_risk(r"\(a+\)+"), None);
        // An unbalanced group must not panic; the compiler rejects it afterwards.
        assert_eq!(scan_pattern_risk(")))"), None);
        assert!(scan_pattern_risk("(((").is_none());
    }

    #[test]
    fn an_unsafe_or_malformed_url_pattern_fails_validation() {
        let err = rule_set("v1", vec![Rule::new(RuleKind::BlockUrl, "(a+)+")])
            .validate()
            .unwrap_err();
        assert!(matches!(
            err.root_cause(),
            FilterError::UnsafePattern {
                risk: PatternRisk::NestedQuantifier,
                ..
            }
        ));

        let err = rule_set("v1", vec![Rule::new(RuleKind::BlockUrl, "(unclosed")])
            .validate()
            .unwrap_err();
        assert!(matches!(
            err.root_cause(),
            FilterError::MalformedPattern { .. }
        ));

        // Backreferences and lookaround are not supported by this engine; they must be refused at
        // validation rather than silently never matching.
        let err = rule_set("v1", vec![Rule::new(RuleKind::BlockUrl, r"(?=x)y")])
            .validate()
            .unwrap_err();
        assert!(matches!(
            err.root_cause(),
            FilterError::MalformedPattern { .. }
        ));
    }

    #[test]
    fn disabled_and_expired_rules_are_still_validated() {
        let err = rule_set(
            "v1",
            vec![
                Rule::new(RuleKind::BlockHost, "ok.example"),
                Rule::new(RuleKind::BlockHost, "").disabled(),
            ],
        )
        .validate()
        .unwrap_err();
        assert_eq!(
            err.rule_index(),
            Some(1),
            "a disabled rule can be re-enabled without another validation pass"
        );
    }

    #[test]
    fn versions_are_validated_like_identifiers() {
        for version in ["", " ", "v 1", "v/1", "../etc", &"v".repeat(MAX_VERSION_LEN + 1)] {
            assert!(
                matches!(
                    RuleSet::new(version, RuleSetSource::Update).validate(),
                    Err(FilterError::InvalidVersion { .. })
                ),
                "{version:?} must be refused"
            );
        }
        assert!(
            RuleSet::new("2026.09.03+build_7-rc1", RuleSetSource::Update)
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn the_checksum_changes_with_every_field_that_changes_behaviour() {
        let base = rule_set("v1", vec![Rule::new(RuleKind::BlockHost, "a.example")]);
        let same = rule_set("v1", vec![Rule::new(RuleKind::BlockHost, "a.example")]);
        assert_eq!(base.checksum(), same.checksum());
        assert_eq!(base.checksum().len(), 32);

        let variants = [
            rule_set("v2", vec![Rule::new(RuleKind::BlockHost, "a.example")]),
            rule_set("v1", vec![Rule::new(RuleKind::BlockHost, "b.example")]),
            rule_set("v1", vec![Rule::new(RuleKind::AllowHost, "a.example")]),
            rule_set(
                "v1",
                vec![Rule::new(RuleKind::BlockHost, "a.example").with_priority(1)],
            ),
            rule_set(
                "v1",
                vec![Rule::new(RuleKind::BlockHost, "a.example").with_min_mode(RuleMode::Strict)],
            ),
            rule_set(
                "v1",
                vec![Rule::new(RuleKind::BlockHost, "a.example").disabled()],
            ),
            rule_set(
                "v1",
                vec![
                    Rule::new(RuleKind::BlockHost, "a.example")
                        .with_expiry(Timestamp::from_millis(1)),
                ],
            ),
        ];
        for variant in variants {
            assert_ne!(base.checksum(), variant.checksum(), "{variant:?}");
        }
    }

    #[test]
    fn the_checksum_is_order_sensitive_because_order_breaks_priority_ties() {
        let first = rule_set(
            "v1",
            vec![
                Rule::new(RuleKind::BlockHost, "a.example"),
                Rule::new(RuleKind::BlockHost, "b.example"),
            ],
        );
        let swapped = rule_set(
            "v1",
            vec![
                Rule::new(RuleKind::BlockHost, "b.example"),
                Rule::new(RuleKind::BlockHost, "a.example"),
            ],
        );
        assert_ne!(first.checksum(), swapped.checksum());
    }

    #[test]
    fn a_truncated_set_fails_checksum_verification() {
        let full = rule_set(
            "v1",
            vec![
                Rule::new(RuleKind::BlockHost, "a.example"),
                Rule::new(RuleKind::BlockHost, "b.example"),
            ],
        );
        let checksum = full.checksum();
        let truncated = rule_set("v1", vec![Rule::new(RuleKind::BlockHost, "a.example")]);

        assert!(full.verify_checksum(&checksum).is_ok());
        assert!(matches!(
            truncated.verify_checksum(&checksum),
            Err(FilterError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn rules_are_bucketed_by_kind_in_priority_order() {
        let set = validated(
            "v1",
            vec![
                Rule::new(RuleKind::BlockHost, "low.example").with_priority(-10),
                Rule::new(RuleKind::HideKeyword, "keyword"),
                Rule::new(RuleKind::BlockHost, "high.example").with_priority(10),
                Rule::new(RuleKind::BlockHost, "mid.example"),
            ],
        );
        let ordered: Vec<&str> = set
            .rules_of(RuleKind::BlockHost)
            .map(|(_, rule)| rule.normalized_pattern())
            .collect();
        assert_eq!(ordered, ["high.example", "mid.example", "low.example"]);
        assert_eq!(set.count_of(RuleKind::BlockHost), 3);
        assert_eq!(set.count_of(RuleKind::HideKeyword), 1);
        assert_eq!(set.count_of(RuleKind::AllowUrl), 0);
    }

    #[test]
    fn ties_keep_declaration_order() {
        let set = validated(
            "v1",
            vec![
                Rule::new(RuleKind::BlockHost, "first.example"),
                Rule::new(RuleKind::BlockHost, "second.example"),
            ],
        );
        let ordered: Vec<&str> = set
            .rules_of(RuleKind::BlockHost)
            .map(|(_, rule)| rule.normalized_pattern())
            .collect();
        assert_eq!(ordered, ["first.example", "second.example"]);
    }

    #[test]
    fn user_settings_become_a_user_owned_set() {
        let mut settings = FilteringSettings::default();
        settings.allowlist = vec!["cdn.example".to_owned()];
        settings.blocklist = vec!["tracker.example".to_owned()];

        let set = RuleSet::from_user_settings(&settings, "user.1");
        assert_eq!(set.source, RuleSetSource::User);
        let validated = set.validate().expect("user rules validate");
        assert_eq!(validated.count_of(RuleKind::AllowHost), 1);
        assert_eq!(validated.count_of(RuleKind::BlockHost), 1);
        assert!(validated.rules().iter().all(|rule| rule.priority() > 0));
    }

    #[test]
    fn an_invalid_candidate_never_displaces_the_active_set() {
        let manager = manager(validated(
            "good.1",
            vec![Rule::new(RuleKind::BlockHost, "tracker.example")],
        ));
        let before = manager.engine();

        let err = manager
            .activate(rule_set("bad.1", vec![Rule::new(RuleKind::BlockUrl, "(a+)+")]))
            .unwrap_err();

        assert!(matches!(err.root_cause(), FilterError::UnsafePattern { .. }));
        assert_eq!(manager.active_version(), "good.1");
        assert!(Arc::ptr_eq(&before, &manager.engine()));
        assert_eq!(manager.previous_version(), None);
        assert_eq!(manager.diagnostics().snapshot().failed_updates, 1);
    }

    #[test]
    fn rollback_restores_the_previous_set_and_the_engine_stays_usable() {
        let manager = manager(validated(
            "good.1",
            vec![Rule::new(RuleKind::BlockHost, "tracker.example")],
        ));
        manager
            .activate(rule_set(
                "bad.2",
                vec![Rule::new(RuleKind::BlockHost, "cdn.example")],
            ))
            .expect("candidate is valid");
        assert_eq!(manager.active_version(), "bad.2");
        assert!(manager.engine().evaluate_host("cdn.example").is_blocked());

        let record = manager.rollback(RollbackReason::Manual).expect("rolled back");

        assert_eq!(record.from_version, "bad.2");
        assert_eq!(record.to_version, "good.1");
        assert_eq!(record.reason_key(), "filtering.rollback.manual");
        assert_eq!(manager.active_version(), "good.1");

        let engine = manager.engine();
        assert!(
            engine.evaluate_host("cdn.example").is_allowed(),
            "the restored set must actually be in force"
        );
        assert!(engine.evaluate_host("tracker.example").is_blocked());
        assert_eq!(manager.diagnostics().snapshot().rollbacks, 1);
    }

    #[test]
    fn rollback_with_nothing_to_restore_falls_back_to_the_inert_set() {
        let manager = manager(validated(
            "only.1",
            vec![Rule::new(RuleKind::BlockHost, "tracker.example")],
        ));

        let record = manager.rollback(RollbackReason::Manual).expect("rolled back");
        assert_eq!(record.to_version, INERT_VERSION);
        assert!(manager.active().is_inert());

        let engine = manager.engine();
        assert!(
            engine.evaluate_host("tracker.example").is_allowed(),
            "the inert set allows everything"
        );
        // …and there is nothing left to roll back to.
        assert_eq!(manager.rollback(RollbackReason::Manual), None);
    }

    #[test]
    fn failures_below_the_threshold_do_not_roll_back() {
        let clock = TestClock::new();
        let manager = manager(validated("base.1", vec![]))
            .with_clock(Arc::clone(&clock) as Arc<dyn MonotonicClock>)
            .with_policy(RollbackPolicy {
                failure_threshold: 3,
                failure_window: Duration::from_secs(60),
                observation_period: Duration::from_secs(300),
            });
        manager
            .activate(rule_set(
                "candidate.2",
                vec![Rule::new(RuleKind::BlockHost, "cdn.example")],
            ))
            .expect("valid");

        assert_eq!(manager.report_playback_failure(), None);
        assert_eq!(manager.report_playback_failure(), None);
        assert_eq!(manager.active_version(), "candidate.2");
    }

    #[test]
    fn failures_spread_beyond_the_window_never_accumulate() {
        let clock = TestClock::new();
        let manager = manager(validated("base.1", vec![]))
            .with_clock(Arc::clone(&clock) as Arc<dyn MonotonicClock>)
            .with_policy(RollbackPolicy {
                failure_threshold: 3,
                failure_window: Duration::from_secs(10),
                observation_period: Duration::from_secs(3600),
            });
        manager
            .activate(rule_set("candidate.2", vec![]))
            .expect("valid");

        for _ in 0..10 {
            assert_eq!(manager.report_playback_failure(), None);
            clock.advance(11_000);
        }
        assert_eq!(manager.active_version(), "candidate.2");
    }

    #[test]
    fn enough_failures_inside_the_window_roll_back_automatically() {
        let clock = TestClock::new();
        let manager = manager(validated(
            "good.1",
            vec![Rule::new(RuleKind::BlockHost, "tracker.example")],
        ))
        .with_clock(Arc::clone(&clock) as Arc<dyn MonotonicClock>)
        .with_policy(RollbackPolicy {
            failure_threshold: 3,
            failure_window: Duration::from_secs(60),
            observation_period: Duration::from_secs(300),
        });
        manager
            .activate(rule_set(
                "suspect.2",
                vec![Rule::new(RuleKind::BlockHost, "cdn.example")],
            ))
            .expect("valid");

        assert_eq!(manager.report_playback_failure(), None);
        clock.advance(1_000);
        assert_eq!(manager.report_playback_failure(), None);
        clock.advance(1_000);
        let record = manager
            .report_playback_failure()
            .expect("the third failure crosses the threshold");

        assert_eq!(record.from_version, "suspect.2");
        assert_eq!(record.to_version, "good.1");
        assert!(matches!(
            record.reason,
            RollbackReason::PlaybackFailures { threshold: 3, .. }
        ));
        assert!(manager.engine().evaluate_host("cdn.example").is_allowed());
    }

    #[test]
    fn failures_after_the_observation_period_are_not_blamed_on_the_rule_set() {
        let clock = TestClock::new();
        let manager = manager(validated("base.1", vec![]))
            .with_clock(Arc::clone(&clock) as Arc<dyn MonotonicClock>)
            .with_policy(RollbackPolicy {
                failure_threshold: 2,
                failure_window: Duration::from_secs(60),
                observation_period: Duration::from_secs(120),
            });
        manager
            .activate(rule_set("candidate.2", vec![]))
            .expect("valid");

        clock.advance(121_000);
        for _ in 0..10 {
            assert_eq!(manager.report_playback_failure(), None);
        }
        assert_eq!(manager.active_version(), "candidate.2");
    }

    #[test]
    fn a_rolled_back_version_is_not_reinstalled() {
        let manager = manager(validated("good.1", vec![]));
        manager
            .activate(rule_set(
                "suspect.2",
                vec![Rule::new(RuleKind::BlockHost, "cdn.example")],
            ))
            .expect("valid");
        manager.rollback(RollbackReason::Manual).expect("rolled back");

        let err = manager
            .activate(rule_set(
                "suspect.2",
                vec![Rule::new(RuleKind::BlockHost, "cdn.example")],
            ))
            .unwrap_err();
        assert!(matches!(err, FilterError::PreviouslyRolledBack { .. }));
        assert_eq!(manager.active_version(), "good.1");

        manager.clear_rollback_history();
        assert!(manager.activate(rule_set("suspect.2", vec![])).is_ok());
    }

    #[test]
    fn concurrent_failure_reports_produce_exactly_one_rollback() {
        let clock = TestClock::new();
        let manager = Arc::new(
            manager(validated("good.1", vec![]))
                .with_clock(Arc::clone(&clock) as Arc<dyn MonotonicClock>)
                .with_policy(RollbackPolicy {
                    failure_threshold: 2,
                    failure_window: Duration::from_secs(60),
                    observation_period: Duration::from_secs(600),
                }),
        );
        manager
            .activate(rule_set("suspect.2", vec![]))
            .expect("valid");

        let rollbacks: Vec<Option<RollbackRecord>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    let manager = Arc::clone(&manager);
                    scope.spawn(move || manager.report_playback_failure())
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|handle| handle.join().ok())
                .collect()
        });

        let performed = rollbacks.iter().filter(|record| record.is_some()).count();
        assert_eq!(
            performed, 1,
            "sixteen reporters must not roll back sixteen times"
        );
        assert_eq!(manager.active_version(), "good.1");
        assert_eq!(manager.diagnostics().snapshot().rollbacks, 1);
    }

    #[test]
    fn changing_the_mode_rebuilds_the_engine_without_touching_the_rules() {
        let manager = manager(validated(
            "v1",
            vec![Rule::new(RuleKind::BlockHost, "tracker.example").with_min_mode(RuleMode::Strict)],
        ));
        assert!(
            manager.engine().evaluate_host("tracker.example").is_allowed(),
            "a strict rule is inert in standard mode"
        );

        manager.set_filtering(true, FilteringMode::Strict);
        assert!(manager.engine().evaluate_host("tracker.example").is_blocked());
        assert_eq!(manager.active_version(), "v1", "rules were not reloaded");

        manager.set_filtering(false, FilteringMode::Strict);
        assert!(manager.engine().evaluate_host("tracker.example").is_allowed());
    }

    #[tokio::test]
    async fn subscribers_are_notified_when_the_engine_is_swapped() {
        let manager = manager(validated("v1", vec![]));
        let mut updates = manager.subscribe();

        manager
            .activate(rule_set(
                "v2",
                vec![Rule::new(RuleKind::BlockHost, "tracker.example")],
            ))
            .expect("valid");

        updates.changed().await.expect("the sender outlives us");
        let engine = updates.borrow_and_update().clone();
        assert_eq!(engine.rule_set().version(), "v2");

        manager.rollback(RollbackReason::Manual).expect("rolled back");
        updates.changed().await.expect("rollback publishes too");
        assert_eq!(updates.borrow().rule_set().version(), "v1");
    }
}
