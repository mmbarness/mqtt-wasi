#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

pub mod codec;
pub mod error;
pub mod trace;

#[cfg(feature = "std")]
pub mod transport;
#[cfg(feature = "std")]
pub mod client;
#[cfg(feature = "std")]
pub mod options;
#[cfg(feature = "std")]
pub mod frame;
#[cfg(feature = "std")]
pub mod async_client;
#[cfg(feature = "std")]
pub mod request;
#[cfg(feature = "tls")]
pub mod tls;

// Re-exports for convenience
pub use crate::codec::types::{Packet, QoS};
pub use crate::error::Error;
pub use crate::trace::TraceContext;

#[cfg(feature = "std")]
pub use crate::client::{Message, MqttClient, RawMessage, Subscription};
#[cfg(feature = "std")]
pub use crate::options::ConnectOptions;
#[cfg(feature = "std")]
pub use crate::transport::Transport;
#[cfg(feature = "std")]
pub use crate::async_client::AsyncMqttClient;
#[cfg(feature = "std")]
pub use crate::request::{RequestEnvelope, ReplyEnvelope};
#[cfg(feature = "tls")]
pub use crate::tls::TlsTransport;
