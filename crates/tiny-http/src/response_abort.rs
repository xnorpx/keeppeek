use crate::connection::Connection;
use std::{
    fmt,
    io::{self, Write},
    net::Shutdown,
    sync::{Arc, Mutex},
    time::Duration,
};

const BOUNDED_WRITE_TIMEOUT: Duration = Duration::from_millis(250);

struct State {
    connection: Option<Connection>,
    original_timeout: Duration,
    bounded: bool,
    started: bool,
    timeout_armed: bool,
    aborted: bool,
    #[cfg(test)]
    restore_error: bool,
}

impl State {
    fn restore_original_timeout(&self) -> io::Result<()> {
        #[cfg(test)]
        if self.restore_error {
            return Err(io::Error::other("injected timeout restoration failure"));
        }
        self.connection.as_ref().map_or(Ok(()), |connection| {
            connection.set_write_timeout(Some(self.original_timeout))
        })
    }
}

/// Stops an unfinished response. Bounded writes require explicit opt-in before writing.
/// A completed response disarms every clone, including on keep-alive connections.
#[derive(Clone)]
pub struct ResponseAbortHandle {
    state: Arc<Mutex<State>>,
}

impl fmt::Debug for ResponseAbortHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ResponseAbortHandle")
    }
}

impl ResponseAbortHandle {
    pub(crate) fn new(connection: Connection, original_timeout: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                connection: Some(connection),
                original_timeout,
                bounded: false,
                started: false,
                timeout_armed: false,
                aborted: false,
                #[cfg(test)]
                restore_error: false,
            })),
        }
    }

    /// Enables a short send timeout after this response owns the writer turn.
    /// Call this before returning the response. Acquiring a handle is passive.
    ///
    /// # Errors
    /// Returns an error after writing starts or when the response is finished or aborted.
    pub fn enable_bounded_writes(&self) -> io::Result<()> {
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        if state.started || state.aborted || state.connection.is_none() {
            return Err(io::Error::other(
                "response can no longer enable bounded writes",
            ));
        }
        state.bounded = true;
        Ok(())
    }

    /// Shuts down the connection. Other requests sharing this connection also stop.
    /// Returns false after this response completes.
    ///
    /// # Errors
    /// Returns socket shutdown errors or a poisoned response-state error.
    pub fn abort(&self) -> io::Result<bool> {
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        state.aborted = true;
        state.connection.as_ref().map_or(Ok(false), |connection| {
            ignore_disconnected_shutdown(connection.shutdown(Shutdown::Both))?;
            Ok(true)
        })
    }

    /// Returns true after this response writer closes and all handles disarm.
    ///
    /// # Errors
    /// Returns an error when the response-state lock is poisoned.
    pub fn is_finished(&self) -> io::Result<bool> {
        self.state
            .lock()
            .map(|state| state.connection.is_none())
            .map_err(|_| poisoned())
    }

    fn begin_write(&self) -> io::Result<bool> {
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        if state.aborted || state.connection.is_none() {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        let first = !state.started;
        state.started = true;
        Ok(first && state.bounded)
    }

    fn arm_timeout(&self) -> io::Result<()> {
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        if state.aborted {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        let connection = state
            .connection
            .as_ref()
            .ok_or(io::ErrorKind::ConnectionAborted)?;
        connection.set_write_timeout(Some(BOUNDED_WRITE_TIMEOUT))?;
        state.timeout_armed = true;
        Ok(())
    }

    fn finish(&self) -> io::Result<()> {
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        let restore = if state.timeout_armed && !state.aborted {
            state.restore_original_timeout()
        } else {
            Ok(())
        };
        if restore.is_err() {
            if let Some(connection) = &state.connection {
                let _ = connection.shutdown(Shutdown::Both);
            }
        }
        state.connection = None;
        restore
    }

    pub(crate) fn wrap(&self, writer: Box<dyn Write + Send>) -> Box<dyn Write + Send> {
        Box::new(ResponseWriter {
            writer,
            handle: self.clone(),
        })
    }
}

fn poisoned() -> io::Error {
    io::Error::other("response cancellation state is poisoned")
}

fn ignore_disconnected_shutdown(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotConnected => Ok(()),
        result => result,
    }
}

struct ResponseWriter {
    writer: Box<dyn Write + Send>,
    handle: ResponseAbortHandle,
}

impl ResponseWriter {
    fn prepare(&mut self) -> io::Result<()> {
        let result = (|| {
            if self.handle.begin_write()? {
                // This flush acquires the sequential writer turn before changing its socket option.
                self.writer.flush()?;
                self.handle.arm_timeout()?;
            }
            Ok(())
        })();
        self.abort_on_error(result)
    }

    fn abort_on_error<T>(&self, result: io::Result<T>) -> io::Result<T> {
        if result.is_err() {
            let _ = self.handle.abort();
        }
        result
    }
}

