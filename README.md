# BEASTUBE

A desktop YouTube client for Windows. What you watch stays on your machine: history, playlists and
bookmarks live in a local SQLite database, there is no account, and nothing is synced anywhere.

Built with Tauri 2 — a Rust core with a React front end, compiled into a single native binary.

## What it does

- **Watch.** Playback goes through YouTube's sanctioned IFrame embed, wrapped in the application's
  own dark controls, with a working quality selector from 360p to 2160p60.
- **Save.** Videos can be downloaded to disk. `yt-dlp` and `ffmpeg` ship inside the installer, so
  this works on a machine with nothing else installed.
- **Keep.** History, playlists, bookmarks and watch positions are stored locally and never leave
  the device.

## Building

Requires [Rust](https://rustup.rs), [Node](https://nodejs.org) with
[pnpm](https://pnpm.io), and the WebView2 runtime (present on Windows 11).

```powershell
pnpm install
pnpm tools:fetch     # downloads yt-dlp and ffmpeg, each verified against a vendor checksum
pnpm tauri dev       # run against a dev server
pnpm tauri build     # produce the installer
```

`pnpm tools:fetch` is not optional for a release build: the two executables it downloads are bundled
as installer resources, and downloads above roughly 360p cannot be produced without ffmpeg because
YouTube serves no combined stream at those sizes.

The build lands at `target/release/bundle/nsis/BEASTUBE_<version>_x64-setup.exe`. It contains only
compiled artifacts — the Rust binary with the minified front end embedded, plus the two bundled
tools. No source is distributed.

## Checks

```powershell
pnpm lint            # eslint, zero warnings tolerated
pnpm typecheck       # tsc, strict with exactOptionalPropertyTypes
pnpm test            # vitest
cargo test --workspace
cargo clippy --workspace --all-targets
```

## Layout

| Path                           | What lives there                                                 |
| ------------------------------ | ---------------------------------------------------------------- |
| `src/`                         | React front end — views, stores, services, i18n catalogues       |
| `src-tauri/`                   | The desktop shell: window setup, IPC commands, logging           |
| `crates/beastube-core`         | Domain model and settings, shared by everything below            |
| `crates/beastube-db`           | SQLite storage and migrations                                    |
| `crates/beastube-provider*`    | Fetching from YouTube, and the traits that keep that swappable   |
| `crates/beastube-download`     | Driving `yt-dlp` as a child process                              |
| `docs/architecture-decisions/` | Why things are the way they are, with the measurements behind it |

The architecture decision records are worth reading before changing playback or downloads; both
have non-obvious constraints that were measured rather than assumed.

## Releasing an update

Updates are delivered in place: the application checks a release feed, downloads the next installer,
verifies its signature and installs it without a wizard. See
[`docs/RELEASING.md`](docs/RELEASING.md) for the signing key and the release steps.

## Third-party software

`ffmpeg` (GPL v3) and `yt-dlp` (Unlicense) are bundled with the installer and run as separate
processes. Neither is vendored into this repository. See
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).

BEASTUBE is not affiliated with, endorsed by, or sponsored by YouTube or Google.

## Licence

GNU General Public License v3.0 or later — the licence this project already declares in
`Cargo.toml` and `package.json`. The full text is in [`LICENSE`](LICENSE).

GPL is also the licence the bundled ffmpeg is under, so the two agree rather than pulling against
each other. See [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) for what that means in practice
and where the corresponding source lives.
