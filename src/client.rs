use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::codec::ping::PINGREQ_BYTES;
use crate::codec::properties::Properties;
use crate::codec::types::*;
use crate::error::{Error, Result};
use crate::options::ConnectOptions;
use crate::trace::TraceContext;
use crate::transport::Transport;

/// Synchronous MQTT v5 client, generic over the transport layer.
///
/// Use `MqttClient::connect()` for the default `std::net::TcpStream` transport.
/// For alternative transports (e.g. WasmEdge), use `MqttClient::connect_with()`.
pub struct MqttClient<T: Transport = TcpStream> {
    stream: T,
    next_packet_id: u16,
    keep_alive_secs: u16,
    last_activity: Instant,
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
        let timeout = Duration::from_secs(options.keep_alive_secs as u64 / 2);
        stream.set_read_timeout(Some(timeout))?;

        let mut client = Self {
            stream,
            next_packet_id: 1,
            keep_alive_secs: options.keep_alive_secs,
            last_activity: Instant::now(),
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
        client.last_activity = Instant::now();

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
        let json = serde_json::to_vec(payload)
            .map_err(|e| Error::Serialize(e.to_string()))?;
        self.publish_raw(topic, &json, QoS::AtMostOnce, false, Properties::new())
    }

    /// Publish with QoS 1 (waits for PUBACK).
    pub fn publish_qos1<P: Serialize>(&mut self, topic: &str, payload: &P) -> Result<()> {
        let json = serde_json::to_vec(payload)
            .map_err(|e| Error::Serialize(e.to_string()))?;
        let packet_id = self.next_packet_id();

        let pkt = PublishPacket {
            topic: String::from(topic),
            packet_id: Some(packet_id),
            payload: json,
            qos: QoS::AtLeastOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
        };
        self.send_encoded(&pkt.encode()?)?;

        loop {
            match self.read_packet_or_ping()? {
                Some(Packet::PubAck(ack)) if ack.packet_id == packet_id => return Ok(()),
                Some(_) => continue,
                None => continue,
            }
        }
    }

    /// Publish with trace context auto-injected into User Properties.
    pub fn publish_traced<P: Serialize>(
        &mut self,
        topic: &str,
        payload: &P,
        trace: &TraceContext,
    ) -> Result<()> {
        let json = serde_json::to_vec(payload)
            .map_err(|e| Error::Serialize(e.to_string()))?;
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
        self.send_encoded(&pkt.encode()?)
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
                Some(Packet::UnsubAck(ack)) if ack.packet_id == packet_id => return Ok(()),
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
        self.last_activity = Instant::now();
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
            Ok(pkt) => Ok(Some(pkt)),
            Err(Error::Io(ref e)) if e.kind() == std::io::ErrorKind::TimedOut
                || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                self.maybe_send_ping()?;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn maybe_send_ping(&mut self) -> Result<()> {
        let elapsed = self.last_activity.elapsed();
        if elapsed >= Duration::from_secs(self.keep_alive_secs as u64 / 2) {
            self.stream.write_all(&PINGREQ_BYTES)?;
            self.last_activity = Instant::now();
        }
        Ok(())
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let mut first = [0u8; 1];
        self.stream.read_exact(&mut first)?;

        let mut remaining_length: u32 = 0;
        let mut multiplier: u32 = 1;
        loop {
            let mut byte = [0u8; 1];
            self.stream.read_exact(&mut byte)?;
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
            self.stream.read_exact(&mut body)?;
        }

        let header = FixedHeader {
            packet_type: PacketType::from_u8(first[0] >> 4)?,
            flags: first[0] & 0x0F,
            remaining_length,
        };

        self.last_activity = Instant::now();
        Packet::decode(header, &body)
    }
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
