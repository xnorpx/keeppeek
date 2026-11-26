//! Publishable receiver service for opt-in KeepPeek usage reporting.
//!
//! Raw per-installation rows are pseudonymous, expire within 60 days, never persist
//! source IP addresses or location, and are never exposed publicly.

pub mod aggregate;
pub mod http;
pub mod retention;
pub mod schema;
pub mod store;
