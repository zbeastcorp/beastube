# ADR 0001 — Playback architecture

**Status:** Accepted (2026-09-03)
**Supersedes:** none

## Problem

BEASTUBE must play YouTube video in a Tauri 2 desktop shell on Windows. The specification asks
for a capability-driven quality ladder, buffer control, frame metrics and an ad-free experience —
all of which presuppose that the application controls the media pipeline.

Research (`docs/research/youtube-platform.md`, `docs/research/rustypipe.md`) established four facts
that make that presupposition unsafe as a sole foundation:

1. **SABR has landed.** YouTube's `web` client no longer returns playable `adaptiveFormats` URLs;
   it returns only a SABR endpoint. SABR is framed as UMP (`application/vnd.yt-ump`), a multiplexed
   protobuf stream requested by POST — it cannot be handed to a `<video>` element, to a Range-based
   fetcher, or to ffmpeg. yt-dlp's SABR downloader (PR #13515) was still unmerged as of Sept 2026.
2. **The clients that still emit plain URLs are a moving target.** yt-dlp's default client list
   changed four times during 2026 (`tv,android_sdkless,web` → … → `visionos,web`). Hardcoding one
   client is a guaranteed future breakage.
3. **The obvious Rust dependency is stale.** `rustypipe` 0.11.4 on crates.io has had no functional
   code commit since 2025-06-18, its deobfuscator is reported broken (issue #75), and `trending()`
   calls a browse ID YouTube retired in July 2025.
4. **Terms are explicit.** The YouTube API Services Developer Policies prohibit, verbatim, blocking
   advertisements (III.I.5), blocking player functionality (III.I.6), and using "any technology
   other than YouTube API Services to access or retrieve API Data, including any YouTube
   audiovisual content" (III.I.14).

## Alternatives considered

**A. IFrame Player API only.** The sanctioned embed. Google's player handles SABR, so the SABR
risk disappears entirely. Costs: YouTube's own advertising plays; `setPlaybackQuality`,
`getPlaybackQuality` and `getAvailableQualityLevels` are documented no-ops since 2025, so there can
be no quality selector; no dropped-frame or buffer telemetry; playback state is limited to what
`onStateChange` reports.

**B. Direct stream extraction only.** Rust resolves stream URLs and proxies them to
`shaka-player`/MSE through a loopback gateway. Delivers the full specification, but is what
III.I.14 prohibits, and in practice requires a vendored fork pinned to a git revision plus a
bundled PoToken sidecar — machinery whose purpose is to defeat an anti-abuse control.

**C. Both, behind one interface, IFrame as the default.**

## Decision

**Alternative C.** A single `PlaybackProvider` abstraction with two adapters:

- **`IframeAdapter` — the production default.** Ships enabled. Uses the official IFrame Player API.
- **`DirectStreamAdapter` — isolated and experimental.** Off by default, compiled behind a
  non-default Cargo feature and a runtime setting. Exists so the architecture is proven replaceable
  (§118) and so the media pipeline can be developed against.

Three constraints bind the experimental adapter, and are treated as architectural invariants rather
than preferences:

1. **No circumvention machinery.** It implements no PoToken/BotGuard minting, no `nsig`/signature
   deobfuscation, no JS-challenge solving, and nothing touching DRM or access controls. It consumes
   stream URLs only where they are served plainly, and reports a capability failure otherwise.

   This rule was originally stated as "no JS engine appears anywhere in the dependency graph — its
   absence is the enforcement mechanism". That is not true and has not been for some time:
   `cargo tree -p beastube-provider-youtube` shows `rquickjs v0.9.0`, pulled in by `rustypipe`, and
   `librquickjs-*.rlib` is built into both profiles. A rule whose stated enforcement can be
   disproved by one command is worse than no rule, because the next person either believes it or
   quietly works around it.

   The invariant that actually holds is the one ADR 0003 states: **no extraction in code we write.**
   Circumvention is not implemented here, and where a provider library carries an engine of its own
   that is a fact to know about rather than a rule being kept.

2. **No provider leakage.** Neither adapter's types cross into `beastube-core` or the UI. The UI
   knows `PlaybackCapabilities`, `PlaybackState` and player commands; it cannot discover which
   adapter is active except through capability flags.
3. **Capability-gated UI.** Controls render only where the active adapter reports support, so the
   quality menu is absent under `IframeAdapter` rather than present and inert (§131).

## Consequences

**Accepted losses under the default adapter.** No quality selector, no buffer/frame metrics, no
custom seek engine, YouTube's advertising plays. `PlaybackCapabilities` reports each of these as
unsupported, and the UI omits the control — the specification's "no fake features" rule (§131)
converts a missing capability into a missing control rather than a broken one.

**Filtering is scoped to capability.** The content-filtering subsystem (§5–§10) is built in full —
rule engine, modes, allowlist/blocklist, versioning, validation, rollback, diagnostics — but its
adapters act only within what the active playback architecture permits. Under `IframeAdapter` that
means SponsorBlock creator-marked segment skipping (user-controlled, via the k-anonymity endpoint
so the server never learns which video is being watched), channel/keyword feed filtering, and
third-party tracker blocking. It does not include suppression of YouTube's advertising.

**The 153 risk — verified, and resolved.** IFrame error 153 ("missing HTTP `Referer` header or API
client identification", added July 2025) breaks Tauri apps on platforms whose webview uses the
`tauri://` scheme origin. This ADR recorded it as the single highest-risk assumption and scheduled
it for empirical verification.

**It does not reproduce on Windows.** Verified in the packaged shell: WebView2 serves the frontend
from an `http://tauri.localhost` origin, which is a real HTTP origin and therefore emits a
compliant `Referer`. With `playerVars.origin` set to `window.location.origin`, the embed accepts
the request and plays. Confirmed by driving the running desktop window: a search result opened and
the playhead advanced past eleven seconds with no error event.

The contingency — a dedicated webview window served from a genuine `https://` origin — is
therefore not needed on Windows. It remains the fallback should a future WebView2 change alter the
origin form, and is the first thing to reach for if error 153 ever appears in the field; the player
maps that code to its own message key precisely so it names itself rather than showing a blank
frame.

**Metadata is a separate decision.** This ADR governs playback only. Search, channel and playlist
metadata are addressed by ADR 0002; the `PlaybackProvider` split means a metadata backend can be
replaced without touching playback, and vice versa.

**Reversibility.** Should SABR-free plain URLs disappear entirely, `DirectStreamAdapter` becomes
non-viable and is deleted; the default path is unaffected. Should a sanctioned API later expose
adaptive playback, it becomes a third adapter behind the same trait.
