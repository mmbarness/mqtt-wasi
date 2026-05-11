use std::io::ErrorKind;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::codec::ping::PINGREQ_BYTES;
use crate::codec::properties::Properties;
use crate::codec::types::*;
use crate::error::{Error, Result};
use crate::frame::FrameReader;
use crate::options::ConnectOptions;
use crate::trace::TraceContext;
use crate::transport::Transport;

/// Synchronous MQTT v5 client, generic over the transport layer.
///
/// Use `MqttClient::connect()` for the default `std::net::TcpStream` transport.
/// For alternative transports (e.g. WasmEdge), use `MqttClient::connect_with()`.
pub struct MqttClient<T: Transport = TcpStream> {
    stream: T,
    frame_reader: FrameReader,
    next_packet_id: u16,
    keep_alive_secs: u16,
    last_read_at: Instant,
    last_write_at: Instant,
}

/// A received message with a deserialized payload.
#[derive(Debug)]
pub struct Message<T> {
    pub topic: String,
    pub payload: T,
    pub qos: QoS,
    pub retain: bool,
    pub trace: Option<TraceContext>,
}

/// A raw received message (bytes, not deserialized).
#[derive(Debug)]
pub struct RawMessage {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: QoS,
    pub retain: bool,
    pub properties: Properties,
}

/// Iterator over incoming messages on a subscription.
pub struct Subscription<'a, T, Tr: Transport = TcpStream> {
    client: &'a mut MqttClient<Tr>,
    _phantom: std::marker::PhantomData<T>,
}

impl MqttClient<TcpStream> {
    /// Connect to an MQTT broker using `std::net::TcpStream`.
    pub fn connect(addr: &str, options: ConnectOptions) -> Result<Self> {
        let stream = TcpStream::connect(addr)?;
        Self::connect_with(stream, options)
    }
}

impl<T: Transport> MqttClient<T> {
    /// Connect using a caller-provided transport.
    pub fn connect_with(stream: T, options: ConnectOptions) -> Result<Self> {
        stream.set_read_timeout(read_timeout(options.keep_alive_secs))?;

        let now = Instant::now();
        let mut client = Self {
            stream,
            frame_reader: FrameReader::new(),
            next_packet_id: 1,
            keep_alive_secs: options.keep_alive_secs,
            last_read_at: now,
            last_write_at: now,
        };

        // Send CONNECT
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
        client.stream.write_all(&bytes)?;
        client.last_write_at = Instant::now();

        // Read CONNACK
        let packet = client.read_packet()?;
        match packet {
            Packet::ConnAck(ack) => {
                if ack.reason_code != 0x00 {
                    return Err(Error::ConnectionRefused(ack.reason_code));
                }
            }
            _ => return Err(Error::UnexpectedPacket("expected CONNACK")),
        }

        Ok(client)
    }

    /// Publish a serializable payload as JSON to a topic (QoS 0).
    pub fn publish<P: Serialize>(&mut self, topic: &str, payload: &P) -> Result<()> {
        let json = serde_json::to_vec(payload).map_err(|e| Error::Serialize(e.to_string()))?;
        self.publish_raw(topic, &json, QoS::AtMostOnce, false, Properties::new())
    }

    /// Publish with QoS 1 (waits for PUBACK).
    pub fn publish_qos1<P: Serialize>(&mut self, topic: &str, payload: &P) -> Result<()> {
        let json = serde_json::to_vec(payload).map_err(|e| Error::Serialize(e.to_string()))?;
        self.publish_raw(topic, &json, QoS::AtLeastOnce, false, Properties::new())
    }

    /// Publish with trace context auto-injected into User Properties.
    pub fn publish_traced<P: Serialize>(
        &mut self,
        topic: &str,
        payload: &P,
        trace: &TraceContext,
    ) -> Result<()> {
        let json = serde_json::to_vec(payload).map_err(|e| Error::Serialize(e.to_string()))?;
        let mut props = Properties::new();
        trace.inject(&mut props);
        self.publish_raw(topic, &json, QoS::AtMostOnce, false, props)
    }

