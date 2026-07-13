#[cfg(not(feature = "std"))]
use alloc::string::String;
use core::fmt;

/// Errors returned by `mqtt-wasi` operations.
#[derive(Debug)]
pub enum Error {
    InvalidOptions(&'static str),
    MalformedPacket(&'static str),
    InvalidPacketType(u8),
    InvalidQoS(u8),
    InvalidReasonCode(u8),
    ConnectionRefused(u8),
    ServerDisconnected(u8),
    PacketTooLarge {
        size: usize,
        max: usize,
    },
    StringTooLong(usize),
    BinaryTooLong(usize),
    Timeout(&'static str),
    KeepAliveTimeout,
    UnexpectedPacket(&'static str),
    AckRejected {
        packet: &'static str,
        reason_code: u8,
    },
    QueueFull(&'static str),
    ClientClosed,
    Serialize(String),
    Deserialize(String),
    ConnectionClosed,
    #[cfg(feature = "std")]
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidOptions(msg) => write!(f, "invalid options: {msg}"),
            Error::MalformedPacket(msg) => write!(f, "malformed packet: {msg}"),
            Error::InvalidPacketType(t) => write!(f, "invalid packet type: {t}"),
            Error::InvalidQoS(q) => write!(f, "invalid QoS: {q}"),
            Error::InvalidReasonCode(c) => write!(f, "invalid reason code: 0x{c:02x}"),
            Error::ConnectionRefused(c) => write!(f, "connection refused: 0x{c:02x}"),
            Error::ServerDisconnected(c) => write!(f, "server disconnected: 0x{c:02x}"),
            Error::PacketTooLarge { size, max } => {
                write!(f, "packet is {size} bytes; configured maximum is {max}")
            }
            Error::StringTooLong(len) => write!(f, "string too long: {len} bytes"),
            Error::BinaryTooLong(len) => write!(f, "binary data too long: {len} bytes"),
            Error::ConnectionClosed => write!(f, "connection closed"),
            Error::ClientClosed => write!(f, "client driver is not running"),
            Error::Timeout(operation) => write!(f, "{operation} timed out"),
            Error::KeepAliveTimeout => write!(f, "broker did not answer PINGREQ"),
            Error::UnexpectedPacket(msg) => write!(f, "unexpected packet: {msg}"),
            Error::AckRejected {
                packet,
                reason_code,
            } => write!(f, "{packet} rejected: 0x{reason_code:02x}"),
            Error::QueueFull(queue) => write!(f, "{queue} queue is full"),
            Error::Serialize(msg) => write!(f, "serialize: {msg}"),
            Error::Deserialize(msg) => write!(f, "deserialize: {msg}"),
            #[cfg(feature = "std")]
            Error::Io(e) => write!(f, "io: {e}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(feature = "std")]
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn io_error_preserves_its_source() {
        let error = Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "transport reset",
        ));

        assert_eq!(error.source().unwrap().to_string(), "transport reset");
        assert!(Error::ConnectionClosed.source().is_none());
    }
}
