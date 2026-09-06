use std::io::Result as IoResult;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr};

use crate::connection::Connection;
use crate::ssl::SslStream;

pub enum Stream {
    Http(Connection),
    Https(SslStream),
}

impl Clone for Stream {
    fn clone(&self) -> Self {
        match self {
            Self::Http(tcp_stream) => Self::Http(tcp_stream.try_clone().unwrap()),
            Self::Https(ssl_stream) => Self::Https(ssl_stream.clone()),
        }
    }
}

impl From<Connection> for Stream {
    fn from(tcp_stream: Connection) -> Self {
        Self::Http(tcp_stream)
    }
}

impl Stream {
    const fn secure(&self) -> bool {
        match self {
            Self::Http(_) => false,
            Self::Https(_) => true,
        }
    }

    fn peer_addr(&mut self) -> IoResult<Option<SocketAddr>> {
        match self {
            Self::Http(tcp_stream) => tcp_stream.peer_addr(),
            Self::Https(ssl_stream) => ssl_stream.peer_addr(),
        }
    }

    fn shutdown(&mut self, how: Shutdown) -> IoResult<()> {
        match self {
            Self::Http(tcp_stream) => tcp_stream.shutdown(how),
            Self::Https(ssl_stream) => ssl_stream.shutdown(how),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        match self {
            Self::Http(tcp_stream) => tcp_stream.read(buf),
            Self::Https(ssl_stream) => ssl_stream.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        match self {
            Self::Http(tcp_stream) => tcp_stream.write(buf),
            Self::Https(ssl_stream) => ssl_stream.write(buf),
        }
    }

    fn flush(&mut self) -> IoResult<()> {
        match self {
            Self::Http(tcp_stream) => tcp_stream.flush(),
            Self::Https(ssl_stream) => ssl_stream.flush(),
        }
    }
}

pub struct RefinedTcpStream {
    stream: Stream,
    read_deadline: Option<std::time::Instant>,
    close_read: bool,
    close_write: bool,
}

impl RefinedTcpStream {
    pub(crate) fn new<S>(stream: S) -> (Self, Self)
    where
        S: Into<Stream>,
    {
        let stream: Stream = stream.into();

        let (read, write) = (stream.clone(), stream);

        let read = Self {
            stream: read,
            read_deadline: None,
            close_read: true,
            close_write: false,
        };

        let write = Self {
            stream: write,
            read_deadline: None,
            close_read: false,
            close_write: true,
        };

        (read, write)
    }

    /// Returns true if this struct wraps around a secure connection.
    #[inline]
    pub(crate) const fn secure(&self) -> bool {
        self.stream.secure()
    }

    pub(crate) fn peer_addr(&mut self) -> IoResult<Option<SocketAddr>> {
        self.stream.peer_addr()
    }

    pub(crate) const fn set_read_deadline(&mut self, deadline: std::time::Instant) {
        self.read_deadline = Some(deadline);
    }
}

impl Drop for RefinedTcpStream {
    fn drop(&mut self) {
        if self.close_read {
            self.stream.shutdown(Shutdown::Read).ok();
        }

        if self.close_write {
            self.stream.shutdown(Shutdown::Write).ok();
        }
    }
}

impl Read for RefinedTcpStream {
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        if self
            .read_deadline
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "request read deadline expired",
            ));
        }
        self.stream.read(buf)
    }
}

impl Write for RefinedTcpStream {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> IoResult<()> {
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::{Connection, RefinedTcpStream};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Instant;

    #[test]
    fn absolute_read_deadline_rejects_even_available_input() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let socket = listener.accept().unwrap().0;
        let (mut reader, _writer) = RefinedTcpStream::new(Connection::from(socket));
        peer.write_all(b"still sending").unwrap();
        reader.set_read_deadline(Instant::now());
        assert_eq!(
            reader.read(&mut [0; 32]).unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
    }
}
