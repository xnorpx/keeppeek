//! Desktop client core logic for KeepPeek on Windows and macOS.
//!
//! By default, `keeppeek-desktop` connects to an independently managed KeepPeek
//! server and renders its SvelteKit UI in a native Tauri webview without
//! spawning or stopping any backend process. When the operator explicitly opts
//! in via `[desktop].manage_local_server = true` or `--manage-local-server`,
//! the trusted Rust backend embeds `keeppeek-supervisor` to supervise a local
//! `keeppeek` child executable while preserving strict Tauri CSP and capability
//! boundaries.
//!
//! All server URL validation, configuration storage, navigation allowlisting,
//! supervisor configuration, and bounded server probing live in
//! platform-independent modules so automated tests run consistently across
//! Linux, macOS, and Windows.

pub mod config_store;
pub mod managed_server;
pub mod navigation;
pub mod probe;
pub mod server_url;
