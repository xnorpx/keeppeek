//! Single-owner bounded retention expiry enforcement (maximum 60 days).

use crate::store::{ExpiryReport, ReceiverStore, StoreError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const MAX_RETENTION_DAYS: u32 = 60;
pub const MAX_RETENTION_SECS: i64 = (MAX_RETENTION_DAYS as i64) * 86_400;
pub const DEFAULT_EXPIRY_SWEEP_INTERVAL: Duration = Duration::from_secs(3_600);

pub fn run_expiry_pass(
    store: &mut ReceiverStore,
    now_secs: i64,
) -> Result<ExpiryReport, StoreError> {
    store.purge_expired(now_secs)
}

pub fn spawn_expiry_worker(
    store: Arc<Mutex<ReceiverStore>>,
    interval: Duration,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while !shutdown.load(std::sync::atomic::Ordering::Acquire) {
            std::thread::sleep(interval.min(Duration::from_secs(60)));
            if shutdown.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            let now_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| {
                    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
                });
            if let Ok(mut guard) = store.lock() {
                let _ = run_expiry_pass(&mut guard, now_secs);
            }
        }
    })
}
