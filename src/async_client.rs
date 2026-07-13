//! Tokio current-thread MQTT client and connection driver.
//!
//! [`AsyncMqttClient`] is a cheap, cloneable command handle. [`MqttConnection`]
//! exclusively owns the transport and must be driven by calling
//! [`MqttConnection::run`]. Operation futures only wait on bounded channels;
//! they never read from or write to the socket themselves.
//!
//! A timed-out or cancelled operation whose packet may have reached the wire
//! retains a bounded packet-identifier tombstone until its late ACK arrives or
//! the connection closes. This can apply backpressure after repeated broker
//! failures, but prevents a late ACK from completing the wrong operation.

#[cfg(feature = "request-response")]
use std::collections::HashSet;
use std::collections::{HashMap, VecDeque};
use std::io::{self, ErrorKind};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
#[cfg(feature = "request-response")]
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, Mutex};

use crate::codec::connect::{validate_connack, ServerCapabilities};
use crate::codec::decode::decode_fixed_header;
use crate::codec::ping::PINGREQ_BYTES;
use crate::codec::properties::{Properties, PropertyId, PropertyValue};
use crate::codec::subscribe::validate_suback_codes;
use crate::codec::topic::{validate_topic_filter, validate_topic_name};
use crate::codec::types::*;
use crate::error::{Error, Result};
use crate::frame::FrameReader;
use crate::options::{ConnectOptions, PublishOptions};
use crate::transport::Transport;

/// Why an [`Event::Disconnected`] event was emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    ClientInitiated,
    Server(u8),
    TransportClosed,
}

/// Unsolicited events emitted by [`MqttConnection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Publish(PublishPacket),
    Disconnected(DisconnectReason),
}

/// Result of a completed outgoing publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishAck {
    /// `None` for QoS 0, and the MQTT packet identifier for QoS 1.
    pub packet_id: Option<u16>,
    /// `None` for QoS 0, and the broker's PUBACK reason code for QoS 1.
    pub reason_code: Option<u8>,
}

/// Result of a completed subscription operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionAck {
    pub packet_id: u16,
    pub reason_codes: Vec<u8>,
}

/// Cloneable command handle for an MQTT connection.
///
/// The handle performs no socket I/O. A corresponding [`MqttConnection`] must
/// be running concurrently for its operation futures to make progress.
#[derive(Clone)]
pub struct AsyncMqttClient {
    commands: mpsc::Sender<Command>,
    events: Arc<Mutex<mpsc::Receiver<Event>>>,
    max_packet_size: usize,
    #[cfg(feature = "request-response")]
    request_lifecycle: Arc<StdMutex<RequestLifecycle>>,
}

#[cfg(feature = "request-response")]
pub(crate) struct RequestLifecycle {
    active: HashSet<Vec<u8>>,
    cancelled: HashSet<Vec<u8>>,
    max_active: usize,
}

#[cfg(feature = "request-response")]
impl RequestLifecycle {
    fn new(max_active: usize) -> Self {
        Self {
            active: HashSet::with_capacity(max_active),
            cancelled: HashSet::with_capacity(max_active),
            max_active,
        }
    }

    fn register(&mut self, correlation_data: Vec<u8>) -> Result<()> {
        if self.active.len() >= self.max_active {
            return Err(Error::QueueFull("pending request"));
        }
        if !self.active.insert(correlation_data) {
            return Err(Error::InvalidOptions("duplicate request correlation data"));
        }
        Ok(())
    }

    pub(crate) fn cancel(&mut self, correlation_data: Vec<u8>) {
        if self.active.contains(&correlation_data) {
            self.cancelled.insert(correlation_data);
        }
    }

    fn consume_cancellation(&mut self, correlation_data: &[u8]) -> bool {
        if self.cancelled.remove(correlation_data) {
            self.active.remove(correlation_data);
            true
        } else {
            false
        }
    }

    fn cancelled(&self) -> impl Iterator<Item = &[u8]> {
        self.cancelled.iter().map(Vec::as_slice)
    }

    pub(crate) fn finish(&mut self, correlation_data: &[u8]) {
        self.active.remove(correlation_data);
        self.cancelled.remove(correlation_data);
    }
}

impl AsyncMqttClient {
    /// Open a TCP connection and perform the MQTT v5 CONNECT/CONNACK handshake.
    ///
    /// The handshake is deliberately synchronous. After this function returns,
    /// call [`MqttConnection::run`] on the returned driver.
    pub fn connect(
        addr: impl ToSocketAddrs,
        options: ConnectOptions,
    ) -> Result<(Self, MqttConnection<TcpStream>)> {
        options.validate()?;
        let deadline = connect_deadline(options.connect_timeout)?;
        let stream = connect_tcp_until(addr, deadline)?;
        Self::connect_with_deadline(stream, options, deadline)
    }

    /// Perform a synchronous MQTT v5 handshake over a caller-provided transport.
    pub fn connect_with<T: Transport>(
        stream: T,
        options: ConnectOptions,
    ) -> Result<(Self, MqttConnection<T>)> {
        options.validate()?;
        let deadline = connect_deadline(options.connect_timeout)?;
        Self::connect_with_deadline(stream, options, deadline)
    }

    fn connect_with_deadline<T: Transport>(
        mut stream: T,
        options: ConnectOptions,
        deadline: Instant,
    ) -> Result<(Self, MqttConnection<T>)> {
        stream.set_nonblocking(false)?;
        set_connect_timeouts(&stream, deadline)?;

        let advertised_max = u32::try_from(options.max_packet_size)
            .map_err(|_| Error::MalformedPacket("maximum packet size exceeds MQTT limit"))?;
        let mut properties = Properties::new();
        properties.push(
            PropertyId::MaximumPacketSize,
            PropertyValue::U32(advertised_max),
        );
        properties.push(
            PropertyId::ReceiveMaximum,
            PropertyValue::U16(options.event_capacity.min(usize::from(u16::MAX)) as u16),
        );
        let connect = ConnectPacket {
            protocol_version: 5,
            clean_start: options.clean_start,
            keep_alive: options.keep_alive_secs,
            client_id: options.client_id.clone(),
            username: options.username.clone(),
            password: options.password.clone(),
            properties,
        };
        let connect_bytes = connect.encode()?;
        write_all_until(&mut stream, &connect_bytes, deadline)?;
        stream.set_write_timeout(Some(connect_remaining(deadline)?))?;
        map_connect_io(stream.flush())?;
        connect_remaining(deadline)?;

        let connack = match read_packet_blocking(&mut stream, options.max_packet_size, deadline)? {
            Packet::ConnAck(connack) if connack.reason_code < 0x80 => connack,
            Packet::ConnAck(connack) => return Err(Error::ConnectionRefused(connack.reason_code)),
            _ => return Err(Error::UnexpectedPacket("expected CONNACK")),
        };
        connect_remaining(deadline)?;
        validate_connack(&connect.client_id, connect.clean_start, &connack)?;
        let server_capabilities = ServerCapabilities::from_connack(&connack);

        let keep_alive_secs = connack
            .properties
            .get_u16(PropertyId::ServerKeepAlive)
            .unwrap_or(options.keep_alive_secs);
        let peer_max_packet_size = connack
            .properties
            .get_u32(PropertyId::MaximumPacketSize)
            .map(|value| value as usize)
            .unwrap_or(268_435_460);
        let peer_receive_maximum = connack
            .properties
            .get_u16(PropertyId::ReceiveMaximum)
            .unwrap_or(u16::MAX);

        stream.set_read_timeout(None)?;
        stream.set_write_timeout(None)?;
        stream.set_nonblocking(true)?;

        let (command_tx, command_rx) = mpsc::channel(options.command_capacity);
        let (event_tx, event_rx) = mpsc::channel(options.event_capacity);
        #[cfg(feature = "request-response")]
        let request_lifecycle = Arc::new(StdMutex::new(RequestLifecycle::new(
            options.command_capacity,
        )));
        let now = Instant::now();

        let client = Self {
            commands: command_tx,
            events: Arc::new(Mutex::new(event_rx)),
            max_packet_size: peer_max_packet_size,
            #[cfg(feature = "request-response")]
            request_lifecycle: request_lifecycle.clone(),
        };
        let connection = MqttConnection {
            stream,
            frame_reader: FrameReader::with_max_packet_size(options.max_packet_size),
            commands: command_rx,
            events: event_tx,
            #[cfg(feature = "request-response")]
            request_lifecycle,
            outbound: VecDeque::new(),
            outbound_bytes: 0,
            pending_acks: HashMap::new(),
            next_packet_id: 1,
            last_write_at: now,
            ping_outstanding_since: None,
            keep_alive_secs,
            peer_max_packet_size,
            peer_receive_maximum,
            server_capabilities,
            max_outbound_queue_bytes: options.max_outbound_queue_bytes,
            max_pending_operations: options.command_capacity,
            ack_timeout: options.ack_timeout,
            poll_interval: options.poll_interval,
            closing: false,
            command_channel_closed: false,
            needs_flush: false,
            flush_actions: VecDeque::new(),
            ping_queued: false,
            terminal_event_sent: false,
            #[cfg(feature = "request-response")]
            pending_requests: HashMap::new(),
            #[cfg(feature = "request-response")]
            response_subscriptions: HashMap::new(),
        };

        Ok((client, connection))
    }

    /// Publish raw bytes and wait until they are written (QoS 0) or acknowledged
    /// by the broker (QoS 1).
    pub async fn publish(
        &self,
        topic: impl Into<String>,
        payload: impl Into<Vec<u8>>,
        options: PublishOptions,
    ) -> Result<PublishAck> {
        let payload = payload.into();
        if payload.len() > self.max_packet_size {
            return Err(Error::PacketTooLarge {
                size: payload.len(),
                max: self.max_packet_size,
            });
        }
        let (result_tx, result_rx) = oneshot::channel();
        self.send_command(Command::Publish {
            topic: topic.into(),
            payload,
            options,
            result: result_tx,
        })?;
        receive_result(result_rx).await
    }

    /// Serialize a payload as JSON before publishing it.
    #[cfg(feature = "serde")]
    pub async fn publish_json<P: serde::Serialize>(
        &self,
        topic: impl Into<String>,
        payload: &P,
        options: PublishOptions,
    ) -> Result<PublishAck> {
        let bytes =
            serde_json::to_vec(payload).map_err(|error| Error::Serialize(error.to_string()))?;
        self.publish(topic, bytes, options).await
    }

    /// Subscribe to one topic filter and wait for SUBACK.
    pub async fn subscribe(&self, filter: impl Into<String>, qos: QoS) -> Result<SubscriptionAck> {
        let (result_tx, result_rx) = oneshot::channel();
        self.send_command(Command::Subscribe {
            filter: filter.into(),
            qos,
            result: result_tx,
        })?;
        receive_result(result_rx).await
    }

    /// Unsubscribe from one topic filter and wait for UNSUBACK.
    pub async fn unsubscribe(&self, filter: impl Into<String>) -> Result<SubscriptionAck> {
        let (result_tx, result_rx) = oneshot::channel();
        self.send_command(Command::Unsubscribe {
            filter: filter.into(),
            result: result_tx,
        })?;
        receive_result(result_rx).await
    }

    /// Request a graceful MQTT disconnect and wait until DISCONNECT is written.
    pub async fn disconnect(&self) -> Result<()> {
        let (result_tx, result_rx) = oneshot::channel();
        self.send_command(Command::Disconnect { result: result_tx })?;
        receive_result(result_rx).await
    }

    /// Wait for the next unsolicited MQTT event.
    ///
    /// Cloned handles share a single bounded event stream. Each event is
    /// delivered to exactly one waiter.
    pub async fn next_event(&mut self) -> Option<Event> {
        self.events.lock().await.recv().await
    }

