//! Reading what the downloader prints.
//!
//! `yt-dlp` is run with output templates this module defines, so every line that matters carries
//! a tag the application chose. Anything else is the tool's own chatter and is kept only as
//! diagnostic context. The tags are looked for anywhere in a line rather than only at its start,
//! because the tool has, across versions, both replaced its progress line with the template and
//! prefixed the template with its own `[download]` marker.
//!
//! A field the tool does not know is printed as `NA`. It is parsed as *absent*, and an absent
//! total means no percentage — the UI shows an indeterminate state rather than a made-up figure.

use std::path::PathBuf;

/// Tag on a download-progress line.
pub const PROGRESS_TAG: &str = "BEASTUBE-PROGRESS|";
/// Tag on a post-processing (merge, remux) line.
pub const POSTPROCESS_TAG: &str = "BEASTUBE-POSTPROCESS|";
/// Tag on the line naming the finished file.
pub const FILE_TAG: &str = "BEASTUBE-FILE|";

/// `--progress-template` value for the download stage.
#[must_use]
pub fn download_progress_template() -> String {
    format!(
        "download:{PROGRESS_TAG}%(progress.status)s|%(progress.downloaded_bytes)s|\
         %(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|\
         %(progress.eta)s"
    )
}

/// `--progress-template` value for the post-processing stage.
#[must_use]
pub fn postprocess_progress_template() -> String {
    format!("postprocess:{POSTPROCESS_TAG}%(progress.status)s|%(progress.postprocessor)s")
}

/// `--print` value that names the file once it is in its final place.
#[must_use]
pub fn final_path_print() -> String {
    format!("after_move:{FILE_TAG}%(filepath)s")
}

/// One reading of the download stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProgressSample {
    /// Whether this file's download is complete. A video with separate video and audio tracks
    /// reports two finished samples before post-processing begins.
    pub finished: bool,
    /// Bytes received so far.
    pub downloaded_bytes: Option<u64>,
    /// Total size, exact or estimated. Absent when the server did not say.
    pub total_bytes: Option<u64>,
    /// Current transfer rate.
    pub speed_bps: Option<u64>,
    /// The tool's own estimate of the seconds remaining.
    pub eta_seconds: Option<u64>,
}

impl ProgressSample {
    /// Completion in `0.0..=1.0`, or `None` when the total is unknown.
    #[must_use]
    pub fn fraction(&self) -> Option<f32> {
        match (self.downloaded_bytes, self.total_bytes) {
            (Some(downloaded), Some(total)) if total > 0 => {
                // Precision loss past 2^53 bytes is not a concern for a video file, and the
                // result is a display ratio, not an accounting figure.
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                Some(((downloaded as f64 / total as f64).clamp(0.0, 1.0)) as f32)
            }
            _ => None,
        }
    }
}

/// One line of the tool's output, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolLine {
    /// A download-stage reading.
    Progress(ProgressSample),
    /// A post-processing step started or finished.
    Postprocess {
        /// `started` or `finished`.
        status: String,
        /// The post-processor's class name, e.g. `Merger`.
        name: String,
    },
    /// The finished file's path.
    File(PathBuf),
    /// Anything else the tool said.
    Other(String),
}

impl ToolLine {
    /// Whether a post-processing line reports work that joins or rewrites media, as opposed to
    /// bookkeeping such as moving the file into place.
    #[must_use]
    pub fn is_media_postprocess(&self) -> bool {
        match self {
            Self::Postprocess { name, .. } => {
                let lower = name.to_lowercase();
                lower.contains("merg") || lower.contains("ffmpeg") || lower.contains("remux")
            }
            _ => false,
        }
    }
}

/// Classifies one line of output.
#[must_use]
pub fn parse_line(line: &str) -> ToolLine {
    let line = line.trim_end_matches(['\r', '\n']);
    if let Some(rest) = after_tag(line, PROGRESS_TAG) {
        return ToolLine::Progress(parse_progress(rest));
    }
    if let Some(rest) = after_tag(line, POSTPROCESS_TAG) {
        let mut parts = rest.splitn(2, '|');
        let status = parts.next().unwrap_or_default().trim().to_owned();
        let name = parts.next().unwrap_or_default().trim().to_owned();
        return ToolLine::Postprocess { status, name };
    }
    if let Some(rest) = after_tag(line, FILE_TAG) {
        return ToolLine::File(PathBuf::from(rest.trim()));
    }
    ToolLine::Other(line.to_owned())
}

