use std::time::{Duration, Instant};

use super::client::{ClientError, Failure};

pub(super) struct Deadline<F> {
    until: Instant,
    now: F,
}

impl<F: Fn() -> Instant> Deadline<F> {
    pub(super) fn new(timeout: Duration, now: F) -> Self {
        Self {
            until: now() + timeout,
            now,
        }
    }

    pub(super) fn remaining(&self) -> Result<Duration, ClientError> {
        self.until
            .checked_duration_since((self.now)())
            .filter(|duration| !duration.is_zero())
            .ok_or(ClientError(Failure::Network))
    }
}