    fn send_command(&self, command: Command) -> Result<()> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => Error::QueueFull("command"),
                mpsc::error::TrySendError::Closed(_) => Error::ClientClosed,
            })
    }

    #[cfg(feature = "request-response")]
    pub(crate) fn start_request(
        &self,
        request: crate::request_response::RequestCommand,
    ) -> Result<oneshot::Receiver<Result<PublishPacket>>> {
        if request.payload.len() > self.max_packet_size {
            return Err(Error::PacketTooLarge {
                size: request.payload.len(),
                max: self.max_packet_size,
            });
        }
        let correlation_data = request.correlation_data.clone();
        self.request_lifecycle
            .lock()
            .expect("request lifecycle mutex poisoned")
            .register(correlation_data.clone())?;
        let (result_tx, result_rx) = oneshot::channel();
        if let Err(error) = self.send_command(Command::Request {
            request,
            result: result_tx,
        }) {
            self.request_lifecycle
                .lock()
                .expect("request lifecycle mutex poisoned")
                .finish(&correlation_data);
            return Err(error);
        }
        Ok(result_rx)
    }

    #[cfg(feature = "request-response")]
    pub(crate) fn request_lifecycle(&self) -> Arc<StdMutex<RequestLifecycle>> {
        self.request_lifecycle.clone()
    }
}

async fn receive_result<T>(receiver: oneshot::Receiver<Result<T>>) -> Result<T> {
    receiver.await.unwrap_or(Err(Error::ClientClosed))
}

/// The sole owner and driver of an MQTT transport.
pub struct MqttConnection<T: Transport = TcpStream> {
    stream: T,
    frame_reader: FrameReader,
    commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Event>,
    #[cfg(feature = "request-response")]
    request_lifecycle: Arc<StdMutex<RequestLifecycle>>,
    outbound: VecDeque<OutboundFrame>,
    outbound_bytes: usize,
    pending_acks: HashMap<u16, PendingAck>,
    next_packet_id: u16,
    last_write_at: Instant,
    ping_outstanding_since: Option<Instant>,
    keep_alive_secs: u16,
    peer_max_packet_size: usize,
    peer_receive_maximum: u16,
    server_capabilities: ServerCapabilities,
    max_outbound_queue_bytes: usize,
    max_pending_operations: usize,
    ack_timeout: Duration,
    poll_interval: Duration,
    closing: bool,
    command_channel_closed: bool,
    needs_flush: bool,
    flush_actions: VecDeque<PendingFlush>,
    ping_queued: bool,
    terminal_event_sent: bool,
    #[cfg(feature = "request-response")]
    pending_requests: HashMap<Vec<u8>, PendingRequest>,
    #[cfg(feature = "request-response")]
    response_subscriptions: HashMap<String, ResponseSubscription>,
}

impl<T: Transport> MqttConnection<T> {
    /// Run the connection until a graceful disconnect, broker disconnect, or
    /// transport/protocol error occurs.
    pub async fn run(mut self) -> Result<()> {
        let result = self.run_inner().await;
        // A clean DISCONNECT still terminates every operation that was waiting
        // for an acknowledgement or response behind it.
        self.fail_pending();
        if result.is_err() && !self.terminal_event_sent {
            let _ = self
                .events
                .try_send(Event::Disconnected(DisconnectReason::TransportClosed));
        }
        let _ = self.stream.shutdown();
        result
    }

    async fn run_inner(&mut self) -> Result<()> {
        loop {
            self.drain_cancellations();
            self.drain_commands()?;
            self.flush_outbound()?;
            if self.closing && self.outbound.is_empty() && !self.needs_flush {
                return Ok(());
            }

            self.read_available()?;
            self.decode_and_dispatch()?;
            self.expire_operations();
            self.maintain_keep_alive()?;
            self.flush_outbound()?;

            if self.closing && self.outbound.is_empty() && !self.needs_flush {
                return Ok(());
            }

            let wait = self.next_wait_duration();
            match tokio::time::timeout(wait, self.commands.recv()).await {
                Ok(Some(command)) => self.process_command(command)?,
                Ok(None) => {
                    self.command_channel_closed = true;
                    self.begin_abandoned_disconnect()?;
                }
                Err(_) => {}
            }
        }
    }

