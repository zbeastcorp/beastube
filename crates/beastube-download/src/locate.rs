//! Finding the programs a download needs.
//!
//! Three places are searched, in order: a path the user set explicitly, the directories the caller
//! nominates (the application's own executable directory, so a copy shipped with the installer
//! wins over whatever else is on the machine), then `PATH`. Nothing is downloaded and nothing is
//! installed — this module only reports what is there, and the settings screen shows the answer so
//! a missing tool is a visible fact rather than a mystery failure (§131).

use std::env;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// Executable name of the downloader, without extension.
pub const DOWNLOADER: &str = "yt-dlp";
/// Executable name of the muxer that joins separate video and audio tracks, without extension.
pub const FFMPEG: &str = "ffmpeg";

/// How long `--version` may take before the tool is assumed hung.
const VERSION_TIMEOUT: Duration = Duration::from_secs(20);

/// Extensions tried for an executable name, in order of preference.
///
/// Windows only; elsewhere the bare name is the executable. Shell-script wrappers (`.cmd`,
/// `.bat`) are accepted because package managers install `yt-dlp` that way, and the standard
/// library spawns them through `cmd.exe` with the arguments escaped.
#[cfg(windows)]
const EXTENSIONS: &[&str] = &[".exe", ".com", ".cmd", ".bat"];
#[cfg(not(windows))]
const EXTENSIONS: &[&str] = &[""];

/// A JavaScript runtime `yt-dlp` can use to solve YouTube's player challenges.
///
/// Without one, `yt-dlp` falls back to the clients that need none and warns that formats may be
/// missing. Deno is what it enables by default; Node has to be named on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JsRuntimeKind {
    /// Deno, `yt-dlp`'s preferred runtime.
    Deno,
    /// Node.js.
    Node,
}

impl JsRuntimeKind {
    /// The name `yt-dlp` uses for this runtime in `--js-runtimes`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deno => "deno",
            Self::Node => "node",
        }
    }
}

/// A located JavaScript runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsRuntime {
    /// Which runtime it is.
    pub kind: JsRuntimeKind,
    /// Where it is.
    pub path: PathBuf,
}

/// Everything that was found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tools {
    /// The downloader, or `None` if no copy exists anywhere searched.
    pub downloader: Option<PathBuf>,
    /// The muxer. Optional: without it downloads are limited to formats that already carry both
    /// video and audio, which caps quality but still produces a playable file.
    pub ffmpeg: Option<PathBuf>,
    /// A JavaScript runtime for the downloader. Optional in the same sense.
    pub js_runtime: Option<JsRuntime>,
}

/// Where to look.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocateOptions<'a> {
    /// An explicit downloader path from settings. Used when it exists; ignored otherwise, so a
    /// stale setting degrades to discovery rather than to a missing tool.
    pub downloader: Option<&'a Path>,
    /// An explicit muxer path from settings, with the same semantics.
    pub ffmpeg: Option<&'a Path>,
    /// Directories searched before `PATH`, e.g. the application's executable directory.
    pub beside: &'a [PathBuf],
}

/// Finds the downloader, the muxer and a JavaScript runtime.
#[must_use]
pub fn locate(options: &LocateOptions<'_>) -> Tools {
    Tools {
        downloader: resolve(options.downloader, DOWNLOADER, options.beside),
        ffmpeg: resolve(options.ffmpeg, FFMPEG, options.beside),
        js_runtime: [JsRuntimeKind::Deno, JsRuntimeKind::Node]
            .into_iter()
            .find_map(|kind| {
                find_program(kind.as_str(), options.beside).map(|path| JsRuntime { kind, path })
            }),
    }
}

/// An explicit path if it names a file, otherwise whatever discovery finds.
fn resolve(explicit: Option<&Path>, name: &str, beside: &[PathBuf]) -> Option<PathBuf> {
    if let Some(path) = explicit
        && path.is_file()
    {
        return Some(simplified(path));
    }
    find_program(name, beside)
}

/// Finds an executable by bare name, first in `beside`, then on `PATH`.
#[must_use]
pub fn find_program(name: &str, beside: &[PathBuf]) -> Option<PathBuf> {
    let path_dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect())
        .unwrap_or_default();

    beside
        .iter()
        .chain(path_dirs.iter())
        .find_map(|dir| {
            EXTENSIONS
                .iter()
                .map(|extension| dir.join(format!("{name}{extension}")))
                .find(|candidate| candidate.is_file())
        })
        .map(|found| simplified(&found))
}

/// The longest path a non-verbatim Windows path may be, including the terminator.
const MAX_ORDINARY_PATH: usize = 260;

