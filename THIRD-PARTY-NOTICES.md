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
  <https://ffmpeg.org/download.html>, and the exact build configuration used by this binary is
  documented at <https://www.gyan.dev/ffmpeg/builds/>. Running `ffmpeg -version` prints the
  configuration and version of the copy actually installed.

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
