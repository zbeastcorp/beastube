# ADR 0004 — Picture quality under the embedded player

**Status:** Accepted (2026-09-04)
**Relates to:** ADR 0001 (playback architecture)

## Problem

The picture was soft, the player's own settings panel rendered white over a dark application, and
there was no way to choose a quality. All three were reported as "the quality is bad".

ADR 0001 establishes that playback goes through YouTube's IFrame embed and that
`setPlaybackQuality` is a no-op, so there is no API call that raises the rendition. That much is
true. The conclusion drawn from it — that picture quality was outside this application's control —
was not.

## What the API actually does

Measured against a live embed rather than read off the documentation, because the documentation
marks the whole quality surface as deprecated and that turns out to be true of exactly one method:

| Call                               | Result                                                                                 |
| ---------------------------------- | -------------------------------------------------------------------------------------- |
| `setPlaybackQuality('hd1080')`     | **Inert.** A player showing `hd720` stays on `hd720`.                                  |
| `setPlaybackQualityRange(...)`     | **Refused across the boundary.** See below — it is the real lever, and it is not ours. |
| `vq=` load parameter               | **Ignored.** `vq=tiny` on a clean profile still served 360p.                           |
| `getPlaybackQuality()`             | **Works.** Returns the tier actually being served.                                     |
| `getAvailableQualityLevels()`      | **Works, and is per-video.** A 240p-era upload answers `["small","auto"]`.             |
| `width` / `height` at construction | **Ignored** whenever CSS sizes the iframe — which it always does here.                 |
| The iframe's **laid-out size**     | **Drives selection**, at load _and_ during playback.                                   |

The last row is the finding the rest of this document rests on.

`setPlaybackQualityRange` deserves its own note, because it is what YouTube's own quality menu
calls and it genuinely works — invoked inside the embed's document it moved a live player from
`hd1080` to `tiny` and rewrote the stored preference. It cannot be reached from here. The embed
whitelists the commands it accepts over `postMessage`, and quality is not on the list: the same
message shape that drives `pauseVideo` and `playVideo` correctly is silently dropped for both
quality setters. So the frame size is not merely the most convenient lever, it is the only one.

## What was wrong

**1. The size the embed was told was never the size it read.** The player was constructed with an
explicit `width`/`height` in device pixels, and a `ResizeObserver` called `setSize` to keep them
current. Neither did anything. The IFrame API copies the mount node's `class` onto the iframe it
builds, that class is `size-full`, and a CSS rule of `width: 100%` beats a `width` attribute.
Measured: an iframe constructed at 1800 and styled `100%` inside a 900px box reports an
`offsetWidth` of 900, and the embed serves the rendition for 900.

So the embed had always been choosing for the player's plain CSS box. Not catastrophic — that box
is roughly the right number — but it meant the device-pixel reasoning was decorative, and it meant
the ceiling on a high-DPI display was the CSS width rather than the physical one.

**2. There was no quality control.** Justified at the time by `setPlaybackQuality` being a no-op.
That reasoning only holds if the _command_ is the only lever, and it is not.

**3. The settings panel followed the operating system, not the application.** That panel is
YouTube's document inside the iframe, and it styles itself from `prefers-color-scheme`, which
answers from the webview's preferred colour scheme. No stylesheet of ours reaches across the
origin to correct it.

## Decision

**Request a quality tier by laying the frame out at the width that produces it, and scale the
result back down.**

The embed measures its own viewport, so the frame is given a real pixel size — 3840 wide for
2160p — and a `transform: scale(box width / render width)` puts it back exactly where the design
wants it. A transform does not affect the transformed element's own layout, so the embed goes on
measuring the large box while the viewer sees the small one.

Width drives; height follows the frame's own proportions. Driving from height would aim the tier
at the _box_ rather than at the picture and land a tier low wherever the box is taller than 16:9 —
which is the normal case here, since the host over-sizes the frame to crop the embed's chrome away.
Because the scale is derived from the same width the layout used, the picture lands on the visible
box to the pixel at every tier, and the letterbox bars scale down to exactly the crop that hides
them.

Measured in the application itself, driving the menu and reading the embed back, one session, no
reload and no rebuffer:

| Picked    | Frame laid out at | Served   |
| --------- | ----------------- | -------- |
| `2160p60` | 3840              | `hd2160` |
| `1440p60` | 2560              | `hd1440` |
| `1080p60` | 1920              | `hd1080` |
| `720p60`  | 1280              | `hd720`  |
| `480p`    | 854               | `large`  |
| `360p`    | 640               | `medium` |

Bidirectional and exact. At the top of the range the player's own statistics read
`3840x2160@60`, codec `av01`, so 4K60 is real rather than 4K at 30.

**Do not offer below 360p.** The embed refuses to go lower however small the frame gets — measured
at 120 pixels wide, still serving `medium` — and every other route to it is closed, as above. So
`240p` and `144p` are absent from the menu rather than present and serving 360p under another name
(§131). The reachable range is 360p to 2160p, at 60fps wherever the video has it.

**Offer only the tiers the video has.** `getAvailableQualityLevels` is per-video, so the menu is
its intersection with the reachable ladder. A video with nothing above 360p offers `360p` and
`Auto`, and nothing else (§131).

