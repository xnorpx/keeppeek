//! Navigation and popup allowlist rules for the KeepPeek Desktop webview.
//!
//! The desktop webview allows navigation only to the bundled Tauri launcher
//! origin and the active verified KeepPeek server origin. All other schemes,
//! credentialed URLs, third-party origins, and popup windows are blocked.

use url::Url;

use crate::server_url::ServerOrigin;

/// Bundled Tauri launcher origins across Windows WebView2 and macOS WKWebView.
const LAUNCHER_ORIGINS: &[&str] = &[
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

/// Outcome of evaluating a top-level webview navigation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationDecision {
    /// Navigation targets the bundled KeepPeek Desktop launcher.
    AllowLauncher,
    /// Navigation targets the active configured KeepPeek server origin.
    AllowConfiguredServer,
    /// Navigation uses a disallowed URL scheme such as `file:` or `javascript:`.
    BlockUntrustedScheme,
    /// Navigation targets an origin outside the launcher and active server.
    BlockUntrustedOrigin,
}

impl NavigationDecision {
    /// Returns true when the webview may proceed with the navigation.
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::AllowLauncher | Self::AllowConfiguredServer)
    }
}

/// Evaluates whether the desktop webview may navigate to `url`.
///
/// The launcher origin is always permitted so the user can recover or change
/// servers. HTTP(S) navigation is allowed only when the exact origin matches
/// `allowed_server` and the URL carries no embedded credentials.
#[must_use]
pub fn decide_navigation(url: &Url, allowed_server: Option<&ServerOrigin>) -> NavigationDecision {
    if is_launcher_url(url) {
        return NavigationDecision::AllowLauncher;
    }

    if !matches!(url.scheme(), "http" | "https") {
        return NavigationDecision::BlockUntrustedScheme;
    }

    if !url.username().is_empty() || url.password().is_some() {
        return NavigationDecision::BlockUntrustedOrigin;
    }

    let Some(server) = allowed_server else {
        return NavigationDecision::BlockUntrustedOrigin;
    };

    let candidate_origin = url.origin().ascii_serialization();
    if candidate_origin != "null" && candidate_origin == server.as_str() {
        NavigationDecision::AllowConfiguredServer
    } else {
        NavigationDecision::BlockUntrustedOrigin
    }
}

/// Returns true when `url` belongs to the bundled KeepPeek Desktop launcher.
#[must_use]
pub fn is_launcher_url(url: &Url) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    if url.scheme() == "tauri" {
        return url.host_str() == Some("localhost") && url.port().is_none();
    }
    let origin = url.origin().ascii_serialization();
    LAUNCHER_ORIGINS.contains(&origin.as_str())
}

/// Evaluates whether the webview may open a child or popup window for `url`.
///
/// KeepPeek Desktop denies all popup windows to prevent untrusted external
/// links from escaping webview navigation controls.
#[must_use]
pub const fn allow_new_window(_url: &Url) -> bool {
    false
}
