<div align="center">

<img src="src-tauri/icons/128x128@2x.png" width="96" alt="BEASTUBE">

# BEASTUBE

**A desktop YouTube client for Windows that keeps what you watch on your own machine.**

[![CI](https://github.com/zbeastcorp/beastube/actions/workflows/ci.yml/badge.svg)](https://github.com/zbeastcorp/beastube/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/zbeastcorp/beastube?sort=semver&color=ff7a59)](https://github.com/zbeastcorp/beastube/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/zbeastcorp/beastube/total?color=ff7a59)](https://github.com/zbeastcorp/beastube/releases)
[![Licence](https://img.shields.io/badge/licence-GPLv3-blue)](LICENSE)
[![Windows 10 | 11](https://img.shields.io/badge/Windows-10%20%7C%2011-0078d4?logo=windows&logoColor=white)](https://github.com/zbeastcorp/beastube/releases/latest)

No account. No sync. Nothing sent anywhere.

<br>

[![Download for Windows](https://img.shields.io/badge/⬇%20Download%20for%20Windows-ff5c5c?style=for-the-badge&logoColor=white)](https://github.com/zbeastcorp/beastube/releases/latest)

<sub>Installs per user — no administrator prompt. `yt-dlp` and `ffmpeg` are bundled, so downloads work on a machine with nothing else installed.</sub>

<br>

![Tauri](https://img.shields.io/badge/Tauri_2-24C8DB?logo=tauri&logoColor=white)
![Rust](https://img.shields.io/badge/Rust-000000?logo=rust&logoColor=white)
![React](https://img.shields.io/badge/React_19-20232a?logo=react&logoColor=61DAFB)
![TypeScript](https://img.shields.io/badge/TypeScript-3178C6?logo=typescript&logoColor=white)
![SQLite](https://img.shields.io/badge/SQLite-003B57?logo=sqlite&logoColor=white)

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

BEASTUBE is a native Windows application for watching YouTube. Playback goes through YouTube's own
embed, wrapped in the application's dark controls.

There is no account to sign into, no telemetry, and no server of ours involved. Your history,
playlists, bookmarks and watch positions live in a SQLite file on your own disk.

Built with [Tauri 2](https://tauri.app): a Rust core with a React front end, compiled to a single
native binary of about 24 MB.

## Installation

Download **`BEASTUBE_<version>_x64-setup.exe`** from the
[latest release](https://github.com/zbeastcorp/beastube/releases/latest) and run it.

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

BEASTUBE keeps itself up to date. Shortly after launch it checks, and if a newer version exists it
downloads it, verifies the signature, installs it and restarts — telling you before it does, not
after. Nothing opens: no setup window, no progress dialog of its own, no prompt. The application
closes and reopens on the new version.

Your library is not touched. History, playlists, bookmarks and watch positions live outside the
program directory, and an update replaces the program only.

It will not do that while something is playing, it will not retry a version that already failed to
install on your machine, and **Settings → About → Update automatically** turns it off, leaving the
button below it as the same update on request.

**Settings → About → Check for updates** is the one that acts. It checks, and if there is a newer
version it downloads it, verifies the signature, installs it without a wizard and restarts into it —
one press, with the progress reported on that row. There is no second confirmation, because the
press already said what you wanted; the row states the download size beforehand rather than after.
There is no uninstall-and-reinstall step.

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

**Two control bars** — Settings → Playback → Player controls.

- **YouTube's own bar**, the default. Its gear drives the embed's quality API directly: every tier
  from 144p up, applied immediately. The screenshot above shows this.
- **BEASTUBE's dark bar.** Matches the theme and crops YouTube's chrome away. 360p to 2160p, at
  60fps where the video has it. YouTube's own settings panel is white and cannot be themed, which
  is why this is a choice rather than the default.

How the quality mechanism works, and the approaches that look correct but are not, is in
[ADR-0004](docs/architecture-decisions/0004-player-quality.md).

Also here: subtitles, playback speed, resume where you left off, downloads, and a related rail that
collapses when the window is too narrow for it.

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
