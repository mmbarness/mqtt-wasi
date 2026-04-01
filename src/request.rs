use std::future::Future;
use std::io;
use std::net::TcpStream;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::async_client::{maybe_ping, next_packet_id, write_blocking, PendingRequest, Shared, SharedInner};
use crate::codec::properties::Properties;
use crate::codec::types::*;
use crate::error::{Error, Result};
use crate::transport::Transport;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const REPLY_TOPIC_PREFIX: &str = "egress/reply/";

/// Outgoing request envelope.
#[derive(Serialize)]
pub struct RequestEnvelope<'a, T: Serialize> {
    #[serde(rename = "type")]
    pub msg_type: &'a str,
    pub action: &'a str,
    pub params: &'a T,
    #[serde(rename = "correlationId")]
    pub correlation_id: &'a str,
    #[serde(rename = "replyTo")]
    pub reply_to: &'a str,
}

/// Incoming reply envelope.
#[derive(Deserialize)]
pub struct ReplyEnvelope {
    #[serde(rename = "correlationId")]
    pub correlation_id: String,
    pub result: serde_json::Value,
}

enum RequestState {
    /// Send SUBSCRIBE + PUBLISH, register pending slot.
    Init {
        topic: String,
        payload_json: Vec<u8>,
    },
    /// Waiting for the correlated reply.
    Waiting,
    /// Already returned a result.
    Done,
}

/// A Future that resolves when the correlated MQTT reply arrives.
///
/// Each poll pumps the shared non-blocking socket, enabling cooperative
/// multiplexing of concurrent request/reply flows via `tokio::join!`.
pub struct RequestFuture<T: Transport = TcpStream> {
    inner: Shared<T>,
    correlation_id: String,
    reply_topic: String,
    state: RequestState,
    deadline: Instant,
}

impl<T: Transport> RequestFuture<T> {
    pub(crate) fn new<Req: Serialize>(
        inner: Shared<T>,
        topic: String,
        payload: &Req,
    ) -> Self {
        let correlation_id = generate_correlation_id();
        // MQTT subscription always uses slashes (MQTT topic separator)
        let reply_topic = format!("{REPLY_TOPIC_PREFIX}{correlation_id}");
        // replyTo in envelope: dots for AMQP repliers, slashes for MQTT repliers
        let amqp_mode = inner.borrow().amqp_reply_format;
        let reply_to = if amqp_mode {
            reply_topic.replace('/', ".")
        } else {
            reply_topic.clone()
        };

        let envelope = RequestEnvelope {
            msg_type: "request",
            action: "chat",
            params: payload,
            correlation_id: &correlation_id,
            reply_to: &reply_to,
        };
        let payload_json = serde_json::to_vec(&envelope)
            .expect("serialization should not fail");

        RequestFuture {
            inner,
            correlation_id,
            reply_topic,
            state: RequestState::Init {
                topic,
                payload_json,
            },
            deadline: Instant::now() + DEFAULT_TIMEOUT,
        }
    }
}

impl<T: Transport> Future for RequestFuture<T> {
    type Output = Result<serde_json::Value>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;

        // Check fatal error
        {
            let inner = this.inner.borrow();
            if let Some(ref _e) = inner.error {
                return Poll::Ready(Err(Error::ConnectionClosed));
            }
        }

        // Check timeout
        if Instant::now() >= this.deadline {
            this.inner.borrow_mut().pending.remove(&this.correlation_id);
            this.state = RequestState::Done;
            return Poll::Ready(Err(Error::Timeout));
        }

