use std::fmt;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, Transport,
};

use crate::blocking::{CONNECT_TIMEOUT, Control};

const SOCKET_POLL: Duration = Duration::from_millis(25);

#[derive(Debug)]
pub struct AudioTcp(pub Control);

impl Connector<()> for AudioTcp {
    type Out = Socket;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<()>,
    ) -> Result<Option<Socket>, ureq::Error> {
        if chained.is_some() || details.config.proxy().is_some() {
            return Err(ureq::Error::ConnectionFailed);
        }
        let timeout = details
            .timeout
            .not_zero()
            .map_or(CONNECT_TIMEOUT, |timeout| *timeout)
            .min(CONNECT_TIMEOUT);
        let deadline = Instant::now() + timeout;
        for address in &details.addrs {
            let remaining = self.0.remaining(deadline)?;
            match TcpStream::connect_timeout(address, remaining) {
                Ok(stream) => {
                    self.0.remaining(deadline)?;
                    stream.set_nodelay(true)?;
                    return Ok(Some(Socket {
                        stream,
                        buffers: LazyBuffers::new(
                            details.config.input_buffer_size(),
                            details.config.output_buffer_size(),
                        ),
                        control: self.0.clone(),
                    }));
                }
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(ureq::Error::ConnectionFailed)
    }
}

pub struct Socket {
    stream: TcpStream,
    buffers: LazyBuffers,
    control: Control,
}

impl Transport for Socket {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        let deadline = Instant::now()
            + timeout
                .not_zero()
                .map_or(CONNECT_TIMEOUT, |timeout| *timeout);
        let bytes = &self.buffers.output()[..amount];
        write_bounded(bytes, &self.control, deadline, |bytes, remaining| {
            self.stream
                .set_write_timeout(Some(remaining.min(SOCKET_POLL)))?;
            self.stream.write(bytes)
        })
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let deadline = Instant::now()
            + timeout
                .not_zero()
                .map_or(CONNECT_TIMEOUT, |timeout| *timeout);
        self.stream
            .set_read_timeout(Some(self.control.remaining(deadline)?))?;
        let count = match self.stream.read(self.buffers.input_append_buf()) {
            Ok(count) => count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(ureq::Error::Timeout(timeout.reason));
            }
            Err(error) => return Err(error.into()),
        };
        self.control.remaining(deadline)?;
        self.buffers.input_appended(count);
        Ok(count > 0)
    }

    fn is_open(&mut self) -> bool {
        false
    }
}

fn write_bounded(
    mut bytes: &[u8],
    control: &Control,
    deadline: Instant,
    mut write: impl FnMut(&[u8], Duration) -> io::Result<usize>,
) -> Result<(), ureq::Error> {
    while !bytes.is_empty() {
        let remaining = control.remaining(deadline)?;
        match write(bytes, remaining) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
            Ok(count) => {
                assert!(
                    count <= bytes.len(),
                    "socket cannot write more bytes than provided"
                );
                bytes = &bytes[count..];
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    control.remaining(deadline)?;
    Ok(())
}

impl fmt::Debug for Socket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Socket").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use crate::blocking::Control;

    #[test]
    fn partial_audio_writes_do_not_repeat_bytes_after_cancellation() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let check = Arc::clone(&cancelled);
        let control = Control::new(Arc::new(move || check.load(Ordering::Acquire)), None);
        let mut received = Vec::new();
        let result = super::write_bounded(
            b"1234",
            &control,
            Instant::now() + Duration::from_secs(1),
            |bytes, _| {
                received.extend_from_slice(&bytes[..2]);
                cancelled.store(true, Ordering::Release);
                Ok(2)
            },
        );
        assert!(crate::Error::from(result.unwrap_err()).is_cancelled());
        assert_eq!(received, b"12");
    }

    #[test]
    fn audio_write_completion_after_session_deadline_is_not_success() {
        let control = Control::new(Arc::new(|| false), None);
        let result = super::write_bounded(
            b"1234",
            &control,
            Instant::now() + Duration::from_secs(1),
            |bytes, _| {
                control.set_deadline(Some(Instant::now()));
                Ok(bytes.len())
            },
        );
        assert!(crate::Error::from(result.unwrap_err()).is_timeout());
    }
}
