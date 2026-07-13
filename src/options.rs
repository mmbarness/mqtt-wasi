use core::fmt;
use std::time::Duration;

use crate::codec::properties::Properties;
use crate::codec::types::QoS;
use crate::error::{Error, Result};

/// Default maximum complete MQTT packet accepted from a peer (1 MiB).
pub const DEFAULT_MAX_PACKET_SIZE: usize = 1024 * 1024;

/// Options shared by the blocking client and the async connection driver.
#[derive(Clone)]
pub struct ConnectOptions {
    pub(crate) client_id: String,
    pub(crate) keep_alive_secs: u16,
    pub(crate) clean_start: bool,
    pub(crate) username: Option<String>,
    pub(crate) password: Option<Vec<u8>>,
    pub(crate) connect_timeout: Duration,
    pub(crate) ack_timeout: Duration,
    pub(crate) poll_interval: Duration,
    pub(crate) max_packet_size: usize,
    pub(crate) max_incoming_messages: usize,
    pub(crate) command_capacity: usize,
    pub(crate) event_capacity: usize,
    pub(crate) max_outbound_queue_bytes: usize,
}

impl fmt::Debug for ConnectOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectOptions")
            .field("client_id", &self.client_id)
            .field("keep_alive_secs", &self.keep_alive_secs)
            .field("clean_start", &self.clean_start)
            .field("username_set", &self.username.is_some())
            .field("password_set", &self.password.is_some())
            .field("connect_timeout", &self.connect_timeout)
            .field("ack_timeout", &self.ack_timeout)
            .field("poll_interval", &self.poll_interval)
            .field("max_packet_size", &self.max_packet_size)
            .field("max_incoming_messages", &self.max_incoming_messages)
            .field("command_capacity", &self.command_capacity)
            .field("event_capacity", &self.event_capacity)
            .field("max_outbound_queue_bytes", &self.max_outbound_queue_bytes)
            .finish()
    }
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            keep_alive_secs: 60,
            clean_start: true,
            username: None,
            password: None,
            connect_timeout: Duration::from_secs(10),
            ack_timeout: Duration::from_secs(10),
            poll_interval: Duration::from_millis(2),
            max_packet_size: DEFAULT_MAX_PACKET_SIZE,
            max_incoming_messages: 256,
            command_capacity: 64,
            event_capacity: 256,
            max_outbound_queue_bytes: 4 * DEFAULT_MAX_PACKET_SIZE,
        }
    }
}

impl ConnectOptions {
    /// Create options with the given client identifier. An empty identifier asks
    /// the broker to assign one.
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            ..Default::default()
        }
    }

    pub fn with_keep_alive(mut self, secs: u16) -> Self {
        self.keep_alive_secs = secs;
        self
    }

    pub fn with_credentials(mut self, user: impl Into<String>, pass: impl Into<Vec<u8>>) -> Self {
        self.username = Some(user.into());
        self.password = Some(pass.into());
        self
    }

    pub fn with_clean_start(mut self, clean: bool) -> Self {
        self.clean_start = clean;
        self
    }

    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    pub fn with_ack_timeout(mut self, timeout: Duration) -> Self {
        self.ack_timeout = timeout;
        self
    }

    /// Configure how often the async driver polls a non-blocking transport when
    /// there is no command activity.
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Set the maximum complete packet accepted from the broker. The value is
    /// also advertised with MQTT v5's Maximum Packet Size CONNECT property.
    pub fn with_max_packet_size(mut self, bytes: usize) -> Self {
        self.max_packet_size = bytes;
        self
    }

    pub fn with_max_incoming_messages(mut self, count: usize) -> Self {
        self.max_incoming_messages = count;
        self.event_capacity = count;
        self
    }

    pub fn with_command_capacity(mut self, count: usize) -> Self {
        self.command_capacity = count;
        self
    }

    pub fn with_event_capacity(mut self, count: usize) -> Self {
        self.event_capacity = count;
        self
    }

    pub fn with_max_outbound_queue_bytes(mut self, bytes: usize) -> Self {
        self.max_outbound_queue_bytes = bytes;
        self
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub fn keep_alive_secs(&self) -> u16 {
        self.keep_alive_secs
    }

    pub fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    pub fn ack_timeout(&self) -> Duration {
        self.ack_timeout
    }

    pub fn max_packet_size(&self) -> usize {
        self.max_packet_size
    }

    pub fn max_incoming_messages(&self) -> usize {
        self.max_incoming_messages
    }

    pub fn command_capacity(&self) -> usize {
        self.command_capacity
    }

    pub fn event_capacity(&self) -> usize {
        self.event_capacity
    }

    pub fn max_outbound_queue_bytes(&self) -> usize {
        self.max_outbound_queue_bytes
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.connect_timeout.is_zero() {
            return Err(Error::InvalidOptions("connect timeout must be non-zero"));
        }
        if self.ack_timeout.is_zero() {
            return Err(Error::InvalidOptions("ack timeout must be non-zero"));
        }
        if self.poll_interval.is_zero() {
            return Err(Error::InvalidOptions("poll interval must be non-zero"));
        }
        if !(2..=268_435_460).contains(&self.max_packet_size) {
            return Err(Error::InvalidOptions("invalid maximum packet size"));
        }
        if self.max_incoming_messages == 0
            || self.command_capacity == 0
            || self.event_capacity == 0
            || self.max_outbound_queue_bytes == 0
        {
            return Err(Error::InvalidOptions("queue capacities must be non-zero"));
        }
        let now = std::time::Instant::now();
        if now.checked_add(self.connect_timeout).is_none()
            || now.checked_add(self.ack_timeout).is_none()
        {
            return Err(Error::InvalidOptions("timeout is too large"));
        }
        Ok(())
    }
}

/// Options for an outgoing PUBLISH packet.
#[derive(Debug, Clone)]
pub struct PublishOptions {
    pub qos: QoS,
    pub retain: bool,
    pub properties: Properties,
}

impl Default for PublishOptions {
    fn default() -> Self {
        Self {
            qos: QoS::AtMostOnce,
            retain: false,
            properties: Properties::new(),
        }
    }
}

impl PublishOptions {
    pub fn with_qos(mut self, qos: QoS) -> Self {
        self.qos = qos;
        self
    }

    pub fn with_retain(mut self, retain: bool) -> Self {
        self.retain = retain;
        self
    }

    pub fn with_properties(mut self, properties: Properties) -> Self {
        self.properties = properties;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_credentials() {
        let options = ConnectOptions::new("debug-client")
            .with_credentials("visible-user", b"super-secret".to_vec());
        let debug = format!("{options:?}");

        assert!(debug.contains("username_set: true"));
        assert!(debug.contains("password_set: true"));
        assert!(!debug.contains("visible-user"));
        assert!(!debug.contains("super-secret"));
        assert!(!debug.contains("[115, 117, 112"));
    }

    #[test]
    fn mqtt_v5_allows_empty_client_identifier_without_clean_start() {
        assert!(ConnectOptions::new("")
            .with_clean_start(false)
            .validate()
            .is_ok());
    }
}
