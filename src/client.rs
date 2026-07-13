use std::collections::VecDeque;
use std::io::{self, ErrorKind};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crate::codec::connect::{validate_connack, ServerCapabilities};
use crate::codec::ping::PINGREQ_BYTES;
use crate::codec::properties::{Properties, PropertyId, PropertyValue};
use crate::codec::subscribe::validate_suback_codes;
use crate::codec::topic::{validate_topic_filter, validate_topic_name};
use crate::codec::types::*;
use crate::error::{Error, Result};
use crate::frame::{FrameReader, MQTT_MAX_PACKET_SIZE};
use crate::options::{ConnectOptions, PublishOptions};
use crate::transport::Transport;

type Deadline = (Instant, &'static str);

/// Synchronous MQTT v5 client, generic over its byte-stream transport.
pub struct MqttClient<T: Transport = TcpStream> {
    stream: T,
    frame_reader: FrameReader,
    incoming: VecDeque<PublishPacket>,
    max_incoming_messages: usize,
    next_packet_id: u16,
    keep_alive_secs: u16,
    ack_timeout: Duration,
    max_outbound_packet_size: usize,
    server_capabilities: ServerCapabilities,
    last_read_at: Instant,
    last_write_at: Instant,
    ping_sent_at: Option<Instant>,
    connected: bool,
}

/// Blocking iterator over incoming PUBLISH packets.
pub struct Incoming<'a, T: Transport = TcpStream> {
    client: &'a mut MqttClient<T>,
    done: bool,
}

impl MqttClient<TcpStream> {
    /// Connect to an MQTT broker using a TCP connection and MQTT handshake
    /// bounded by [`ConnectOptions::connect_timeout`].
    pub fn connect(addr: &str, mut options: ConnectOptions) -> Result<Self> {
        options.validate()?;
        let deadline = deadline_after(options.connect_timeout, "connect")?.0;
        let stream = match connect_tcp(addr, deadline) {
            Err(error) if error.kind() == ErrorKind::TimedOut => {
                return Err(Error::Timeout("connect"));
            }
            result => result?,
        };
        options.connect_timeout = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(Error::Timeout("connect"))?;
        Self::connect_with(stream, options)
    }
}

impl<T: Transport> MqttClient<T> {
    /// Perform an MQTT v5 handshake on a caller-provided blocking transport.
    pub fn connect_with(mut stream: T, options: ConnectOptions) -> Result<Self> {
        options.validate()?;
        let deadline = deadline_after(options.connect_timeout, "connect")?;
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(options.connect_timeout))?;
        stream.set_write_timeout(Some(options.connect_timeout))?;

        let now = Instant::now();
        let mut client = Self {
            stream,
            frame_reader: FrameReader::with_max_packet_size(options.max_packet_size),
            incoming: VecDeque::with_capacity(options.max_incoming_messages.min(1024)),
            max_incoming_messages: options.max_incoming_messages,
            next_packet_id: 1,
            keep_alive_secs: options.keep_alive_secs,
            ack_timeout: options.ack_timeout,
            max_outbound_packet_size: MQTT_MAX_PACKET_SIZE,
            server_capabilities: ServerCapabilities::default(),
            last_read_at: now,
            last_write_at: now,
            ping_sent_at: None,
            connected: false,
        };

        let advertised_max = u32::try_from(options.max_packet_size)
            .map_err(|_| Error::InvalidOptions("maximum packet size exceeds u32"))?;
        let mut properties = Properties::new();
        properties.push(
            PropertyId::MaximumPacketSize,
            PropertyValue::U32(advertised_max),
        );
        properties.push(
            PropertyId::ReceiveMaximum,
            PropertyValue::U16(options.max_incoming_messages.min(usize::from(u16::MAX)) as u16),
        );
        let connect = ConnectPacket {
            protocol_version: 5,
            clean_start: options.clean_start,
            keep_alive: options.keep_alive_secs,
            client_id: options.client_id,
            username: options.username,
            password: options.password,
            properties,
        };
        client.send_encoded(&connect.encode()?, "connect")?;