/// Drops the `\\?\` prefix from a Windows path where doing so is safe.
///
/// Tauri's `resource_dir()` answers in the verbatim form, so the bundled tools were found at
/// `\\?\C:\…\yt-dlp.exe`. It works — the prefix is a real path — but it is shown to the user on
/// the settings screen, where it reads as corruption, and it is handed to a child process, where
/// it depends on that program parsing a form it has no reason to expect.
///
/// The prefix is only removed when the result means exactly the same thing: the drive-letter form,
/// short enough to be a legal ordinary path. A long path, or the `\\?\UNC\` form, is left alone —
/// stripping either changes what the path refers to or makes it unusable, and an ugly path that
/// works beats a tidy one that does not.
#[must_use]
pub fn simplified(path: &Path) -> PathBuf {
    if !cfg!(windows) {
        return path.to_path_buf();
    }
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    let Some(rest) = text.strip_prefix(r"\\?\") else {
        return path.to_path_buf();
    };
    // `C:\…` and nothing else. `UNC\…` and device paths keep their prefix.
    let drive_form = {
        let mut chars = rest.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
            && chars.next() == Some(':')
            && matches!(chars.next(), Some('\\'))
    };
    if drive_form && rest.len() < MAX_ORDINARY_PATH {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

/// Runs `tool --version` and returns the first line it prints.
///
/// `None` if the tool cannot be run, prints nothing, exits unsuccessfully, or takes longer than
/// [`VERSION_TIMEOUT`]. The settings screen shows "not found" in every one of those cases rather
/// than a version it did not actually read.
pub async fn version_of(tool: &Path) -> Option<String> {
    let mut command = Command::new(tool);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    hide_console(&mut command);

    let output = tokio::time::timeout(VERSION_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.lines().next()?.trim();
    (!first.is_empty()).then(|| first.to_owned())
}

/// Keeps a child process from flashing a console window.
///
/// Every tool this crate runs is a console program. Without this flag Windows opens a black
/// window for each one, in front of the application, for as long as the download takes.
pub(crate) fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        /// `CREATE_NO_WINDOW` from `processthreadsapi.h`.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name nothing on any real machine is called, so `PATH` cannot satisfy it by accident.
    const UNIQUE: &str = "beastube-locate-test-tool-9f3a";

    fn fake_executable(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(format!("{name}{}", EXTENSIONS[0]));
        std::fs::write(&path, b"").unwrap();
        path
    }

    #[test]
    fn a_tool_beside_the_application_is_found_before_path() {
        let dir = tempfile::tempdir().unwrap();
        let expected = fake_executable(dir.path(), UNIQUE);
        assert_eq!(
            find_program(UNIQUE, &[dir.path().to_path_buf()]),
            Some(expected)
        );
    }

    #[test]
    fn an_absent_tool_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_program(UNIQUE, &[dir.path().to_path_buf()]), None);
    }

    #[test]
    fn an_explicit_path_wins_when_it_exists_and_is_ignored_when_it_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let discovered = fake_executable(dir.path(), UNIQUE);
        let explicit = fake_executable(dir.path(), "chosen-by-hand");
        let beside = [dir.path().to_path_buf()];

        assert_eq!(
            resolve(Some(&explicit), UNIQUE, &beside),
            Some(explicit.clone())
        );
        assert_eq!(
            resolve(Some(&dir.path().join("gone")), UNIQUE, &beside),
            Some(discovered),
            "a stale explicit path must fall back to discovery"
        );
    }

    #[test]
    fn the_verbatim_prefix_is_dropped_only_where_it_is_safe() {
        // What `resource_dir()` answers with, and what the settings screen was showing.
        let verbatim = Path::new(r"\\?\C:\Users\me\app\binaries\yt-dlp.exe");
        let expected = if cfg!(windows) {
            PathBuf::from(r"C:\Users\me\app\binaries\yt-dlp.exe")
        } else {
            verbatim.to_path_buf()
        };
        assert_eq!(simplified(verbatim), expected);

        // A network path keeps its prefix: the two spellings are not interchangeable to every
        // consumer, so it is left exactly as it came.
        let unc = Path::new(r"\\?\UNC\server\share\ffmpeg.exe");
        assert_eq!(simplified(unc), unc.to_path_buf());

        // Already ordinary, and relative, are both returned untouched.
        let plain = Path::new(r"C:\Tools\ffmpeg.exe");
        assert_eq!(simplified(plain), plain.to_path_buf());
        assert_eq!(simplified(Path::new("yt-dlp")), PathBuf::from("yt-dlp"));
    }

    #[test]
    fn a_path_too_long_to_be_ordinary_keeps_its_prefix() {
        // Stripping here would produce a path Windows itself refuses, which is worse than an ugly
        // one that works.
        let long = format!(r"\\?\C:\{}", "dir\\".repeat(100));
        assert_eq!(simplified(Path::new(&long)), PathBuf::from(&long));
    }

    #[tokio::test]
    async fn a_tool_that_does_not_exist_has_no_version() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(version_of(&dir.path().join("missing")).await, None);
    }
}
