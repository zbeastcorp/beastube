# ADR 0003 — Downloading videos

**Status:** Accepted (2026-09-04)
**Relates to:** ADR 0001 (playback architecture)

## Problem

A desktop YouTube client is expected to save a video as a file. BEASTUBE could not, and its
absence was recorded as deliberate: commit `62a9fac` shipped an "open in browser" control with the
rationale that a download button would mean building circumvention machinery.

That rationale conflated two different things, and the conflation is what this ADR corrects.

Obtaining a stream URL from YouTube requires solving the signature cipher, the `n`-parameter
transform and, increasingly, a BotGuard attestation. ADR 0001 rules that BEASTUBE will not
implement any of it. That rule used to be stated as a structural one — no JavaScript engine may
appear in the dependency graph — but the graph has carried `rquickjs` by way of `rustypipe` for
some time, so the structure was never actually enforcing it. The rule that holds is the plain one:
none of it is implemented in code we write.

What ADR 0001 does _not_ say is that the user may not run software which does solve it. `yt-dlp` is
that software: widely distributed, actively maintained against exactly these changes, and already
installed on a large fraction of the machines this application runs on.

## Alternatives considered

**A. No download at all.** The status quo. Honest about what the application implements, and
wrong about what the user can do — the tool is a `winget install` away, and refusing to call it
does not protect anything. It also leaves "open in browser" standing in for a feature it does not
provide: YouTube's own offline download is a Premium feature, so for most users that button leads
to a page where the answer is no.

**B. Implement extraction in-process.** Requires a JavaScript engine to run the player's
challenges, or a vendored fork plus a PoToken sidecar. This is precisely what ADR 0001 forbids,
and forbids structurally rather than as a preference. Rejected without further analysis.

**C. Bundle `yt-dlp` and `ffmpeg` in the installer.** Removes the "is it installed?" question
entirely, at the cost of owning their update cadence — `yt-dlp` ships roughly monthly and a stale
copy fails in ways that look like our bug — and of carrying their licences in our installer.

**D. Drive an external `yt-dlp` the user installs.**

## Decision

**Alternative C, driving the mechanism built for D.** A `beastube-download` crate finds a `yt-dlp`
executable, runs it as a child process, parses its progress output, and reports typed state. The
installer ships `yt-dlp` and `ffmpeg` so that finding them succeeds on a machine nobody prepared.

This started as D and was changed after building it: requiring the user to install two command-line
programs before a Download button appears is a working feature that most people would never reach.
The discovery mechanism is unchanged and still does the work — a bundled copy is simply the first
thing it finds, and a user who keeps their own can still point at it.

Four constraints bind it:

1. **No extraction, ever, in our code.** This crate contains no cipher, no `n`-parameter solver, no
   token minting and no HTTP call to YouTube. It builds an argument list and reads stdout. ADR
   The download crate's entire dependency set is `tokio`, `serde`, `thiserror`, `tracing`, `uuid`
   and `parking_lot` — no HTTP client, no parser, no engine. (The workspace as a whole _does_
   contain `rquickjs`, via `rustypipe`; see ADR 0001, where that claim has been corrected.)

2. **The application itself fetches nothing at runtime.** Tools are fetched at _build_ time by
   `scripts/fetch-tools.ps1`, from pinned upstream releases, each verified against a checksum the
   vendor publishes separately from the artifact; a mismatch fails the build. The running
   application never downloads a program, never updates one, and never reads cookies from a
   browser. It looks in its own resource directory, then beside its executable, then on `PATH`.

3. **The control is absent when the tool is.** `get_download_tools` reports availability and the
   download button renders only when both tools exist — §131 applied literally, the same rule that
   removes the quality menu under the embedded player. With the bundle this should never happen;
   it remains the behaviour for a broken installation or a bad path in settings, and it takes
   effect without a restart.

4. **The page never reaches the command line.** Arguments are built from a validated `VideoId` and
   paths the application resolved. `--ignore-config` is passed so a user's own `yt-dlp`
   configuration cannot change the output template or progress format the parser depends on.

## Consequences

**A bigger installer, and an update cadence to own.** The two executables add roughly 120 MB.
`yt-dlp` in particular goes stale: YouTube changes, a release follows within weeks, and a bundled
copy from six months ago fails in ways that read as our bug. The pinned version in
`scripts/fetch-tools.ps1` is therefore a thing to bump deliberately and often, and the diagnostics
screen shows the version in use so a stale copy can be identified rather than guessed at.

**The licences ship with them.** `yt-dlp` is Unlicense and the `ffmpeg` essentials build is GPL,
both compatible with this application's GPL-3.0-or-later. The fetch script copies each licence into
`binaries/licenses/` so the installer carries them.

**`ffmpeg` is required, not a quality option.** This was checked rather than assumed, and the
assumption was wrong. Listing formats for two videos against live YouTube on 2026-09-04 returned
**no combined audio-and-video format at all** — every entry was `video only` or `audio only`, from
144p to 1080p. A selector that falls back to a single-file format therefore fails outright:

```
$ yt-dlp -f "b[height<=360][ext=mp4]/b[height<=360]/b" https://www.youtube.com/watch?v=jNQXAC9IVRw
ERROR: Requested format is not available
```

whereas the paired selector resolves cleanly:

```
$ yt-dlp -f "bv*[height<=1080][ext=mp4]+ba[ext=m4a]/..." --print "%(format_id)s %(resolution)s"
399+140  1920x1080
```

So `DownloadPlan` takes a non-optional `ffmpeg`, `download_plan` refuses with `MuxerMissing`
before anything starts, and `available` requires both tools — which means the button is absent on a
machine with `yt-dlp` alone, rather than present and failing with "requested format is not
available". The earlier wording, that a missing muxer merely capped quality near 720p, described a
YouTube that no longer exists.

**Failures are classified, not pasted.** `yt-dlp` reports failure as English prose on stderr. That
prose is kept as the diagnostic; the user sees a message key chosen by matching against known
phrases, so a bot check reads as "YouTube refused, try again shortly" and a full disk as a full
disk. An unmatched phrase degrades to a generic failure with the text preserved — never to a wrong
classification.

**The tool's output format is a coupling.** Progress is parsed from `--progress-template` output
using tags this application chooses, which is far more stable than scraping the default progress
line, but it is still a contract with a program we do not control. A format change surfaces as
downloads that complete with no intermediate progress, not as a failure.

**Downloads are session state, not library state.** The manager holds them in memory and the list
is empty after a restart. The files persist; the record of having downloaded them does not. This
avoids a database migration for something whose durable artifact is the file itself, and it means
a download history is not a second watch history to keep private.

**Toasts became real.** The UI store had carried a toast queue, its durations and its optional
action since the beginning with nothing rendering it — the offline notice and the filter-rollback
warning were both being raised into a void. A download that finishes while the user is on another
screen has to say so, so `ToastHost` was written; the two pre-existing notices work now as a
side effect.

**Reversibility.** The subsystem is one crate, one command module and one store. If driving an
external tool ever becomes untenable, deleting it returns the application to exactly the state ADR
0001 describes, with no other subsystem affected.
