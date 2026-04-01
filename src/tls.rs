//! TLS transport using rustls. Feature-gated behind `tls`.
//!
//! ```rust,ignore
//! // Sync client
//! let tls = TlsTransport::connect("broker.example.com:8883")?;
//! let client = MqttClient::connect_with(tls, opts)?;
//!
//! // Async client
//! let tls = TlsTransport::connect("broker.example.com:8883")?;
//! let client = AsyncMqttClient::connect_with(tls, opts).await?;
//! ```

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};

use crate::transport::Transport;

/// A TLS-wrapped TCP connection that implements `Transport`.
pub struct TlsTransport {
    stream: StreamOwned<ClientConnection, TcpStream>,
}

impl TlsTransport {
    /// Connect to a broker over TLS with system/Mozilla root certificates.
    ///
    /// The `addr` should be `host:port` (e.g. `"broker.example.com:8883"`).
    /// The hostname is extracted for SNI and certificate verification.
    pub fn connect(addr: &str) -> io::Result<Self> {
        let config = default_tls_config()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        Self::connect_with_config(addr, config)
    }

    /// Connect with a custom `rustls::ClientConfig`.
    pub fn connect_with_config(addr: &str, config: Arc<ClientConfig>) -> io::Result<Self> {
        let host = addr
            .split(':')
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing host in address"))?;

        let server_name = ServerName::try_from(host.to_string())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        let tcp = TcpStream::connect(addr)?;
        let conn = ClientConnection::new(config, server_name)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        Ok(Self {
            stream: StreamOwned::new(conn, tcp),
        })
    }

    /// Access the underlying TCP stream (e.g. for `set_nonblocking`).
    fn tcp(&self) -> &TcpStream {
        self.stream.get_ref()
    }
}

impl Transport for TlsTransport {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        Write::write_all(&mut self.stream, buf)
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Read::read(&mut self.stream, buf)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        Read::read_exact(&mut self.stream, buf)
    }

    fn set_nonblocking(&mut self, nonblocking: bool) -> io::Result<()> {
        self.tcp().set_nonblocking(nonblocking)
    }

    fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.tcp().set_read_timeout(dur)
    }

    fn shutdown(&self) -> io::Result<()> {
        self.tcp().shutdown(std::net::Shutdown::Both)
    }
}

/// Build a default TLS client config with Mozilla root certificates
/// and the pure-Rust RustCrypto provider (no C dependencies, compiles to Wasm).
fn default_tls_config() -> std::result::Result<Arc<ClientConfig>, rustls::Error> {
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = ClientConfig::builder_with_provider(Arc::new(
        rustls_rustcrypto::provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(root_store)
    .with_no_client_auth();

    Ok(Arc::new(config))
}
