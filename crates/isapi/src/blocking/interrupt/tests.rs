use super::Control;
use std::io::{self, Read};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct CancelledBody(Control);

impl Read for CancelledBody {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        self.0.check().map_err(ureq::Error::into_io)?;
        Ok(0)
    }
}

#[test]
fn cancellation_is_not_retried_by_read_to_end() {
    let cancellation = AtomicBool::new(true);
    let mut body = CancelledBody(Control::new(
        Arc::new(move || cancellation.swap(false, Ordering::AcqRel)),
        None,
    ));
    let error = body.read_to_end(&mut Vec::new()).unwrap_err();
    assert_ne!(error.kind(), io::ErrorKind::Interrupted);
    assert!(crate::Error::from(error).is_cancelled());
}
