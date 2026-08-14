use crate::connection::Connection;
use crate::util::refined_tcp_stream::Stream as RefinedStream;
use std::error::Error;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr};
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct NativeTlsStream(Arc<Mutex<native_tls::TlsStream<Connection>>>);

impl NativeTlsStream {
    pub(crate) fn peer_addr(&self) -> std::io::Result<Option<SocketAddr>> {
        self.0
            .lock()
            .expect("Failed to lock TLS stream mutex")
            .get_mut()
            .peer_addr()
    }

    pub(crate) fn shutdown(&self, how: Shutdown) -> std::io::Result<()> {
        self.0
            .lock()
            .expect("Failed to lock TLS stream mutex")
            .get_mut()
            .shutdown(how)
    }
}

impl Read for NativeTlsStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("Failed to lock TLS stream mutex")
            .read(buffer)
    }
}

impl Write for NativeTlsStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("Failed to lock TLS stream mutex")
            .write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0
            .lock()
            .expect("Failed to lock TLS stream mutex")
            .flush()
    }
}

pub struct NativeTlsContext(native_tls::TlsAcceptor);

impl NativeTlsContext {
    pub(crate) fn from_pem(
        certificates: Vec<u8>,
        private_key: Zeroizing<Vec<u8>>,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let identity = native_tls::Identity::from_pkcs8(&certificates, &private_key)?;
        Ok(Self(native_tls::TlsAcceptor::new(identity)?))
    }

    pub(crate) fn accept(
        &self,
        stream: Connection,
    ) -> Result<NativeTlsStream, Box<dyn Error + Send + Sync + 'static>> {
        Ok(NativeTlsStream(Arc::new(Mutex::new(
            self.0.accept(stream)?,
        ))))
    }
}

impl From<NativeTlsStream> for RefinedStream {
    fn from(stream: NativeTlsStream) -> Self {
        Self::Https(stream)
    }
}