    fn drain_commands(&mut self) -> Result<()> {
        for _ in 0..self.max_pending_operations {
            match self.commands.try_recv() {
                Ok(command) => self.process_command(command)?,
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.command_channel_closed = true;
                    self.begin_abandoned_disconnect()?;
                    break;
                }
            }
        }
        Ok(())
    }

    fn process_command(&mut self, command: Command) -> Result<()> {
        if self.closing {
            command.fail(Error::ClientClosed);
            return Ok(());
        }

        match command {
            Command::Publish {
                topic,
                payload,
                options,
                result,
            } => self.begin_publish(topic, payload, options, result),
            Command::Subscribe {
                filter,
                qos,
                result,
            } => self.begin_subscribe(filter, qos, result),
            Command::Unsubscribe { filter, result } => self.begin_unsubscribe(filter, result),
            Command::Disconnect { result } => self.begin_disconnect(result),
            #[cfg(feature = "request-response")]
            Command::Request { request, result } => {
                let correlation_data = request.correlation_data.clone();
                if let Err(error) = self.begin_request(request, result) {
                    self.fail_request(&correlation_data, error);
                }
                Ok(())
            }
        }
    }

    fn begin_publish(
        &mut self,
        topic: String,
        payload: Vec<u8>,
        options: PublishOptions,
        result: oneshot::Sender<Result<PublishAck>>,
    ) -> Result<()> {
        if let Err(error) = validate_topic_name(&topic).and_then(|()| {
            self.server_capabilities
                .validate_publish(options.qos, options.retain)
        }) {
            let _ = result.send(Err(error));
            return Ok(());
        }
        let packet_id = if options.qos == QoS::AtLeastOnce {
            match self.reserve_packet_id() {
                Ok(id) => Some(id),
                Err(error) => {
                    let _ = result.send(Err(error));
                    return Ok(());
                }
            }
        } else {
            None
        };
        let packet = PublishPacket {
            topic,
            packet_id,
            payload,
            qos: options.qos,
            retain: options.retain,
            dup: false,
            properties: options.properties,
        };
        let bytes = match packet.encode() {
            Ok(bytes) => bytes,
            Err(error) => {
                let _ = result.send(Err(error));
                return Ok(());
            }
        };

        if let Some(packet_id) = packet_id {
            if self.outgoing_qos1_count() >= usize::from(self.peer_receive_maximum) {
                let _ = result.send(Err(Error::QueueFull("broker receive maximum")));
                return Ok(());
            }
            if let Err(error) = self.insert_pending_ack(
                packet_id,
                PendingAckKind::Publish { result },
                "publish acknowledgement",
            ) {
                if let PendingAckKind::Publish { result } = error.kind {
                    let _ = result.send(Err(error.error));
                }
                return Ok(());
            }
            if let Err(error) = self.enqueue(bytes, WrittenAction::None, None) {
                if let Some(pending) = self.pending_acks.remove(&packet_id) {
                    pending.fail(error);
                }
            }
        } else {
            let action = WrittenAction::PublishQos0 { result };
            if let Err((error, action)) = self.enqueue_with_action(bytes, action, None) {
                action.fail(error);
            }
        }
        Ok(())
    }

    fn begin_subscribe(
        &mut self,
        filter: String,
        qos: QoS,
        result: oneshot::Sender<Result<SubscriptionAck>>,
    ) -> Result<()> {
        if let Err(error) = self
            .server_capabilities
            .validate_subscription_filter(&filter)
        {
            let _ = result.send(Err(error));
            return Ok(());
        }
        let packet_id = match self.reserve_packet_id() {
            Ok(id) => id,
            Err(error) => {
                let _ = result.send(Err(error));
                return Ok(());
            }
        };
        let packet = SubscribePacket {
            packet_id,
            filters: vec![(filter, qos)],
            properties: Properties::new(),
        };
        let bytes = match packet.encode() {
            Ok(bytes) => bytes,
            Err(error) => {
                let _ = result.send(Err(error));
                return Ok(());
            }
        };
        if let Err(error) = self.insert_pending_ack(
            packet_id,
            PendingAckKind::Subscribe {
                requested_qos: qos,
                result,
            },
            "subscription acknowledgement",
        ) {
            if let PendingAckKind::Subscribe { result, .. } = error.kind {
                let _ = result.send(Err(error.error));
            }
            return Ok(());
        }
        if let Err(error) = self.enqueue(bytes, WrittenAction::None, None) {
            if let Some(pending) = self.pending_acks.remove(&packet_id) {
                pending.fail(error);
            }
        }
        Ok(())
    }

    fn begin_unsubscribe(
        &mut self,
        filter: String,
        result: oneshot::Sender<Result<SubscriptionAck>>,
    ) -> Result<()> {
        if let Err(error) = validate_topic_filter(&filter) {
            let _ = result.send(Err(error));
            return Ok(());
        }
        let packet_id = match self.reserve_packet_id() {
            Ok(id) => id,
            Err(error) => {
                let _ = result.send(Err(error));
                return Ok(());
            }
        };
        let packet = UnsubscribePacket {
            packet_id,
            filters: vec![filter.clone()],
            properties: Properties::new(),
        };
        let bytes = match packet.encode() {
            Ok(bytes) => bytes,
            Err(error) => {
                let _ = result.send(Err(error));
                return Ok(());
            }
        };
        if let Err(error) = self.insert_pending_ack(
            packet_id,
            PendingAckKind::Unsubscribe { filter, result },
            "unsubscribe acknowledgement",
        ) {
            if let PendingAckKind::Unsubscribe { result, .. } = error.kind {
                let _ = result.send(Err(error.error));
            }
            return Ok(());
        }
        if let Err(error) = self.enqueue(bytes, WrittenAction::None, None) {
            if let Some(pending) = self.pending_acks.remove(&packet_id) {
                pending.fail(error);
            }
        }
        Ok(())
    }

    fn begin_disconnect(&mut self, result: oneshot::Sender<Result<()>>) -> Result<()> {
        self.closing = true;
        let bytes = DisconnectPacket { reason_code: 0x00 }.encode()?;
        if let Err((error, action)) =
            self.enqueue_with_action(bytes, WrittenAction::Disconnect { result }, None)
        {
            action.fail(error);
            self.closing = false;
        }
        Ok(())
    }

    fn begin_abandoned_disconnect(&mut self) -> Result<()> {
        if self.closing || !self.command_channel_closed {
            return Ok(());
        }
        self.closing = true;
        let bytes = DisconnectPacket { reason_code: 0x00 }.encode()?;
        self.enqueue(bytes, WrittenAction::AbandonedDisconnect, None)
    }

    fn insert_pending_ack(
        &mut self,
        packet_id: u16,
        kind: PendingAckKind,
        timeout_label: &'static str,
    ) -> std::result::Result<(), PendingInsertError> {
        if self.pending_acks.len() >= self.max_pending_operations {
            return Err(PendingInsertError {
                error: Error::QueueFull("pending operation"),
                kind,
            });
        }
        self.pending_acks.insert(
            packet_id,
            PendingAck {
                deadline: Instant::now() + self.ack_timeout,
                timeout_label,
                kind,
            },
        );
        Ok(())
    }

    fn enqueue(
        &mut self,
        bytes: Vec<u8>,
        action: WrittenAction,
        request_correlation: Option<Vec<u8>>,
    ) -> Result<()> {
        self.enqueue_with_action(bytes, action, request_correlation)
            .map_err(|(error, _)| error)
    }

    fn enqueue_with_action(
        &mut self,
        bytes: Vec<u8>,
        action: WrittenAction,
        request_correlation: Option<Vec<u8>>,
    ) -> std::result::Result<(), (Error, WrittenAction)> {
        #[cfg(not(feature = "request-response"))]
        let _ = request_correlation;
        if bytes.len() > self.peer_max_packet_size {
            return Err((
                Error::PacketTooLarge {
                    size: bytes.len(),
                    max: self.peer_max_packet_size,
                },
                action,
            ));
        }
        let Some(next_size) = self.outbound_bytes.checked_add(bytes.len()) else {
            return Err((Error::QueueFull("outbound bytes"), action));
        };
        if next_size > self.max_outbound_queue_bytes {
            return Err((Error::QueueFull("outbound bytes"), action));
        }
        self.outbound_bytes = next_size;
        self.outbound.push_back(OutboundFrame {
            bytes,
            offset: 0,
            action,
            #[cfg(feature = "request-response")]
            request_correlation,
        });
        Ok(())
    }

    fn flush_outbound(&mut self) -> Result<()> {
        // A transport such as rustls may accept plaintext and then report
        // WouldBlock while flushing ciphertext. Do not feed it more plaintext
        // until that buffered batch is flushed.
        if self.needs_flush && !self.flush_accepted()? {
            return Ok(());
        }

        loop {
            let Some(frame) = self.outbound.front_mut() else {
                break;
            };
            match self.stream.write(&frame.bytes[frame.offset..]) {
                Ok(0) => return Err(Error::Io(ErrorKind::WriteZero.into())),
                Ok(written) => {
                    if written > frame.bytes.len() - frame.offset {
                        return Err(Error::Io(io::Error::new(
                            ErrorKind::InvalidData,
                            "transport reported an oversized write",
                        )));
                    }
                    frame.offset += written;
                    self.last_write_at = Instant::now();
                    self.needs_flush = true;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => return Err(Error::Io(error)),
            }

            if frame.offset == frame.bytes.len() {
                let frame = self.outbound.pop_front().expect("front frame disappeared");
                self.flush_actions.push_back(PendingFlush {
                    bytes: frame.bytes.len(),
                    action: frame.action,
                });
            }
        }

        if self.needs_flush {
            self.flush_accepted()?;
        }
        Ok(())
    }

    /// Flush bytes already accepted by the transport. `Ok(false)` means the
    /// transport remains backpressured and no new writes may be attempted.
    fn flush_accepted(&mut self) -> Result<bool> {
        match self.stream.flush() {
            Ok(()) => {
                self.needs_flush = false;
                while let Some(flush) = self.flush_actions.pop_front() {
                    self.outbound_bytes = self.outbound_bytes.saturating_sub(flush.bytes);
                    self.complete_written(flush.action);
                }
                Ok(true)
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => Ok(false),
            Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(Error::Io(error)),
        }
    }

    fn complete_written(&mut self, action: WrittenAction) {
        match action {
            WrittenAction::None => {}
            WrittenAction::PublishQos0 { result } => {
                let _ = result.send(Ok(PublishAck {
                    packet_id: None,
                    reason_code: None,
                }));
            }
            WrittenAction::PingReq => {
                self.ping_queued = false;
                self.ping_outstanding_since = Some(Instant::now());
            }
            WrittenAction::Disconnect { result } => {
                let _ = result.send(Ok(()));
                let _ = self
                    .events
                    .try_send(Event::Disconnected(DisconnectReason::ClientInitiated));
                self.terminal_event_sent = true;
            }
            WrittenAction::AbandonedDisconnect => {}
        }
    }

    fn read_available(&mut self) -> Result<()> {
        let mut buf = [0u8; 8192];
        for _ in 0..64 {
            let capacity = self.frame_reader.remaining_capacity().min(buf.len());
            if capacity == 0 {
                break;
            }
            match self.stream.read(&mut buf[..capacity]) {
                Ok(0) => {
                    let _ = self
                        .events
                        .try_send(Event::Disconnected(DisconnectReason::TransportClosed));
                    self.terminal_event_sent = true;
                    return Err(Error::ConnectionClosed);
                }
                Ok(read) => {
                    self.frame_reader.push(&buf[..read]);
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => return Err(Error::Io(error)),
            }
        }
        Ok(())
    }

    fn decode_and_dispatch(&mut self) -> Result<()> {
        while let Some(packet) = self.frame_reader.try_decode()? {
            self.dispatch_packet(packet)?;
        }
        Ok(())
    }

    fn dispatch_packet(&mut self, packet: Packet) -> Result<()> {
        match packet {
            Packet::Publish(packet) => self.dispatch_publish(packet),
            Packet::PubAck(ack) => self.dispatch_puback(ack),
            Packet::SubAck(ack) => self.dispatch_suback(ack),
            Packet::UnsubAck(ack) => self.dispatch_unsuback(ack),
            Packet::PingResp => {
                self.ping_outstanding_since = None;
                Ok(())
            }
            Packet::Disconnect(disconnect) => {
                self.fail_pending();
                let _ = self
                    .events
                    .try_send(Event::Disconnected(DisconnectReason::Server(
                        disconnect.reason_code,
                    )));
                self.terminal_event_sent = true;
                Err(Error::ServerDisconnected(disconnect.reason_code))
            }
            Packet::PingReq => Err(Error::UnexpectedPacket("broker sent PINGREQ")),
            Packet::ConnAck(_) => Err(Error::UnexpectedPacket("duplicate CONNACK")),
            Packet::Connect(_) | Packet::Subscribe(_) | Packet::Unsubscribe(_) => {
                Err(Error::UnexpectedPacket("server sent client-only packet"))
            }
        }
    }

    fn dispatch_publish(&mut self, packet: PublishPacket) -> Result<()> {
        if packet.qos == QoS::AtLeastOnce {
            let packet_id = packet
                .packet_id
                .ok_or(Error::MalformedPacket("QoS 1 PUBLISH missing packet id"))?;
            let ack = PubAckPacket {
                packet_id,
                reason_code: 0x00,
            }
            .encode()?;
            self.enqueue(ack, WrittenAction::None, None)?;
        }

        #[cfg(feature = "request-response")]
        if self.try_complete_request(&packet) {
            return Ok(());
        }

        self.events
            .try_send(Event::Publish(packet))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => Error::QueueFull("event"),
                mpsc::error::TrySendError::Closed(_) => Error::ClientClosed,
            })
    }

    fn dispatch_puback(&mut self, ack: PubAckPacket) -> Result<()> {
        let Some(pending) = self.take_pending_ack(ack.packet_id, AckClass::Publish)? else {
            return Ok(());
        };
        match pending.kind {
            PendingAckKind::Publish { result } => {
                if ack.reason_code >= 0x80 {
                    let _ = result.send(Err(Error::AckRejected {
                        packet: "PUBACK",
                        reason_code: ack.reason_code,
                    }));
                } else {
                    let _ = result.send(Ok(PublishAck {
                        packet_id: Some(ack.packet_id),
                        reason_code: Some(ack.reason_code),
                    }));
                }
            }
            #[cfg(feature = "request-response")]
            PendingAckKind::RequestPublish { correlation_data } => {
                if ack.reason_code >= 0x80 {
                    self.fail_request(
                        &correlation_data,
                        Error::AckRejected {
                            packet: "PUBACK",
                            reason_code: ack.reason_code,
                        },
                    );
                }
            }
            other => {
                other.fail(Error::UnexpectedPacket("PUBACK packet id type mismatch"));
            }
        }
        Ok(())
    }

    fn dispatch_suback(&mut self, ack: SubAckPacket) -> Result<()> {
        let Some(pending) = self.take_pending_ack(ack.packet_id, AckClass::Subscribe)? else {
            return Ok(());
        };
        match pending.kind {
            PendingAckKind::Subscribe {
                requested_qos,
                result,
            } => {
                self.complete_subscribe(ack, requested_qos, result)?;
            }
            #[cfg(feature = "request-response")]
            PendingAckKind::ResponseSubscribe { response_topic } => {
                match validate_suback_codes(&ack.reason_codes, &[QoS::AtLeastOnce]) {
                    Ok(()) => {
                        self.response_subscriptions
                            .insert(response_topic.clone(), ResponseSubscription::Ready);
                        self.publish_waiting_requests(&response_topic)?;
                    }
                    Err(error @ Error::AckRejected { .. }) => {
                        self.response_subscriptions.remove(&response_topic);
                        self.fail_requests_for_topic(&response_topic, error);
                    }
                    Err(error) => {
                        self.response_subscriptions.remove(&response_topic);
                        self.fail_requests_for_topic(&response_topic, duplicate_error(&error));
                        return Err(error);
                    }
                }
            }
            other => {
                other.fail(Error::UnexpectedPacket("SUBACK packet id type mismatch"));
            }
        }
        Ok(())
    }

    fn take_pending_ack(
        &mut self,
        packet_id: u16,
        received: AckClass,
    ) -> Result<Option<PendingAck>> {
        let Some(pending) = self.pending_acks.get(&packet_id) else {
            return Ok(None);
        };
        if pending.kind.ack_class() != received {
            return Err(Error::UnexpectedPacket(
                "acknowledgement type does not match pending packet id",
            ));
        }
        Ok(self.pending_acks.remove(&packet_id))
    }

    fn complete_subscribe(
        &mut self,
        ack: SubAckPacket,
        requested_qos: QoS,
        result: oneshot::Sender<Result<SubscriptionAck>>,
    ) -> Result<()> {
        match validate_suback_codes(&ack.reason_codes, &[requested_qos]) {
            Ok(()) => {
                let _ = result.send(Ok(SubscriptionAck {
                    packet_id: ack.packet_id,
                    reason_codes: ack.reason_codes,
                }));
                Ok(())
            }
            Err(error @ Error::AckRejected { .. }) => {
                let _ = result.send(Err(error));
                Ok(())
            }
            Err(error) => {
                let _ = result.send(Err(duplicate_error(&error)));
                Err(error)
            }
        }
    }

    fn dispatch_unsuback(&mut self, ack: UnsubAckPacket) -> Result<()> {
        let Some(pending) = self.take_pending_ack(ack.packet_id, AckClass::Unsubscribe)? else {
            return Ok(());
        };
        match pending.kind {
            PendingAckKind::Unsubscribe { filter, result } => {
                #[cfg(not(feature = "request-response"))]
                let _ = filter;
                let response = validate_reason_codes("UNSUBACK", &ack.reason_codes, 1).map(|()| {
                    SubscriptionAck {
                        packet_id: ack.packet_id,
                        reason_codes: ack.reason_codes,
                    }
                });
                #[cfg(feature = "request-response")]
                if response.is_ok() {
                    self.response_subscriptions.remove(&filter);
                }
                let _ = result.send(response);
            }
            other => {
                other.fail(Error::UnexpectedPacket("UNSUBACK packet id type mismatch"));
            }
        }
        Ok(())
    }

    fn expire_operations(&mut self) {
        let now = Instant::now();
        // A cancelled waiter becomes a tombstone until the broker ACK arrives
        // or the connection closes. Reusing its packet identifier while its
        // frame may already be on the wire could misdeliver a late ACK.
        for pending in self.pending_acks.values_mut() {
            if pending.is_cancelled() {
                let counts_toward_receive_maximum = pending.kind.is_qos1_publish();
                let expected_ack = pending.kind.ack_class();
                pending.kind = PendingAckKind::Tombstone {
                    expected_ack,
                    counts_toward_receive_maximum,
                };
            }
        }
        let expired_acks: Vec<u16> = self
            .pending_acks
            .iter()
            .filter_map(|(packet_id, pending)| {
                if now >= pending.deadline && !pending.kind.is_tombstone() {
                    Some(*packet_id)
                } else {
                    None
                }
            })
            .collect();
        for packet_id in expired_acks {
            let Some(pending) = self.pending_acks.get_mut(&packet_id) else {
                continue;
            };
            let timeout_label = pending.timeout_label;
            let counts_toward_receive_maximum = pending.kind.is_qos1_publish();
            let expected_ack = pending.kind.ack_class();
            let kind = std::mem::replace(
                &mut pending.kind,
                PendingAckKind::Tombstone {
                    expected_ack,
                    counts_toward_receive_maximum,
                },
            );

            #[cfg(feature = "request-response")]
            match kind {
                PendingAckKind::ResponseSubscribe { response_topic } => {
                    self.response_subscriptions.remove(&response_topic);
                    self.fail_requests_for_topic(&response_topic, Error::Timeout(timeout_label));
                }
                PendingAckKind::RequestPublish { correlation_data } => {
                    self.fail_request(&correlation_data, Error::Timeout(timeout_label));
                }
                other => other.fail(Error::Timeout(timeout_label)),
            }

            #[cfg(not(feature = "request-response"))]
            kind.fail(Error::Timeout(timeout_label));
        }

        #[cfg(feature = "request-response")]
        {
            let expired_requests: Vec<Vec<u8>> = self
                .pending_requests
                .iter()
                .filter(|(_, pending)| now >= pending.deadline || pending.result.is_closed())
                .map(|(correlation_data, _)| correlation_data.clone())
                .collect();
            for correlation_data in expired_requests {
                let cancelled = self
                    .pending_requests
                    .get(&correlation_data)
                    .is_some_and(|pending| pending.result.is_closed());
                if cancelled {
                    self.remove_request(&correlation_data);
                } else {
                    self.fail_request(&correlation_data, Error::Timeout("request response"));
                }
            }
        }
    }

    fn maintain_keep_alive(&mut self) -> Result<()> {
        let Some(interval) = keep_alive_interval(self.keep_alive_secs) else {
            return Ok(());
        };
        let now = Instant::now();
        if let Some(sent_at) = self.ping_outstanding_since {
            if now.duration_since(sent_at) >= Duration::from_secs(u64::from(self.keep_alive_secs)) {
                return Err(Error::KeepAliveTimeout);
            }
            return Ok(());
        }
        if !self.ping_queued && now.duration_since(self.last_write_at) >= interval {
            self.enqueue(PINGREQ_BYTES.to_vec(), WrittenAction::PingReq, None)?;
            self.ping_queued = true;
        }
        Ok(())
    }

    fn next_wait_duration(&self) -> Duration {
        let now = Instant::now();
        let mut wait = self.poll_interval;

        for pending in self.pending_acks.values() {
            wait = wait.min(pending.deadline.saturating_duration_since(now));
        }
        #[cfg(feature = "request-response")]
        for pending in self.pending_requests.values() {
            wait = wait.min(pending.deadline.saturating_duration_since(now));
        }
        if let Some(interval) = keep_alive_interval(self.keep_alive_secs) {
            if let Some(sent) = self.ping_outstanding_since {
                let deadline = sent + Duration::from_secs(u64::from(self.keep_alive_secs));
                wait = wait.min(deadline.saturating_duration_since(now));
            } else if !self.ping_queued {
                wait = wait.min((self.last_write_at + interval).saturating_duration_since(now));
            }
        }
        wait
    }

    fn reserve_packet_id(&mut self) -> Result<u16> {
        for _ in 0..u16::MAX {
            let packet_id = self.next_packet_id;
            self.next_packet_id = self.next_packet_id.wrapping_add(1);
            if self.next_packet_id == 0 {
                self.next_packet_id = 1;
            }
            if !self.pending_acks.contains_key(&packet_id) {
                return Ok(packet_id);
            }
        }
        Err(Error::QueueFull("packet identifiers"))
    }

    fn outgoing_qos1_count(&self) -> usize {
        self.pending_acks
            .values()
            .filter(|pending| {
                matches!(&pending.kind, PendingAckKind::Publish { .. })
                    || cfg!(feature = "request-response") && matches_request_publish(&pending.kind)
                    || matches!(
                        &pending.kind,
                        PendingAckKind::Tombstone {
                            counts_toward_receive_maximum: true,
                            ..
                        }
                    )
            })
            .count()
    }

    fn fail_pending(&mut self) {
        for (_, pending) in self.pending_acks.drain() {
            pending.fail(Error::ConnectionClosed);
        }
        for frame in self.outbound.drain(..) {
            frame.action.fail(Error::ConnectionClosed);
        }
        for flush in self.flush_actions.drain(..) {
            flush.action.fail(Error::ConnectionClosed);
        }
        self.outbound_bytes = 0;

        #[cfg(feature = "request-response")]
        for (correlation_data, pending) in self.pending_requests.drain() {
            self.request_lifecycle
                .lock()
                .expect("request lifecycle mutex poisoned")
                .finish(&correlation_data);
            let _ = pending.result.send(Err(Error::ConnectionClosed));
        }
    }

    fn drain_cancellations(&mut self) {
        #[cfg(feature = "request-response")]
        {
            let cancellations: Vec<Vec<u8>> = self
                .request_lifecycle
                .lock()
                .expect("request lifecycle mutex poisoned")
                .cancelled()
                .filter(|correlation_data| self.pending_requests.contains_key(*correlation_data))
                .map(<[u8]>::to_vec)
                .collect();
            for correlation_data in cancellations {
                self.remove_request(&correlation_data);
            }
        }
    }

    #[cfg(feature = "request-response")]
    fn begin_request(
        &mut self,
        request: crate::request_response::RequestCommand,
        result: oneshot::Sender<Result<PublishPacket>>,
    ) -> Result<()> {
        if self
            .request_lifecycle
            .lock()
            .expect("request lifecycle mutex poisoned")
            .consume_cancellation(&request.correlation_data)
        {
            return Ok(());
        }
        if request.timeout.is_zero() {
            self.finish_request_lifecycle(&request.correlation_data);
            let _ = result.send(Err(Error::InvalidOptions(
                "request timeout must be non-zero",
            )));
            return Ok(());
        }
        if let Err(error) = validate_topic_name(&request.topic).and_then(|()| {
            self.server_capabilities
                .validate_publish(request.qos, false)
        }) {
            self.finish_request_lifecycle(&request.correlation_data);
            let _ = result.send(Err(error));
            return Ok(());
        }
        if self.pending_requests.len() >= self.max_pending_operations {
            self.finish_request_lifecycle(&request.correlation_data);
            let _ = result.send(Err(Error::QueueFull("pending request")));
            return Ok(());
        }
        if self
            .pending_requests
            .contains_key(&request.correlation_data)
        {
            self.finish_request_lifecycle(&request.correlation_data);
            let _ = result.send(Err(Error::MalformedPacket(
                "duplicate request correlation data",
            )));
            return Ok(());
        }

        let correlation_data = request.correlation_data.clone();
        let response_topic = request.response_topic.clone();
        let Some(deadline) = Instant::now().checked_add(request.timeout) else {
            self.finish_request_lifecycle(&correlation_data);
            let _ = result.send(Err(Error::InvalidOptions("request timeout is too large")));
            return Ok(());
        };
        self.pending_requests.insert(
            correlation_data.clone(),
            PendingRequest {
                topic: request.topic,
                payload: request.payload,
                response_topic: response_topic.clone(),
                qos: request.qos,
                properties: request.properties,
                deadline,
                result,
                publish_packet_id: None,
                published: false,
            },
        );

        match self.response_subscriptions.get(&response_topic) {
            Some(ResponseSubscription::Ready) => self.publish_request(&correlation_data),
            Some(ResponseSubscription::Subscribing) => Ok(()),
            None if self.response_subscriptions.len() >= self.max_pending_operations => {
                Err(Error::QueueFull("response subscriptions"))
            }
            None => self.subscribe_for_responses(response_topic),
        }
    }

    #[cfg(feature = "request-response")]
    fn subscribe_for_responses(&mut self, response_topic: String) -> Result<()> {
        let packet_id = self.reserve_packet_id()?;
        let packet = SubscribePacket {
            packet_id,
            filters: vec![(response_topic.clone(), QoS::AtLeastOnce)],
            properties: Properties::new(),
        };
        let bytes = packet.encode()?;
        self.insert_pending_ack(
            packet_id,
            PendingAckKind::ResponseSubscribe {
                response_topic: response_topic.clone(),
            },
            "request subscription acknowledgement",
        )
        .map_err(|error| error.error)?;
        self.response_subscriptions
            .insert(response_topic.clone(), ResponseSubscription::Subscribing);
        if let Err(error) = self.enqueue(bytes, WrittenAction::None, None) {
            self.pending_acks.remove(&packet_id);
            self.response_subscriptions.remove(&response_topic);
            return Err(error);
        }
        Ok(())
    }

    #[cfg(feature = "request-response")]
    fn publish_waiting_requests(&mut self, response_topic: &str) -> Result<()> {
        let correlations: Vec<Vec<u8>> = self
            .pending_requests
            .iter()
            .filter(|(_, pending)| !pending.published && pending.response_topic == response_topic)
            .map(|(correlation, _)| correlation.clone())
            .collect();
        for correlation in correlations {
            if let Err(error) = self.publish_request(&correlation) {
                self.fail_request(&correlation, error);
            }
        }
        Ok(())
    }

    #[cfg(feature = "request-response")]
    fn publish_request(&mut self, correlation_data: &[u8]) -> Result<()> {
        let Some(pending) = self.pending_requests.get(correlation_data) else {
            return Ok(());
        };
        if pending.published {
            return Ok(());
        }
        let topic = pending.topic.clone();
        let payload = pending.payload.clone();
        let qos = pending.qos;
        let properties = pending.properties.clone();

        let packet_id = if qos == QoS::AtLeastOnce {
            if self.outgoing_qos1_count() >= usize::from(self.peer_receive_maximum) {
                return Err(Error::QueueFull("broker receive maximum"));
            }
            Some(self.reserve_packet_id()?)
        } else {
            None
        };
        let packet = PublishPacket {
            topic,
            packet_id,
            payload,
            qos,
            retain: false,
            dup: false,
            properties,
        };
        let bytes = packet.encode()?;

        if let Some(packet_id) = packet_id {
            self.insert_pending_ack(
                packet_id,
                PendingAckKind::RequestPublish {
                    correlation_data: correlation_data.to_vec(),
                },
                "request publish acknowledgement",
            )
            .map_err(|error| error.error)?;
        }
        if let Err(error) =
            self.enqueue(bytes, WrittenAction::None, Some(correlation_data.to_vec()))
        {
            if let Some(packet_id) = packet_id {
                self.pending_acks.remove(&packet_id);
            }
            return Err(error);
        }
        if let Some(pending) = self.pending_requests.get_mut(correlation_data) {
            pending.published = true;
            pending.publish_packet_id = packet_id;
        }
        Ok(())
    }

    #[cfg(feature = "request-response")]
    fn try_complete_request(&mut self, packet: &PublishPacket) -> bool {
        let Some(correlation_data) = packet
            .properties
            .get_binary(PropertyId::CorrelationData)
            .map(<[u8]>::to_vec)
        else {
            return false;
        };
        let matches = self
            .pending_requests
            .get(&correlation_data)
            .is_some_and(|pending| pending.response_topic == packet.topic);
        if !matches {
            return false;
        }

        if let Some(pending) = self.remove_request(&correlation_data) {
            let _ = pending.result.send(Ok(packet.clone()));
        }
        true
    }

    #[cfg(feature = "request-response")]
    fn fail_requests_for_topic(&mut self, response_topic: &str, error: Error) {
        let correlations: Vec<Vec<u8>> = self
            .pending_requests
            .iter()
            .filter(|(_, pending)| pending.response_topic == response_topic)
            .map(|(correlation, _)| correlation.clone())
            .collect();
        for correlation in correlations {
            self.fail_request(&correlation, duplicate_error(&error));
        }
    }

    #[cfg(feature = "request-response")]
    fn fail_request(&mut self, correlation_data: &[u8], error: Error) {
        if let Some(pending) = self.remove_request(correlation_data) {
            let _ = pending.result.send(Err(error));
        }
    }

    #[cfg(feature = "request-response")]
    fn remove_request(&mut self, correlation_data: &[u8]) -> Option<PendingRequest> {
        let pending = self.pending_requests.remove(correlation_data)?;
        self.finish_request_lifecycle(correlation_data);

        // A frame that has not started can be safely removed. Once any prefix
        // is written, its remainder must stay queued to preserve stream framing.
        let mut removed_unsent_publish = false;
        let mut retained = VecDeque::with_capacity(self.outbound.len());
        while let Some(frame) = self.outbound.pop_front() {
            if frame.offset == 0 && frame.request_correlation.as_deref() == Some(correlation_data) {
                self.outbound_bytes = self.outbound_bytes.saturating_sub(frame.bytes.len());
                removed_unsent_publish = true;
            } else {
                retained.push_back(frame);
            }
        }
        self.outbound = retained;

        if let Some(packet_id) = pending.publish_packet_id {
            if removed_unsent_publish {
                self.pending_acks.remove(&packet_id);
            } else if let Some(ack) = self.pending_acks.get_mut(&packet_id) {
                ack.kind = PendingAckKind::Tombstone {
                    expected_ack: AckClass::Publish,
                    counts_toward_receive_maximum: true,
                };
            }
        }
        Some(pending)
    }

    #[cfg(feature = "request-response")]
    fn finish_request_lifecycle(&self, correlation_data: &[u8]) {
        self.request_lifecycle
            .lock()
            .expect("request lifecycle mutex poisoned")
            .finish(correlation_data);
    }
}

