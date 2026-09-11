<div align="center">

<img src="src-tauri/icons/128x128@2x.png" width="96" alt="BEASTUBE">

# BEASTUBE

**A native YouTube client for Windows.**

Watch, search, save and download — from one desktop app, with your library on your own disk.

[![CI](https://github.com/beastops/beastube/actions/workflows/ci.yml/badge.svg)](https://github.com/beastops/beastube/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/beastops/beastube?sort=semver&color=ff7a59)](https://github.com/beastops/beastube/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/beastops/beastube/total?color=ff7a59)](https://github.com/beastops/beastube/releases)
[![Licence](https://img.shields.io/badge/licence-GPLv3-blue)](LICENSE)
[![Windows 10 | 11](https://img.shields.io/badge/Windows-10%20%7C%2011-0078d4?logo=windows&logoColor=white)](https://github.com/beastops/beastube/releases/latest)

<br>

[![Download for Windows](https://img.shields.io/badge/⬇%20Download%20for%20Windows-ff5c5c?style=for-the-badge&logoColor=white)](https://github.com/beastops/beastube/releases/latest)

<sub>Installs per user — no administrator prompt. `yt-dlp` and `ffmpeg` are bundled, so downloads work straight away.</sub>

<br>

<img src="docs/screenshots/home.png" width="90%" alt="The BEASTUBE home feed">

</div>

---

Playback goes through YouTube's own embed, wrapped in BEASTUBE's dark controls. There's no account
to sign into and no telemetry — your history, playlists, bookmarks and watch positions live in a
SQLite file on your disk.

Built with Tauri 2 · Rust · React · TypeScript · SQLite, compiled to a single native binary of
about 24 MB.

## Features

**Watching.** Two control bars, switchable in Settings → Playback. YouTube's own drives the embed's
quality API directly — every tier from 144p up, applied immediately. BEASTUBE's matches the theme
and reaches 360p to 2160p at 60fps. Subtitles can be moved, resized and given a background;
dubbed audio tracks are selectable where a video has them.

<img src="docs/screenshots/watch.png" width="100%" alt="The watch page">

**Explore and search.** Seven category hubs — Music, Gaming, Live, News, Sport, Learning, Fashion &
Beauty — reading YouTube's own pages. The home feed is built from the same hubs, newest first.

**Shorts.** A full-height vertical feed with keyboard and wheel navigation.

<img src="docs/screenshots/shorts.png" width="100%" alt="The Shorts feed">

**Library.** History, playlists, bookmarks and watch positions, all local. Incognito stops watches
and searches being recorded, but still honours what you ask for explicitly — bookmarking a video
writes, because you pressed a button that means keep this.

**Downloads.** Save videos with the bundled `yt-dlp` and `ffmpeg`. Neither needs installing
separately.

**Settings.** Theme (dark, light, AMOLED, or follow Windows), interface scale, density, reduced
motion, maximum quality, playback speed, seek steps, autoplay, subtitles, download location, and a
hardware-acceleration switch for drivers that render video incorrectly.

<img src="docs/screenshots/settings.png" width="100%" alt="The settings screen">

## Download

Get **`BEASTUBE_<version>_x64-setup.exe`** from the
[latest release](https://github.com/beastops/beastube/releases/latest) and run it. It installs the
WebView2 runtime if Windows doesn't already have it (Windows 11 always does).

**Requires** Windows 10 (1809 or newer) or Windows 11, 64-bit, and about 250 MB of disk.

| File                                   | Description                                                           |
| -------------------------------------- | --------------------------------------------------------------------- |
| **`BEASTUBE_<version>_x64-setup.exe`** | **The installer — take this one.** ~51 MB, per-user, no admin prompt. |
| `BEASTUBE_<version>_x64_en-US.msi`     | Same app as an MSI, for Group Policy and managed deployment.          |
| `*.sig`, `latest.json`                 | Used by the built-in updater. Not needed by hand.                     |

**Updates** install themselves. BEASTUBE checks shortly after launch, and a newer version is
downloaded, verified against the signing key compiled into the app, installed and restarted into —
no setup window, no prompt. It won't do that mid-playback, and **Settings → About → Update
automatically** turns it off. Your library is untouched; an update replaces the program only.

To uninstall, use Settings → Apps in Windows. Your library is left in place.

## Privacy

- No account, and nothing to sign into.
- No telemetry and no server of ours. BEASTUBE talks to YouTube and its media and image hosts, and
  to GitHub to check for updates.
- Your library lives in a SQLite file on your disk and is never uploaded.

Two folders hold everything:

```
%APPDATA%\app.beastube.desktop\library.db     your library — a few MB
%LOCALAPPDATA%\app.beastube.desktop\          caches and logs — can reach several hundred MB
```

The first is the part that is _you_. The second is machinery — the embedded browser's profile, the
metadata cache, `yt-dlp`'s cache and the log — and is safe to delete while BEASTUBE is closed.
Delete both to reset completely.

The one identifier in play is the anonymous visitor token YouTube's own endpoints require. It's
fetched per session, is not a login, and doesn't identify you.

## Build from source

Requires [Rust](https://rustup.rs) 1.94+, [Node](https://nodejs.org) 22.12+ with
[pnpm](https://pnpm.io), [PowerShell 7](https://aka.ms/powershell), and the WebView2 runtime.

```powershell
pnpm install
pnpm tools:fetch     # downloads yt-dlp and ffmpeg, each verified against a vendor checksum
pnpm tauri dev       # run it
pnpm tauri build     # produce the installer
```

`pnpm tools:fetch` isn't optional for a bundled build: both executables are installer resources, and
downloads above roughly 360p need ffmpeg, because YouTube serves no combined audio-and-video stream
at those sizes.

Checks, all of which CI runs:

```powershell
pnpm typecheck && pnpm lint && pnpm format:check && pnpm test
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Project structure

| Path                           | What lives there                                               |
| ------------------------------ | -------------------------------------------------------------- |
| `src/`                         | React front end — views, stores, services, i18n catalogues     |
| `src-tauri/`                   | Desktop shell: window setup, IPC commands, logging             |
| `crates/beastube-core`         | Domain model and settings                                      |
| `crates/beastube-db`           | SQLite storage and migrations                                  |
| `crates/beastube-provider*`    | Fetching from YouTube, and the traits that keep that swappable |
| `crates/beastube-download`     | Driving `yt-dlp` as a child process                            |
| `docs/architecture-decisions/` | Why playback and downloads work the way they do                |

The architecture decision records are worth reading before changing playback or downloads — both
have constraints that were measured rather than assumed.

## Contributing

Bug reports and pull requests are welcome. [`CONTRIBUTING.md`](CONTRIBUTING.md) has the setup, the
checks, and the few things this codebase is strict about. Release steps are in
[`docs/RELEASING.md`](docs/RELEASING.md); what changed in each version is in
[`CHANGELOG.md`](CHANGELOG.md).

## Security

Report anything security-shaped privately rather than in an issue — see
[`SECURITY.md`](SECURITY.md). Every release is signed with the project's updater key, and a build
refuses an update it can't verify.

## Third-party software

[`yt-dlp`](https://github.com/yt-dlp/yt-dlp) (Unlicense) and [`ffmpeg`](https://ffmpeg.org) (GPL v3)
ship with the installer and run as separate processes. Neither is vendored into this repository. See
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).

BEASTUBE is not affiliated with, endorsed by, or sponsored by YouTube or Google.

## Licence

GNU General Public License v3.0 or later — see [`LICENSE`](LICENSE). That's also ffmpeg's licence,
so the two agree; [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) covers what that means in
practice and where the corresponding source lives.
