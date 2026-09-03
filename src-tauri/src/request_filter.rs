//! Network request filtering, attached to the webview.
//!
//! This is the piece that makes the filtering subsystem act rather than merely exist. Everything
//! the webview fetches — from the shell itself and from the embedded player — passes through
//! [`FilterEngine::decide`] before the network sees it, which is the same model Brave and uBlock
//! Origin use: match the request against a rule set, and refuse the ones that exist only to track
//! the viewer.
//!
//! ## What it blocks, and what it deliberately does not
//!
//! Third-party tracking and analytics endpoints. Those are requests the application never needs and
//! whose only purpose is to profile the person watching.
//!
//! It does **not** block the provider's own advertising, and it cannot: under the sanctioned embed
//! player (ADR-0001) the ad path and the media path are the same connection to the same hosts, so a
//! rule aimed at one would take the other with it and playback would simply stop. That is not a
//! limitation this layer papers over — [`NeverBlockList::playback_critical`] makes blocking the
//! media path *impossible* even if a downloaded rule set tried, and the filtering engine's tests
//! assert it in every mode.
//!
//! ## Why it is on the UI thread, and why that is fine
//!
//! WebView2 raises `WebResourceRequested` on the thread that owns the webview, and the handler runs
//! synchronously: whatever it does, the request waits. A decision therefore has to be cheap and
//! must never block. It is:
//!
//! * The engine handle is cloned out from behind an uncontended `parking_lot` read lock — writers
//!   appear only when a rule set is activated — and the decision itself is a hash lookup over an
//!   immutable snapshot, allocating nothing on the common path.
//! * Configuration (enabled, mode) was pushed into the engine when settings changed, rather than
//!   read back from the settings store per request.
//! * Diagnostics are relaxed atomic increments.
//!
//! ## Failure policy
//!
//! Every failure here allows the request. A filter that fails closed would turn a bug in this file
//! into an application that cannot load its own assets, and the worst case of failing open is one
//! tracker request that should have been refused (§81).

// COM interop is unavoidable here: WebView2's request interception has no safe wrapper, and this is
// the only place in the workspace that needs one. The unsafe blocks are confined to reading a
// string out of the request and handing back a response object; each is annotated with the
// invariant it relies on.
#![allow(unsafe_code)]

use std::sync::Arc;

use beastube_filtering::engine::FilterEngine;
use beastube_filtering::ruleset::RuleSetManager;
use tauri::WebviewWindow;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL, ICoreWebView2Environment,
    ICoreWebView2WebResourceRequestedEventArgs,
};
use webview2_com::WebResourceRequestedEventHandler;
use windows::core::{PWSTR, w};

/// Status code returned for a blocked request.
///
/// 403 rather than a connection failure: a page that receives a well-formed refusal renders its
/// fallback, whereas an aborted request often produces a console error and a retry loop.
const BLOCKED_STATUS: i32 = 403;

/// Attaches the filter to `window`'s webview.
///
/// Registration happens on the webview's own thread — that is what `with_webview` guarantees — and
/// the handler outlives this call because WebView2 holds the only reference it needs.
///
/// Failure is logged and otherwise ignored. A webview whose request filter could not be attached
/// still browses and still plays; it simply does not filter, which the diagnostics screen shows as
/// zero evaluated requests rather than claiming a protection that is not there.
pub(crate) fn attach(window: &WebviewWindow, filtering: Arc<RuleSetManager>) {
    let result = window.with_webview(move |webview| {
        if let Err(error) = install(&webview, &filtering) {
            tracing::error!(%error, "request filtering could not be attached to the webview");
        } else {
            tracing::info!("request filtering attached to the webview");
        }
    });

    if let Err(error) = result {
        tracing::error!(%error, "the platform webview handle is unavailable");
    }
}

