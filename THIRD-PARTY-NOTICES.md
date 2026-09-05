# Third-party notices

BEASTUBE ships two programs it did not write. They are complete, unmodified executables that the
application starts as separate processes; nothing here is linked into BEASTUBE's own binary. Both
are placed beside the application by the installer, which is what lets a download work on a machine
the user has not prepared.

Their licence texts are installed alongside them, under `binaries/licenses/`.

## ffmpeg — GNU General Public License v3

- **Used for:** joining the separate video and audio streams YouTube serves into one playable file.
  Without it, downloads above roughly 360p cannot be produced at all, because no combined stream is
  offered (see `docs/architecture-decisions/0004-player-quality.md`).
- **Build:** the "release essentials" build published by Gyan Doshi at <https://www.gyan.dev/ffmpeg/builds/>.
- **Licence:** GPL v3. Full text: `binaries/licenses/ffmpeg-LICENSE.txt`, installed with the
  application.
- **Source code:** ffmpeg's corresponding source is published by the FFmpeg project at
  <https://ffmpeg.org/download.html>, and the build configuration is documented at
  <https://www.gyan.dev/ffmpeg/builds/>.

  **Which version you have, and why that matters.** `scripts/fetch-tools.ps1` downloads the
  publisher's current release build at the time the installer was made, so different installers
  carry different ffmpeg versions and this file cannot name one. GPL v3 §6 requires the offer of
  source to correspond to _the binary you received_ — so the version is the thing you need, and it
  is on your own machine rather than in this document: run

  ```powershell
  & "$env:LOCALAPPDATA\Programs\BEASTUBE\binaries\ffmpeg.exe" -version
  ```

  The first line names the version and the full `--enable`/`--disable` configuration that build was
  made with, which is what to ask the FFmpeg project or the build publisher for. If you cannot
  obtain it from either, open an issue and we will provide the corresponding source for your
  build.

### Why this does not make BEASTUBE GPL

BEASTUBE runs `ffmpeg.exe` as a child process over its documented command-line interface. It does
not link against ffmpeg's libraries, include its headers, or embed any part of it. Communication is
by process arguments and standard output. That is aggregation rather than derivation, and it is the
boundary the FSF itself describes as separate programs. The GPL obligations that do apply — shipping
the licence text and identifying where the corresponding source can be obtained — are met above.

## yt-dlp — The Unlicense

- **Used for:** resolving and fetching the media streams for a download.
- **Source:** <https://github.com/yt-dlp/yt-dlp>
- **Licence:** The Unlicense (public domain dedication). Full text:
  `binaries/licenses/yt-dlp-LICENSE.txt`, installed with the application.

## Neither is vendored into this repository

`scripts/fetch-tools.ps1` downloads both at build time and verifies each against a checksum the
vendor publishes separately from the artifact; a mismatch fails the build. `src-tauri/binaries/` is
git-ignored, so the repository carries no redistributed binaries and no licence obligations of its
own from them.

## YouTube

BEASTUBE plays video through YouTube's sanctioned IFrame Player embed. It is not affiliated with,
endorsed by, or sponsored by YouTube or Google. "YouTube" is a trademark of Google LLC.