        let ack = match client.read_packet_until(Some(deadline))? {
            Packet::ConnAck(ack) => ack,
            _ => return Err(Error::UnexpectedPacket("expected CONNACK")),
        };
        if ack.reason_code != 0x00 {
            return Err(Error::ConnectionRefused(ack.reason_code));
        }
        validate_connack(&connect.client_id, connect.clean_start, &ack)?;
        client.server_capabilities = ServerCapabilities::from_connack(&ack);

        if let Some(server_keep_alive) = ack.properties.get_u16(PropertyId::ServerKeepAlive) {
            client.keep_alive_secs = server_keep_alive;
        }
        if let Some(server_maximum) = ack.properties.get_u32(PropertyId::MaximumPacketSize) {
            if server_maximum == 0 {
                return Err(Error::MalformedPacket(
                    "server maximum packet size must be non-zero",
                ));
            }
            client.max_outbound_packet_size = server_maximum as usize;
        }

        client.connected = true;
        client.stream.set_read_timeout(None)?;
        client.stream.set_write_timeout(Some(client.ack_timeout))?;
        Ok(client)
    }

    /// Publish an opaque byte payload.
    pub fn publish(
        &mut self,
        topic: &str,
        payload: impl AsRef<[u8]>,
        options: PublishOptions,
    ) -> Result<()> {
        self.ensure_connected()?;
        validate_topic_name(topic)?;
        self.server_capabilities
            .validate_publish(options.qos, options.retain)?;
        let packet_id = (options.qos != QoS::AtMostOnce).then(|| self.next_packet_id());
        let packet = PublishPacket {
            topic: String::from(topic),
            packet_id,
            payload: payload.as_ref().to_vec(),
            qos: options.qos,
            retain: options.retain,
            dup: false,
            properties: options.properties,
        };
        self.send_encoded(&packet.encode()?, "publish")?;

        if let Some(packet_id) = packet_id {
            self.wait_for_puback(packet_id)?;
        }
        Ok(())
    }

    /// Serialize a payload as JSON before publishing it.
    #[cfg(feature = "serde")]
    pub fn publish_json<P: serde::Serialize>(
        &mut self,
        topic: &str,
        payload: &P,
        options: PublishOptions,
    ) -> Result<()> {
        let bytes =
            serde_json::to_vec(payload).map_err(|error| Error::Serialize(error.to_string()))?;
        self.publish(topic, bytes, options)
    }

    /// Subscribe to a single topic filter and wait for its SUBACK.
    pub fn subscribe(&mut self, filter: &str, qos: QoS) -> Result<()> {
        self.ensure_connected()?;
        self.server_capabilities
            .validate_subscription_filter(filter)?;
        let packet_id = self.next_packet_id();
        let packet = SubscribePacket {
            packet_id,
            filters: vec![(String::from(filter), qos)],
            properties: Properties::new(),
        };
        self.send_encoded(&packet.encode()?, "subscribe")?;
        self.wait_for_suback(packet_id, qos)
    }

    /// Unsubscribe from a single topic filter and wait for its UNSUBACK.
    pub fn unsubscribe(&mut self, filter: &str) -> Result<()> {
        self.ensure_connected()?;
        validate_topic_filter(filter)?;
        let packet_id = self.next_packet_id();
        let packet = UnsubscribePacket {
            packet_id,
            filters: vec![String::from(filter)],
            properties: Properties::new(),
        };
        self.send_encoded(&packet.encode()?, "unsubscribe")?;
        self.wait_for_unsuback(packet_id)
    }

    /// Receive the next incoming PUBLISH, blocking while maintaining keepalive.
    /// A normal server DISCONNECT returns `Ok(None)`.
    pub fn recv(&mut self) -> Result<Option<PublishPacket>> {
        self.ensure_connected()?;
        if let Some(message) = self.incoming.pop_front() {
            return Ok(Some(message));
        }

        loop {
            match self.read_packet_until(None)? {
                Packet::Publish(packet) => {
                    self.accept_publish(packet)?;
                    return Ok(self.incoming.pop_front());
                }
                Packet::Disconnect(packet) => {
                    self.connected = false;
                    return if packet.reason_code == 0x00 {
                        Ok(None)
                    } else {
                        Err(Error::ServerDisconnected(packet.reason_code))
                    };
                }
                Packet::PingResp => {}
                _ => return self.protocol_failure("unexpected packet while receiving"),
            }
        }
    }

    pub fn incoming(&mut self) -> Incoming<'_, T> {
        Incoming {
            client: self,
            done: false,
        }
    }

    /// Send a normal MQTT DISCONNECT and close the transport.
    pub fn disconnect(mut self) -> Result<()> {
        if self.connected {
            let packet = DisconnectPacket { reason_code: 0x00 };
            self.send_encoded(&packet.encode()?, "disconnect")?;
            self.connected = false;
        }
        self.stream.shutdown()?;
        Ok(())
    }

    fn ensure_connected(&self) -> Result<()> {
        if self.connected {
            Ok(())
        } else {
            Err(Error::ClientClosed)
        }
    }

    fn send_encoded(&mut self, bytes: &[u8], operation: &'static str) -> Result<()> {
        if bytes.len() > self.max_outbound_packet_size {
            return Err(Error::PacketTooLarge {
                size: bytes.len(),
                max: self.max_outbound_packet_size,
            });
        }
        if let Err(error) = map_write_result(self.stream.write_all(bytes), operation) {
            self.connected = false;
            return Err(error);
        }
        if let Err(error) = map_write_result(self.stream.flush(), operation) {
            self.connected = false;
            return Err(error);
        }
        self.last_write_at = Instant::now();
        Ok(())
    }

    fn send_puback(&mut self, packet_id: u16) -> Result<()> {
        let packet = PubAckPacket {
            packet_id,
            reason_code: 0x00,
        };
        self.send_encoded(&packet.encode()?, "PUBACK")
    }

    fn next_packet_id(&mut self) -> u16 {
        let id = self.next_packet_id;
        self.next_packet_id = self.next_packet_id.wrapping_add(1);
        if self.next_packet_id == 0 {
            self.next_packet_id = 1;
        }
        id
    }

    fn accept_publish(&mut self, packet: PublishPacket) -> Result<()> {
        if self.incoming.len() >= self.max_incoming_messages {
            return Err(Error::QueueFull("incoming message"));
        }

        let packet_id = packet.packet_id;
        let qos = packet.qos;
        self.incoming.push_back(packet);

        if qos == QoS::AtLeastOnce {
            self.send_puback(packet_id.ok_or(Error::MalformedPacket(
                "QoS 1 PUBLISH missing packet identifier",
            ))?)?;
        }
        Ok(())
    }

    fn read_packet_until(&mut self, deadline: Option<Deadline>) -> Result<Packet> {
        loop {
            match self.frame_reader.try_decode() {
                Ok(Some(packet)) => {
                    self.last_read_at = Instant::now();
                    if matches!(packet, Packet::PingResp) {
                        self.ping_sent_at = None;
                    }
                    return Ok(packet);
                }
                Ok(None) => {}
                Err(error) => {
                    self.connected = false;
                    return Err(error);
                }
            }

            self.service_timers(deadline)?;
            if let Err(error) = self
                .stream
                .set_read_timeout(self.next_read_timeout(deadline))
            {
                self.connected = false;
                return Err(Error::Io(error));
            }

            let capacity = self.frame_reader.remaining_capacity();
            if capacity == 0 {
                self.connected = false;
                return Err(Error::PacketTooLarge {
                    size: self.frame_reader.max_packet_size().saturating_add(1),
                    max: self.frame_reader.max_packet_size(),
                });
            }

            let mut bytes = [0u8; 8192];
            let read_len = capacity.min(bytes.len());
            match self.stream.read(&mut bytes[..read_len]) {
                Ok(0) => {
                    self.connected = false;
                    return Err(Error::ConnectionClosed);
                }
                Ok(read) => {
                    self.last_read_at = Instant::now();
                    self.frame_reader.push(&bytes[..read]);
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error)
                    if error.kind() == ErrorKind::TimedOut
                        || error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => {
                    self.connected = false;
                    return Err(Error::Io(error));
                }
            }
        }
    }

    fn service_timers(&mut self, deadline: Option<Deadline>) -> Result<()> {
        let now = Instant::now();
        if let Some((at, operation)) = deadline {
            if now >= at {
                return Err(Error::Timeout(operation));
            }
        }
        if !self.connected {
            return Ok(());
        }

        if let Some(sent_at) = self.ping_sent_at {
            if now.duration_since(sent_at) >= self.ack_timeout {
                self.connected = false;
                return Err(Error::KeepAliveTimeout);
            }
            return Ok(());
        }

        if let Some(interval) = ping_interval(self.keep_alive_secs) {
            if now.duration_since(self.last_write_at) >= interval {
                self.send_encoded(&PINGREQ_BYTES, "keepalive")?;
                self.ping_sent_at = Some(Instant::now());
            }
        }
        Ok(())
    }

    fn next_read_timeout(&self, deadline: Option<Deadline>) -> Option<Duration> {
        let now = Instant::now();
        let mut wake_at = deadline.map(|(at, _)| at);

        if self.connected {
            let timer_at = if let Some(sent_at) = self.ping_sent_at {
                sent_at.checked_add(self.ack_timeout)
            } else {
                ping_interval(self.keep_alive_secs)
                    .and_then(|interval| self.last_write_at.checked_add(interval))
            };
            if let Some(timer_at) = timer_at {
                wake_at = Some(wake_at.map_or(timer_at, |current| current.min(timer_at)));
            }
        }

        wake_at.map(|at| {
            at.saturating_duration_since(now)
                .max(Duration::from_millis(1))
        })
    }

    fn ack_deadline(&self, packet: &'static str) -> Result<Deadline> {
        deadline_after(self.ack_timeout, packet)
    }

    fn wait_for_puback(&mut self, packet_id: u16) -> Result<()> {
        let deadline = self.ack_deadline("PUBACK")?;
        loop {
            match self.read_ack_packet(deadline)? {
                Packet::PubAck(ack) if ack.packet_id == packet_id => {
                    return validate_ack_code("PUBACK", ack.reason_code);
                }
                Packet::Publish(packet) => self.accept_publish_during_ack(packet)?,
                Packet::Disconnect(packet) => return self.server_disconnect(packet),
                Packet::PingResp => {}
                _ => return self.protocol_failure("unexpected packet while awaiting PUBACK"),
            }
        }
    }

    fn wait_for_suback(&mut self, packet_id: u16, requested_qos: QoS) -> Result<()> {
        let deadline = self.ack_deadline("SUBACK")?;
        loop {
            match self.read_ack_packet(deadline)? {
                Packet::SubAck(ack) if ack.packet_id == packet_id => {
                    let result = validate_suback_codes(&ack.reason_codes, &[requested_qos]);
                    if matches!(result, Err(Error::MalformedPacket(_))) {
                        self.connected = false;
                    }
                    return result;
                }
                Packet::Publish(packet) => self.accept_publish_during_ack(packet)?,
                Packet::Disconnect(packet) => return self.server_disconnect(packet),
                Packet::PingResp => {}
                _ => return self.protocol_failure("unexpected packet while awaiting SUBACK"),
            }
        }
    }

    fn wait_for_unsuback(&mut self, packet_id: u16) -> Result<()> {
        let deadline = self.ack_deadline("UNSUBACK")?;
        loop {
            match self.read_ack_packet(deadline)? {
                Packet::UnsubAck(ack) if ack.packet_id == packet_id => {
                    let result = validate_ack_codes("UNSUBACK", &ack.reason_codes, 1);
                    if matches!(result, Err(Error::MalformedPacket(_))) {
                        self.connected = false;
                    }
                    return result;
                }
                Packet::Publish(packet) => self.accept_publish_during_ack(packet)?,
                Packet::Disconnect(packet) => return self.server_disconnect(packet),
                Packet::PingResp => {}
                _ => return self.protocol_failure("unexpected packet while awaiting UNSUBACK"),
            }
        }
    }

    fn server_disconnect(&mut self, packet: DisconnectPacket) -> Result<()> {
        self.connected = false;
        if packet.reason_code == 0x00 {
            Err(Error::ConnectionClosed)
        } else {
            Err(Error::ServerDisconnected(packet.reason_code))
        }
    }

    fn read_ack_packet(&mut self, deadline: Deadline) -> Result<Packet> {
        match self.read_packet_until(Some(deadline)) {
            Err(error @ Error::Timeout(_)) => {
                // A late acknowledgement cannot be distinguished from an ACK
                // for a packet identifier reused by a later operation.
                self.connected = false;
                Err(error)
            }
            result => result,
        }
    }

    fn accept_publish_during_ack(&mut self, packet: PublishPacket) -> Result<()> {
        match self.accept_publish(packet) {
            Err(error @ Error::QueueFull(_)) => {
                // Returning before the outstanding control ACK arrives would
                // leave its packet identifier unsafe to reuse.
                self.connected = false;
                Err(error)
            }
            result => result,
        }
    }

    fn protocol_failure<R>(&mut self, message: &'static str) -> Result<R> {
        self.connected = false;
        Err(Error::UnexpectedPacket(message))
    }
}