**Load every video into the 60fps family.** The embed settles on a 30fps or 60fps track family
when a video _loads_ and keeps it for that load, while resolution goes on following the frame for
as long as the video plays. YouTube encodes 60fps from 720p up, so a player loaded into a
1050-wide box — whose picture is 590 lines — lands in the 30fps family and then climbs to 2160p
**at 30fps** and stays there, which is precisely the defect this is here to prevent. So the frame
is widened to 1280 for the duration of every load, construction and video swap alike, and settles
to its real size once playback has started. The parked, off-screen box is 1280 × 720 for the same
reason.

**Report what is served, not what was clicked.** Requesting a tier is a resize, and the embed acts
on it over the next few seconds. The menu's quality row shows `getPlaybackQuality()`, sampled with
the playhead, so `Auto` reads as `Auto (1080p)` and a switch reads as having happened when it has.
This is also what keeps the control honest in the failure case below.

**`2160p` is the ceiling.** The embed names a `highres` level above it; honouring it would mean
laying the frame out at 7680 pixels. A tier the player would answer at 4K is a control that lies
about what it did, so it is not offered.

**The `max_quality` setting bounds `Auto` only.** That is what it has always claimed to be — "a
ceiling applied to adaptive selection, so `Auto` on a metered connection cannot climb to 4K". It is
applied to the frame width, so a capped `Auto` is capped in fact and not merely in the menu.
Choosing a tier by hand is a deliberate act and goes as high as the video offers, which is how
YouTube's own player behaves.

**Ship YouTube's own controls by default, and keep ours one setting away.**

Both bars exist and both work; `playback.player_controls` chooses. YouTube's is the default because
its gear reaches the player's _internal_ quality API — the same `setPlaybackQualityRange` that is
refused across the postMessage boundary — so it offers every tier from 144p and applies each one the
instant it is chosen. The frame-size mechanism below cannot match that: it stops at 360p, and a
downward change waits for the buffered high-quality segments to drain unless the video is reloaded.

The cost is real and is the reason this was not the original choice: YouTube's settings panel is
their document, styles itself from the operating system, and renders white over a dark application.
Appearance yields to quality here because quality is what people actually reach for.

**Crop YouTube's chrome and use our own controls, when asked to.** This reverses commit `0818c1e`, which had
itself reverted `d806832`. `0818c1e` restored YouTube's band because cropping it takes the settings
gear, and that gear held the only working quality selector. That is no longer true: the gear's
replacement selects quality by the mechanism above, reports the tiers the video actually has, and
is the application's own dark UI. YouTube's panel **cannot be themed** — verified with the
operating system in dark mode, where WebView2 reports `prefers-color-scheme: dark` to the frame and
the panel stays white regardless.

**Set the webview's preferred colour scheme from the application's resolved theme.** The window is
created with `"theme": "Dark"` so the first frame is right, and `set_window_theme` pushes changes at
runtime — `WebviewWindow::set_theme` reaches WebView2's `SetPreferredColorScheme` by way of tao's
`ThemeChanged` event. Everything that is not `light` counts as dark.

**Ask the GPU for the work, and leave the display timing alone.**

Added: `--ignore-gpu-blocklist` (a blocklisted driver otherwise silently drops the whole page to
software rendering), `--enable-gpu-rasterization`, `--enable-zero-copy` and
`--canvas-oop-rasterization`.

Deliberately _not_ added: `--disable-gpu-vsync` and `--disable-frame-rate-limit`. They are the
obvious things to reach for when asked for "more frames", and for a video player they are actively
harmful — playback is paced by the media clock, not the compositor, so uncapping the compositor
renders frames nobody sees, and disabling vsync introduces tearing on the one surface where tearing
is most visible. The correct frame rate for video is the display's refresh rate, which is what
vsync already delivers.

## Consequences

**Quality is selectable from 360p to 2160p60, bounded by what each video has.** `Auto` remains the
default and tracks the window, which is the right behaviour for someone who never opens the menu.

**Rendering at 4K into a smaller box is deliberate.** The compositor downsamples a 3840-wide
surface into the player's real width, which costs GPU bandwidth and pulls 4K bitrate for a picture
shown smaller. That is what "give me 2160p in a windowed player" means, and it is what YouTube's
own player does when a tier is picked in a small window. `Auto` — the default — never does it.

**A stored preference in the embed pins quality and defeats every lever.** The embed keeps
`yt-player-quality` in its own `localStorage` — `{"quality":1080,"previousQuality":144}` was found
there during development — and while it is set the player is in _manual_ mode and correctly ignores
its viewport. A pinned player served `hd1080` to a 3840-wide frame and to a 582-wide one alike;
deleting the key restored viewport control immediately and completely.

Only code inside the embed writes that key, which in practice means YouTube's own quality menu. The
gear that opens it is cropped away and `controls` is off, so under this player nothing can set it —
but a profile that used an earlier build, when YouTube's chrome was still visible, can carry one.
The parent cannot read, clear, or override it.

The design answer is the one already in place: the menu reports `getPlaybackQuality()`. If the
embed refuses to move, the row keeps showing what is genuinely playing rather than the tier that
was clicked. The control never claims a change it did not achieve (§131).

**AV1 remains outside our reach.** YouTube may serve AV1 to the embed, and a GPU without AV1
hardware decode falls back to software, which shows up as dropped frames on high-resolution
content. Nothing in the embed's supported surface lets a caller refuse a codec, so this is stated
rather than fixed. The GPU flags above ensure everything that _can_ be hardware-decoded is.

**The colour-scheme push is best-effort.** If it fails, the panel is the wrong colour and nothing
else changes; the failure is swallowed rather than surfaced, because there is nothing the viewer
could do about it and the application is otherwise unaffected.