enum Command {
    Publish {
        topic: String,
        payload: Vec<u8>,
        options: PublishOptions,
        result: oneshot::Sender<Result<PublishAck>>,
    },
    Subscribe {
        filter: String,
        qos: QoS,
        result: oneshot::Sender<Result<SubscriptionAck>>,
    },
    Unsubscribe {
        filter: String,
        result: oneshot::Sender<Result<SubscriptionAck>>,
    },
    Disconnect {
        result: oneshot::Sender<Result<()>>,
    },
    #[cfg(feature = "request-response")]
    Request {
        request: crate::request_response::RequestCommand,
        result: oneshot::Sender<Result<PublishPacket>>,
    },
}

impl Command {
    fn fail(self, error: Error) {
        match self {
            Command::Publish { result, .. } => {
                let _ = result.send(Err(error));
            }
            Command::Subscribe { result, .. } | Command::Unsubscribe { result, .. } => {
                let _ = result.send(Err(error));
            }
            Command::Disconnect { result } => {
                let _ = result.send(Err(error));
            }
            #[cfg(feature = "request-response")]
            Command::Request { result, .. } => {
                let _ = result.send(Err(error));
            }
        }
    }
}

struct OutboundFrame {
    bytes: Vec<u8>,
    offset: usize,
    action: WrittenAction,
    #[cfg(feature = "request-response")]
    request_correlation: Option<Vec<u8>>,
}

