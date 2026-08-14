//! Provides the native TLS server implementation.

pub mod native_tls;
pub use self::native_tls::NativeTlsContext as SslContextImpl;
pub use self::native_tls::NativeTlsStream as SslStream;
