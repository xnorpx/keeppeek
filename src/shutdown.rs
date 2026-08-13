use std::{
    process::Command,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

struct State {
    cancelled: Mutex<bool>,
    changed: Condvar,
}

/// A cloneable cancellation signal for blocking worker loops.
#[derive(Clone)]
pub struct Shutdown(Arc<State>);

/// A cloneable signal indicating that the process should restart after shutdown.
#[derive(Clone, Default)]
pub struct Restart(Arc<AtomicBool>);

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}

impl Shutdown {
    /// Creates a signal that has not been cancelled.
    pub fn new() -> Self {
        Self(Arc::new(State {
            cancelled: Mutex::new(false),
            changed: Condvar::new(),
        }))
    }

    /// Requests cancellation and wakes all waiting clones.
    pub fn cancel(&self) {
        let mut cancelled = self.0.cancelled.lock().unwrap();
        *cancelled = true;
        self.0.changed.notify_all();
    }

    /// Returns whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        *self.0.cancelled.lock().unwrap()
    }

    /// Waits for cancellation or until the supplied duration elapses.
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let cancelled = self.0.cancelled.lock().unwrap();
        if *cancelled {
            return true;
        }
        let (cancelled, _) = self.0.changed.wait_timeout(cancelled, timeout).unwrap();
        *cancelled
    }
}

impl Restart {
    pub fn request(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub fn restart_current_process() -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        Err(Command::new(executable).args(args).exec().into())
    }

    #[cfg(not(unix))]
    {
        Command::new(executable).args(args).spawn()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Restart, Shutdown};
    use std::time::Duration;

    #[test]
    fn cancellation_is_visible_to_all_clones() {
        let shutdown = Shutdown::new();
        let clone = shutdown.clone();
        assert!(!clone.wait_timeout(Duration::ZERO));
        shutdown.cancel();
        assert!(clone.is_cancelled());
        assert!(clone.wait_timeout(Duration::ZERO));
    }

    #[test]
    fn restart_request_is_visible_to_all_clones() {
        let restart = Restart::default();
        let clone = restart.clone();
        assert!(!clone.is_requested());
        restart.request();
        assert!(clone.is_requested());
    }
}