struct PendingFlush {
    bytes: usize,
    action: WrittenAction,
}

enum WrittenAction {
    None,
    PublishQos0 {
        result: oneshot::Sender<Result<PublishAck>>,
    },
    PingReq,
    Disconnect {
        result: oneshot::Sender<Result<()>>,
    },
    AbandonedDisconnect,
}

impl WrittenAction {
    fn fail(self, error: Error) {
        match self {
            WrittenAction::PublishQos0 { result } => {
                let _ = result.send(Err(error));
            }
            WrittenAction::Disconnect { result } => {
                let _ = result.send(Err(error));
            }
            WrittenAction::None | WrittenAction::PingReq | WrittenAction::AbandonedDisconnect => {}
        }
    }
}

struct PendingAck {
    deadline: Instant,
    timeout_label: &'static str,
    kind: PendingAckKind,
}

impl PendingAck {
    fn is_cancelled(&self) -> bool {
        self.kind.is_cancelled()
    }

    fn fail(self, error: Error) {
        self.kind.fail(error);
    }
}

enum PendingAckKind {
    Publish {
        result: oneshot::Sender<Result<PublishAck>>,
    },
    Subscribe {
        requested_qos: QoS,
        result: oneshot::Sender<Result<SubscriptionAck>>,
    },
    Unsubscribe {
        filter: String,
        result: oneshot::Sender<Result<SubscriptionAck>>,
    },
    #[cfg(feature = "request-response")]
    RequestPublish { correlation_data: Vec<u8> },
    #[cfg(feature = "request-response")]
    ResponseSubscribe { response_topic: String },
    Tombstone {
        expected_ack: AckClass,
        counts_toward_receive_maximum: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AckClass {
    Publish,
    Subscribe,
    Unsubscribe,
}

impl PendingAckKind {
    fn is_cancelled(&self) -> bool {
        match self {
            PendingAckKind::Publish { result } => result.is_closed(),
            PendingAckKind::Subscribe { result, .. } => result.is_closed(),
            PendingAckKind::Unsubscribe { result, .. } => result.is_closed(),
            PendingAckKind::Tombstone { .. } => false,
            #[cfg(feature = "request-response")]
            PendingAckKind::RequestPublish { .. } | PendingAckKind::ResponseSubscribe { .. } => {
                false
            }
        }
    }

    fn fail(self, error: Error) {
        match self {
            PendingAckKind::Publish { result } => {
                let _ = result.send(Err(error));
            }
            PendingAckKind::Subscribe { result, .. }
            | PendingAckKind::Unsubscribe { result, .. } => {
                let _ = result.send(Err(error));
            }
            PendingAckKind::Tombstone { .. } => {}
            #[cfg(feature = "request-response")]
            PendingAckKind::RequestPublish { .. } | PendingAckKind::ResponseSubscribe { .. } => {}
        }
    }

    fn is_qos1_publish(&self) -> bool {
        matches!(self, PendingAckKind::Publish { .. })
            || matches_request_publish(self)
            || matches!(
                self,
                PendingAckKind::Tombstone {
                    counts_toward_receive_maximum: true,
                    ..
                }
            )
    }

    fn is_tombstone(&self) -> bool {
        matches!(self, PendingAckKind::Tombstone { .. })
    }

    fn ack_class(&self) -> AckClass {
        match self {
            PendingAckKind::Publish { .. } => AckClass::Publish,
            PendingAckKind::Subscribe { .. } => AckClass::Subscribe,
            PendingAckKind::Unsubscribe { .. } => AckClass::Unsubscribe,
            #[cfg(feature = "request-response")]
            PendingAckKind::RequestPublish { .. } => AckClass::Publish,
            #[cfg(feature = "request-response")]
            PendingAckKind::ResponseSubscribe { .. } => AckClass::Subscribe,
            PendingAckKind::Tombstone { expected_ack, .. } => *expected_ack,
        }
    }
}

struct PendingInsertError {
    error: Error,
    kind: PendingAckKind,
}

#[cfg(feature = "request-response")]
struct PendingRequest {
    topic: String,
    payload: Vec<u8>,
    response_topic: String,
    qos: QoS,
    properties: Properties,
    deadline: Instant,
    result: oneshot::Sender<Result<PublishPacket>>,
    publish_packet_id: Option<u16>,
    published: bool,
}

#[cfg(feature = "request-response")]
enum ResponseSubscription {
    Subscribing,
    Ready,
}

fn keep_alive_interval(keep_alive_secs: u16) -> Option<Duration> {
    (keep_alive_secs != 0).then(|| Duration::from_secs((u64::from(keep_alive_secs) / 2).max(1)))
}

fn connect_deadline(timeout: Duration) -> Result<Instant> {
    Instant::now()
        .checked_add(timeout)
        .ok_or(Error::InvalidOptions("connect timeout is too large"))
}

fn connect_remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or(Error::Timeout("connect"))
}

fn map_connect_io<T>(result: io::Result<T>) -> Result<T> {
    match result {
        Err(error)
            if error.kind() == ErrorKind::TimedOut || error.kind() == ErrorKind::WouldBlock =>
        {
            Err(Error::Timeout("connect"))
        }
        Err(error) => Err(Error::Io(error)),
        Ok(value) => Ok(value),
    }
}

fn set_connect_timeouts<T: Transport>(stream: &T, deadline: Instant) -> Result<()> {
    stream.set_read_timeout(Some(connect_remaining(deadline)?))?;
    stream.set_write_timeout(Some(connect_remaining(deadline)?))?;
    Ok(())
}

fn connect_tcp_until(addr: impl ToSocketAddrs, deadline: Instant) -> Result<TcpStream> {
    // Name resolution is blocking on std today. It cannot be interrupted, but
    // time spent resolving still counts against the shared connect deadline.
    let addresses = addr.to_socket_addrs();
    connect_remaining(deadline)?;
    let addresses = addresses?;
    let mut attempted = false;
    let mut last_error = None;

    for socket_addr in addresses {
        attempted = true;
        let remaining = connect_remaining(deadline)?;
        match TcpStream::connect_timeout(&socket_addr, remaining) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }

    connect_remaining(deadline)?;
    if !attempted {
        return Err(Error::Io(io::Error::new(
            ErrorKind::InvalidInput,
            "address resolved to no endpoints",
        )));
    }
    map_connect_io(Err(last_error.unwrap_or_else(|| {
        io::Error::new(ErrorKind::ConnectionRefused, "TCP connect failed")
    })))
}

fn write_all_until<T: Transport>(
    stream: &mut T,
    mut bytes: &[u8],
    deadline: Instant,
) -> Result<()> {
    while !bytes.is_empty() {
        stream.set_write_timeout(Some(connect_remaining(deadline)?))?;
        match stream.write(bytes) {
            Ok(0) => return Err(Error::Io(ErrorKind::WriteZero.into())),
            Ok(written) if written <= bytes.len() => {
                bytes = &bytes[written..];
                connect_remaining(deadline)?;
            }
            Ok(_) => {
                return Err(Error::Io(io::Error::new(
                    ErrorKind::InvalidData,
                    "transport reported an oversized write",
                )));
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return map_connect_io(Err(error)),
        }
    }
    Ok(())
}

fn read_exact_until<T: Transport>(
    stream: &mut T,
    mut bytes: &mut [u8],
    deadline: Instant,
) -> Result<()> {
    while !bytes.is_empty() {
        stream.set_read_timeout(Some(connect_remaining(deadline)?))?;
        match stream.read(bytes) {
            Ok(0) => return Err(Error::ConnectionClosed),
            Ok(read) if read <= bytes.len() => {
                bytes = &mut bytes[read..];
                connect_remaining(deadline)?;
            }
            Ok(_) => {
                return Err(Error::Io(io::Error::new(
                    ErrorKind::InvalidData,
                    "transport reported an oversized read",
                )));
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return map_connect_io(Err(error)),
        }
    }
    Ok(())
}

fn validate_reason_codes(packet: &'static str, reason_codes: &[u8], expected: usize) -> Result<()> {
    if reason_codes.len() != expected {
        return Err(Error::MalformedPacket("ack reason code count mismatch"));
    }
    if let Some(reason_code) = reason_codes.iter().copied().find(|code| *code >= 0x80) {
        return Err(Error::AckRejected {
            packet,
            reason_code,
        });
    }
    Ok(())
}

fn duplicate_error(error: &Error) -> Error {
    match error {
        Error::InvalidOptions(message) => Error::InvalidOptions(message),
        Error::MalformedPacket(message) => Error::MalformedPacket(message),
        Error::InvalidPacketType(value) => Error::InvalidPacketType(*value),
        Error::InvalidQoS(value) => Error::InvalidQoS(*value),
        Error::InvalidReasonCode(value) => Error::InvalidReasonCode(*value),
        Error::ConnectionRefused(value) => Error::ConnectionRefused(*value),
        Error::ServerDisconnected(value) => Error::ServerDisconnected(*value),
        Error::PacketTooLarge { size, max } => Error::PacketTooLarge {
            size: *size,
            max: *max,
        },
        Error::StringTooLong(length) => Error::StringTooLong(*length),
        Error::BinaryTooLong(length) => Error::BinaryTooLong(*length),
        Error::Timeout(operation) => Error::Timeout(operation),
        Error::KeepAliveTimeout => Error::KeepAliveTimeout,
        Error::UnexpectedPacket(message) => Error::UnexpectedPacket(message),
        Error::AckRejected {
            packet,
            reason_code,
        } => Error::AckRejected {
            packet,
            reason_code: *reason_code,
        },
        Error::QueueFull(queue) => Error::QueueFull(queue),
        Error::ClientClosed => Error::ClientClosed,
        Error::Serialize(message) => Error::Serialize(message.clone()),
        Error::Deserialize(message) => Error::Deserialize(message.clone()),
        Error::ConnectionClosed => Error::ConnectionClosed,
        Error::Io(error) => Error::Io(io::Error::new(error.kind(), error.to_string())),
    }
}

#[cfg(feature = "request-response")]
fn matches_request_publish(kind: &PendingAckKind) -> bool {
    matches!(kind, PendingAckKind::RequestPublish { .. })
}

#[cfg(not(feature = "request-response"))]
fn matches_request_publish(_kind: &PendingAckKind) -> bool {
    false
}

/// Read one complete MQTT packet during the blocking handshake.
fn read_packet_blocking<T: Transport>(
    stream: &mut T,
    max_packet_size: usize,
    deadline: Instant,
) -> Result<Packet> {
    let mut first = [0u8; 1];
    read_exact_until(stream, &mut first, deadline)?;

    let mut multiplier = 1u32;
    let mut header_bytes = vec![first[0]];
    loop {
        let mut byte = [0u8; 1];
        read_exact_until(stream, &mut byte, deadline)?;
        header_bytes.push(byte[0]);
        if byte[0] & 0x80 == 0 {
            break;
        }
        if multiplier == 128 * 128 * 128 {
            return Err(Error::MalformedPacket("variable int too long"));
        }
        multiplier *= 128;
    }

    let (header, header_len) = decode_fixed_header(&header_bytes)?;
    let remaining_length = header.remaining_length;

    let total_size = header_len
        .checked_add(remaining_length as usize)
        .ok_or(Error::MalformedPacket("packet size overflow"))?;
    if total_size > max_packet_size {
        return Err(Error::PacketTooLarge {
            size: total_size,
            max: max_packet_size,
        });
    }

    let mut body = vec![0; remaining_length as usize];
    read_exact_until(stream, &mut body, deadline)?;
    Packet::decode(header, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::decode::decode_fixed_header;
    use std::sync::{Arc as SyncArc, Mutex as SyncMutex};

    #[derive(Default)]
    struct MockState {
        reads: VecDeque<Vec<u8>>,
        writes: Vec<u8>,
        max_write: Option<usize>,
        write_budget: Option<usize>,
        write_blocked: bool,
        read_delay: Duration,
        flush_results: VecDeque<Option<ErrorKind>>,
        flush_count: usize,
        shutdown: bool,
    }

    #[derive(Clone)]
    struct MockTransport {
        state: SyncArc<SyncMutex<MockState>>,
    }

    impl MockTransport {
        fn new(state: MockState) -> (Self, SyncArc<SyncMutex<MockState>>) {
            let state = SyncArc::new(SyncMutex::new(state));
            (
                Self {
                    state: state.clone(),
                },
                state,
            )
        }
    }

    impl Transport for MockTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut state = self.state.lock().unwrap();
            if state.write_blocked {
                return Err(ErrorKind::WouldBlock.into());
            }
            if state.write_budget == Some(0) {
                return Err(ErrorKind::WouldBlock.into());
            }
            let written = state
                .max_write
                .unwrap_or(buf.len())
                .min(state.write_budget.unwrap_or(usize::MAX))
                .min(buf.len());
            state.writes.extend_from_slice(&buf[..written]);
            if let Some(budget) = &mut state.write_budget {
                *budget -= written;
            }
            Ok(written)
        }

        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut state = self.state.lock().unwrap();
            if !state.read_delay.is_zero() {
                std::thread::sleep(state.read_delay);
            }
            let Some(mut data) = state.reads.pop_front() else {
                return Err(ErrorKind::WouldBlock.into());
            };
            let read = data.len().min(buf.len());
            buf[..read].copy_from_slice(&data[..read]);
            if read < data.len() {
                data.drain(..read);
                state.reads.push_front(data);
            }
            Ok(read)
        }

        fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
            let mut offset = 0;
            while offset < buf.len() {
                let read = self.read(&mut buf[offset..])?;
                if read == 0 {
                    return Err(ErrorKind::UnexpectedEof.into());
                }
                offset += read;
            }
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            let mut state = self.state.lock().unwrap();
            state.flush_count += 1;
            match state.flush_results.pop_front().flatten() {
                Some(kind) => Err(kind.into()),
                None => Ok(()),
            }
        }

        fn set_nonblocking(&mut self, _nonblocking: bool) -> io::Result<()> {
            Ok(())
        }

        fn set_read_timeout(&self, _dur: Option<Duration>) -> io::Result<()> {
            Ok(())
        }

        fn set_write_timeout(&self, _dur: Option<Duration>) -> io::Result<()> {
            Ok(())
        }

        fn shutdown(&self) -> io::Result<()> {
            self.state.lock().unwrap().shutdown = true;
            Ok(())
        }
    }

