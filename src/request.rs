use std::future::Future;
use std::io;
use std::net::TcpStream;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::async_client::{maybe_ping, write_blocking, PendingRequest, Shared, SharedInner};
use crate::codec::types::*;
use crate::error::{Error, Result};
use crate::transport::Transport;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const IDLE_POLL_BACKOFF: Duration = Duration::from_millis(1);

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
    state: RequestState,
    deadline: Instant,
}

impl<T: Transport> RequestFuture<T> {
    pub(crate) fn new<Req: Serialize>(inner: Shared<T>, topic: String, payload: &Req) -> Self {
        let correlation_id = generate_correlation_id();
        let (reply_topic, amqp_mode) = {
            let inner = inner.borrow();
            (
                format!("{}{}", inner.reply_topic_prefix, correlation_id),
                inner.amqp_reply_format,
            )
        };
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
        let payload_json = serde_json::to_vec(&envelope).expect("serialization should not fail");

        RequestFuture {
            inner,
            correlation_id,
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
                drop(inner);
                this.inner.borrow_mut().pending.remove(&this.correlation_id);
                this.state = RequestState::Done;
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
                RequestState::Init {
                    topic,
                    payload_json,
                } => {
                    let mut inner = this.inner.borrow_mut();

                    // Register pending slot
                    inner.pending.insert(
                        this.correlation_id.clone(),
                        PendingRequest {
                            waker: Some(cx.waker().clone()),
                            result: None,
                        },
                    );

                    // Send PUBLISH with request envelope
                    let pub_pkt = PublishPacket {
                        topic,
                        packet_id: None,
                        payload: payload_json,
                        qos: QoS::AtMostOnce,
                        retain: false,
                        dup: false,
                        properties: Default::default(),
                    };
                    let pub_bytes = match pub_pkt.encode() {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            inner.pending.remove(&this.correlation_id);
                            this.state = RequestState::Done;
                            return Poll::Ready(Err(e));
                        }
                    };
                    if let Err(e) = write_blocking(&mut inner, &pub_bytes) {
                        inner.pending.remove(&this.correlation_id);
                        this.state = RequestState::Done;
                        return Poll::Ready(Err(e));
                    }

                    drop(inner);
                    this.state = RequestState::Waiting;
                    // fall through to Waiting
                }

                RequestState::Waiting => {
                    // Pump the socket — cooperative polling
                    let pump_result = match pump_socket(&this.inner) {
                        Ok(result) => result,
                        Err(e) => {
                            this.inner.borrow_mut().pending.remove(&this.correlation_id);
                            this.state = RequestState::Done;
                            return Poll::Ready(Err(e));
                        }
                    };

                    // Check our slot
                    let mut inner = this.inner.borrow_mut();
                    if let Some(pending) = inner.pending.get_mut(&this.correlation_id) {
                        if let Some(result) = pending.result.take() {
                            inner.pending.remove(&this.correlation_id);
                            drop(inner);

                            this.state = RequestState::Done;
                            return match result {
                                Ok(payload_bytes) => {
                                    match serde_json::from_slice::<ReplyEnvelope>(&payload_bytes) {
                                        Ok(env) => Poll::Ready(Ok(env.result)),
                                        Err(e) => {
                                            Poll::Ready(Err(Error::Deserialize(e.to_string())))
                                        }
                                    }
                                }
                                Err(e) => Poll::Ready(Err(e)),
                            };
                        }
                        // Not ready — update waker and yield
                        pending.waker = Some(cx.waker().clone());
                    } else {
                        this.state = RequestState::Done;
                        return Poll::Ready(Err(Error::ConnectionClosed));
                    }
                    drop(inner);

                    this.state = RequestState::Waiting;
                    if pump_result == PumpResult::Idle {
                        thread::sleep(IDLE_POLL_BACKOFF);
                    }
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

impl<T: Transport> Drop for RequestFuture<T> {
    fn drop(&mut self) {
        if !matches!(self.state, RequestState::Done) {
            self.inner.borrow_mut().pending.remove(&self.correlation_id);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PumpResult {
    Active,
    Idle,
}

/// Read available data from the non-blocking socket, parse packets, dispatch.
fn pump_socket<T: Transport>(shared: &Shared<T>) -> Result<PumpResult> {
    let mut inner = shared.borrow_mut();

    if inner.error.is_some() {
        return Ok(PumpResult::Idle);
    }

    let mut active = false;

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
                return Ok(PumpResult::Active);
            }
            Ok(n) => {
                active = true;
                inner.last_read_at = Instant::now();
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
            Ok(Some(packet)) => {
                active = true;
                dispatch_packet(&mut inner, packet)?;
            }
            Ok(None) => break,
            Err(e) => {
                inner.error = Some(Error::MalformedPacket("frame decode error"));
                return Err(e);
            }
        }
    }

    maybe_ping(&mut inner)?;
    Ok(if active {
        PumpResult::Active
    } else {
        PumpResult::Idle
    })
}

fn dispatch_packet<T: Transport>(inner: &mut SharedInner<T>, packet: Packet) -> Result<()> {
    match packet {
        Packet::Publish(pub_pkt) => {
            // ACK QoS 1
            if pub_pkt.qos == QoS::AtLeastOnce {
                if let Some(id) = pub_pkt.packet_id {
                    let ack = PubAckPacket {
                        packet_id: id,
                        reason_code: 0x00,
                    };
                    let bytes = ack.encode()?;
                    write_blocking(inner, &bytes)?;
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
            inner.last_read_at = Instant::now();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FrameReader;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::io;
    use std::rc::Rc;
    use std::task::{RawWaker, RawWakerVTable, Waker};

    struct MockTransport {
        writes: Vec<Vec<u8>>,
    }

    impl Transport for MockTransport {
        fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
            self.writes.push(buf.to_vec());
            Ok(())
        }

        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        }

        fn read_exact(&mut self, _buf: &mut [u8]) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::WouldBlock))
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

    fn test_shared() -> Shared<MockTransport> {
        let now = Instant::now();
        Rc::new(RefCell::new(SharedInner {
            stream: MockTransport { writes: Vec::new() },
            frame_reader: FrameReader::new(),
            pending: HashMap::new(),
            next_packet_id: 1,
            keep_alive_secs: 60,
            last_read_at: now,
            last_write_at: now,
            error: None,
            amqp_reply_format: false,
            reply_topic_prefix: String::from("egress/reply/test-client/"),
        }))
    }

    fn noop_waker() -> Waker {
        unsafe fn clone(_: *const ()) -> RawWaker {
            raw_waker()
        }
        unsafe fn wake(_: *const ()) {}
        unsafe fn wake_by_ref(_: *const ()) {}
        unsafe fn drop(_: *const ()) {}

        fn raw_waker() -> RawWaker {
            RawWaker::new(
                std::ptr::null(),
                &RawWakerVTable::new(clone, wake, wake_by_ref, drop),
            )
        }

        unsafe { Waker::from_raw(raw_waker()) }
    }

    #[test]
    fn drop_in_flight_request_removes_pending_slot() {
        let shared = test_shared();
        let mut future = RequestFuture::new(
            shared.clone(),
            String::from("request/topic"),
            &serde_json::json!({"hello": "world"}),
        );
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        assert!(std::future::Future::poll(Pin::new(&mut future), &mut cx).is_pending());
        assert_eq!(shared.borrow().pending.len(), 1);

        drop(future);
        assert!(shared.borrow().pending.is_empty());
    }
}
