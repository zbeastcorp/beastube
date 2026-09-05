# Security policy

## Reporting a vulnerability

Please report privately, through GitHub's
[private vulnerability reporting](https://github.com/zbeastcorp/beastube/security/advisories/new),
rather than in a public issue.

Include what an attacker can achieve, how to reproduce it, and the version from Settings → About.
A proof of concept helps, but a clear description of the mechanism is worth more than an exploit.

You can expect an acknowledgement within a few days and an assessment within two weeks. If a fix
is warranted it ships in the next release, and the advisory credits you unless you would rather it
did not.

## What is in scope

BEASTUBE runs untrusted content from the internet inside a webview and drives two bundled
executables, so the interesting surface is fairly small and fairly sharp:

- **The updater.** It installs software without asking a second time. Anything that would let an
  update install without a valid signature, or would let a signature be forged, is the most serious
  class of bug this project has.
- **The IPC boundary.** Every command in `src-tauri/src/commands.rs` takes input that ultimately
  comes from a webview rendering third-party content. Path traversal, argument injection into the
  bundled tools, or anything that escapes the intended data directory.
- **The bundled tools.** `yt-dlp` and `ffmpeg` are launched as child processes with arguments
  derived from provider data. Anything that turns a video title or id into a command-line argument.
- **The webview boundary.** The embedded player is cross-origin by design. Anything that lets it
  reach the application's own IPC.
- **Local data.** History, playlists and bookmarks are stored unencrypted in SQLite, which is
  stated rather than defended — see below. Anything that exposes them _off_ the machine is in
  scope.

## What is not in scope

- **The local database is not encrypted.** BEASTUBE has no account and no sync; the threat model is
  that your machine is yours. Someone with access to your user profile can read your history, the
  same way they can read your browser's.
- **The bundled tools' own vulnerabilities.** Report those to
  [yt-dlp](https://github.com/yt-dlp/yt-dlp) or [FFmpeg](https://ffmpeg.org/security.html). We will
  ship an updated copy once they do.
- **YouTube's own behaviour**, including what the embedded player does inside its frame. BEASTUBE
  cannot reach into another origin, which is also why it cannot fix it.
- **Denial of service by feeding the application absurd input locally.** If you can already run
  code as the user, you do not need BEASTUBE.

## Signing keys

Releases are signed with a minisign key whose public half is compiled into the application and
whose private half exists only as a GitHub Actions secret and an offline copy. Every release must
carry a `.sig` beside the installer; the release workflow fails rather than publishing one without.

If you believe the key has been exposed, treat it as the most urgent possible report. Rotating it
means every existing installation must be updated by hand, because they will refuse anything signed
by the replacement.