    fn test_connection(
        transport: MockTransport,
        max_packet_size: usize,
        keep_alive_secs: u16,
    ) -> (
        MqttConnection<MockTransport>,
        mpsc::Receiver<Event>,
        mpsc::Sender<Command>,
    ) {
        let (command_tx, command_rx) = mpsc::channel(8);
        let (event_tx, event_rx) = mpsc::channel(8);
        let now = Instant::now();
        let connection = MqttConnection {
            stream: transport,
            frame_reader: FrameReader::with_max_packet_size(max_packet_size),
            commands: command_rx,
            events: event_tx,
            #[cfg(feature = "request-response")]
            request_lifecycle: Arc::new(StdMutex::new(RequestLifecycle::new(8))),
            outbound: VecDeque::new(),
            outbound_bytes: 0,
            pending_acks: HashMap::new(),
            next_packet_id: 1,
            last_write_at: now,
            ping_outstanding_since: None,
            keep_alive_secs,
            peer_max_packet_size: 268_435_460,
            peer_receive_maximum: u16::MAX,
            server_capabilities: ServerCapabilities::default(),
            max_outbound_queue_bytes: 1024 * 1024,
            max_pending_operations: 8,
            ack_timeout: Duration::from_secs(10),
            poll_interval: Duration::from_millis(1),
            closing: false,
            command_channel_closed: false,
            needs_flush: false,
            flush_actions: VecDeque::new(),
            ping_queued: false,
            terminal_event_sent: false,
            #[cfg(feature = "request-response")]
            pending_requests: HashMap::new(),
            #[cfg(feature = "request-response")]
            response_subscriptions: HashMap::new(),
        };
        (connection, event_rx, command_tx)
    }