        loop {
            match std::mem::replace(&mut this.state, RequestState::Done) {
                RequestState::Init { topic, payload_json } => {
                    let mut inner = this.inner.borrow_mut();

                    // Register pending slot
                    inner.pending.insert(
                        this.correlation_id.clone(),
                        PendingRequest {
                            waker: Some(cx.waker().clone()),
                            result: None,
                        },
                    );

                    // Send SUBSCRIBE for reply topic
                    let sub_id = next_packet_id(&mut inner);
                    let sub = SubscribePacket {
                        packet_id: sub_id,
                        filters: vec![(this.reply_topic.clone(), QoS::AtMostOnce)],
                        properties: Properties::new(),
                    };
                    let sub_bytes = sub.encode()?;
                    write_blocking(&mut inner.stream, &sub_bytes)?;

                    // Send PUBLISH with request envelope
                    let pub_pkt = PublishPacket {
                        topic,
                        packet_id: None,
                        payload: payload_json,
                        qos: QoS::AtMostOnce,
                        retain: false,
                        dup: false,
                        properties: Properties::new(),
                    };
                    let pub_bytes = pub_pkt.encode()?;
                    write_blocking(&mut inner.stream, &pub_bytes)?;
                    inner.last_activity = Instant::now();

                    drop(inner);
                    this.state = RequestState::Waiting;
                    // fall through to Waiting
                }

                RequestState::Waiting => {
                    // Pump the socket — cooperative polling
                    pump_socket(&this.inner)?;

                    // Check our slot
                    let mut inner = this.inner.borrow_mut();
                    if let Some(pending) = inner.pending.get_mut(&this.correlation_id) {
                        if let Some(result) = pending.result.take() {
                            inner.pending.remove(&this.correlation_id);

                            // Fire-and-forget UNSUBSCRIBE
                            let unsub_id = next_packet_id(&mut inner);
                            let unsub = UnsubscribePacket {
                                packet_id: unsub_id,
                                filters: vec![this.reply_topic.clone()],
                                properties: Properties::new(),
                            };
                            if let Ok(bytes) = unsub.encode() {
                                let _ = write_blocking(&mut inner.stream, &bytes);
                            }
                            drop(inner);

                            this.state = RequestState::Done;
                            return match result {
                                Ok(payload_bytes) => {
                                    match serde_json::from_slice::<ReplyEnvelope>(&payload_bytes) {
                                        Ok(env) => Poll::Ready(Ok(env.result)),
                                        Err(e) => Poll::Ready(Err(Error::Deserialize(e.to_string()))),
                                    }
                                }
                                Err(e) => Poll::Ready(Err(e)),
                            };
                        }
                        // Not ready — update waker and yield
                        pending.waker = Some(cx.waker().clone());
                    }
                    drop(inner);

                    this.state = RequestState::Waiting;
                    // Self-wake: no I/O reactor to wake us
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }

                RequestState::Done => {
                    panic!("RequestFuture polled after completion");
                }
            }
        }
    }
}

/// Read available data from the non-blocking socket, parse packets, dispatch.
fn pump_socket<T: Transport>(shared: &Shared<T>) -> Result<()> {
    let mut inner = shared.borrow_mut();

    if inner.error.is_some() {
        return Ok(());
    }

    // Non-blocking read loop
    let mut tmp = [0u8; 8192];
    loop {
        match inner.stream.read(&mut tmp) {
            Ok(0) => {
                // Connection closed
                inner.error = Some(Error::ConnectionClosed);
                for (_, pending) in inner.pending.iter_mut() {
                    pending.result = Some(Err(Error::ConnectionClosed));
                    if let Some(w) = pending.waker.take() {
                        w.wake();
                    }
                }
                return Ok(());
            }
            Ok(n) => {
                inner.frame_reader.push(&tmp[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) => {
                inner.error = Some(Error::Io(io::Error::new(e.kind(), e.to_string())));
                return Err(Error::Io(e));
            }
        }
    }

    // Decode and dispatch all complete packets
    loop {
        match inner.frame_reader.try_decode() {
            Ok(Some(packet)) => dispatch_packet(&mut inner, packet)?,
            Ok(None) => break,
            Err(e) => {
                inner.error = Some(Error::MalformedPacket("frame decode error"));
                return Err(e);
            }
        }
    }

    maybe_ping(&mut inner)?;
    Ok(())
}

fn dispatch_packet<T: Transport>(
    inner: &mut SharedInner<T>,
    packet: Packet,
) -> Result<()> {
    match packet {
        Packet::Publish(pub_pkt) => {
            // ACK QoS 1
            if pub_pkt.qos == QoS::AtLeastOnce {
                if let Some(id) = pub_pkt.packet_id {
                    let ack = PubAckPacket { packet_id: id, reason_code: 0x00 };
                    let bytes = ack.encode()?;
                    write_blocking(&mut inner.stream, &bytes)?;
                }
            }

            // Try to match by correlation ID
            if let Ok(envelope) = serde_json::from_slice::<ReplyEnvelope>(&pub_pkt.payload) {
                if let Some(pending) = inner.pending.get_mut(&envelope.correlation_id) {
                    pending.result = Some(Ok(pub_pkt.payload));
                    if let Some(w) = pending.waker.take() {
                        w.wake();
                    }
                }
                // Unmatched replies silently dropped (late/duplicate)
            }
        }
        Packet::PingResp => {
            inner.last_activity = Instant::now();
        }
        Packet::Disconnect(disc) => {
            inner.error = Some(Error::ConnectionRefused(disc.reason_code));
            for (_, pending) in inner.pending.iter_mut() {
                pending.result = Some(Err(Error::ConnectionClosed));
                if let Some(w) = pending.waker.take() {
                    w.wake();
                }
            }
        }
        // SubAck, UnsubAck, PubAck — ignored for QoS 0 request/reply
        _ => {}
    }
    Ok(())
}

fn generate_correlation_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
