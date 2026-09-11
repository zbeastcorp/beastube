//! URL and filesystem validation for untrusted input.
//!
//! Every string in this module's inputs came from somewhere we do not control: a provider
//! response, a filter rule set, an imported playlist file, or an IPC argument. The functions here
//! are the single place those strings become safe to act on, so that no call site has to remember
//! the Windows-specific rules.
//!
//! ## Threats addressed
//!
//! * **Dangerous URL schemes** — a provider or imported file supplying `javascript:`,
//!   `file:`, `data:` or `vbscript:` must never reach the shell opener or the webview.
//! * **Path traversal** — an identifier or filename used to build a cache path must not be
//!   able to escape the cache root.
//! * **Windows filesystem rules** — reserved device names (`CON`, `NUL`, `COM1`…), trailing
//!   dots and spaces, and reserved characters produce files that cannot be created, opened or
//!   deleted. On Windows these are not merely awkward, they are unrecoverable-looking bugs.

use std::path::{Component, Path, PathBuf};

use url::{Host, Url};

/// Maximum length of a single path component. NTFS allows 255; staying under it avoids failures
/// that only appear on deeply nested cache directories.
pub const MAX_COMPONENT_LEN: usize = 128;

/// Reasons a URL was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    /// The string could not be parsed as an absolute URL.
    #[error("malformed URL: {0}")]
    Malformed(String),
    /// The scheme is not on the allowlist for this use.
    #[error("scheme `{scheme}` is not permitted here")]
    SchemeNotAllowed {
        /// The rejected scheme.
        scheme: String,
    },
    /// The URL has no host, which every remote use requires.
    #[error("URL has no host")]
    MissingHost,
    /// The host is not on the allowlist for this use.
    #[error("host `{host}` is not permitted here")]
    HostNotAllowed {
        /// The rejected host.
        host: String,
    },
    /// The URL contains embedded credentials, which we never propagate.
    #[error("URL contains embedded credentials")]
    EmbeddedCredentials,
    /// The URL exceeded the length bound.
    #[error("URL is {len} bytes, maximum is {max}")]
    TooLong {
        /// Actual length.
        len: usize,
        /// Maximum permitted length.
        max: usize,
    },
}

/// Upper bound on URL length. Signed media URLs are long (~2 KiB is common), so the bound is
/// generous, but unbounded strings from a provider must not reach the network layer.
pub const MAX_URL_LEN: usize = 8192;

/// Parses and validates a URL that will be **opened in the user's external browser**.
///
/// Only `https` is accepted. `http` is excluded deliberately: nothing in this application needs to
/// hand a plaintext URL to the OS, and permitting it widens the surface for a hostile provider
/// response or imported file.
///
/// # Errors
///
/// Returns [`UrlError`] if the URL is malformed, over-long, uses a scheme other than `https`, has
/// no host, or embeds credentials.
pub fn validate_external_url(raw: &str) -> Result<Url, UrlError> {
    let url = parse_bounded(raw)?;
    if url.scheme() != "https" {
        return Err(UrlError::SchemeNotAllowed {
            scheme: url.scheme().to_owned(),
        });
    }
    require_host(&url)?;
    reject_credentials(&url)?;
    Ok(url)
}

/// Parses and validates a URL the application will **fetch over the network**.
///
/// Accepts `https` only, and additionally rejects hosts that resolve to the loopback or
/// link-local ranges by literal address. This prevents a hostile or drifted provider response from
/// steering our own fetcher at the local media gateway or at a cloud metadata endpoint.
///
/// Hostname-based SSRF (a DNS name that resolves to a private address) is not defeated here — that
/// requires resolution-time checks in the network layer, which owns the resolver.
///
/// # Errors
///
/// Returns [`UrlError`] if the URL is malformed, over-long, not `https`, hostless, embeds
/// credentials, or targets a literal loopback/link-local/unspecified address.
pub fn validate_fetch_url(raw: &str) -> Result<Url, UrlError> {
    let url = validate_external_url(raw)?;
    if let Some(host) = url.host() {
        let blocked = match host {
            Host::Ipv4(addr) => {
                addr.is_loopback()
                    || addr.is_link_local()
                    || addr.is_unspecified()
                    || addr.is_broadcast()
            }
            Host::Ipv6(addr) => addr.is_loopback() || addr.is_unspecified(),
            Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
        };
        if blocked {
            return Err(UrlError::HostNotAllowed {
                host: host.to_string(),
            });
        }
    }
    Ok(url)
}