impl<T: Transport> Iterator for Incoming<'_, T> {
    type Item = Result<PublishPacket>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.client.recv() {
            Ok(Some(message)) => Some(Ok(message)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }
}

pub(crate) fn ping_interval(keep_alive_secs: u16) -> Option<Duration> {
    (keep_alive_secs != 0).then(|| Duration::from_secs(u64::from(keep_alive_secs)))
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
    reason_codes
        .iter()
        .try_for_each(|reason_code| validate_ack_code(packet, *reason_code))
}

fn deadline_after(timeout: Duration, operation: &'static str) -> Result<Deadline> {
    Instant::now()
        .checked_add(timeout)
        .map(|deadline| (deadline, operation))
        .ok_or(Error::InvalidOptions("timeout is too large"))
}

fn map_write_result(result: io::Result<()>, operation: &'static str) -> Result<()> {
    match result {
        Err(error)
            if error.kind() == ErrorKind::TimedOut || error.kind() == ErrorKind::WouldBlock =>
        {
            Err(Error::Timeout(operation))
        }
        Err(error) => Err(Error::Io(error)),
        Ok(()) => Ok(()),
    }
}

fn connect_tcp(addr: &str, deadline: Instant) -> io::Result<TcpStream> {
    let mut attempted = false;
    let mut last_error = None;

    for socket_addr in addr.to_socket_addrs()? {
        attempted = true;
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(ErrorKind::TimedOut, "TCP connect timed out"));
        }
        match TcpStream::connect_timeout(&socket_addr, deadline.duration_since(now)) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }

    if !attempted {
        Err(io::Error::new(
            ErrorKind::InvalidInput,
            "address resolved to no socket addresses",
        ))
    } else {
        Err(last_error
            .unwrap_or_else(|| io::Error::new(ErrorKind::ConnectionRefused, "TCP connect failed")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    enum ReadStep {
        Data(Vec<u8>),
        Error(ErrorKind),
        TimeoutForever,
        Eof,
    }

    struct MockTransport {
        reads: VecDeque<ReadStep>,
        writes: Vec<Vec<u8>>,
        max_read_request: usize,
    }

    impl MockTransport {
        fn new(reads: impl IntoIterator<Item = ReadStep>) -> Self {
            Self {
                reads: reads.into_iter().collect(),
                writes: Vec::new(),
                max_read_request: 0,
            }
        }
    }

    impl Transport for MockTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes.push(buf.to_vec());
            Ok(buf.len())
        }

        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.max_read_request = self.max_read_request.max(buf.len());
            match self.reads.pop_front().unwrap_or(ReadStep::Eof) {
                ReadStep::Data(data) => {
                    let read = data.len().min(buf.len());
                    buf[..read].copy_from_slice(&data[..read]);
                    if read < data.len() {
                        self.reads.push_front(ReadStep::Data(data[read..].to_vec()));
                    }
                    Ok(read)
                }
                ReadStep::Error(kind) => Err(io::Error::from(kind)),
                ReadStep::TimeoutForever => {
                    self.reads.push_front(ReadStep::TimeoutForever);
                    Err(io::Error::from(ErrorKind::TimedOut))
                }
                ReadStep::Eof => Ok(0),
            }
        }

        fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
            let mut read = 0;
            while read < buf.len() {
                let count = self.read(&mut buf[read..])?;
                if count == 0 {
                    return Err(ErrorKind::UnexpectedEof.into());
                }
                read += count;
            }
            Ok(())
        }

        fn set_nonblocking(&mut self, _nonblocking: bool) -> io::Result<()> {
            Ok(())
        }

        fn set_read_timeout(&self, _duration: Option<Duration>) -> io::Result<()> {
            Ok(())
        }

        fn shutdown(&self) -> io::Result<()> {
            Ok(())
        }
    }

    fn client_with(
        stream: MockTransport,
        keep_alive_secs: u16,
        ack_timeout: Duration,
        max_incoming_messages: usize,
    ) -> MqttClient<MockTransport> {
        let now = Instant::now();
        MqttClient {
            stream,
            frame_reader: FrameReader::with_max_packet_size(1024),
            incoming: VecDeque::new(),
            max_incoming_messages,
            next_packet_id: 1,
            keep_alive_secs,
            ack_timeout,
            max_outbound_packet_size: MQTT_MAX_PACKET_SIZE,
            server_capabilities: ServerCapabilities::default(),
            last_read_at: now,
            last_write_at: now,
            ping_sent_at: None,
            connected: true,
        }
    }

    fn encode_suback(packet_id: u16, reason_code: u8) -> Vec<u8> {
        let mut body = Vec::new();
        crate::codec::encode::encode_u16(&mut body, packet_id);
        Properties::new().encode(&mut body).unwrap();
        body.push(reason_code);
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

    fn encode_unsuback(packet_id: u16, reason_code: u8) -> Vec<u8> {
        let mut body = Vec::new();
        crate::codec::encode::encode_u16(&mut body, packet_id);
        Properties::new().encode(&mut body).unwrap();
        body.push(reason_code);
        let mut packet = Vec::new();
        crate::codec::encode::encode_fixed_header(
            &mut packet,
            PacketType::UnsubAck,
            0,
            body.len() as u32,
        )
        .unwrap();
        packet.extend_from_slice(&body);
        packet
    }

    fn encode_connack(session_present: bool, properties: Properties) -> Vec<u8> {
        let mut body = vec![u8::from(session_present), 0x00];
        properties.encode(&mut body).unwrap();
        let mut packet = Vec::new();
        crate::codec::encode::encode_fixed_header(
            &mut packet,
            PacketType::ConnAck,
            0,
            body.len() as u32,
        )
        .unwrap();
        packet.extend_from_slice(&body);
        packet
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
        let mut client = client_with(stream, 0, Duration::from_secs(1), 8);

        let error = client
            .publish(
                "topic",
                b"payload",
                PublishOptions::default().with_qos(QoS::AtLeastOnce),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            Error::AckRejected {
                packet: "PUBACK",
                reason_code: 0x80
            }
        ));
    }

    #[test]
    fn inbound_publish_is_preserved_while_waiting_for_ack() {
        let inbound = PublishPacket {
            topic: String::from("events/one"),
            packet_id: None,
            payload: b"event".to_vec(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
        }
        .encode()
        .unwrap();
        let puback = PubAckPacket {
            packet_id: 1,
            reason_code: 0x00,
        }
        .encode()
        .unwrap();
        let mut bytes = inbound;
        bytes.extend_from_slice(&puback);
        let stream = MockTransport::new([ReadStep::Data(bytes)]);
        let mut client = client_with(stream, 0, Duration::from_secs(1), 8);

        client
            .publish(
                "commands",
                b"go",
                PublishOptions::default().with_qos(QoS::AtLeastOnce),
            )
            .unwrap();
        let message = client.recv().unwrap().unwrap();
        assert_eq!(message.topic, "events/one");
        assert_eq!(message.payload, b"event");
    }

    #[test]
    fn subscribe_surfaces_negative_suback() {
        let stream = MockTransport::new([ReadStep::Data(encode_suback(1, 0x80))]);
        let mut client = client_with(stream, 0, Duration::from_secs(1), 8);

        let error = client.subscribe("topic", QoS::AtMostOnce).unwrap_err();
        assert!(matches!(
            error,
            Error::AckRejected {
                packet: "SUBACK",
                reason_code: 0x80
            }
        ));
    }

    #[test]
    fn invalid_topics_fail_without_writing_or_closing_the_blocking_client() {
        let stream = MockTransport::new([]);
        let mut client = client_with(stream, 0, Duration::from_secs(1), 8);

        assert!(matches!(
            client.publish("events/#", b"payload", PublishOptions::default()),
            Err(Error::MalformedPacket("topic name contains a wildcard"))
        ));
        assert!(client.subscribe("events/#/new", QoS::AtMostOnce).is_err());
        assert!(client.unsubscribe("").is_err());
        assert!(client.connected);
        assert!(client.stream.writes.is_empty());
    }

    #[test]
    fn broker_capabilities_reject_new_operations_but_allow_unsubscribe() {
        let stream = MockTransport::new([
            ReadStep::Data(encode_unsuback(1, 0x00)),
            ReadStep::Data(encode_unsuback(2, 0x00)),
        ]);
        let mut client = client_with(stream, 0, Duration::from_secs(1), 8);
        client.server_capabilities = ServerCapabilities {
            maximum_qos: QoS::AtMostOnce,
            retain_available: false,
            wildcard_subscriptions_available: false,
            shared_subscriptions_available: false,
        };

        assert!(matches!(
            client.publish(
                "events/new",
                b"payload",
                PublishOptions::default().with_qos(QoS::AtLeastOnce),
            ),
            Err(Error::InvalidOptions(
                "broker does not support QoS 1 publishing"
            ))
        ));
        assert!(matches!(
            client.publish(
                "events/new",
                b"payload",
                PublishOptions::default().with_retain(true),
            ),
            Err(Error::InvalidOptions(
                "broker does not support retained publishing"
            ))
        ));
        assert!(matches!(
            client.subscribe("events/+", QoS::AtMostOnce),
            Err(Error::InvalidOptions(
                "broker does not support wildcard subscriptions"
            ))
        ));
        assert!(matches!(
            client.subscribe("$share/workers/events", QoS::AtMostOnce),
            Err(Error::InvalidOptions(
                "broker does not support shared subscriptions"
            ))
        ));
        assert!(client.stream.writes.is_empty());

        client.unsubscribe("events/#").unwrap();
        client.unsubscribe("$share/workers/events/#").unwrap();
        assert_eq!(client.stream.writes.len(), 2);
        assert!(client.connected);
    }

    #[test]
    fn subscribe_rejects_qos_grant_above_request_and_closes_session() {
        let stream = MockTransport::new([ReadStep::Data(encode_suback(1, 0x01))]);
        let mut client = client_with(stream, 0, Duration::from_secs(1), 8);

        assert!(matches!(
            client.subscribe("topic", QoS::AtMostOnce),
            Err(Error::MalformedPacket(
                "SUBACK granted unsupported or unrequested QoS"
            ))
        ));
        assert!(matches!(
            client.subscribe("other", QoS::AtMostOnce),
            Err(Error::ClientClosed)
        ));
    }

    #[test]
    fn subscribe_rejects_unsupported_qos_two_grant() {
        let stream = MockTransport::new([ReadStep::Data(encode_suback(1, 0x02))]);
        let mut client = client_with(stream, 0, Duration::from_secs(1), 8);

        assert!(matches!(
            client.subscribe("topic", QoS::AtLeastOnce),
            Err(Error::MalformedPacket(
                "SUBACK granted unsupported or unrequested QoS"
            ))
        ));
        assert!(!client.connected);
    }

    #[test]
    fn full_incoming_queue_is_reported_during_ack_wait() {
        let inbound = PublishPacket {
            topic: String::from("events/overflow"),
            packet_id: None,
            payload: Vec::new(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
        }
        .encode()
        .unwrap();
        let stream = MockTransport::new([ReadStep::Data(inbound)]);
        let mut client = client_with(stream, 0, Duration::from_secs(1), 1);
        client.incoming.push_back(PublishPacket {
            topic: String::from("already/queued"),
            payload: Vec::new(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            packet_id: None,
            properties: Properties::new(),
        });

        let error = client
            .publish(
                "commands",
                b"go",
                PublishOptions::default().with_qos(QoS::AtLeastOnce),
            )
            .unwrap_err();
        assert!(matches!(error, Error::QueueFull("incoming message")));
        assert!(matches!(
            client.subscribe("events", QoS::AtMostOnce),
            Err(Error::ClientClosed)
        ));
    }

    #[test]
    fn missing_ping_response_has_a_deadline() {
        let stream = MockTransport::new([ReadStep::Error(ErrorKind::TimedOut)]);
        let mut client = client_with(stream, 10, Duration::from_millis(1), 8);
        client.ping_sent_at = Some(Instant::now() - Duration::from_millis(2));

        assert!(matches!(
            client.read_packet_until(None),
            Err(Error::KeepAliveTimeout)
        ));
    }

    #[test]
    fn acknowledgement_timeout_closes_ambiguous_session() {
        let stream = MockTransport::new([ReadStep::TimeoutForever]);
        let mut client = client_with(stream, 0, Duration::from_millis(1), 8);

        assert!(matches!(
            client.publish(
                "commands",
                b"go",
                PublishOptions::default().with_qos(QoS::AtLeastOnce),
            ),
            Err(Error::Timeout("PUBACK"))
        ));
        assert!(matches!(
            client.subscribe("events", QoS::AtMostOnce),
            Err(Error::ClientClosed)
        ));
    }

    #[test]
    fn packet_reads_respect_frame_capacity() {
        let packet = PublishPacket {
            topic: String::from("bounded"),
            packet_id: None,
            payload: vec![0; 24],
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
        }
        .encode()
        .unwrap();
        let stream = MockTransport::new([ReadStep::Data(packet)]);
        let mut client = client_with(stream, 0, Duration::from_secs(1), 8);
        client.frame_reader = FrameReader::with_max_packet_size(64);

        assert!(matches!(
            client.read_packet_until(None).unwrap(),
            Packet::Publish(_)
        ));
        assert!(client.stream.max_read_request <= 64);
    }

    #[test]
    fn fatal_read_error_poisons_client_and_terminates_incoming_iterator() {
        let stream = MockTransport::new([ReadStep::Error(ErrorKind::ConnectionReset)]);
        let mut client = client_with(stream, 0, Duration::from_secs(1), 8);

        {
            let mut incoming = client.incoming();
            assert!(matches!(incoming.next(), Some(Err(Error::Io(_)))));
            assert!(incoming.next().is_none());
        }
        assert!(matches!(
            client.subscribe("events", QoS::AtMostOnce),
            Err(Error::ClientClosed)
        ));
    }

    #[test]
    fn connect_advertises_packet_and_receive_limits() {
        // CONNACK: session=false, success, empty properties.
        let stream = MockTransport::new([ReadStep::Data(vec![0x20, 0x03, 0, 0, 0])]);
        let options = ConnectOptions::new("bounded-client")
            .with_max_packet_size(4096)
            .with_max_incoming_messages(7);
        let client = MqttClient::connect_with(stream, options).unwrap();
        let connect = &client.stream.writes[0];

        let (header, header_len) = crate::codec::decode::decode_fixed_header(connect).unwrap();
        assert_eq!(header.packet_type, PacketType::Connect);
        let mut cursor = crate::codec::decode::Cursor::new(&connect[header_len..]);
        assert_eq!(cursor.read_string().unwrap(), "MQTT");
        assert_eq!(cursor.read_u8().unwrap(), 5);
        cursor.read_u8().unwrap();
        cursor.read_u16().unwrap();
        let properties = Properties::decode(&mut cursor).unwrap();

        assert_eq!(
            properties.get_u32(PropertyId::MaximumPacketSize),
            Some(4096)
        );
        assert_eq!(properties.get_u16(PropertyId::ReceiveMaximum), Some(7));
    }

    #[test]
    fn connect_validates_session_present_and_assigned_client_identifier() {
        let missing_assignment =
            MockTransport::new([ReadStep::Data(encode_connack(false, Properties::new()))]);
        assert!(matches!(
            MqttClient::connect_with(missing_assignment, ConnectOptions::new("")),
            Err(Error::MalformedPacket(
                "CONNACK omitted a non-empty assigned client identifier"
            ))
        ));

        let resumed_after_clean_start =
            MockTransport::new([ReadStep::Data(encode_connack(true, Properties::new()))]);
        assert!(matches!(
            MqttClient::connect_with(
                resumed_after_clean_start,
                ConnectOptions::new("requested-client")
            ),
            Err(Error::MalformedPacket(
                "CONNACK has session present after clean start"
            ))
        ));

        let mut properties = Properties::new();
        properties.push(
            PropertyId::AssignedClientIdentifier,
            PropertyValue::Str(String::from("assigned-client")),
        );
        let assigned = MockTransport::new([ReadStep::Data(encode_connack(false, properties))]);
        assert!(MqttClient::connect_with(
            assigned,
            ConnectOptions::new("").with_clean_start(false)
        )
        .is_ok());
    }
}
