use std::{
    io::{self, Write},
    sync::atomic::{AtomicBool, Ordering},
};

// Only the foreground text sink shares its terminal with the dashboard.
static PAUSED: AtomicBool = AtomicBool::new(false);

pub struct ConsolePause;

impl ConsolePause {
    pub(crate) fn acquire() -> io::Result<Self> {
        // Let any stderr record already being written finish before entering the alternate screen.
        let _previous_writer = io::stderr().lock();
        PAUSED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "terminal dashboard already open",
                )
            })?;
        Ok(Self)
    }
}

impl Drop for ConsolePause {
    fn drop(&mut self) {
        PAUSED.store(false, Ordering::Release);
    }
}

pub(super) fn write(output: &[u8]) {
    if PAUSED.load(Ordering::Acquire) {
        return;
    }
    let mut stderr = io::stderr().lock();
    if !PAUSED.load(Ordering::Acquire) {
        let _ = stderr.write_all(output);
    }
}
