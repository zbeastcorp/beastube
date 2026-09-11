# Contributing to BEASTUBE

Thanks for looking.

## Getting it running

Requires [Rust](https://rustup.rs), [Node](https://nodejs.org) 22.12 or newer with
[pnpm](https://pnpm.io), and the WebView2 runtime (already present on Windows 11).

```powershell
pnpm install
pnpm tools:fetch     # downloads yt-dlp and ffmpeg, each verified against a vendor checksum
pnpm tauri dev
```

`pnpm dev:app` does the same thing and stops the previous run first. `tauri dev` binds a fixed
port and fails outright if it is still held, which it routinely is after an earlier run was killed
without its child going with it.

`pnpm tools:fetch` is not optional for a bundled build. The two executables it downloads are
installer resources, and downloads above roughly 360p cannot be produced without ffmpeg, because
YouTube serves no combined audio-and-video stream at those sizes.

Windows only. The shell links against WebView2 and the download subsystem uses Windows process
APIs; there is no cross-platform build to break.

## Before you open a pull request

Everything CI runs, you can run:

```powershell
pnpm typecheck        # tsc, strict, with exactOptionalPropertyTypes
pnpm lint             # eslint, zero warnings tolerated
pnpm format:check     # prettier
pnpm test             # vitest
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Zero warnings is the standard, not an aspiration. `--max-warnings 0` and `-D warnings` are in CI
because a project that tolerates one warning has, within a year, four hundred.

## What this codebase asks of a change

**Measure, then write it down.** Most of the non-obvious code here exists because something was
measured and the obvious approach did not work. If you change one of those, measure again and
update the comment. If you add one, say what you measured. "This is faster" is not a claim this
codebase accepts without a number.

**Comments explain why, not what.** The code says what. A comment earns its place by recording the
reason a reader would otherwise have to rediscover — the failure that motivated a guard, the
platform behaviour that forced an odd shape, the thing that was tried first and did not work.

**No control that does not work.** A button, a menu row, or a settings toggle that is present and
does nothing is treated as a bug of the same severity as a crash. If the feature is not ready, the
control does not ship. This has bitten this project more than once: translated strings for a
"Check for updates" button existed for a long time with no updater behind them, and a
`hardware_acceleration` setting sat in three languages with nothing reading it.

**Nothing engineer-facing on screen.** Error codes, stack traces and native error strings go to the
log. What a viewer sees is a sentence they can act on, from the localisation catalogue.

**All user-facing text goes through the catalogue.** `src/i18n/locales/en.ts` is the source of
truth and its shape is the type; a missing key is a compile error rather than a blank label found
by a user. Translations may lag — lookup falls back to English — so adding an English string alone
is fine.

## Architecture decisions

`docs/architecture-decisions/` holds the reasoning behind playback, downloads, and the player's
quality mechanism, including the measurements. Read the relevant one before changing that area.
The quality record in particular documents several approaches that look correct and are not.

## Commit messages

Explain the change and why it was needed. If a number motivated it, put the number in. The history
is the main record of why this project is shaped the way it is, and it is written to be read.

## Reporting a bug

Open an issue with the version from Settings → About, what you did, what happened, and what you
expected. If it involves playback, the diagnostics screen has a "Copy report" button — everything
in it is read from your own machine and nothing is sent anywhere until you paste it.

## Security

Do not open a public issue for a security problem. See [SECURITY.md](SECURITY.md).

## Licence

By contributing you agree that your contributions are licensed under the GPL v3.0 or later, the
same licence as the project.
