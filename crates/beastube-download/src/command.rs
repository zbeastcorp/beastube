//! Building the downloader's command line.
//!
//! Every argument is either a literal chosen here, a path the application resolved, or a URL built
//! from a validated video identifier. The frontend never contributes a string, so there is no
//! argument-injection surface to reason about. `--ignore-config` is passed so a user's own
//! `yt-dlp` configuration cannot change the output template or the progress format this crate
//! parses.

use std::ffi::OsString;
use std::path::PathBuf;

use beastube_core::ids::VideoId;

use crate::locate::JsRuntime;
use crate::progress::{download_progress_template, final_path_print, postprocess_progress_template};

/// Output filename template.
///
/// The title is limited to 150 bytes so the whole name stays inside Windows' path limit even in a
/// deep download directory, and the identifier is kept so the file can be matched back to the
/// video — and so two videos with the same title do not overwrite each other.
pub const OUTPUT_TEMPLATE: &str = "%(title).150B [%(id)s].%(ext)s";

/// Everything a run needs, resolved by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadPlan {
    /// The downloader executable.
    pub tool: PathBuf,
    /// The muxer.
    ///
    /// Not optional, and that is a measured decision rather than a convenience. YouTube no longer
    /// serves a combined audio-and-video file for the clients this reaches: checked against live
    /// responses on 2026-09-04, every offered format was video-only or audio-only, so a download
    /// without something to join them produces nothing at all. The caller therefore refuses the
    /// download before it starts (`DownloadError::MuxerMissing`) rather than letting the tool fail
    /// with "requested format is not available".
    pub ffmpeg: PathBuf,
    /// A JavaScript runtime for the downloader, if one was found.
    pub js_runtime: Option<JsRuntime>,
    /// Directory the file is saved into. Created by the manager before the run.
    pub directory: PathBuf,
    /// Where the downloader keeps its own cache, so it lives under the application's data rather
    /// than wherever the tool's default points.
    pub cache_dir: Option<PathBuf>,
    /// Ceiling on video height, or `None` for the best available.
    pub max_height: Option<u32>,
}

/// How the downloader ranks the formats that pass the filter.
///
/// The filter decides what is *allowed* (a height ceiling); this decides which of those is best,
/// and the default answer was wrong in a way that is easy to miss. Measured against a live 1080p60
/// video on 2026-09-04:
///
/// | Ordering | Picked | Bitrate |
/// |---|---|---|
/// | yt-dlp's default | `399` (AV1) | 3135 kbps |
/// | this one | `299` (AVC) | 5782 kbps |
///
/// Both are 1920x1080, so both are honestly "1080p" — and the first looks visibly softer, because
/// YouTube encodes its AV1 ladder at roughly half the bitrate. Someone who asked for 1080p and got
/// the AV1 rendition is entitled to say the setting did not work.
///
/// Read left to right: the tallest picture allowed, then the higher frame rate, then a direct HTTPS
/// stream over an HLS one (fewer requests, no fragment reassembly), then **the higher bitrate**,
/// then MP4/M4A so joining the tracks is a remux rather than a re-encode.
pub const FORMAT_SORT: &str = "res,fps,proto,br,ext:mp4:m4a";

/// The watch URL for a video.
#[must_use]
pub fn watch_url(video_id: &VideoId) -> String {
    format!("https://www.youtube.com/watch?v={}", video_id.as_str())
}

/// The `--format` selector.
///
/// The best video track under the ceiling paired with the best audio track, preferring MP4/M4A so
/// the merged file plays everywhere without re-encoding. Verified against live YouTube on
/// 2026-09-04: for a 1080p video this resolves to `399+140`, an AV1 video track and an M4A audio
/// track, merged to a single MP4.
///
/// The last fallback is a bare `b`, so a video with nothing under the ceiling still downloads at
/// whatever it does have rather than failing. Duplicate candidates are removed: with no ceiling
/// set several of them collapse to the same expression, and repeating one in the selector makes
/// the tool retry an alternative it has already rejected.
#[must_use]
pub fn format_selector(max_height: Option<u32>) -> String {
    let ceiling = max_height
        .map(|height| format!("[height<={height}]"))
        .unwrap_or_default();

    let candidates = [
        format!("bv*{ceiling}[ext=mp4]+ba[ext=m4a]"),
        format!("bv*{ceiling}+ba"),
        format!("b{ceiling}"),
        "b".to_owned(),
    ];

    let mut distinct: Vec<String> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if !distinct.contains(&candidate) {
            distinct.push(candidate);
        }
    }
    distinct.join("/")
}

