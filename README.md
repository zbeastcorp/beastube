<div align="center">

<img src="src-tauri/icons/128x128@2x.png" width="96" alt="BEASTUBE">

# BEASTUBE

**A desktop YouTube client for Windows that keeps what you watch on your own machine.**

[![CI](https://github.com/BEASTUBE/beastube/actions/workflows/ci.yml/badge.svg)](https://github.com/BEASTUBE/beastube/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/BEASTUBE/beastube?sort=semver&color=ff7a59)](https://github.com/BEASTUBE/beastube/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/BEASTUBE/beastube/total?color=ff7a59)](https://github.com/BEASTUBE/beastube/releases)
[![Licence](https://img.shields.io/badge/licence-GPLv3-blue)](LICENSE)
[![Windows 10 | 11](https://img.shields.io/badge/Windows-10%20%7C%2011-0078d4?logo=windows&logoColor=white)](https://github.com/BEASTUBE/beastube/releases/latest)

No account. No sync. Nothing sent anywhere.

</div>

<div align="center">
  <img src="docs/screenshots/home.png" width="90%" alt="The BEASTUBE home feed">
</div>

---

## Contents

- [What it is](#what-it-is)
- [Installation](#installation)
  - [Release files](#release-files)
  - [Updating](#updating)
  - [Uninstalling](#uninstalling)
- [Features](#features)
  - [Watching](#watching)
  - [Shorts](#shorts)
  - [Your library](#your-library)
  - [Downloads](#downloads)
  - [Settings](#settings)
- [What is stored, and where](#what-is-stored-and-where)
- [Building from source](#building-from-source)
- [Project layout](#project-layout)
- [Contributing](#contributing)
- [Third-party software](#third-party-software)
- [Licence](#licence)

---

## What it is

BEASTUBE is a native Windows application for watching YouTube. It is not a browser wrapper around
youtube.com and it is not a scraper pretending to be one: playback goes through YouTube's own
sanctioned embed, wrapped in the application's own dark controls.

What makes it different is what it does **not** do. There is no account to sign into, no telemetry,
and no server of ours anywhere in the picture. Your history, playlists, bookmarks and watch
positions live in a SQLite file on your disk and are never uploaded.

Built with [Tauri 2](https://tauri.app) — a Rust core with a React front end, compiled to a single
native binary. It is not Electron; the installed application is around 24 MB of program.

## Installation

Download **`BEASTUBE_<version>_x64-setup.exe`** from the
[latest release](https://github.com/BEASTUBE/beastube/releases/latest) and run it.

That is the whole of it. The installer carries `yt-dlp` and `ffmpeg`, so downloads work on a machine
you have not prepared, and it installs the WebView2 runtime if Windows does not already have it
(Windows 11 always does).

**Requirements:** Windows 10 (1809 or newer) or Windows 11, 64-bit. About 250 MB of disk.

### Release files

| File                                   | Description                                                          |
| -------------------------------------- | -------------------------------------------------------------------- |
| **`BEASTUBE_<version>_x64-setup.exe`** | **The installer. This is the one you want.** ~51 MB.                 |
| `BEASTUBE_<version>_x64_en-US.msi`     | Same application as an MSI, for Group Policy and managed deployment. |
| `*.sig`                                | Signatures, used by the built-in updater. Not needed by hand.        |
| `latest.json`                          | Update manifest. Installed copies read this; you do not need it.     |

**Which one.** Take the `.exe` unless you know you want the `.msi`. The `.exe` installs per-user
without an administrator prompt and is what the in-app updater downloads. The `.msi` exists for
deploying across a fleet — `msiexec /i BEASTUBE_<version>_x64_en-US.msi /qn` — and is larger,
because MSI cannot compress as well as NSIS.

Every release is signed with the project's updater key. The public half is compiled into the
application, so a build will refuse an update it cannot verify — including one we did not sign.

### Updating

BEASTUBE updates itself, and it tells you when there is something to update to: about ten seconds
after launch it checks, and if a newer version exists a notice appears with a link to it. Nothing is
downloaded until you say so.

**Settings → About → Check for updates** does the same on demand, and is where the download reports
its progress. The installer runs without a wizard and the application restarts into the new version
— there is no uninstall-and-reinstall step.

It is a full installer each time rather than a patch, because Windows offers no delta mechanism on
this path — around 50 MB, most of which is ffmpeg. The settings row says so before you press it.

### Uninstalling

Through **Settings → Apps** in Windows, or the `uninstall.exe` beside the program. Your library is
left in place; see [What is stored, and where](#what-is-stored-and-where) if you want it gone too.

## Features

### Watching

<div align="center">
  <img src="docs/screenshots/watch.png" width="90%" alt="The watch page, with the player's own dark controls">
</div>

**Two control bars, and the choice is a real trade-off** — Settings → Playback → Player controls.

- **YouTube's own bar (the default).** Its gear drives the embed's internal quality API directly:
  every tier from 144p up, applied the instant it is picked. Quality is what people actually reach
  for, so it wins the default. The screenshot above shows this.
- **BEASTUBE's dark bar.** Matches the theme and crops YouTube's chrome away. Quality is 360p to
  2160p, at 60fps where the video has it — the embed picks its rendition from the size of its own
  viewport, so a tier is requested by laying the frame out at that tier's true pixel width and
  scaling the result back down, live and without a reload. YouTube's settings panel is white and
  cannot be themed, which is the other half of why this is a choice rather than a default.

The mechanism, and the several approaches that look correct and are not, are in
[ADR-0004](docs/architecture-decisions/0004-player-quality.md).

Also here: subtitles, playback speed, resume-where-you-left-off, downloads, and a related rail that
gets out of the way when the window is too narrow to earn it.

### Shorts

<div align="center">
  <img src="docs/screenshots/shorts.png" width="90%" alt="The Shorts feed">
</div>

A full-height vertical feed with keyboard and wheel navigation, in the portrait shape shorts are
actually made in.

### Your library

History, playlists, bookmarks and watch positions, all local. Entries can be removed one at a time
from a card's menu.

Incognito stops watches and searches being recorded. It does not stop the things you ask for
explicitly: bookmarking a video or adding it to a playlist still writes, because you pressed a
button that means "keep this".

### Downloads

Videos can be saved to disk with the bundled `yt-dlp`, joined by the bundled `ffmpeg`. Neither has
to be installed separately and neither is vendored into this repository — the installer fetches them
at build time, each verified against a vendor checksum.

### Settings

<div align="center">
  <img src="docs/screenshots/settings.png" width="90%" alt="The settings screen">
</div>

Theme (dark, light, AMOLED, or follow Windows), interface scale, density, reduced motion, maximum
quality, playback speed, seek steps, autoplay, subtitles on by default, download location, and a
hardware-acceleration switch for machines whose graphics driver renders video incorrectly.

There is a **Diagnostics** screen too: version, runtime, storage sizes and filtering counters, read
from your own machine, with a copy button. Nothing on it is transmitted anywhere.

## What is stored, and where

Two folders, and it is worth knowing which is which:

```
%APPDATA%\app.beastube.desktop\library.db     your library — a few MB
%LOCALAPPDATA%\app.beastube.desktop\          caches and logs — can reach several hundred MB
```

The SQLite file is the part that is _you_: history, playlists, bookmarks and watch positions.

The second folder is machinery. It holds the embedded browser's own profile, the metadata cache,
`yt-dlp`'s cache, and the application log. It grows — on a well-used installation it passes 500 MB,
most of that the browser profile — and it is safe to delete while BEASTUBE is closed.

To reset completely, delete both. Deleting only `library.db` clears your library and leaves the
caches, which is usually what you want but is not the same as starting fresh.

Neither folder leaves your machine. There is no account and nothing is uploaded. The one identifier
in play is the anonymous visitor token YouTube's own endpoints require; it is fetched per session,
is not a login, and does not identify you.

## Building from source

Requires [Rust](https://rustup.rs) 1.94+, [Node](https://nodejs.org) 22.12+ with
[pnpm](https://pnpm.io), [PowerShell 7](https://aka.ms/powershell) (the setup scripts are `pwsh`),
and the WebView2 runtime.

```powershell
pnpm install
pnpm tools:fetch     # downloads yt-dlp and ffmpeg, each verified against a vendor checksum
pnpm tauri dev       # run it
pnpm tauri build     # produce the installer
```

`pnpm tools:fetch` is not optional for a bundled build: the two executables it downloads are
installer resources, and downloads above roughly 360p cannot be produced without ffmpeg, because
YouTube serves no combined audio-and-video stream at those sizes.

The build lands at `target/release/bundle/nsis/BEASTUBE_<version>_x64-setup.exe` and contains only
compiled artefacts — the Rust binary with the minified front end embedded, plus the two tools. No
source is distributed.

### Checks

```powershell
pnpm typecheck       # tsc, strict, with exactOptionalPropertyTypes
pnpm lint            # eslint, zero warnings tolerated
pnpm format:check    # prettier
pnpm test            # vitest
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Project layout

| Path                           | What lives there                                                 |
| ------------------------------ | ---------------------------------------------------------------- |
| `src/`                         | React front end — views, stores, services, i18n catalogues       |
| `src-tauri/`                   | The desktop shell: window setup, IPC commands, logging           |
| `crates/beastube-core`         | Domain model and settings, shared by everything below            |
| `crates/beastube-db`           | SQLite storage and migrations                                    |
| `crates/beastube-provider*`    | Fetching from YouTube, and the traits that keep that swappable   |
| `crates/beastube-download`     | Driving `yt-dlp` as a child process                              |
| `docs/architecture-decisions/` | Why things are the way they are, with the measurements behind it |

The architecture decision records are worth reading before changing playback or downloads. Both
have non-obvious constraints that were measured rather than assumed.

## Contributing

Bug reports and pull requests are welcome. [`CONTRIBUTING.md`](CONTRIBUTING.md) covers the setup, the
checks, and the few things this codebase is strict about — chiefly that a control which does nothing
is treated as a bug, and that a claim about performance comes with the measurement behind it.

For anything security-shaped, read [`SECURITY.md`](SECURITY.md) and report it privately rather than
in an issue. Release steps and the signing key are in [`docs/RELEASING.md`](docs/RELEASING.md); what
changed in each version is in [`CHANGELOG.md`](CHANGELOG.md).

## Third-party software

[`yt-dlp`](https://github.com/yt-dlp/yt-dlp) (Unlicense) and [`ffmpeg`](https://ffmpeg.org)
(GPL v3) are bundled with the installer and run as separate processes. Neither is vendored into this
repository. See [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).

BEASTUBE is not affiliated with, endorsed by, or sponsored by YouTube or Google.

## Licence

GNU General Public License v3.0 or later. The full text is in [`LICENSE`](LICENSE).

GPL is also the licence the bundled ffmpeg is under, so the two agree rather than pulling against
each other. See [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) for what that means in practice
and where the corresponding source lives.
