use super::{Clock, Error};

/// Advances effective expiry time within one runtime without trusting a frozen wall clock.
/// The durable Store watermark separately rejects observed rollback across restart.
#[derive(Debug)]
pub struct ClockAnchor {
    anchor: Clock,
    last_raw: Clock,
}

impl ClockAnchor {
    pub const fn new(clock: Clock) -> Result<Self, Error> {
        if clock.unix_ms < 0 {
            return Err(Error::Invalid);
        }
        Ok(Self {
            anchor: clock,
            last_raw: clock,
        })
    }

    pub fn sample(&mut self, raw: Clock) -> Result<Clock, Error> {
        if raw.unix_ms < self.last_raw.unix_ms || raw.monotonic < self.last_raw.monotonic {
            return Err(Error::Denied);
        }
        let elapsed = raw
            .monotonic
            .checked_duration_since(self.anchor.monotonic)
            .ok_or(Error::Denied)?;
        let elapsed_ms = i64::try_from(elapsed.as_millis()).map_err(|_| Error::Denied)?;
        let projected = self
            .anchor
            .unix_ms
            .checked_add(elapsed_ms)
            .ok_or(Error::Denied)?;
        if raw.unix_ms > projected {
            self.anchor = raw;
        }
        self.last_raw = raw;
        Ok(Clock {
            unix_ms: raw.unix_ms.max(projected),
            monotonic: raw.monotonic,
        })
    }
}