/// The full argument list for one download.
#[must_use]
pub fn arguments(plan: &DownloadPlan, video_id: &VideoId) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut literal = |values: &[&str]| args.extend(values.iter().map(OsString::from));

    literal(&[
        "--ignore-config",
        "--no-playlist",
        // `--print` alone implies a dry run; this keeps the download real.
        "--no-simulate",
        "--quiet",
        "--progress",
        "--newline",
        "--color",
        "never",
        "--encoding",
        "utf-8",
        // The tool otherwise stamps the file with the upload date, which sorts a download made
        // today under files from years ago.
        "--no-mtime",
        "--windows-filenames",
        "--retries",
        "3",
        "--fragment-retries",
        "3",
        "--socket-timeout",
        "30",
        "--progress-template",
    ]);
    args.push(download_progress_template().into());
    args.push("--progress-template".into());
    args.push(postprocess_progress_template().into());
    args.push("--print".into());
    args.push(final_path_print().into());
    args.push("--paths".into());
    args.push(plan.directory.as_os_str().to_owned());
    args.push("--output".into());
    args.push(OUTPUT_TEMPLATE.into());

    if let Some(cache) = &plan.cache_dir {
        args.push("--cache-dir".into());
        args.push(cache.as_os_str().to_owned());
    }
    args.push("--ffmpeg-location".into());
    args.push(plan.ffmpeg.as_os_str().to_owned());
    args.push("--merge-output-format".into());
    args.push("mp4".into());
    args.push("--format".into());
    args.push(format_selector(plan.max_height).into());
    args.push("--format-sort".into());
    args.push(FORMAT_SORT.into());

    if let Some(runtime) = &plan.js_runtime {
        args.push("--js-runtimes".into());
        let mut spec = OsString::from(runtime.kind.as_str());
        spec.push(":");
        spec.push(runtime.path.as_os_str());
        args.push(spec);
    }

    args.push("--".into());
    args.push(watch_url(video_id).into());
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locate::JsRuntimeKind;

    fn plan() -> DownloadPlan {
        DownloadPlan {
            tool: PathBuf::from("yt-dlp"),
            ffmpeg: PathBuf::from("C:/Tools/ffmpeg.exe"),
            js_runtime: None,
            directory: PathBuf::from("C:/Videos"),
            cache_dir: None,
            max_height: Some(1080),
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    /// The value of `arg`, for a flag that takes one.
    fn after(args: &[String], flag: &str) -> String {
        let index = args.iter().position(|arg| arg == flag).expect(flag);
        args[index + 1].clone()
    }

    #[test]
    fn separate_tracks_are_paired_under_the_ceiling() {
        // Checked against live YouTube: this selector resolves to `399+140` at 1920x1080.
        assert_eq!(
            format_selector(Some(1080)),
            "bv*[height<=1080][ext=mp4]+ba[ext=m4a]/bv*[height<=1080]+ba/b[height<=1080]/b"
        );
        assert_eq!(format_selector(None), "bv*[ext=mp4]+ba[ext=m4a]/bv*+ba/b");
    }

    #[test]
    fn no_candidate_is_ever_repeated() {
        for height in [None, Some(360), Some(2160)] {
            let selector = format_selector(height);
            let parts: Vec<&str> = selector.split('/').collect();
            let mut unique = parts.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(
                unique.len(),
                parts.len(),
                "`{selector}` repeats an alternative the tool has already rejected"
            );
        }
    }

    #[test]
    fn every_selector_ends_in_an_unconstrained_fallback() {
        // Without this, a video with nothing under the ceiling fails outright rather than
        // downloading at whatever quality it does have.
        for height in [None, Some(360), Some(2160)] {
            assert!(
                format_selector(height).ends_with("/b"),
                "{height:?} has no final fallback"
            );
        }
    }

    #[test]
    fn the_url_is_built_from_the_identifier_and_ends_the_argument_list() {
        let video = VideoId::new("dQw4w9WgXcQ").unwrap();
        let args = strings(&arguments(&plan(), &video));

        assert_eq!(
            args.last().map(String::as_str),
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
        );
        // `--` before it, so a video id that started with a dash could never be read as a flag.
        assert_eq!(args[args.len() - 2], "--");
    }

    #[test]
    fn the_users_own_yt_dlp_configuration_cannot_change_what_is_parsed() {
        let video = VideoId::new("dQw4w9WgXcQ").unwrap();
        let args = strings(&arguments(&plan(), &video));

        // Without this, a config file setting its own `--output` or `--progress-template` would
        // silently break the progress parser and the finished-file path.
        assert!(args.contains(&"--ignore-config".to_owned()));
        // `--print` alone implies a dry run; this is what keeps the download real.
        assert!(args.contains(&"--no-simulate".to_owned()));
        assert_eq!(after(&args, "--output"), OUTPUT_TEMPLATE);
    }

    #[test]
    fn the_muxer_is_always_named_because_it_is_always_required() {
        let video = VideoId::new("dQw4w9WgXcQ").unwrap();
        let args = strings(&arguments(&plan(), &video));

        assert_eq!(after(&args, "--ffmpeg-location"), "C:/Tools/ffmpeg.exe");
        assert_eq!(after(&args, "--merge-output-format"), "mp4");
        assert!(after(&args, "--format").starts_with("bv*[height<=1080]"));
    }

    #[test]
    fn bitrate_outranks_the_codec_the_downloader_would_otherwise_prefer() {
        let video = VideoId::new("dQw4w9WgXcQ").unwrap();
        let args = strings(&arguments(&plan(), &video));
        let sort = after(&args, "--format-sort");

        // Without this the default ordering takes YouTube's AV1 ladder, which is the same
        // resolution at roughly half the bitrate — 1080p by the numbers and soft on screen.
        let position = |key: &str| sort.find(key).unwrap_or_else(|| panic!("{key} missing"));
        assert!(position("res") < position("br"), "resolution still leads: {sort}");
        assert!(
            position("proto") < position("br"),
            "a direct stream should beat a higher-bitrate HLS one: {sort}"
        );
        assert!(position("br") < position("ext"), "bitrate decides before container: {sort}");
    }

    #[test]
    fn a_javascript_runtime_is_named_only_when_one_was_found() {
        let video = VideoId::new("dQw4w9WgXcQ").unwrap();
        assert!(!strings(&arguments(&plan(), &video)).contains(&"--js-runtimes".to_owned()));

        let mut with_runtime = plan();
        with_runtime.js_runtime = Some(JsRuntime {
            kind: JsRuntimeKind::Node,
            path: PathBuf::from("C:/Program Files/nodejs/node.exe"),
        });
        let args = strings(&arguments(&with_runtime, &video));
        // The runtime is passed as `kind:path`, and a path with spaces must survive it intact —
        // this is one argument, not two, because it is never rendered through a shell.
        assert_eq!(
            after(&args, "--js-runtimes"),
            "node:C:/Program Files/nodejs/node.exe"
        );
    }
}
