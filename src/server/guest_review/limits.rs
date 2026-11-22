use std::{
    collections::HashMap,
    net::IpAddr,
    time::{Duration, Instant},
};

const WINDOW: Duration = Duration::from_secs(60);
const MAX_ADDRESSES: usize = 128;
const CHUNK_BYTES: u64 = 65_536;
const GLOBAL_RATE: u64 = 64 * 1_024 * 1_024;
const ADDRESS_RATE: u64 = 16 * 1_024 * 1_024;

#[derive(Clone, Copy)]
pub(super) enum Action {
    Exchange,
    Status,
    Media,
    Download,
}

impl Action {
    const fn limits(self) -> (usize, u32, u32) {
        match self {
            Self::Exchange => (0, 20, 120),
            Self::Status => (1, 240, 1_200),
            Self::Media => (2, 120, 480),
            Self::Download => (3, 12, 48),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Limited;

struct Window {
    started: Instant,
    counts: [u32; 4],
}

struct Address {
    window: Window,
    egress: Tokens,
}

struct Tokens {
    available: u64,
    capacity: u64,
    rate: u64,
    refilled: Instant,
}

impl Tokens {
    const fn new(capacity: u64, rate: u64, now: Instant) -> Self {
        Self {
            available: capacity,
            capacity,
            rate,
            refilled: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.refilled).as_nanos();
        let added = elapsed.saturating_mul(u128::from(self.rate)) / 1_000_000_000;
        let added = u64::try_from(added.min(u128::from(self.capacity)))
            .expect("capped token credit fits u64");
        if added > 0 || self.available == self.capacity {
            self.refilled = now;
            self.available = self.available.saturating_add(added).min(self.capacity);
        }
    }

    const fn delay(&self, bytes: u64) -> Duration {
        let missing = bytes.saturating_sub(self.available);
        Duration::from_nanos((missing * 1_000_000_000).div_ceil(self.rate))
    }
}

pub(super) struct Budget {
    addresses: HashMap<IpAddr, Address>,
    global: Window,
    egress: Tokens,
}

impl Budget {
    pub fn new(now: Instant) -> Self {
        Self {
            addresses: HashMap::new(),
            global: Window {
                started: now,
                counts: [0; 4],
            },
            egress: Tokens::new(4 * CHUNK_BYTES, GLOBAL_RATE, now),
        }
    }

    pub fn admit(&mut self, peer: IpAddr, action: Action, now: Instant) -> Result<(), Limited> {
        if now.saturating_duration_since(self.global.started) >= WINDOW {
            self.global = Window {
                started: now,
                counts: [0; 4],
            };
        }
        // ponytail: scan at most 128 transport peers; index expiry only if this ceiling changes.
        self.addresses.retain(|_, address| {
            now.saturating_duration_since(address.window.started) < WINDOW
                || now.saturating_duration_since(address.egress.refilled) < WINDOW
        });
        if !self.addresses.contains_key(&peer) && self.addresses.len() >= MAX_ADDRESSES {
            return Err(Limited);
        }
        let (index, address_limit, global_limit) = action.limits();
        if self.global.counts[index] >= global_limit {
            return Err(Limited);
        }
        let address = self.addresses.entry(peer).or_insert_with(|| Address {
            window: Window {
                started: now,
                counts: [0; 4],
            },
            egress: Tokens::new(CHUNK_BYTES, ADDRESS_RATE, now),
        });
        if now.saturating_duration_since(address.window.started) >= WINDOW {
            address.window = Window {
                started: now,
                counts: [0; 4],
            };
        }
        if address.window.counts[index] >= address_limit {
            return Err(Limited);
        }
        address.window.counts[index] += 1;
        self.global.counts[index] += 1;
        Ok(())
    }

    pub fn egress_delay(
        &mut self,
        peer: IpAddr,
        bytes: u64,
        now: Instant,
    ) -> Result<Duration, Limited> {
        if bytes == 0 || bytes > CHUNK_BYTES {
            return Err(Limited);
        }
        let address = self.addresses.get_mut(&peer).ok_or(Limited)?;
        self.egress.refill(now);
        address.egress.refill(now);
        let wait = self.egress.delay(bytes).max(address.egress.delay(bytes));
        if wait.is_zero() {
            self.egress.available -= bytes;
            address.egress.available -= bytes;
        }
        Ok(wait)
    }
}

#[cfg(test)]
#[path = "limit_tests.rs"]
mod tests;