    fn decode_packet(bytes: &[u8]) -> Packet {
        let (header, header_len) = decode_fixed_header(bytes).unwrap();
        let end = header_len + header.remaining_length as usize;
        Packet::decode(header, &bytes[header_len..end]).unwrap()
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
    fn invalid_topics_fail_commands_without_poisoning_the_async_driver() {
        let (transport, _state) = MockTransport::new(MockState::default());
        let (mut connection, _events, _commands) = test_connection(transport, 1024, 60);

        let (publish_tx, mut publish_rx) = oneshot::channel();
        connection
            .begin_publish(
                "events/#".to_owned(),
                Vec::new(),
                PublishOptions::default(),
                publish_tx,
            )
            .unwrap();
        assert!(matches!(
            publish_rx.try_recv().unwrap(),
            Err(Error::MalformedPacket("topic name contains a wildcard"))
        ));

        let (subscribe_tx, mut subscribe_rx) = oneshot::channel();
        connection
            .begin_subscribe("events/#/new".to_owned(), QoS::AtMostOnce, subscribe_tx)
            .unwrap();
        assert!(matches!(
            subscribe_rx.try_recv().unwrap(),
            Err(Error::MalformedPacket(_))
        ));

        let (unsubscribe_tx, mut unsubscribe_rx) = oneshot::channel();
        connection
            .begin_unsubscribe(String::new(), unsubscribe_tx)
            .unwrap();
        assert!(matches!(
            unsubscribe_rx.try_recv().unwrap(),
            Err(Error::MalformedPacket("topic filter is empty"))
        ));
        assert!(!connection.closing);
        assert!(connection.outbound.is_empty());
        assert!(connection.pending_acks.is_empty());
    }

    #[test]
    fn broker_capabilities_reject_new_commands_but_queue_unsubscribe() {
        let (transport, _state) = MockTransport::new(MockState::default());
        let (mut connection, _events, _commands) = test_connection(transport, 1024, 60);
        connection.server_capabilities = ServerCapabilities {
            maximum_qos: QoS::AtMostOnce,
            retain_available: false,
            wildcard_subscriptions_available: false,
            shared_subscriptions_available: false,
        };

        let (qos_tx, mut qos_rx) = oneshot::channel();
        connection
            .begin_publish(
                "events/new".to_owned(),
                Vec::new(),
                PublishOptions::default().with_qos(QoS::AtLeastOnce),
                qos_tx,
            )
            .unwrap();
        assert!(matches!(
            qos_rx.try_recv().unwrap(),
            Err(Error::InvalidOptions(
                "broker does not support QoS 1 publishing"
            ))
        ));

        let (retain_tx, mut retain_rx) = oneshot::channel();
        connection
            .begin_publish(
                "events/new".to_owned(),
                Vec::new(),
                PublishOptions::default().with_retain(true),
                retain_tx,
            )
            .unwrap();
        assert!(matches!(
            retain_rx.try_recv().unwrap(),
            Err(Error::InvalidOptions(
                "broker does not support retained publishing"
            ))
        ));

        let (wildcard_tx, mut wildcard_rx) = oneshot::channel();
        connection
            .begin_subscribe("events/+".to_owned(), QoS::AtMostOnce, wildcard_tx)
            .unwrap();
        assert!(matches!(
            wildcard_rx.try_recv().unwrap(),
            Err(Error::InvalidOptions(
                "broker does not support wildcard subscriptions"
            ))
        ));

        let (shared_tx, mut shared_rx) = oneshot::channel();
        connection
            .begin_subscribe(
                "$share/workers/events".to_owned(),
                QoS::AtMostOnce,
                shared_tx,
            )
            .unwrap();
        assert!(matches!(
            shared_rx.try_recv().unwrap(),
            Err(Error::InvalidOptions(
                "broker does not support shared subscriptions"
            ))
        ));
        assert!(connection.outbound.is_empty());
        assert!(connection.pending_acks.is_empty());

        let (wildcard_unsub_tx, mut wildcard_unsub_rx) = oneshot::channel();
        connection
            .begin_unsubscribe("events/#".to_owned(), wildcard_unsub_tx)
            .unwrap();
        let (shared_unsub_tx, mut shared_unsub_rx) = oneshot::channel();
        connection
            .begin_unsubscribe("$share/workers/events/#".to_owned(), shared_unsub_tx)
            .unwrap();

        assert!(matches!(
            wildcard_unsub_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            shared_unsub_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(connection.outbound.len(), 2);
        assert_eq!(connection.pending_acks.len(), 2);
        assert!(!connection.closing);
    }

    #[test]
    fn qos0_completion_waits_for_partial_write_and_flush() {
        let (transport, state) = MockTransport::new(MockState {
            max_write: Some(2),
            flush_results: VecDeque::from([Some(ErrorKind::WouldBlock), None]),
            ..MockState::default()
        });
        let (mut connection, _events, _commands) = test_connection(transport, 1024, 60);
        let (result_tx, mut result_rx) = oneshot::channel();

        connection
            .begin_publish(
                "events/new".to_owned(),
                b"payload".to_vec(),
                PublishOptions::default(),
                result_tx,
            )
            .unwrap();
        connection.flush_outbound().unwrap();

        assert!(matches!(
            result_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(connection.outbound.is_empty());
        assert_eq!(connection.flush_actions.len(), 1);

        connection.flush_outbound().unwrap();
        let ack = result_rx.try_recv().unwrap().unwrap();
        assert_eq!(ack.packet_id, None);
        assert!(connection.flush_actions.is_empty());
        assert!(matches!(
            decode_packet(&state.lock().unwrap().writes),
            Packet::Publish(_)
        ));
    }

    #[test]
    fn persistent_flush_backpressure_blocks_new_writes_and_stays_byte_bounded() {
        let (transport, state) = MockTransport::new(MockState {
            flush_results: VecDeque::from([
                Some(ErrorKind::WouldBlock),
                Some(ErrorKind::WouldBlock),
                None,
            ]),
            ..MockState::default()
        });
        let (mut connection, _events, _commands) = test_connection(transport, 1024, 60);
        connection.max_outbound_queue_bytes = 8;
        connection
            .enqueue(vec![1; 4], WrittenAction::None, None)
            .unwrap();
        connection.flush_outbound().unwrap();
        assert_eq!(state.lock().unwrap().writes, vec![1; 4]);
        assert_eq!(connection.outbound_bytes, 4);

        connection
            .enqueue(vec![2; 4], WrittenAction::None, None)
            .unwrap();
        assert!(matches!(
            connection.enqueue(vec![3], WrittenAction::None, None),
            Err(Error::QueueFull("outbound bytes"))
        ));

        connection.flush_outbound().unwrap();
        assert_eq!(state.lock().unwrap().writes, vec![1; 4]);
        assert_eq!(connection.outbound_bytes, 8);

        connection.flush_outbound().unwrap();
        assert_eq!(
            state.lock().unwrap().writes,
            [vec![1; 4], vec![2; 4]].concat()
        );
        assert_eq!(connection.outbound_bytes, 0);
    }

    #[test]
    fn cancelled_qos1_ack_keeps_packet_id_tombstone() {
        let (transport, _state) = MockTransport::new(MockState::default());
        let (mut connection, _events, _commands) = test_connection(transport, 1024, 60);
        let (result_tx, result_rx) = oneshot::channel();
        connection
            .begin_publish(
                "events/new".to_owned(),
                Vec::new(),
                PublishOptions::default().with_qos(QoS::AtLeastOnce),
                result_tx,
            )
            .unwrap();
        drop(result_rx);

        connection.expire_operations();
        assert!(matches!(
            connection.pending_acks.get(&1).map(|pending| &pending.kind),
            Some(PendingAckKind::Tombstone {
                counts_toward_receive_maximum: true,
                ..
            })
        ));

        connection.next_packet_id = 1;
        assert_eq!(connection.reserve_packet_id().unwrap(), 2);
    }

    #[test]
    fn timed_out_partially_written_ack_keeps_packet_id_tombstone() {
        let (transport, _state) = MockTransport::new(MockState {
            write_budget: Some(1),
            ..MockState::default()
        });
        let (mut connection, _events, _commands) = test_connection(transport, 1024, 60);
        let (result_tx, mut result_rx) = oneshot::channel();
        connection
            .begin_publish(
                "events/new".to_owned(),
                Vec::new(),
                PublishOptions::default().with_qos(QoS::AtLeastOnce),
                result_tx,
            )
            .unwrap();
        connection.flush_outbound().unwrap();
        assert_eq!(connection.outbound.front().unwrap().offset, 1);
        connection.pending_acks.get_mut(&1).unwrap().deadline = Instant::now();

        connection.expire_operations();
        assert!(matches!(
            result_rx.try_recv().unwrap(),
            Err(Error::Timeout("publish acknowledgement"))
        ));
        assert!(matches!(
            connection.pending_acks.get(&1).map(|pending| &pending.kind),
            Some(PendingAckKind::Tombstone {
                counts_toward_receive_maximum: true,
                ..
            })
        ));
        connection.next_packet_id = 1;
        assert_eq!(connection.reserve_packet_id().unwrap(), 2);
    }

    #[test]
    fn packet_id_wrap_skips_every_in_flight_identifier() {
        let (transport, _state) = MockTransport::new(MockState::default());
        let (mut connection, _events, _commands) = test_connection(transport, 1024, 60);
        for packet_id in [u16::MAX, 1] {
            connection.pending_acks.insert(
                packet_id,
                PendingAck {
                    deadline: Instant::now() + Duration::from_secs(1),
                    timeout_label: "test",
                    kind: PendingAckKind::Tombstone {
                        expected_ack: AckClass::Publish,
                        counts_toward_receive_maximum: false,
                    },
                },
            );
        }
        connection.next_packet_id = u16::MAX;
        assert_eq!(connection.reserve_packet_id().unwrap(), 2);
    }

    #[test]
    fn mismatched_ack_classes_are_terminal_and_keep_active_ids_reserved() {
        let (transport, _state) = MockTransport::new(MockState::default());
        let (mut connection, _events, _commands) = test_connection(transport, 1024, 60);
        let (publish_tx, mut publish_rx) = oneshot::channel();
        let (subscribe_tx, mut subscribe_rx) = oneshot::channel();
        let (unsubscribe_tx, mut unsubscribe_rx) = oneshot::channel();
        let deadline = Instant::now() + Duration::from_secs(1);
        connection.pending_acks.insert(
            10,
            PendingAck {
                deadline,
                timeout_label: "publish acknowledgement",
                kind: PendingAckKind::Publish { result: publish_tx },
            },
        );
        connection.pending_acks.insert(
            11,
            PendingAck {
                deadline,
                timeout_label: "subscription acknowledgement",
                kind: PendingAckKind::Subscribe {
                    requested_qos: QoS::AtMostOnce,
                    result: subscribe_tx,
                },
            },
        );
        connection.pending_acks.insert(
            12,
            PendingAck {
                deadline,
                timeout_label: "unsubscribe acknowledgement",
                kind: PendingAckKind::Unsubscribe {
                    filter: "events/#".to_owned(),
                    result: unsubscribe_tx,
                },
            },
        );

        assert!(matches!(
            connection.dispatch_suback(SubAckPacket {
                packet_id: 10,
                reason_codes: vec![0],
            }),
            Err(Error::UnexpectedPacket(
                "acknowledgement type does not match pending packet id"
            ))
        ));
        assert!(matches!(
            connection.dispatch_unsuback(UnsubAckPacket {
                packet_id: 11,
                reason_codes: vec![0],
            }),
            Err(Error::UnexpectedPacket(
                "acknowledgement type does not match pending packet id"
            ))
        ));
        assert!(matches!(
            connection.dispatch_puback(PubAckPacket {
                packet_id: 12,
                reason_code: 0,
            }),
            Err(Error::UnexpectedPacket(
                "acknowledgement type does not match pending packet id"
            ))
        ));

        assert_eq!(
            connection.pending_acks[&10].kind.ack_class(),
            AckClass::Publish
        );
        assert_eq!(
            connection.pending_acks[&11].kind.ack_class(),
            AckClass::Subscribe
        );
        assert_eq!(
            connection.pending_acks[&12].kind.ack_class(),
            AckClass::Unsubscribe
        );
        assert!(matches!(
            publish_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            subscribe_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            unsubscribe_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn mismatched_ack_classes_do_not_free_timed_out_tombstones() {
        let (transport, _state) = MockTransport::new(MockState::default());
        let (mut connection, _events, _commands) = test_connection(transport, 1024, 60);
        let (publish_tx, mut publish_rx) = oneshot::channel();
        let (subscribe_tx, mut subscribe_rx) = oneshot::channel();
        let (unsubscribe_tx, mut unsubscribe_rx) = oneshot::channel();
        for (packet_id, kind, timeout_label) in [
            (
                20,
                PendingAckKind::Publish { result: publish_tx },
                "publish acknowledgement",
            ),
            (
                21,
                PendingAckKind::Subscribe {
                    requested_qos: QoS::AtLeastOnce,
                    result: subscribe_tx,
                },
                "subscription acknowledgement",
            ),
            (
                22,
                PendingAckKind::Unsubscribe {
                    filter: "events/#".to_owned(),
                    result: unsubscribe_tx,
                },
                "unsubscribe acknowledgement",
            ),
        ] {
            connection.pending_acks.insert(
                packet_id,
                PendingAck {
                    deadline: Instant::now(),
                    timeout_label,
                    kind,
                },
            );
        }
        connection.expire_operations();
        assert!(matches!(
            publish_rx.try_recv().unwrap(),
            Err(Error::Timeout(_))
        ));
        assert!(matches!(
            subscribe_rx.try_recv().unwrap(),
            Err(Error::Timeout(_))
        ));
        assert!(matches!(
            unsubscribe_rx.try_recv().unwrap(),
            Err(Error::Timeout(_))
        ));

        assert!(matches!(
            connection.dispatch_suback(SubAckPacket {
                packet_id: 20,
                reason_codes: vec![0],
            }),
            Err(Error::UnexpectedPacket(_))
        ));
        assert!(matches!(
            connection.dispatch_unsuback(UnsubAckPacket {
                packet_id: 21,
                reason_codes: vec![0],
            }),
            Err(Error::UnexpectedPacket(_))
        ));
        assert!(matches!(
            connection.dispatch_puback(PubAckPacket {
                packet_id: 22,
                reason_code: 0,
            }),
            Err(Error::UnexpectedPacket(_))
        ));
        assert_eq!(
            connection.pending_acks[&20].kind.ack_class(),
            AckClass::Publish
        );
        assert_eq!(
            connection.pending_acks[&21].kind.ack_class(),
            AckClass::Subscribe
        );
        assert_eq!(
            connection.pending_acks[&22].kind.ack_class(),
            AckClass::Unsubscribe
        );
    }

    #[test]
    fn suback_grant_above_requested_qos_is_terminal() {
        let (transport, _state) = MockTransport::new(MockState::default());
        let (mut connection, _events, _commands) = test_connection(transport, 1024, 60);
        let (result_tx, mut result_rx) = oneshot::channel();
        connection
            .begin_subscribe("events/#".to_owned(), QoS::AtMostOnce, result_tx)
            .unwrap();

        assert!(matches!(
            connection.dispatch_suback(SubAckPacket {
                packet_id: 1,
                reason_codes: vec![1],
            }),
            Err(Error::MalformedPacket(
                "SUBACK granted unsupported or unrequested QoS"
            ))
        ));
        assert!(matches!(
            result_rx.try_recv().unwrap(),
            Err(Error::MalformedPacket(
                "SUBACK granted unsupported or unrequested QoS"
            ))
        ));
    }

    #[cfg(feature = "request-response")]
    #[test]
    fn response_subscription_rejects_unsupported_qos_two_grant() {
        let (transport, _state) = MockTransport::new(MockState::default());
        let (mut connection, _events, _commands) = test_connection(transport, 1024, 60);
        connection.pending_acks.insert(
            1,
            PendingAck {
                deadline: Instant::now() + Duration::from_secs(1),
                timeout_label: "request subscription acknowledgement",
                kind: PendingAckKind::ResponseSubscribe {
                    response_topic: "responses/client-1".to_owned(),
                },
            },
        );
        connection.response_subscriptions.insert(
            "responses/client-1".to_owned(),
            ResponseSubscription::Subscribing,
        );

        assert!(matches!(
            connection.dispatch_suback(SubAckPacket {
                packet_id: 1,
                reason_codes: vec![2],
            }),
            Err(Error::MalformedPacket(
                "SUBACK granted unsupported or unrequested QoS"
            ))
        ));
        assert!(!connection
            .response_subscriptions
            .contains_key("responses/client-1"));
    }

    #[cfg(feature = "request-response")]
    #[test]
    fn failed_response_subscription_enqueue_does_not_poison_retry() {
        use crate::request_response::RequestCommand;

        let make_request = |correlation_data: Vec<u8>| {
            let mut properties = Properties::new();
            properties.set_response_topic("responses/client-1");
            properties.set_correlation_data(correlation_data.clone());
            RequestCommand {
                topic: "services/search".to_owned(),
                payload: b"query".to_vec(),
                response_topic: "responses/client-1".to_owned(),
                correlation_data,
                qos: QoS::AtLeastOnce,
                timeout: Duration::from_secs(2),
                properties,
            }
        };

        let (transport, _state) = MockTransport::new(MockState::default());
        let (mut connection, _events, _commands) = test_connection(transport, 1024, 60);
        connection.max_outbound_queue_bytes = 1;

        let (first_tx, mut first_rx) = oneshot::channel();
        connection
            .process_command(Command::Request {
                request: make_request(vec![1]),
                result: first_tx,
            })
            .unwrap();
        assert!(matches!(
            first_rx.try_recv().unwrap(),
            Err(Error::QueueFull("outbound bytes"))
        ));
        assert!(connection.pending_requests.is_empty());
        assert!(connection.pending_acks.is_empty());
        assert!(!connection
            .response_subscriptions
            .contains_key("responses/client-1"));

        connection.max_outbound_queue_bytes = 1024;
        let second_correlation = vec![2];
        let (second_tx, mut second_rx) = oneshot::channel();
        connection
            .process_command(Command::Request {
                request: make_request(second_correlation.clone()),
                result: second_tx,
            })
            .unwrap();
        assert!(matches!(
            connection.response_subscriptions.get("responses/client-1"),
            Some(ResponseSubscription::Subscribing)
        ));
        let packet_id = *connection.pending_acks.keys().next().unwrap();
        assert_eq!(
            decode_fixed_header(&connection.outbound.front().unwrap().bytes)
                .unwrap()
                .0
                .packet_type,
            PacketType::Subscribe
        );

        connection
            .dispatch_suback(SubAckPacket {
                packet_id,
                reason_codes: vec![1],
            })
            .unwrap();
        assert!(matches!(
            connection.response_subscriptions.get("responses/client-1"),
            Some(ResponseSubscription::Ready)
        ));
        connection
            .dispatch_publish(PublishPacket {
                topic: "responses/client-1".to_owned(),
                packet_id: None,
                payload: b"result".to_vec(),
                qos: QoS::AtMostOnce,
                retain: false,
                dup: false,
                properties: Properties::new().with_correlation_data(second_correlation),
            })
            .unwrap();
        assert_eq!(second_rx.try_recv().unwrap().unwrap().payload, b"result");
    }

    #[test]
    fn only_one_ping_is_queued_while_transport_is_blocked() {
        let (transport, _state) = MockTransport::new(MockState {
            write_blocked: true,
            ..MockState::default()
        });
        let (mut connection, _events, _commands) = test_connection(transport, 1024, 2);
        connection.last_write_at = Instant::now() - Duration::from_secs(2);

        connection.maintain_keep_alive().unwrap();
        connection.maintain_keep_alive().unwrap();

        assert!(connection.ping_queued);
        assert_eq!(connection.outbound.len(), 1);
        assert_eq!(connection.outbound.front().unwrap().bytes, PINGREQ_BYTES);
    }

    #[test]
    fn failed_ping_enqueue_does_not_stick_queued_state() {
        let (transport, _state) = MockTransport::new(MockState::default());
        let (mut connection, _events, _commands) = test_connection(transport, 1024, 2);
        connection.max_outbound_queue_bytes = 1;
        connection.last_write_at = Instant::now() - Duration::from_secs(2);

        assert!(matches!(
            connection.maintain_keep_alive(),
            Err(Error::QueueFull("outbound bytes"))
        ));
        assert!(!connection.ping_queued);
        assert!(connection.outbound.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn clean_disconnect_fails_operations_still_waiting_for_ack() {
        let (transport, state) = MockTransport::new(MockState::default());
        let (connection, mut events, commands) = test_connection(transport, 1024, 60);
        let (publish_tx, publish_rx) = oneshot::channel();
        let (disconnect_tx, disconnect_rx) = oneshot::channel();
        commands
            .try_send(Command::Publish {
                topic: "events/new".to_owned(),
                payload: Vec::new(),
                options: PublishOptions::default().with_qos(QoS::AtLeastOnce),
                result: publish_tx,
            })
            .unwrap();
        commands
            .try_send(Command::Disconnect {
                result: disconnect_tx,
            })
            .unwrap();

        assert!(connection.run().await.is_ok());
        assert!(disconnect_rx.await.unwrap().is_ok());
        assert!(matches!(
            publish_rx.await.unwrap(),
            Err(Error::ConnectionClosed)
        ));
        assert_eq!(
            events.try_recv().unwrap(),
            Event::Disconnected(DisconnectReason::ClientInitiated)
        );
        assert!(state.lock().unwrap().shutdown);
    }

    #[test]
    fn read_capacity_handles_two_packets_larger_than_single_packet_limit() {
        let first = PublishPacket {
            topic: "a".to_owned(),
            packet_id: None,
            payload: b"1".to_vec(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
        }
        .encode()
        .unwrap();
        let second = PublishPacket {
            topic: "b".to_owned(),
            packet_id: None,
            payload: b"2".to_vec(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
        }
        .encode()
        .unwrap();
        let max_packet_size = first.len().max(second.len());
        let mut combined = first;
        combined.extend_from_slice(&second);
        assert!(combined.len() > max_packet_size);

        let (transport, _state) = MockTransport::new(MockState {
            reads: VecDeque::from([combined]),
            ..MockState::default()
        });
        let (mut connection, mut events, _commands) =
            test_connection(transport, max_packet_size, 60);

        connection.read_available().unwrap();
        connection.decode_and_dispatch().unwrap();
        connection.read_available().unwrap();
        connection.decode_and_dispatch().unwrap();

        let topics: Vec<String> = [events.try_recv().unwrap(), events.try_recv().unwrap()]
            .into_iter()
            .map(|event| match event {
                Event::Publish(message) => message.topic,
                Event::Disconnected(_) => panic!("unexpected disconnect"),
            })
            .collect();
        assert_eq!(topics, ["a", "b"]);
    }

    #[test]
    fn blocking_handshake_rejects_invalid_connack_flags() {
        let (mut transport, _state) = MockTransport::new(MockState {
            reads: VecDeque::from([vec![0x21, 0x03, 0x00, 0x00, 0x00]]),
            ..MockState::default()
        });
        assert!(matches!(
            read_packet_blocking(
                &mut transport,
                1024,
                Instant::now() + Duration::from_secs(1)
            ),
            Err(Error::MalformedPacket("invalid fixed-header flags"))
        ));
    }

    #[test]
    fn connect_advertises_packet_and_receive_limits() {
        let (transport, state) = MockTransport::new(MockState {
            reads: VecDeque::from([vec![0x20, 0x03, 0x00, 0x00, 0x00]]),
            ..MockState::default()
        });
        let options = ConnectOptions::new("limit-test")
            .with_max_packet_size(128)
            .with_event_capacity(7);
        let (_client, _connection) = AsyncMqttClient::connect_with(transport, options).unwrap();

        let writes = state.lock().unwrap().writes.clone();
        let (header, header_len) = decode_fixed_header(&writes).unwrap();
        assert_eq!(header.packet_type, PacketType::Connect);
        let mut cursor = crate::codec::decode::Cursor::new(&writes[header_len..]);
        assert_eq!(cursor.read_string().unwrap(), "MQTT");
        assert_eq!(cursor.read_u8().unwrap(), 5);
        let _connect_flags = cursor.read_u8().unwrap();
        let _keep_alive = cursor.read_u16().unwrap();
        let properties = Properties::decode(&mut cursor).unwrap();
        assert_eq!(properties.get_u32(PropertyId::MaximumPacketSize), Some(128));
        assert_eq!(properties.get_u16(PropertyId::ReceiveMaximum), Some(7));
    }

    #[test]
    fn connect_validates_session_present_and_assigned_client_identifier() {
        let (missing_assignment, _state) = MockTransport::new(MockState {
            reads: VecDeque::from([encode_connack(false, Properties::new())]),
            ..MockState::default()
        });
        assert!(matches!(
            AsyncMqttClient::connect_with(missing_assignment, ConnectOptions::new("")),
            Err(Error::MalformedPacket(
                "CONNACK omitted a non-empty assigned client identifier"
            ))
        ));

        let (resumed_after_clean_start, _state) = MockTransport::new(MockState {
            reads: VecDeque::from([encode_connack(true, Properties::new())]),
            ..MockState::default()
        });
        assert!(matches!(
            AsyncMqttClient::connect_with(
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
            PropertyValue::Str("assigned-client".to_owned()),
        );
        let (assigned, _state) = MockTransport::new(MockState {
            reads: VecDeque::from([encode_connack(false, properties)]),
            ..MockState::default()
        });
        assert!(AsyncMqttClient::connect_with(
            assigned,
            ConnectOptions::new("").with_clean_start(false)
        )
        .is_ok());
    }

    #[test]
    fn connect_timeout_is_shared_across_slow_progress_reads() {
        let (transport, _state) = MockTransport::new(MockState {
            reads: VecDeque::from([vec![0x20, 0x03, 0x00, 0x00, 0x00]]),
            read_delay: Duration::from_millis(5),
            ..MockState::default()
        });
        let options =
            ConnectOptions::new("slow-handshake").with_connect_timeout(Duration::from_millis(1));

        assert!(matches!(
            AsyncMqttClient::connect_with(transport, options),
            Err(Error::Timeout("connect"))
        ));
    }

    #[cfg(feature = "request-response")]
    #[test]
    fn request_uses_standard_properties_and_consumes_matching_response() {
        use crate::request_response::RequestCommand;

        let (transport, _state) = MockTransport::new(MockState::default());
        let (mut connection, mut events, _commands) = test_connection(transport, 1024, 60);
        let (result_tx, mut result_rx) = oneshot::channel();
        let correlation_data = vec![0, 1, 2, 3];
        let mut properties = Properties::new();
        properties.set_response_topic("responses/client-1");
        properties.set_correlation_data(correlation_data.clone());
        connection
            .begin_request(
                RequestCommand {
                    topic: "services/search".to_owned(),
                    payload: b"query".to_vec(),
                    response_topic: "responses/client-1".to_owned(),
                    correlation_data: correlation_data.clone(),
                    qos: QoS::AtLeastOnce,
                    timeout: Duration::from_secs(2),
                    properties,
                },
                result_tx,
            )
            .unwrap();

        connection
            .dispatch_suback(SubAckPacket {
                packet_id: 1,
                reason_codes: vec![0],
            })
            .unwrap();
        let request_frame = connection.outbound.back().unwrap();
        let Packet::Publish(request) = decode_packet(&request_frame.bytes) else {
            panic!("expected request PUBLISH");
        };
        assert_eq!(
            request.properties.response_topic(),
            Some("responses/client-1")
        );
        assert_eq!(
            request.properties.correlation_data(),
            Some(correlation_data.as_slice())
        );
        assert_eq!(request.payload, b"query");

        let response = PublishPacket {
            topic: "responses/client-1".to_owned(),
            packet_id: None,
            payload: b"result".to_vec(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::new().with_correlation_data(correlation_data),
        };
        connection.dispatch_publish(response).unwrap();

        assert_eq!(result_rx.try_recv().unwrap().unwrap().payload, b"result");
        assert!(matches!(
            events.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[cfg(feature = "request-response")]
    #[tokio::test(flavor = "current_thread")]
    async fn request_cancelled_before_driver_processing_is_never_published() {
        use crate::request_response::RequestOptions;

        let (transport, _state) = MockTransport::new(MockState::default());
        let (mut connection, events, commands) = test_connection(transport, 1024, 60);
        let client = AsyncMqttClient {
            commands,
            events: Arc::new(Mutex::new(events)),
            max_packet_size: 1024,
            request_lifecycle: connection.request_lifecycle.clone(),
        };

        let mut request = Box::pin(client.request(
            "services/search",
            b"query",
            RequestOptions::new("responses/client-1").with_correlation_data([7, 7, 7]),
        ));
        tokio::select! {
            biased;
            result = &mut request => panic!("request unexpectedly completed: {result:?}"),
            () = tokio::task::yield_now() => {}
        }
        drop(request);

        // This is the driver's normal ordering: cancellations are observed
        // before commands. An unmatched marker must remain for begin_request.
        connection.drain_cancellations();
        connection.drain_commands().unwrap();
        assert!(connection.pending_requests.is_empty());
        assert!(connection.outbound.is_empty());
        let lifecycle = connection.request_lifecycle.lock().unwrap();
        assert!(lifecycle.active.is_empty());
        assert!(lifecycle.cancelled.is_empty());
    }

    #[cfg(feature = "request-response")]
    #[tokio::test(flavor = "current_thread")]
    async fn driver_drop_releases_request_lifecycle_registration() {
        use crate::request_response::RequestOptions;

        let (transport, _state) = MockTransport::new(MockState::default());
        let (connection, events, commands) = test_connection(transport, 1024, 60);
        let lifecycle = connection.request_lifecycle.clone();
        let client = AsyncMqttClient {
            commands,
            events: Arc::new(Mutex::new(events)),
            max_packet_size: 1024,
            request_lifecycle: lifecycle.clone(),
        };
        let mut request = Box::pin(client.request(
            "services/search",
            b"query",
            RequestOptions::new("responses/client-1").with_correlation_data([8, 8, 8]),
        ));
        tokio::select! {
            biased;
            result = &mut request => panic!("request unexpectedly completed: {result:?}"),
            () = tokio::task::yield_now() => {}
        }
        assert_eq!(lifecycle.lock().unwrap().active.len(), 1);

        drop(connection);
        assert!(matches!(request.await, Err(Error::ClientClosed)));
        assert!(lifecycle.lock().unwrap().active.is_empty());
    }
}
