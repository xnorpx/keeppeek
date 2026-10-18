//! Owns bounded guest grants independently of normal authentication and media transports.

mod clock;
mod database;
mod model;
mod receipts;
mod record;
mod secret;
mod sessions;
mod store;

pub use clock::ClockAnchor;
pub use model::{
    AUDIT_RETENTION_MS, Access, BoundSession, Clock, CreateGrant, Error, ExportBinding,
    GrantSummary, IssuedGrant, IssuedSession, IssuerBinding, MAX_MEDIA_BYTES,
    MAX_MEDIA_DURATION_MS, MediaDeadline, Mode, SessionContext, Ticket,
};
pub use secret::Secret;
pub use store::Store;

#[cfg(test)]
mod tests;
