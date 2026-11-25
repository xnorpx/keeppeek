//! Desktop client core logic for KeepPeek on Windows and macOS.
//!
//! `keeppeek-desktop` connects to an independently managed KeepPeek server and
//! renders that server's existing SvelteKit UI in a native Tauri webview.
//! It does not spawn, own, or terminate any backend server process.
//!
//! All server URL validation, configuration storage, navigation allowlisting,
//! and bounded server probing live in platform-independent modules so that
//! automated tests execute consistently across Linux, macOS, and Windows.

pub mod config_store;
pub mod navigation;
pub mod probe;
pub mod server_url;
