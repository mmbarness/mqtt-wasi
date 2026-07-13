use std::io;

/// Abstraction over a TCP-like byte stream.
///
/// Default implementation is provided for `std::net::TcpStream`, which works
/// on wasmtime (wasip2). WasmEdge users can wrap
/// `wasmedge_wasi_socket::TcpStream` in a local newtype and implement this
/// trait for the wrapper.
pub trait Transport {
    /// Attempt to write bytes, returning the number accepted by the transport.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize>;

    /// Write an entire buffer on a blocking transport.
    ///
    /// Async drivers should use [`Transport::write`] and retain partial-write
    /// progress instead of calling this method on a non-blocking stream.
    fn write_all(&mut self, mut buf: &[u8]) -> io::Result<()> {
        while !buf.is_empty() {
            match self.write(buf) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(written) => buf = &buf[written..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Flush buffered output. Byte-stream transports may use the default.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()>;
    fn set_nonblocking(&mut self, nonblocking: bool) -> io::Result<()>;
    fn set_read_timeout(&self, dur: Option<std::time::Duration>) -> io::Result<()>;

    /// Set the blocking write deadline where the transport supports it.
    fn set_write_timeout(&self, _dur: Option<std::time::Duration>) -> io::Result<()> {
        Ok(())
    }

    fn shutdown(&self) -> io::Result<()>;
}

impl Transport for std::net::TcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        std::io::Write::write(self, buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        std::io::Write::write_all(self, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        std::io::Write::flush(self)
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        std::io::Read::read(self, buf)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        std::io::Read::read_exact(self, buf)
    }

    fn set_nonblocking(&mut self, nonblocking: bool) -> io::Result<()> {
        std::net::TcpStream::set_nonblocking(self, nonblocking)
    }

    fn set_read_timeout(&self, dur: Option<std::time::Duration>) -> io::Result<()> {
        std::net::TcpStream::set_read_timeout(self, dur)
    }

    fn set_write_timeout(&self, dur: Option<std::time::Duration>) -> io::Result<()> {
        std::net::TcpStream::set_write_timeout(self, dur)
    }

    fn shutdown(&self) -> io::Result<()> {
        std::net::TcpStream::shutdown(self, std::net::Shutdown::Both)
    }
}