    /// Publish raw bytes.
    pub fn publish_raw(
        &mut self,
        topic: &str,
        payload: &[u8],
        qos: QoS,
        retain: bool,
        properties: Properties,
    ) -> Result<()> {
        let packet_id = if qos != QoS::AtMostOnce {
            Some(self.next_packet_id())
        } else {
            None
        };

        let pkt = PublishPacket {
            topic: String::from(topic),
            packet_id,
            payload: payload.to_vec(),
            qos,
            retain,
            dup: false,
            properties,
        };
        self.send_encoded(&pkt.encode()?)?;

        if let Some(packet_id) = packet_id {
            self.wait_for_puback(packet_id)
        } else {
            Ok(())
        }
    }

    /// Subscribe to a topic and return a typed message iterator.
    pub fn subscribe<P: DeserializeOwned>(
        &mut self,
        filter: &str,
    ) -> Result<Subscription<'_, P, T>> {
        self.subscribe_raw(filter, QoS::AtMostOnce)?;
        Ok(Subscription {
            client: self,
            _phantom: std::marker::PhantomData,
        })
    }

    /// Subscribe to a topic (raw, returns the SubAck reason codes).
    pub fn subscribe_raw(&mut self, filter: &str, qos: QoS) -> Result<Vec<u8>> {
        let packet_id = self.next_packet_id();
        let pkt = SubscribePacket {
            packet_id,
            filters: vec![(String::from(filter), qos)],
            properties: Properties::new(),
        };
        self.send_encoded(&pkt.encode()?)?;

        loop {
            match self.read_packet_or_ping()? {
                Some(Packet::SubAck(ack)) if ack.packet_id == packet_id => {
                    validate_ack_codes("SUBACK", &ack.reason_codes, 1)?;
                    return Ok(ack.reason_codes);
                }
                Some(_) => continue,
                None => continue,
            }
        }
    }

    /// Unsubscribe from a topic.
    pub fn unsubscribe(&mut self, filter: &str) -> Result<()> {
        let packet_id = self.next_packet_id();
        let pkt = UnsubscribePacket {
            packet_id,
            filters: vec![String::from(filter)],
            properties: Properties::new(),
        };
        self.send_encoded(&pkt.encode()?)?;

        loop {
            match self.read_packet_or_ping()? {
                Some(Packet::UnsubAck(ack)) if ack.packet_id == packet_id => {
                    validate_ack_codes("UNSUBACK", &ack.reason_codes, 1)?;
                    return Ok(());
                }
                Some(_) => continue,
                None => continue,
            }
        }
    }

    /// Send a graceful DISCONNECT and close the connection.
    pub fn disconnect(mut self) -> Result<()> {
        let pkt = DisconnectPacket { reason_code: 0x00 };
        self.send_encoded(&pkt.encode()?)?;
        self.stream.shutdown()?;
        Ok(())
    }

    /// Read the next incoming message (blocks).
    pub fn recv_raw(&mut self) -> Result<Option<RawMessage>> {
        loop {
            match self.read_packet_or_ping()? {
                Some(Packet::Publish(pkt)) => {
                    if pkt.qos == QoS::AtLeastOnce {
                        if let Some(id) = pkt.packet_id {
                            self.send_puback(id)?;
                        }
                    }
                    return Ok(Some(RawMessage {
                        topic: pkt.topic,
                        payload: pkt.payload,
                        qos: pkt.qos,
                        retain: pkt.retain,
                        properties: pkt.properties,
                    }));
                }
                Some(Packet::Disconnect(_)) => return Ok(None),
                Some(_) => continue,
                None => continue,
            }
        }
    }

    // -- internal helpers --

    fn send_encoded(&mut self, bytes: &[u8]) -> Result<()> {
        self.stream.write_all(bytes)?;
        self.last_write_at = Instant::now();
        Ok(())
    }

    fn send_puback(&mut self, packet_id: u16) -> Result<()> {
        let pkt = PubAckPacket {
            packet_id,
            reason_code: 0x00,
        };
        self.send_encoded(&pkt.encode()?)
    }

    fn next_packet_id(&mut self) -> u16 {
        let id = self.next_packet_id;
        self.next_packet_id = self.next_packet_id.wrapping_add(1);
        if self.next_packet_id == 0 {
            self.next_packet_id = 1;
        }
        id
    }

    fn read_packet_or_ping(&mut self) -> Result<Option<Packet>> {
        match self.read_packet() {
            Ok(pkt) => {
                self.maybe_send_ping()?;
                Ok(Some(pkt))
            }
            Err(Error::Io(ref e))
                if e.kind() == ErrorKind::TimedOut || e.kind() == ErrorKind::WouldBlock =>
            {
                self.maybe_send_ping()?;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn maybe_send_ping(&mut self) -> Result<()> {
        let Some(interval) = ping_interval(self.keep_alive_secs) else {
            return Ok(());
        };

        if self.last_write_at.elapsed() >= interval {
            self.stream.write_all(&PINGREQ_BYTES)?;
            self.last_write_at = Instant::now();
        }
        Ok(())
    }

    fn read_packet(&mut self) -> Result<Packet> {
        loop {
            if let Some(packet) = self.frame_reader.try_decode()? {
                self.last_read_at = Instant::now();
                return Ok(packet);
            }

            let mut tmp = [0u8; 8192];
            match self.stream.read(&mut tmp) {
                Ok(0) => return Err(Error::ConnectionClosed),
                Ok(n) => {
                    self.last_read_at = Instant::now();
                    self.frame_reader.push(&tmp[..n]);
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::Io(e)),
            }
        }
    }

    fn wait_for_puback(&mut self, packet_id: u16) -> Result<()> {
        loop {
            match self.read_packet_or_ping()? {
                Some(Packet::PubAck(ack)) if ack.packet_id == packet_id => {
                    validate_ack_code("PUBACK", ack.reason_code)?;
                    return Ok(());
                }
                Some(_) => continue,
                None => continue,
            }
        }
    }
}

pub(crate) fn ping_interval(keep_alive_secs: u16) -> Option<Duration> {
    if keep_alive_secs == 0 {
        None
    } else {
        Some(Duration::from_secs((keep_alive_secs as u64 / 2).max(1)))
    }
}

fn read_timeout(keep_alive_secs: u16) -> Option<Duration> {
    ping_interval(keep_alive_secs)
}

pub(crate) fn validate_ack_code(packet: &'static str, reason_code: u8) -> Result<()> {
    if reason_code >= 0x80 {
        Err(Error::AckRejected {
            packet,
            reason_code,
        })
    } else {
        Ok(())
    }
}

pub(crate) fn validate_ack_codes(
    packet: &'static str,
    reason_codes: &[u8],
    expected: usize,
) -> Result<()> {
    if reason_codes.len() != expected {
        return Err(Error::MalformedPacket("ack reason code count mismatch"));
    }
    for &reason_code in reason_codes {
        validate_ack_code(packet, reason_code)?;
    }
    Ok(())
}

impl<'a, P: DeserializeOwned, T: Transport> Iterator for Subscription<'a, P, T> {
    type Item = Result<Message<P>>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.client.recv_raw() {
            Ok(Some(raw)) => {
                let trace = TraceContext::from_properties(&raw.properties);
                match serde_json::from_slice(&raw.payload) {
                    Ok(payload) => Some(Ok(Message {
                        topic: raw.topic,
                        payload,
                        qos: raw.qos,
                        retain: raw.retain,
                        trace,
                    })),
                    Err(e) => Some(Err(Error::Deserialize(e.to_string()))),
                }
            }
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io;

    enum ReadStep {
        Data(Vec<u8>),
        Error(ErrorKind),
        Eof,
    }

    struct MockTransport {
        reads: VecDeque<ReadStep>,
        writes: Vec<Vec<u8>>,
    }

    impl MockTransport {
        fn new(reads: impl IntoIterator<Item = ReadStep>) -> Self {
            Self {
                reads: reads.into_iter().collect(),
                writes: Vec::new(),
            }
        }
    }

    impl Transport for MockTransport {
        fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
            self.writes.push(buf.to_vec());
            Ok(())
        }

        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.reads.pop_front().unwrap_or(ReadStep::Eof) {
                ReadStep::Data(data) => {
                    let n = data.len().min(buf.len());
                    buf[..n].copy_from_slice(&data[..n]);
                    Ok(n)
                }
                ReadStep::Error(kind) => Err(io::Error::from(kind)),
                ReadStep::Eof => Ok(0),
            }
        }

        fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
            let mut read = 0;
            while read < buf.len() {
                let n = self.read(&mut buf[read..])?;
                if n == 0 {
                    return Err(io::Error::from(ErrorKind::UnexpectedEof));
                }
                read += n;
            }
            Ok(())
        }

        fn set_nonblocking(&mut self, _nonblocking: bool) -> io::Result<()> {
            Ok(())
        }

        fn set_read_timeout(&self, _dur: Option<Duration>) -> io::Result<()> {
            Ok(())
        }

        fn shutdown(&self) -> io::Result<()> {
            Ok(())
        }
    }

    fn client_with(stream: MockTransport, keep_alive_secs: u16) -> MqttClient<MockTransport> {
        let now = Instant::now();
        MqttClient {
            stream,
            frame_reader: FrameReader::new(),
            next_packet_id: 1,
            keep_alive_secs,
            last_read_at: now,
            last_write_at: now - Duration::from_secs(60),
        }
    }

    #[test]
    fn timeout_after_partial_frame_preserves_buffer() {
        let pkt = PublishPacket {
            topic: String::from("t"),
            packet_id: None,
            payload: b"ok".to_vec(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
        };
        let bytes = pkt.encode().unwrap();
        let split = 3;
        let stream = MockTransport::new([
            ReadStep::Data(bytes[..split].to_vec()),
            ReadStep::Error(ErrorKind::TimedOut),
            ReadStep::Data(bytes[split..].to_vec()),
        ]);
        let mut client = client_with(stream, 10);

        assert!(client.read_packet_or_ping().unwrap().is_none());
        let packet = client.read_packet_or_ping().unwrap().unwrap();

        match packet {
            Packet::Publish(publish) => {
                assert_eq!(publish.topic, "t");
                assert_eq!(publish.payload, b"ok");
            }
            other => panic!("expected publish, got {other:?}"),
        }
        assert_eq!(client.stream.writes, vec![PINGREQ_BYTES.to_vec()]);
    }

    #[test]
    fn keepalive_zero_disables_ping() {
        let stream = MockTransport::new([ReadStep::Error(ErrorKind::TimedOut)]);
        let mut client = client_with(stream, 0);

        assert!(client.read_packet_or_ping().unwrap().is_none());
        assert!(client.stream.writes.is_empty());
    }

    #[test]
    fn qos1_publish_surfaces_negative_puback() {
        let puback = PubAckPacket {
            packet_id: 1,
            reason_code: 0x80,
        }
        .encode()
        .unwrap();
        let stream = MockTransport::new([ReadStep::Data(puback)]);
        let mut client = client_with(stream, 10);

        let err = client
            .publish_raw("t", b"payload", QoS::AtLeastOnce, false, Properties::new())
            .unwrap_err();

        match err {
            Error::AckRejected {
                packet: "PUBACK",
                reason_code: 0x80,
            } => {}
            other => panic!("expected negative PUBACK, got {other:?}"),
        }
    }

    #[test]
    fn subscribe_surfaces_negative_suback() {
        let suback = Packet::SubAck(SubAckPacket {
            packet_id: 1,
            reason_codes: vec![0x80],
        });
        let bytes = match suback {
            Packet::SubAck(ack) => {
                let mut body = Vec::new();
                crate::codec::encode::encode_u16(&mut body, ack.packet_id);
                Properties::new().encode(&mut body).unwrap();
                body.extend_from_slice(&ack.reason_codes);
                let mut packet = Vec::new();
                crate::codec::encode::encode_fixed_header(
                    &mut packet,
                    PacketType::SubAck,
                    0,
                    body.len() as u32,
                )
                .unwrap();
                packet.extend_from_slice(&body);
                packet
            }
            _ => unreachable!(),
        };
        let stream = MockTransport::new([ReadStep::Data(bytes)]);
        let mut client = client_with(stream, 10);

        let err = client.subscribe_raw("t", QoS::AtMostOnce).unwrap_err();

        match err {
            Error::AckRejected {
                packet: "SUBACK",
                reason_code: 0x80,
            } => {}
            other => panic!("expected negative SUBACK, got {other:?}"),
        }
    }
}
