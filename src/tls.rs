//! TLS transport using rustls. Feature-gated behind `tls`.
//!
//! ```rust,ignore
//! // Sync client
//! let tls = TlsTransport::connect("broker.example.com:8883")?;
//! let client = MqttClient::connect_with(tls, opts)?;
//!
//! // Async client
//! let tls = TlsTransport::connect("broker.example.com:8883")?;
//! let (client, connection) = AsyncMqttClient::connect_with(tls, opts)?;
//! ```

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};

use crate::transport::Transport;

/// Default TCP connection timeout used by [`TlsTransport::connect`] and
/// [`TlsTransport::connect_with_config`].
pub const DEFAULT_TLS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A TLS-wrapped TCP connection that implements `Transport`.
pub struct TlsTransport {
    stream: StreamOwned<ClientConnection, TcpStream>,
}

impl TlsTransport {
    /// Connect to a broker using Mozilla root certificates.
    ///
    /// The `addr` should be `host:port` (e.g. `"broker.example.com:8883"`).
    /// Bracketed IPv6 addresses are accepted. TCP connection attempts share a
    /// global ten-second deadline; use [`Self::connect_with_timeout`] to
    /// override it. The TLS handshake itself occurs during the MQTT handshake,
    /// where the client's connect timeout applies.
    pub fn connect(addr: &str) -> io::Result<Self> {
        Self::connect_with_timeout(addr, DEFAULT_TLS_CONNECT_TIMEOUT)
    }

    /// Connect with a caller-provided global TCP connection timeout.
    pub fn connect_with_timeout(addr: &str, timeout: Duration) -> io::Result<Self> {
        let config = default_tls_config().map_err(io::Error::other)?;
        Self::connect_with_config_and_timeout(addr, config, timeout)
    }

    /// Connect with a custom [`ClientConfig`] and the default TCP timeout.
    pub fn connect_with_config(addr: &str, config: Arc<ClientConfig>) -> io::Result<Self> {
        Self::connect_with_config_and_timeout(addr, config, DEFAULT_TLS_CONNECT_TIMEOUT)
    }

    /// Connect with a custom [`ClientConfig`] and global TCP timeout.
    pub fn connect_with_config_and_timeout(
        addr: &str,
        config: Arc<ClientConfig>,
        timeout: Duration,
    ) -> io::Result<Self> {
        let host = host_from_address(addr)?;
        let server_name = ServerName::try_from(host.to_owned())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let tcp = connect_tcp_with_deadline(addr, timeout)?;
        let conn = ClientConnection::new(config, server_name).map_err(io::Error::other)?;

        Ok(Self {
            stream: StreamOwned::new(conn, tcp),
        })
    }

    /// Access the underlying TCP stream (e.g. for `set_nonblocking`).
    fn tcp(&self) -> &TcpStream {
        self.stream.get_ref()
    }
}

fn connect_tcp_with_deadline(addr: &str, timeout: Duration) -> io::Result<TcpStream> {
    if timeout.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TLS connect timeout must be non-zero",
        ));
    }
    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "TLS connect timeout is too large",
        )
    })?;

    // std's resolver is synchronous. Starting the deadline before resolution
    // makes its elapsed time consume the shared budget, although the platform
    // resolver itself cannot be interrupted by std::net.
    let addresses = addr.to_socket_addrs()?;
    let mut attempted = false;
    let mut last_error = None;
    for address in addresses {
        attempted = true;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "TLS TCP connection deadline elapsed",
            ));
        }
        match TcpStream::connect_timeout(&address, remaining) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }

    if !attempted {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "address resolved to no endpoints",
        ));
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotConnected,
            "unable to connect to TLS endpoint",
        )
    }))
}

fn host_from_address(addr: &str) -> io::Result<&str> {
    let (host, port) = if let Some(bracketed) = addr.strip_prefix('[') {
        let closing = bracketed
            .find(']')
            .ok_or_else(|| invalid_address("missing closing ']'"))?;
        let host = &bracketed[..closing];
        let port = bracketed[closing + 1..]
            .strip_prefix(':')
            .ok_or_else(|| invalid_address("missing port after bracketed host"))?;
        (host, port)
    } else {
        let (host, port) = addr
            .rsplit_once(':')
            .ok_or_else(|| invalid_address("address must be host:port"))?;
        if host.contains(':') {
            return Err(invalid_address(
                "IPv6 addresses must be enclosed in brackets",
            ));
        }
        (host, port)
    };

    if host.is_empty() {
        return Err(invalid_address("host must not be empty"));
    }
    if port.parse::<u16>().is_err() {
        return Err(invalid_address("port must be an unsigned 16-bit integer"));
    }
    Ok(host)
}

fn invalid_address(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

impl Transport for TlsTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Write::write(&mut self.stream, buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        Write::write_all(&mut self.stream, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.stream)
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

    fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.tcp().set_write_timeout(dur)
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

    let config = ClientConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(root_store)
        .with_no_client_auth();

    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_dns_and_ipv4_hosts() {
        assert_eq!(
            host_from_address("broker.example.com:8883").unwrap(),
            "broker.example.com"
        );
        assert_eq!(host_from_address("127.0.0.1:8883").unwrap(), "127.0.0.1");
    }

    #[test]
    fn extracts_bracketed_ipv6_host_for_sni() {
        assert_eq!(
            host_from_address("[2001:db8::1]:8883").unwrap(),
            "2001:db8::1"
        );
        assert!(
            ServerName::try_from(host_from_address("[2001:db8::1]:8883").unwrap().to_owned())
                .is_ok()
        );
    }

    #[test]
    fn rejects_ambiguous_or_incomplete_addresses() {
        for address in [
            "broker.example.com",
            ":8883",
            "broker.example.com:not-a-port",
            "2001:db8::1:8883",
            "[2001:db8::1]",
            "[2001:db8::1:8883",
        ] {
            assert!(host_from_address(address).is_err(), "accepted {address}");
        }
    }

    #[test]
    fn rejects_zero_connect_timeout_before_resolution() {
        let error = connect_tcp_with_deadline("127.0.0.1:1", Duration::ZERO).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
