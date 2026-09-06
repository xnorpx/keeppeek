use std::fmt;
use std::sync::{Arc, Mutex};

use ureq::unversioned::transport::{Buffers, ConnectionDetails, Connector, NextTimeout, Transport};

#[derive(Clone, Default)]
pub struct Capture(Arc<Mutex<Option<Box<dyn Transport>>>>);

impl Capture {
    pub fn take(&self) -> Option<Box<dyn Transport>> {
        self.0
            .lock()
            .expect("audio transport capture is not poisoned")
            .take()
    }
}

impl fmt::Debug for Capture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Capture").finish_non_exhaustive()
    }
}

impl<Inner: Transport> Connector<Inner> for Capture {
    type Out = Retained<Inner>;

    fn connect(
        &self,
        _: &ConnectionDetails,
        chained: Option<Inner>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(|inner| Retained {
            inner: Some(inner),
            capture: self.clone(),
        }))
    }
}

pub struct Retained<Inner: Transport> {
    inner: Option<Inner>,
    capture: Capture,
}

impl<Inner: Transport> Retained<Inner> {
    const fn inner(&mut self) -> &mut Inner {
        self.inner
            .as_mut()
            .expect("audio transport exists until drop")
    }
}

impl<Inner: Transport> Transport for Retained<Inner> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner().buffers()
    }
    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.inner().transmit_output(amount, timeout)
    }
    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        self.inner().await_input(timeout)
    }
    fn is_open(&mut self) -> bool {
        self.inner().is_open()
    }
    fn is_tls(&self) -> bool {
        self.inner.as_ref().is_some_and(Transport::is_tls)
    }
}

impl<Inner: Transport> Drop for Retained<Inner> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            *self
                .capture
                .0
                .lock()
                .expect("audio transport capture is not poisoned") = Some(Box::new(inner));
        }
    }
}

impl<Inner: Transport> fmt::Debug for Retained<Inner> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Retained").finish_non_exhaustive()
    }
}
