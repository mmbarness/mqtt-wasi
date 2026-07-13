//! Runtime-light MQTT v5 client for native Rust and `wasm32-wasip2`.
//!
//! The blocking [`MqttClient`] and the optional async [`AsyncMqttClient`] use
//! raw byte payloads and standard MQTT properties. Async connections are driven
//! independently by [`MqttConnection`], so network I/O and keepalive continue
//! without an active publish or request future.
//!
//! Optional TLS is available through the experimental `tls` feature.
//!
//! See the [README](https://github.com/mmbarness/mqtt-wasi) for full usage examples.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

#[cfg(not(feature = "std"))]
extern crate alloc;

pub mod codec;
pub mod error;
pub mod trace;

#[cfg(feature = "async-client")]
pub mod async_client;
#[cfg(feature = "std")]
pub mod client;
#[cfg(feature = "std")]
pub mod frame;
#[cfg(feature = "std")]
pub mod options;
#[cfg(feature = "request-response")]
pub mod request_response;
#[cfg(feature = "tls")]
pub mod tls;
#[cfg(feature = "std")]
pub mod transport;

// Re-exports for convenience
pub use crate::codec::types::{Packet, PublishPacket, QoS};
pub use crate::error::Error;
pub use crate::trace::TraceContext;

#[cfg(feature = "async-client")]
pub use crate::async_client::{
    AsyncMqttClient, DisconnectReason, Event, MqttConnection, PublishAck, SubscriptionAck,
};
#[cfg(feature = "std")]
pub use crate::client::{Incoming, MqttClient};
#[cfg(feature = "std")]
pub use crate::options::{ConnectOptions, PublishOptions};
#[cfg(feature = "request-response")]
pub use crate::request_response::RequestOptions;
#[cfg(feature = "tls")]
pub use crate::tls::TlsTransport;
#[cfg(feature = "std")]
pub use crate::transport::Transport;
