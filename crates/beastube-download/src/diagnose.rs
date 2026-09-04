//! Asking the provider why a video will not play.
//!
//! The embedded player reports refusals as numbers. Codes 101 and 150 in particular cover several
//! quite different situations — the uploader disabling embedding, an age gate, a region block, a
//! copyright claim, a video that has simply gone — and the player says nothing about which. For a
//! long time this application guessed, and told the viewer the uploader had disabled playback. That
//! guess was often wrong.
//!
//! `yt-dlp` is already here, already bundled, already driven as a child process for downloads. It
//! can answer the question properly, and this asks it.
//!
//! ## Why it asks more than one client
//!
//! YouTube's clients are not equally forthcoming. Measured on a video this application refused:
//!
//! ```text
//! default / web / mweb  ->  "Video unavailable"
//! android / ios / tv    ->  "It was blocked due to the claimed content by Netflix."
//! ```
//!
//! The same video, the same moment, the same tool — one set of clients gives the reason and the
//! other does not. So the reason is asked of the clients that actually give one, and only the
//! reason is kept.
//!
//! ## What this is not
//!
//! It does not make an unplayable video playable, and it must never look as though it might. A
//! copyright block is a copyright block. This turns "something went wrong" into "blocked due to a
//! claim by Netflix", which is the difference between a viewer wondering whether the application is
//! broken and a viewer knowing exactly where they stand.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::locate::hide_console;

/// How long to wait for the provider to answer before giving up.
///
/// Generous, because this runs only after playback has already failed — the viewer is looking at an
/// error either way, and a slow answer is better than none. Bounded because a hung child must never
/// leave the explanation spinning forever.
const DIAGNOSE_TIMEOUT: Duration = Duration::from_secs(25);

/// The clients asked, in order, and why.
///
/// `android` first because it was the most forthcoming in testing. `web` is deliberately absent:
/// it is the client that answers "Video unavailable", which is precisely the non-answer this
/// exists to improve on.
const CLIENTS: [&str; 3] = ["android", "ios", "tv"];

/// What the provider says about a video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Playability {
    /// The provider offers the video. Whatever refused it, it was not this.
    Available,
    /// The provider refuses, and this is the reason it gave, in its own words.
    Refused {
        /// The provider's own sentence, with its `ERROR: [youtube] <id>:` prefix removed.
        reason: String,
    },
    /// Nothing could be established — the tool is missing, timed out, or said nothing useful.
    Unknown,
}

/// Asks the provider why `video_id` will not play.
///
/// Never returns an error: a diagnosis that cannot be made is [`Playability::Unknown`], because the
/// caller is already showing a failure and a second failure on top of it helps nobody.
pub async fn playability(tool: &Path, video_id: &str) -> Playability {
    let url = format!("https://www.youtube.com/watch?v={video_id}");
    let mut fallback = Playability::Unknown;

    for client in CLIENTS {
        match ask(tool, &url, client).await {
            // Available from any client is the end of it: the video exists and can be fetched.
            Playability::Available => return Playability::Available,
            // A named reason is what this is for, so the first real one wins.
            reason @ Playability::Refused { .. } => return reason,
            // Keep asking; a later client may be more forthcoming than this one.
            Playability::Unknown => fallback = Playability::Unknown,
        }
    }
    fallback
}

/// Runs one client's probe. `--simulate` so nothing is ever downloaded.
async fn ask(tool: &Path, url: &str, client: &str) -> Playability {
    let mut command = Command::new(tool);
    command
        .arg("--no-warnings")
        .arg("--simulate")
        .arg("--no-playlist")
        .arg("--extractor-args")
        .arg(format!("youtube:player_client={client}"))
        .arg("--print")
        .arg("available")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    hide_console(&mut command);

    let Ok(Ok(output)) = tokio::time::timeout(DIAGNOSE_TIMEOUT, command.output()).await else {
        return Playability::Unknown;
    };

    if output.status.success() && String::from_utf8_lossy(&output.stdout).contains("available") {
        return Playability::Available;
    }

    match reason_from(&String::from_utf8_lossy(&output.stderr)) {
        Some(reason) => Playability::Refused { reason },
        None => Playability::Unknown,
    }
}

/// Pulls the human sentence out of yt-dlp's stderr.
///
/// The lines look like `ERROR: [youtube] <id>: <reason>`, and only the reason is wanted — the
/// prefix is noise to a viewer, and the id is already on screen. A reason that is itself a
/// non-answer is rejected here rather than shown, because "Video unavailable" over a player that
/// plainly did not play tells nobody anything.
fn reason_from(stderr: &str) -> Option<String> {
    for line in stderr.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("ERROR:") else {
            continue;
        };
        // Drop the `[youtube] <id>:` prefix when it is there.
        let reason = rest
            .rsplit_once(": ")
            .map_or_else(|| rest.trim(), |(_, tail)| tail.trim());
        if reason.is_empty() || reason.eq_ignore_ascii_case("Video unavailable") {
            continue;
        }
        return Some(reason.trim_end_matches('.').to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_reason_is_extracted_without_its_prefix() {
        let stderr = "ERROR: [youtube] ag5Q2iKQyOo: It was blocked due to the claimed content by Netflix.";
        assert_eq!(
            reason_from(stderr),
            Some("It was blocked due to the claimed content by Netflix".to_owned())
        );
    }

    #[test]
    fn the_non_answer_is_rejected_rather_than_shown() {
        // The whole point is to improve on this string, so repeating it would be worse than
        // saying nothing and letting the caller keep its own wording.
        assert_eq!(reason_from("ERROR: [youtube] abc: Video unavailable"), None);
    }

    #[test]
    fn a_later_line_is_used_when_the_first_says_nothing() {
        let stderr = "ERROR: [youtube] abc: Video unavailable\nERROR: [youtube] abc: Sign in to confirm your age";
        assert_eq!(
            reason_from(stderr),
            Some("Sign in to confirm your age".to_owned())
        );
    }

    #[test]
    fn output_with_no_error_line_yields_nothing() {
        assert_eq!(reason_from("[youtube] abc: Downloading webpage"), None);
        assert_eq!(reason_from(""), None);
    }
}
