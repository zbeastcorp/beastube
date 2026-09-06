# Changelog

Notable changes, newest first. Dates are the day the release was tagged.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Before 1.0 the minor number carries
breaking changes.

## [0.1.5] — 2026-09-07

### Added

- **Pick the language a video is dubbed into.** Videos are increasingly published with the original
  audio plus dubs, and the player offered no way to reach them. The audio row appears in the player
  menu only when a video actually has a choice — measured against the live service, a MrBeast upload
  carries 24 audio tracks and 17 subtitle tracks, while most videos carry one track and get no row
  at all rather than a menu holding a single entry. The original is listed first and marked as such,
  so a dub is distinguishable from the performance.

## [0.1.4] — 2026-09-07

### Fixed

- **Subtitles work.** The caption control was asking the wrong thing of the wrong component. Whether
  a video has captions was read from the embedded player, which only names its caption module once
  that module is loaded — and it is not loaded until captions are switched on. So every video with
  captions off reported having none, the button was hidden, and there was no way to switch them on.
  The provider reads the track list from the video's own player response instead and simply knows:
  measured against the live service, six tracks for a long-form video and one auto-generated track
  for each short tested. The embed can still confirm captions late, but it can no longer deny them.

## [0.1.3] — 2026-09-07

### Fixed

- **An update no longer opens a setup window.** The updater ran the installer in its "passive"
  mode, which asks nothing of the viewer but still puts an installer progress dialog on screen —
  so an update that was meant to be invisible announced itself with a window belonging to a program
  nobody launched. It now runs quiet: the application closes, is replaced, and reopens. Nothing
  else appears. What an update replaces is unchanged — the program directory only; history,
  playlists, bookmarks and watch positions live elsewhere and are never touched by it.

## [0.1.2] — 2026-09-06

### Fixed

- **Subtitles can be turned on.** The caption control could not work, for a circular reason: the
  embed's name for its caption module was read from a call that reports the modules currently
  _loaded_ rather than the ones available, and captions start unloaded. So the name came back
  empty, the control reported the video had no captions, and switching them on returned without
  doing anything — the name only appears once the module is loaded, and loading it was exactly what
  was being asked for. The embed announces the module through an event that was never subscribed
  to; it is now, and the name is remembered for as long as the video is loaded.

## [0.1.1] — 2026-09-06

### Changed

- **BEASTUBE keeps itself up to date.** Shortly after launch it now installs a newer version rather
  than only mentioning one, because a notice still asks someone to act and the measured outcome of
  asking was that installations did not update at all. It is bounded on four sides: a switch in
  Settings → About turns it off; a version that already failed to install on this machine is not
  retried, so a bad release cannot fetch 50 MB on every launch for ever; nothing installs while
  something is playing; and an install already running is left alone. The restart is announced
  before it happens rather than after, and the notice opens About, where a running install reports
  its real percentage.
- **Checking for an update installs it.** Settings → About → Check for updates took two presses:
  one to find the update and one to accept it, and the second only ever had one sensible answer.
  It is now a single press — check, download, verify, install, restart — with progress on the same
  row and the download size still stated before it is pressed. That control remains the way to
  update deliberately, including a version automatic updates have skipped.

### Fixed

- **Search returns what the site returns.** Results were coming back long-form only, so a casual
  query looked nearly empty and search appeared to want the exact title of a video. Every
  short-form result in the response was being discarded before it reached the page: the extractor
  parses shorts as a shelf variant it does not recognise and drops the shelf whole. Measured across
  five live searches it lost 25 to 30 per query, and on `funny cat` the response carried 8 ordinary
  videos against 30 shorts — the search returned 5 results where the site had 38. They are now read
  out of the same response the Shorts feed already reads, and the same query returns 16.
- **A thin first page keeps looking.** Some queries are answered with a page of nothing but shorts,
  and send the ordinary videos further down the chain: `roblox trends` returns 25 shorts and no
  long-form result at all on page one, then two videos on page two. The feed asked once and stopped,
  so those queries showed nothing whatever. A first page that comes back short of long-form results
  now follows the continuation up to three pages until it has enough to be worth reading, advancing
  the scroll cursor with it so nothing is repeated. `roblox trends` went from 0 results to 15.
- **"BEASTUBE could not read the response" when opening a video.** The watch-page parser treats one
  absent optional section as fatal — it reports `could not find secondary_info` and returns nothing
  — so a video that plays perfectly well opened on a full-page error instead. It is a property of
  the video rather than a transient failure, and because the Shorts feed asks for these details
  once per card as it scrolls, a single unreadable id repeated the error for as long as playback
  continued. Details are now rebuilt from the player payload when the watch page cannot be read: a
  flat object of strings rather than a tree of renderers, and one that answers for ids the watch
  page refuses.

## [0.1.0] — 2026-09-06

First release.

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
- **Two housekeeping ceilings that are actually enforced.** History can be deleted past a chosen
  age, and the caches can be emptied once they pass a chosen size. Both are off by default.
- **One press to clear every cache**, and a Content Security Policy, which the application
  previously did without entirely.

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
- **Storage controls that reported one quantity and acted on another.** The browser row measured
  the whole 29 MB profile while the button removed five subdirectories worth about 2 MB, so the
  figure barely moved and the button — disabled only at a total that could never reach zero —
  never disabled. Measuring and clearing now read one list. Logs could not be cleared at all,
  because a single file held open made the whole delete fail.
- **"Stored data" counting the download folder**, which is the viewer's own files in a directory
  they chose. On a machine where that folder shared a name with something else, the screen
  reported gigabytes of unrelated content as data BEASTUBE was storing.
- **"Delete history older than", which deleted nothing.** The setting existed in five places and
  was connected in none of them.
- **A renderer holding more privilege than it used.** The window could reach the opener and dialog
  plugins directly, bypassing the URL validation that exists for exactly that reason, and could
  end the process. Free text reaching the native side is now bounded there rather than only in the
  interface.

### Changed

- The feed grid, the card menu and the loading skeletons were rebuilt around what was measured
  rather than assumed; the card glow is promoted to its own compositor layer only while hovered,
  which took a home feed from 120 composited layers and 101 MB of texture to 12 and 46 MB.

### Removed

- Two workspace crates that contained a single doc comment each and no code, along with the HTTP
  framework they pulled into the build, six unused npm packages, three Tauri plugins that were
  compiled in and never registered, and eleven exported functions with no caller anywhere.

[0.1.0]: https://github.com/zbeastcorp/beastube/releases/tag/v0.1.0