impl Write for ResponseWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.prepare()?;
        let result = self.writer.write(buffer);
        self.abort_on_error(result)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.prepare()?;
        let result = self.writer.flush();
        self.abort_on_error(result)
    }
}

impl Drop for ResponseWriter {
    fn drop(&mut self) {
        // Keep the turn barrier on cancellation; a queued drop must not release the next writer.
        if self.writer.flush().is_err() {
            let _ = self.handle.abort();
        }
        // Restore before the inner SequentialWriter drops and signals its successor.
        if self.handle.finish().is_err() {
            log::debug!("response write-timeout restoration failed; connection closed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::SequentialWriterBuilder;
    use std::{
        io::BufWriter,
        net::{TcpListener, TcpStream},
        sync::mpsc,
        time::Instant,
    };

    fn pair() -> (TcpStream, Connection) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let socket = listener.accept().unwrap().0;
        socket
            .set_write_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        (peer, Connection::from(socket))
    }

    fn handle(socket: &Connection) -> ResponseAbortHandle {
        ResponseAbortHandle::new(socket.try_clone().unwrap(), Duration::from_secs(30))
    }

    fn timeout(connection: &Connection) -> Option<Duration> {
        match connection {
            Connection::Tcp(stream) => stream.write_timeout().unwrap(),
            #[cfg(unix)]
            Connection::Unix(stream) => stream.write_timeout().unwrap(),
        }
    }

    fn wait_started(handle: &ResponseAbortHandle) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !handle.state.lock().unwrap().started && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(handle.state.lock().unwrap().started);
    }

    #[test]
    fn queued_opt_in_preserves_prior_timeout_and_restores_next_normal_response() {
        let (_peer, socket) = pair();
        let observer = socket.try_clone().unwrap();
        let mut writers = SequentialWriterBuilder::new(BufWriter::new(socket));
        let first_handle = handle(&observer);
        let mut first = first_handle.wrap(Box::new(writers.next().unwrap()));
        let guest_handle = handle(&observer);
        guest_handle.enable_bounded_writes().unwrap();
        let mut guest = guest_handle.wrap(Box::new(writers.next().unwrap()));
        let last_handle = handle(&observer);
        let mut last = last_handle.wrap(Box::new(writers.next().unwrap()));
        let (written_tx, written_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            guest.write_all(b"guest").unwrap();
            guest.flush().unwrap();
            written_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            drop(guest);
        });
        wait_started(&guest_handle);
        let queued_timeout = timeout(&observer);
        first.write_all(b"first").unwrap();
        first.flush().unwrap();
        let normal_timeout = timeout(&observer);
        drop(first);
        written_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let active_timeout = timeout(&observer);
        release_tx.send(()).unwrap();
        thread.join().unwrap();
        let restored_timeout = timeout(&observer);
        last.write_all(b"last").unwrap();
        last.flush().unwrap();
        drop(last);
        assert_eq!(queued_timeout, Some(Duration::from_secs(30)));
        assert_eq!(normal_timeout, Some(Duration::from_secs(30)));
        assert_eq!(active_timeout, Some(BOUNDED_WRITE_TIMEOUT));
        assert_eq!(restored_timeout, Some(Duration::from_secs(30)));
        assert!(first_handle.is_finished().unwrap());
        assert!(guest_handle.is_finished().unwrap());
        assert!(!guest_handle.abort().unwrap());
        assert!(last_handle.is_finished().unwrap());
    }

    #[test]
    fn restoration_failure_closes_the_connection_before_disarming() {
        use std::io::Read as _;
        let (mut peer, socket) = pair();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let observer = socket.try_clone().unwrap();
        let mut writers = SequentialWriterBuilder::new(BufWriter::new(socket));
        let handle = handle(&observer);
        handle.enable_bounded_writes().unwrap();
        let mut writer = handle.wrap(Box::new(writers.next().unwrap()));
        writer.write_all(b"x").unwrap();
        writer.flush().unwrap();
        assert_eq!(timeout(&observer), Some(BOUNDED_WRITE_TIMEOUT));
        handle.state.lock().unwrap().restore_error = true;
        drop(writer);
        assert!(handle.is_finished().unwrap());
        assert!(!handle.abort().unwrap());
        assert_eq!(timeout(&observer), Some(BOUNDED_WRITE_TIMEOUT));
        let mut sent = [0];
        peer.read_exact(&mut sent).unwrap();
        assert_eq!(sent, *b"x");
        let terminated = match peer.read(&mut [0]) {
            Ok(0) => true,
            Err(error) => matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
            ),
            _ => false,
        };
        assert!(
            terminated,
            "failed restoration must not leave a reusable connection"
        );
    }

    #[test]
    fn abort_accepts_an_already_disconnected_connection() {
        assert!(ignore_disconnected_shutdown(Err(io::ErrorKind::NotConnected.into())).is_ok());
        assert_eq!(
            ignore_disconnected_shutdown(Err(io::Error::other("shutdown failed")))
                .unwrap_err()
                .to_string(),
            "shutdown failed"
        );
    }
}
