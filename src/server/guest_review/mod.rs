//! Owns bounded guest transport independently of ordinary API roles.

mod codecs;
mod http;
mod limits;
mod media;
mod ranges;
mod registry;
mod session;
mod verified;

pub(super) use http::handle;
pub(super) use registry::Registry;

pub(super) fn tick(state: &super::ServerState, watchdog: &std::thread::JoinHandle<()>) {
    state.guest_review.sweep(state);
    super::sharing::maintain(state);
    if watchdog.is_finished() {
        state.guest_review.stop();
    }
}

pub(super) fn stop(state: &super::ServerState, watchdog: std::thread::JoinHandle<()>) {
    state.guest_review.stop();
    if watchdog.join().is_err() {
        tracing::warn!("guest cancellation worker stopped unexpectedly");
    }
}