/// Parses and validates a URL pointing at the application's own loopback media gateway.
///
/// This is the inverse allowlist of [`validate_fetch_url`]: `http` is permitted, but *only* on a
/// loopback address, because the gateway is bound to `127.0.0.1` and never exposed off-host.
///
/// # Errors
///
/// Returns [`UrlError`] if the URL is malformed, over-long, not `http`/`https`, or does not target
/// a loopback address.
pub fn validate_loopback_url(raw: &str) -> Result<Url, UrlError> {
    let url = parse_bounded(raw)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UrlError::SchemeNotAllowed {
            scheme: url.scheme().to_owned(),
        });
    }
    reject_credentials(&url)?;
    let host = url.host().ok_or(UrlError::MissingHost)?;
    let is_loopback = match host {
        Host::Ipv4(addr) => addr.is_loopback(),
        Host::Ipv6(addr) => addr.is_loopback(),
        Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
    };
    if !is_loopback {
        return Err(UrlError::HostNotAllowed {
            host: host.to_string(),
        });
    }
    Ok(url)
}

fn parse_bounded(raw: &str) -> Result<Url, UrlError> {
    if raw.len() > MAX_URL_LEN {
        return Err(UrlError::TooLong {
            len: raw.len(),
            max: MAX_URL_LEN,
        });
    }
    Url::parse(raw).map_err(|e| UrlError::Malformed(e.to_string()))
}

fn require_host(url: &Url) -> Result<(), UrlError> {
    match url.host_str() {
        Some(host) if !host.is_empty() => Ok(()),
        _ => Err(UrlError::MissingHost),
    }
}

fn reject_credentials(url: &Url) -> Result<(), UrlError> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlError::EmbeddedCredentials);
    }
    Ok(())
}

/// Returns `true` if `host` is `domain` or a subdomain of it, comparing case-insensitively.
///
/// Used to build host allowlists (`googlevideo.com`, `ytimg.com`) without the classic
/// `ends_with(".googlevideo.com")` bug that also matches `evilgooglevideo.com`.
#[must_use]
pub fn is_host_within(host: &str, domain: &str) -> bool {
    if host.eq_ignore_ascii_case(domain) {
        return true;
    }
    host.len() > domain.len()
        && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
        && host[host.len() - domain.len()..].eq_ignore_ascii_case(domain)
}

/// Reasons a path or filename was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// The name was empty, or became empty after trimming.
    #[error("path component is empty")]
    Empty,
    /// The component exceeded [`MAX_COMPONENT_LEN`].
    #[error("path component is {len} characters, maximum is {max}")]
    TooLong {
        /// Actual length.
        len: usize,
        /// Maximum permitted length.
        max: usize,
    },
    /// The component contained a separator, a reserved character, or a control character.
    #[error("path component contains the reserved character {ch:?}")]
    ReservedCharacter {
        /// The offending character.
        ch: char,
    },
    /// The path attempted to escape its root via `..`, an absolute path, or a drive prefix.
    #[error("path escapes its root")]
    Traversal,
    /// The name matches a Windows reserved device name.
    #[error("`{name}` is a reserved Windows device name")]
    ReservedName {
        /// The offending name.
        name: String,
    },
    /// The name ends with a dot or space, which Windows silently strips, producing collisions.
    #[error("path component may not end with a dot or space")]
    TrailingDotOrSpace,
}

/// Windows device names that cannot be used as a file name in any directory, with or without an
/// extension. Creating `CON.jpg` fails just as `CON` does.
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "COM0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT0",
];

/// Characters Windows forbids in a file name, plus the separators we forbid everywhere so that a
/// single component can never expand into several.
const RESERVED_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Validates a single filename component destined for the cache or export directories.
///
/// # Errors
///
/// Returns [`PathError`] if the name is empty, over-long, contains a reserved or control
/// character, is `.`/`..`, matches a Windows device name, or ends with a dot or space.
pub fn validate_file_component(name: &str) -> Result<(), PathError> {
    if name.is_empty() {
        return Err(PathError::Empty);
    }
    let len = name.chars().count();
    if len > MAX_COMPONENT_LEN {
        return Err(PathError::TooLong {
            len,
            max: MAX_COMPONENT_LEN,
        });
    }
    if name == "." || name == ".." {
        return Err(PathError::Traversal);
    }
    if let Some(ch) = name
        .chars()
        .find(|&c| RESERVED_CHARS.contains(&c) || c.is_control())
    {
        return Err(PathError::ReservedCharacter { ch });
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(PathError::TrailingDotOrSpace);
    }
    // `CON`, `con`, `CON.jpg` and `con.tar.gz` are all the console device.
    let stem = name.split('.').next().unwrap_or(name);
    if WINDOWS_RESERVED_NAMES
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        return Err(PathError::ReservedName {
            name: stem.to_owned(),
        });
    }
    Ok(())
}

