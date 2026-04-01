#[cfg(not(feature = "std"))]
use alloc::string::String;
use core::fmt;

#[derive(Debug)]
pub enum Error {
    MalformedPacket(&'static str),
    InvalidPacketType(u8),
    InvalidQoS(u8),
    InvalidReasonCode(u8),
    ConnectionRefused(u8),
    NotConnected,
    PacketTooLarge,
    StringTooLong(usize),
    Timeout,
    UnexpectedPacket(&'static str),
    Serialize(String),
    Deserialize(String),
    ConnectionClosed,
    #[cfg(feature = "std")]
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::MalformedPacket(msg) => write!(f, "malformed packet: {msg}"),
            Error::InvalidPacketType(t) => write!(f, "invalid packet type: {t}"),
            Error::InvalidQoS(q) => write!(f, "invalid QoS: {q}"),
            Error::InvalidReasonCode(c) => write!(f, "invalid reason code: 0x{c:02x}"),
            Error::ConnectionRefused(c) => write!(f, "connection refused: 0x{c:02x}"),
            Error::NotConnected => write!(f, "not connected"),
            Error::PacketTooLarge => write!(f, "packet too large"),
            Error::StringTooLong(len) => write!(f, "string too long: {len} bytes"),
            Error::ConnectionClosed => write!(f, "connection closed"),
            Error::Timeout => write!(f, "timeout"),
            Error::UnexpectedPacket(msg) => write!(f, "unexpected packet: {msg}"),
            Error::Serialize(msg) => write!(f, "serialize: {msg}"),
            Error::Deserialize(msg) => write!(f, "deserialize: {msg}"),
            #[cfg(feature = "std")]
            Error::Io(e) => write!(f, "io: {e}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

#[cfg(feature = "std")]
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;
