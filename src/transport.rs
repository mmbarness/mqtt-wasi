use std::io;

/// Abstraction over a TCP-like byte stream.
///
/// Default implementation is provided for `std::net::TcpStream`, which works
/// on wasmtime (wasip2). WasmEdge users can implement this for
/// `wasmedge_wasi_socket::TcpStream`.
pub trait Transport {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()>;
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()>;
    fn set_nonblocking(&mut self, nonblocking: bool) -> io::Result<()>;
    fn set_read_timeout(&self, dur: Option<std::time::Duration>) -> io::Result<()>;
    fn shutdown(&self) -> io::Result<()>;
}

impl Transport for std::net::TcpStream {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        std::io::Write::write_all(self, buf)
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

    fn shutdown(&self) -> io::Result<()> {
        std::net::TcpStream::shutdown(self, std::net::Shutdown::Both)
    }
}
