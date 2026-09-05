# Changelog

Notable changes, newest first. Dates are the day the release was tagged.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Before 1.0 the minor number carries
breaking changes.

## [Unreleased]

### Added

- **A working quality selector, 360p to 2160p60.** The embed picks its rendition from the size of
  its own viewport, so the frame is laid out at the tier's real pixel width — 3840 for 2160p — and
  scaled back down to the box the design wants. Frame rate is fixed when a video loads rather than
  following the frame, so the player boots into the 60fps family and every later tier inherits it.
  See `docs/architecture-decisions/0004-player-quality.md` for the approaches that look correct and
  are not.
- **In-place updates.** The application checks a release feed, verifies the next installer's
  signature against the key compiled into it, and installs without a wizard.
- **A reason when a video will not play.** The embed reports refusals as numbers that cover several
  unrelated situations, so the failure path asks `yt-dlp` — already bundled — and shows what it
  says, rather than guessing at a cause.
- **Real subtitles and per-item history deletion.**
- **Channel navigation** by clicking a channel name anywhere it appears.
- **A hardware acceleration setting** that does something: turning it off makes the next launch
  render in software, which is the remedy for a driver that paints a black rectangle.

### Fixed

- **The interface at small window sizes.** The codebase had two responsive utilities and no width
  media query, so the sidebar was 240px at every size. Below 1312px it is now a 72px rail, with the
  expanded form arriving over the content; 800px went from one column of cards to two, and at 480px
  the cards no longer run off the right edge. Nothing above 1312px changed.
- **The watch page between 1024 and about 1150 pixels**, where the two-column layout gave the video
  310px next to a 402px rail of recommendations, and cropped it. The split is now decided by the
  box the columns are in rather than by the window.
- **YouTube's own chrome reappearing when the quality changed in fullscreen.** The crop that hides
  the embed's title band and "More videos" strip was cut in screen pixels, while what it hides is
  drawn in the embed's pixels and scaled — so at 360p the bands were three times taller than the
  crop. It now tracks the scale.
- **The cursor flickering on every card preview.** `pointer-events: none` stops a cross-origin
  embed taking the click but not the cursor, because the browser asks whichever frame is under the
  mouse and that question never reaches the parent.
- **A damaged library taking the application with it.** A database that fails to open or fails its
  integrity check is now set aside and replaced, instead of leaving a fully drawn window in which
  every command answered with a Tauri internal error.
- **A machine with no WebView2 runtime**, which used to produce no window, no dialog and nothing in
  the log — the process simply exited.
- **A player that retried a failed construction every two seconds for ever**, relaunching `yt-dlp`
  each time for an answer that could not change.
- **An `ffmpeg` left running after a download was cancelled**, still holding the partial files.
- **`--ignore-gpu-blocklist`**, which forced GPU rasterisation on exactly the drivers Chromium
  blocklists for crashing.
- **"Match system" never resolving to light**, because the window's theme was pinned in
  configuration and the check read back the value the application had itself pinned.
- **A window taller than the screen it opens on.** 1280×800 centred on a 1366×768 laptop put the
  title bar above the top of the display.
- **Seven keyboard, screen reader and touch defects**: dialogs now trap Tab and restore focus,
  navigation moves focus to the content landmark, `<html lang>` follows the interface language, the
  player's control bar stays visible while focused, and the card menu is reachable without a hover.
- **High Contrast painting every link yellow.**

### Changed

- The feed grid, the card menu and the loading skeletons were rebuilt around what was measured
  rather than assumed; the card glow is promoted to its own compositor layer only while hovered,
  which took a home feed from 120 composited layers and 101 MB of texture to 12 and 46 MB.

### Removed

- Two workspace crates that contained a single doc comment each and no code, along with the HTTP
  framework they pulled into the build, six unused npm packages, three Tauri plugins that were
  compiled in and never registered, and eleven exported functions with no caller anywhere.

[Unreleased]: https://github.com/BEASTUBE/beastube/commits/main