/// The part of `line` after `tag`, wherever the tag sits.
fn after_tag<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    line.find(tag).map(|index| &line[index + tag.len()..])
}

fn parse_progress(fields: &str) -> ProgressSample {
    let mut parts = fields.split('|');
    let status = parts.next().unwrap_or_default().trim();
    let downloaded = number(parts.next());
    let total = number(parts.next());
    let estimate = number(parts.next());
    let speed = number(parts.next());
    let eta = number(parts.next());
    ProgressSample {
        finished: status == "finished",
        downloaded_bytes: downloaded,
        // The exact total when the server sent a length, the estimate otherwise. Either is good
        // enough for a bar; neither is invented.
        total_bytes: total.or(estimate),
        speed_bps: speed,
        eta_seconds: eta,
    }
}

/// A numeric field, or `None` for the tool's `NA` placeholder and anything unparsable.
///
/// The tool prints floats for some byte counts (`1234.0`), so everything is read as a float and
/// rounded.
fn number(field: Option<&str>) -> Option<u64> {
    let text = field?.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("na") || text == "None" {
        return None;
    }
    let value: f64 = text.parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    // Non-negative, finite and rounded: the cast cannot truncate anything a byte count needs.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(value.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_progress_line_is_read_into_numbers() {
        let line = parse_line("BEASTUBE-PROGRESS|downloading|1048576|4194304|NA|524288.5|6");
        assert_eq!(
            line,
            ToolLine::Progress(ProgressSample {
                finished: false,
                downloaded_bytes: Some(1_048_576),
                total_bytes: Some(4_194_304),
                speed_bps: Some(524_289),
                eta_seconds: Some(6),
            })
        );
        if let ToolLine::Progress(sample) = line {
            assert!((sample.fraction().unwrap() - 0.25).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn an_estimate_stands_in_for_a_missing_total_and_na_means_unknown() {
        let ToolLine::Progress(sample) =
            parse_line("[download] BEASTUBE-PROGRESS|downloading|10|NA|100|NA|NA")
        else {
            panic!("expected a progress line");
        };
        assert_eq!(sample.total_bytes, Some(100));
        assert_eq!(sample.speed_bps, None);
        assert_eq!(sample.eta_seconds, None);

        let ToolLine::Progress(unknown) =
            parse_line("BEASTUBE-PROGRESS|downloading|10|NA|NA|NA|NA")
        else {
            panic!("expected a progress line");
        };
        assert_eq!(
            unknown.fraction(),
            None,
            "no total means no percentage, not a guessed one"
        );
    }

    #[test]
    fn the_finished_file_and_postprocessing_are_recognised() {
        assert_eq!(
            parse_line("BEASTUBE-FILE|C:\\Videos\\Clip [abc123].mp4\r\n"),
            ToolLine::File(PathBuf::from("C:\\Videos\\Clip [abc123].mp4"))
        );
        let merge = parse_line("BEASTUBE-POSTPROCESS|started|Merger");
        assert_eq!(
            merge,
            ToolLine::Postprocess {
                status: "started".to_owned(),
                name: "Merger".to_owned()
            }
        );
        assert!(merge.is_media_postprocess());
        assert!(!parse_line("BEASTUBE-POSTPROCESS|started|MoveFiles").is_media_postprocess());
    }

    #[test]
    fn untagged_output_is_kept_verbatim() {
        assert_eq!(
            parse_line("[youtube] abc: Downloading webpage"),
            ToolLine::Other("[youtube] abc: Downloading webpage".to_owned())
        );
    }

    #[test]
    fn the_templates_carry_their_tags() {
        assert!(download_progress_template().starts_with("download:BEASTUBE-PROGRESS|"));
        assert!(postprocess_progress_template().starts_with("postprocess:BEASTUBE-POSTPROCESS|"));
        assert_eq!(final_path_print(), "after_move:BEASTUBE-FILE|%(filepath)s");
    }
}