/// Joins a caller-supplied relative path onto `root`, guaranteeing the result stays inside `root`.
///
/// Validation is purely lexical and therefore works before the file exists, which is what callers
/// need when *creating* a cache entry. It rejects `..`, absolute paths and Windows drive/UNC
/// prefixes outright rather than normalizing them away, because a request containing `..` is
/// always a bug or an attack, never a legitimate cache key.
///
/// Symlink escape is not addressed here — a symlink planted inside the cache root could still
/// redirect a write. The cache directory is created by the application under its own app-data
/// directory, and cache writes are content-addressed, so the exposure is limited; callers handling
/// user-chosen directories must canonicalize after creation.
///
/// # Errors
///
/// Returns [`PathError`] if any component is invalid or the path attempts to escape `root`.
pub fn resolve_within(root: &Path, relative: &str) -> Result<PathBuf, PathError> {
    if relative.is_empty() {
        return Err(PathError::Empty);
    }
    let candidate = Path::new(relative);
    let mut out = root.to_path_buf();
    let mut pushed = 0usize;
    for component in candidate.components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str().ok_or(PathError::Traversal)?;
                validate_file_component(part)?;
                out.push(part);
                pushed += 1;
            }
            // `.` is harmless but signals a caller building paths by string concatenation.
            Component::CurDir => return Err(PathError::Traversal),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(PathError::Traversal);
            }
        }
    }
    if pushed == 0 {
        return Err(PathError::Empty);
    }
    Ok(out)
}