/// Registers the filter on the WebView2 instance.
fn install(
    webview: &tauri::webview::PlatformWebview,
    filtering: &Arc<RuleSetManager>,
) -> windows::core::Result<()> {
    let environment: ICoreWebView2Environment = webview.environment();
    let engine = Arc::clone(filtering);

    let handler = WebResourceRequestedEventHandler::create(Box::new(move |_sender, args| {
        // A missing args pointer is a WebView2 contract violation; there is no request to decide
        // about, so there is nothing to do but let it proceed.
        let Some(args) = args else {
            return Ok(());
        };
        // The engine snapshot is taken per request so a rule-set activation takes effect on the
        // next request rather than on the next launch.
        decide(&engine.engine(), &environment, &args);
        Ok(())
    }));

    // SAFETY: the controller is a live COM object owned by the webview, and this runs on the thread
    // that owns it. The filter string and the token are valid for the duration of each call.
    unsafe {
        let core = webview.controller().CoreWebView2()?;
        // `*` with ALL context means every request the webview makes is offered to the handler.
        // Filtering narrowly here would mean the rule set could not decide about a category the
        // filter had not anticipated.
        core.AddWebResourceRequestedFilter(w!("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?;
        let mut token = 0_i64;
        core.add_WebResourceRequested(&handler, &raw mut token)?;
    }

    Ok(())
}

/// Applies the rule set to one request, substituting a refusal when it is blocked.
fn decide(
    engine: &FilterEngine,
    environment: &ICoreWebView2Environment,
    args: &ICoreWebView2WebResourceRequestedEventArgs,
) {
    let Some(url) = request_url(args) else {
        return;
    };
    let Some(host) = host_of(&url) else {
        // A request with no host is a data:, blob: or about: URL — nothing a network rule applies
        // to, and nothing worth counting as an evaluated request.
        return;
    };

    if engine.decide(&host, &url).is_allowed() {
        return;
    }

    // SAFETY: both COM objects are live and owned by the caller; the response is released when the
    // local binding drops.
    let refusal = unsafe {
        environment.CreateWebResourceResponse(None, BLOCKED_STATUS, w!("Blocked"), w!(""))
    };

    match refusal {
        // SAFETY: `args` is live for the duration of the event callback.
        Ok(response) => unsafe {
            if let Err(error) = args.SetResponse(&response) {
                tracing::warn!(%error, "a blocked request could not be refused; allowing it");
            }
        },
        Err(error) => {
            // Failing open: the alternative is an unanswered request that hangs the page.
            tracing::warn!(%error, "a refusal response could not be built; allowing the request");
        }
    }
}

/// Reads the request URL.
///
/// Returns `None` on any failure, which allows the request — see the failure policy above.
fn request_url(args: &ICoreWebView2WebResourceRequestedEventArgs) -> Option<String> {
    // SAFETY: `args` is live for the duration of the callback, and `Uri` writes a `CoTaskMemAlloc`
    // string that `take_pwstr` takes ownership of and frees.
    unsafe {
        let request = args.Request().ok()?;
        let mut raw = PWSTR::null();
        request.Uri(&raw mut raw).ok()?;
        if raw.is_null() {
            return None;
        }
        Some(webview2_com::take_pwstr(raw))
    }
}

/// Extracts the host from a URL, lowercased.
///
/// Parsed with the same URL crate the rest of the application uses rather than by string surgery:
/// a hand-rolled split is exactly how `https://evil.com/@youtube.com/` ends up being treated as a
/// request to YouTube.
fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .host_str()
        .map(str::to_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_is_taken_from_the_authority_not_from_the_text() {
        // The whole reason this parses rather than splits: everything after the authority is
        // attacker-controlled and must not be able to impersonate a host.
        assert_eq!(
            host_of("https://www.youtube.com/watch?v=x").as_deref(),
            Some("www.youtube.com")
        );
        assert_eq!(
            host_of("https://evil.example/@www.youtube.com/x").as_deref(),
            Some("evil.example")
        );
        assert_eq!(
            host_of("https://WWW.YouTube.COM/x").as_deref(),
            Some("www.youtube.com"),
            "hosts are compared lowercased, so rules need only one form"
        );
    }

    #[test]
    fn schemes_without_a_host_are_not_network_requests() {
        assert_eq!(host_of("data:text/plain,hello"), None);
        assert_eq!(host_of("about:blank"), None);
        assert_eq!(host_of("not a url"), None);
    }
}
