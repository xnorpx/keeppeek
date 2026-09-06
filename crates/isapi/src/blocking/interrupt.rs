use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ureq::unversioned::transport::{Buffers, ConnectionDetails, Connector, NextTimeout, Transport};

use crate::Error;
use crate::error::Kind;

const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub struct Control {
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    deadline: Arc<Mutex<Option<Instant>>>,
}

impl Control {
    pub fn new(cancelled: Arc<dyn Fn() -> bool + Send + Sync>, deadline: Option<Instant>) -> Self {
        Self {
            cancelled,
            deadline: Arc::new(Mutex::new(deadline)),
        }
    }

    pub fn set_deadline(&self, deadline: Option<Instant>) {
        *self
            .deadline
            .lock()
            .expect("ISAPI deadline mutex is not poisoned") = deadline;
    }

    pub fn check(&self) -> Result<(), ureq::Error> {
        if (self.cancelled)() {
            return Err(ureq::Error::Other(Box::new(Error::new(Kind::Cancelled))));
        }
        if self
            .deadline
            .lock()
            .expect("ISAPI deadline mutex is not poisoned")
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(ureq::Error::Timeout(ureq::Timeout::Global));
        }
        Ok(())
    }

    pub fn remaining(&self, deadline: Instant) -> Result<Duration, ureq::Error> {
        self.check()?;
        let until = self
            .deadline
            .lock()
            .expect("ISAPI deadline mutex is not poisoned")
            .map_or(deadline, |session| session.min(deadline));
        until
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(ureq::Error::Timeout(ureq::Timeout::Global))
    }
}

impl fmt::Debug for Control {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Control").finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct Interruptible(Control);

impl Interruptible {
    pub const fn new(control: Control) -> Self {
        Self(control)
    }
}

impl<Inner: Transport> Connector<Inner> for Interruptible {
    type Out = Guarded<Inner>;

    fn connect(
        &self,
        _: &ConnectionDetails,
        chained: Option<Inner>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        self.0.check()?;
        Ok(chained.map(|inner| Guarded {
            inner,
            control: self.0.clone(),
        }))
    }
}

#[derive(Debug)]
pub struct Guarded<Inner> {
    inner: Inner,
    control: Control,
}

impl<Inner: Transport> Transport for Guarded<Inner> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.control.check()?;
        self.inner.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let deadline = timeout
            .not_zero()
            .map(|duration| Instant::now() + *duration);
        loop {
            self.control.check()?;
            let remaining = deadline.map_or(POLL_INTERVAL, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
            if remaining.is_zero() {
                return Err(ureq::Error::Timeout(timeout.reason));
            }
            let slice = NextTimeout {
                after: remaining.min(POLL_INTERVAL).into(),
                reason: timeout.reason,
            };
            match self.inner.await_input(slice) {
                Err(ureq::Error::Timeout(_)) => {}
                result => return result,
            }
        }
    }

    fn is_open(&mut self) -> bool {
        self.control.check().is_ok() && self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

#[cfg(test)]
mod tests;