/// Rewrites an arbitrary string into a safe filename component.
///
/// Used for user-facing exports (`My Playlist.json`) where rejecting the name would be hostile —
/// unlike cache keys, where a bad name means a bug and must fail loudly.
///
/// Reserved and control characters become `_`, trailing dots and spaces are trimmed, Windows
/// device names are suffixed, over-long names are truncated on a character boundary, and an input
/// that reduces to nothing yields `fallback`.
#[must_use]
pub fn sanitize_file_component(raw: &str, fallback: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if RESERVED_CHARS.contains(&c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();

    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    let trimmed_start = out.trim_start();
    if trimmed_start.len() != out.len() {
        out = trimmed_start.to_owned();
    }

    if out.chars().count() > MAX_COMPONENT_LEN {
        out = out.chars().take(MAX_COMPONENT_LEN).collect();
        // Truncation can re-expose a trailing dot or space.
        while out.ends_with('.') || out.ends_with(' ') {
            out.pop();
        }
    }

    if out.is_empty() || out == "." || out == ".." {
        return fallback.to_owned();
    }

    let stem = out.split('.').next().unwrap_or(&out);
    if WINDOWS_RESERVED_NAMES
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        out.insert(0, '_');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_dangerous_schemes_for_external_open() {
        for hostile in [
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///C:/Windows/System32/config/SAM",
            "vbscript:msgbox(1)",
            "about:blank",
            "chrome://settings",
            "ms-settings:privacy",
        ] {
            assert!(
                validate_external_url(hostile).is_err(),
                "{hostile} must be rejected"
            );
        }
    }

    #[test]
    fn accepts_https_only_for_external_open() {
        assert!(validate_external_url("https://www.youtube.com/watch?v=abc").is_ok());
        assert!(matches!(
            validate_external_url("http://www.youtube.com/"),
            Err(UrlError::SchemeNotAllowed { .. })
        ));
    }

    #[test]
    fn rejects_embedded_credentials() {
        assert_eq!(
            validate_external_url("https://user:pass@example.com/"),
            Err(UrlError::EmbeddedCredentials)
        );
        assert_eq!(
            validate_external_url("https://user@example.com/"),
            Err(UrlError::EmbeddedCredentials)
        );
    }

    #[test]
    fn rejects_overlong_urls() {
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_LEN));
        assert!(matches!(
            validate_external_url(&long),
            Err(UrlError::TooLong { .. })
        ));
    }

    #[test]
    fn fetch_urls_may_not_target_loopback_or_metadata_addresses() {
        for hostile in [
            "https://127.0.0.1/steal",
            "https://localhost/steal",
            "https://[::1]/steal",
            "https://169.254.169.254/latest/meta-data/",
            "https://0.0.0.0/",
        ] {
            assert!(
                matches!(
                    validate_fetch_url(hostile),
                    Err(UrlError::HostNotAllowed { .. })
                ),
                "{hostile} must be rejected for outbound fetch"
            );
        }
        assert!(
            validate_fetch_url("https://rr3---sn-abc.googlevideo.com/videoplayback?x=1").is_ok()
        );
    }

    #[test]
    fn loopback_urls_accept_http_but_only_on_loopback() {
        assert!(validate_loopback_url("http://127.0.0.1:53411/media/abc").is_ok());
        assert!(validate_loopback_url("http://localhost:53411/media/abc").is_ok());
        assert!(matches!(
            validate_loopback_url("http://192.168.1.10:53411/media/abc"),
            Err(UrlError::HostNotAllowed { .. })
        ));
        assert!(matches!(
            validate_loopback_url("file://127.0.0.1/etc"),
            Err(UrlError::SchemeNotAllowed { .. })
        ));
    }

    #[test]
    fn host_matching_is_not_a_suffix_match() {
        assert!(is_host_within(
            "rr3---sn-abc.googlevideo.com",
            "googlevideo.com"
        ));
        assert!(is_host_within("googlevideo.com", "googlevideo.com"));
        assert!(is_host_within("GOOGLEVIDEO.COM", "googlevideo.com"));
        // The classic suffix-match bug.
        assert!(!is_host_within("evilgooglevideo.com", "googlevideo.com"));
        assert!(!is_host_within(
            "googlevideo.com.evil.tld",
            "googlevideo.com"
        ));
        assert!(!is_host_within("com", "googlevideo.com"));
    }

    #[test]
    fn rejects_windows_reserved_names_with_and_without_extension() {
        for name in ["CON", "con", "NUL.jpg", "com1", "LPT9.tar.gz", "AuX"] {
            assert!(
                matches!(
                    validate_file_component(name),
                    Err(PathError::ReservedName { .. })
                ),
                "{name} must be rejected"
            );
        }
        assert!(validate_file_component("console.jpg").is_ok());
        assert!(validate_file_component("nulls.json").is_ok());
    }

    #[test]
    fn rejects_trailing_dot_or_space() {
        assert_eq!(
            validate_file_component("thumbnail."),
            Err(PathError::TrailingDotOrSpace)
        );
        assert_eq!(
            validate_file_component("thumbnail "),
            Err(PathError::TrailingDotOrSpace)
        );
    }

    #[test]
    fn rejects_separators_and_control_characters() {
        for name in [
            "a/b", "a\\b", "a:b", "a*b", "a?b", "a|b", "a\"b", "a<b", "a>b", "a\u{7}b",
        ] {
            assert!(
                matches!(
                    validate_file_component(name),
                    Err(PathError::ReservedCharacter { .. })
                ),
                "{name:?} must be rejected"
            );
        }
    }

    #[test]
    fn resolve_within_blocks_traversal() {
        let root = Path::new("C:\\cache");
        for hostile in [
            "../secret",
            "..\\secret",
            "a/../../secret",
            "/etc/passwd",
            "C:\\Windows\\System32",
            "\\\\server\\share",
            "./relative",
        ] {
            assert!(
                resolve_within(root, hostile).is_err(),
                "{hostile} must not resolve"
            );
        }
    }

    #[test]
    fn resolve_within_accepts_nested_safe_paths() {
        let root = Path::new("C:\\cache");
        let resolved = resolve_within(root, "thumbs/ab/dQw4w9WgXcQ.webp").unwrap();
        assert!(resolved.starts_with(root));
        assert!(resolved.ends_with("dQw4w9WgXcQ.webp"));
        assert!(resolve_within(root, "").is_err());
    }

    #[test]
    fn sanitize_rewrites_instead_of_failing() {
        assert_eq!(
            sanitize_file_component("My/Playlist:2024", "export"),
            "My_Playlist_2024"
        );
        assert_eq!(sanitize_file_component("   ", "export"), "export");
        assert_eq!(sanitize_file_component("...", "export"), "export");
        assert_eq!(sanitize_file_component("CON", "export"), "_CON");
        assert_eq!(sanitize_file_component("watch me.", "export"), "watch me");
    }

    #[test]
    fn sanitized_output_always_validates() {
        for raw in [
            "CON.jpg",
            "a/b\\c:d",
            "trailing...   ",
            &"x".repeat(500),
            "\u{1}\u{2}\u{3}",
            "..",
            "🎬 My Playlist 🎬",
        ] {
            let safe = sanitize_file_component(raw, "export");
            assert!(
                validate_file_component(&safe).is_ok(),
                "sanitize({raw:?}) produced invalid component {safe:?}"
            );
        }
    }
}
