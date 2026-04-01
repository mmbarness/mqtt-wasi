use std::cell::RefCell;
use std::collections::HashMap;
use std::net::TcpStream;
use std::rc::Rc;
use std::task::Waker;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::codec::ping::PINGREQ_BYTES;
use crate::codec::properties::Properties;
use crate::codec::types::*;
use crate::error::{Error, Result};
use crate::frame::FrameReader;
use crate::options::ConnectOptions;
use crate::request::RequestFuture;
use crate::transport::Transport;

/// A response slot for a pending request.
pub(crate) struct PendingRequest {
    pub waker: Option<Waker>,
    pub result: Option<Result<Vec<u8>>>,
}

/// Shared mutable state behind the async client.
pub(crate) struct SharedInner<T: Transport = TcpStream> {
    pub stream: T,
    pub frame_reader: FrameReader,
    pub pending: HashMap<String, PendingRequest>,
    pub next_packet_id: u16,
    pub keep_alive_secs: u16,
    pub last_activity: Instant,
    pub error: Option<Error>,
    pub amqp_reply_format: bool,
}

pub(crate) type Shared<T = TcpStream> = Rc<RefCell<SharedInner<T>>>;

/// Async MQTT client for concurrent request/reply over one connection.
///
/// Uses cooperative non-blocking I/O — each pending request Future
/// pumps the shared socket when polled by `tokio::join!`.
///
/// **Not Send/Sync.** Works with `tokio::main(flavor = "current_thread")`
/// and `tokio::join!`. For `tokio::spawn`, use `tokio::task::spawn_local`.
pub struct AsyncMqttClient<T: Transport = TcpStream> {
    inner: Shared<T>,
}

impl AsyncMqttClient<TcpStream> {
    /// Connect to an MQTT broker using `std::net::TcpStream`.
    pub async fn connect(addr: &str, options: ConnectOptions) -> Result<Self> {
        let stream = TcpStream::connect(addr)?;
        Self::connect_with(stream, options).await
    }
}

impl<T: Transport> AsyncMqttClient<T> {
    /// Connect using a caller-provided transport.
    ///
    /// The CONNECT/CONNACK handshake is done synchronously (blocking),
    /// then the socket is switched to non-blocking for cooperative polling.
    pub async fn connect_with(stream: T, options: ConnectOptions) -> Result<Self> {
        let client = Self {
            inner: Rc::new(RefCell::new(SharedInner {
                stream,
                frame_reader: FrameReader::new(),
                pending: HashMap::new(),
                next_packet_id: 1,
                keep_alive_secs: options.keep_alive_secs,
                last_activity: Instant::now(),
                error: None,
                amqp_reply_format: options.amqp_reply_format,
            })),
        };

        // Blocking handshake
        {
            let mut inner = client.inner.borrow_mut();
            let timeout = Duration::from_secs(options.keep_alive_secs as u64 / 2);
            inner.stream.set_read_timeout(Some(timeout))?;

            let connect = ConnectPacket {
                protocol_version: 5,
                clean_start: options.clean_start,
                keep_alive: options.keep_alive_secs,
                client_id: options.client_id,
                username: options.username,
                password: options.password,
                properties: Properties::new(),
            };
            let bytes = connect.encode()?;
            inner.stream.write_all(&bytes)?;
            inner.last_activity = Instant::now();

            // Read CONNACK (blocking)
            let packet = read_packet_blocking(&mut inner.stream)?;
            match packet {
                Packet::ConnAck(ack) => {
                    if ack.reason_code != 0x00 {
                        return Err(Error::ConnectionRefused(ack.reason_code));
                    }
                }
                _ => return Err(Error::UnexpectedPacket("expected CONNACK")),
            }

            // Switch to non-blocking
            inner.stream.set_nonblocking(true)?;
            inner.stream.set_read_timeout(None)?;
        }

        Ok(client)
    }

    /// Send a request and await the correlated reply.
    pub fn request<Req: Serialize>(
        &self,
        topic: &str,
        payload: &Req,
    ) -> RequestFuture<T> {
        RequestFuture::new(self.inner.clone(), topic.to_string(), payload)
    }

    /// Graceful disconnect.
    pub fn disconnect(self) -> Result<()> {
        let mut inner = self.inner.borrow_mut();
        inner.stream.set_nonblocking(false)?;
        let pkt = DisconnectPacket { reason_code: 0x00 };
        let bytes = pkt.encode()?;
        inner.stream.write_all(&bytes)?;
        inner.stream.shutdown()?;
        Ok(())
    }
}

// -- helpers --

pub(crate) fn next_packet_id<T: Transport>(inner: &mut SharedInner<T>) -> u16 {
    let id = inner.next_packet_id;
    inner.next_packet_id = inner.next_packet_id.wrapping_add(1);
    if inner.next_packet_id == 0 {
        inner.next_packet_id = 1;
    }
    id
}

pub(crate) fn write_blocking<T: Transport>(
    stream: &mut T,
    buf: &[u8],
) -> Result<()> {
    // Temporarily switch to blocking for writes to avoid WouldBlock on write_all.
    stream.set_nonblocking(false)?;
    let result = stream.write_all(buf);
    stream.set_nonblocking(true)?;
    result.map_err(Error::from)
}

pub(crate) fn maybe_ping<T: Transport>(inner: &mut SharedInner<T>) -> Result<()> {
    let elapsed = inner.last_activity.elapsed();
    if elapsed >= Duration::from_secs(inner.keep_alive_secs as u64 / 2) {
        write_blocking(&mut inner.stream, &PINGREQ_BYTES)?;
        inner.last_activity = Instant::now();
    }
    Ok(())
}

/// Read a complete MQTT packet from a blocking stream.
fn read_packet_blocking<T: Transport>(stream: &mut T) -> Result<Packet> {
    let mut first = [0u8; 1];
    stream.read_exact(&mut first)?;

    let mut remaining_length: u32 = 0;
    let mut multiplier: u32 = 1;
    loop {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte)?;
        remaining_length += (byte[0] as u32 & 0x7F) * multiplier;
        if byte[0] & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
        if multiplier > 128 * 128 * 128 {
            return Err(Error::MalformedPacket("variable int too long"));
        }
    }

    let mut body = vec![0u8; remaining_length as usize];
    if remaining_length > 0 {
        stream.read_exact(&mut body)?;
    }

    let header = FixedHeader {
        packet_type: PacketType::from_u8(first[0] >> 4)?,
        flags: first[0] & 0x0F,
        remaining_length,
    };
    Packet::decode(header, &body)
}
