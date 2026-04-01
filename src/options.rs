use crate::codec::types::QoS;

/// Options for connecting to an MQTT broker.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    pub client_id: String,
    pub keep_alive_secs: u16,
    pub clean_start: bool,
    pub username: Option<String>,
    pub password: Option<Vec<u8>>,
    /// When true, the `replyTo` field in request envelopes uses dots instead
    /// of slashes (e.g. `replies.{id}` instead of `replies/{id}`).
    ///
    /// Enable this when the reply publisher is an AMQP consumer (e.g. behind
    /// RabbitMQ's MQTT plugin), since AMQP routing keys use dots while MQTT
    /// topics use slashes. The MQTT plugin bridges between the two formats.
    pub amqp_reply_format: bool,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            client_id: String::new(), // empty = broker assigns one
            keep_alive_secs: 60,
            clean_start: true,
            username: None,
            password: None,
            amqp_reply_format: false,
        }
    }
}

impl ConnectOptions {
    /// Create options with the given client ID. Empty string lets the broker assign one.
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            ..Default::default()
        }
    }

    /// Set the keep-alive interval in seconds (default: 60).
    pub fn with_keep_alive(mut self, secs: u16) -> Self {
        self.keep_alive_secs = secs;
        self
    }

    /// Set username and password for broker authentication.
    pub fn with_credentials(mut self, user: impl Into<String>, pass: impl Into<Vec<u8>>) -> Self {
        self.username = Some(user.into());
        self.password = Some(pass.into());
        self
    }

    pub fn with_clean_start(mut self, clean: bool) -> Self {
        self.clean_start = clean;
        self
    }

    /// Enable AMQP-compatible reply routing for RabbitMQ MQTT plugin.
    ///
    /// When the reply publisher is an AMQP consumer (not an MQTT client),
    /// `replyTo` must use dots (AMQP routing key format) while the MQTT
    /// subscription uses slashes. RabbitMQ's MQTT plugin bridges between them.
    pub fn with_amqp_reply_format(mut self, enabled: bool) -> Self {
        self.amqp_reply_format = enabled;
        self
    }
}

/// Options for publishing a message.
#[derive(Debug, Clone)]
pub struct PublishOptions {
    pub qos: QoS,
    pub retain: bool,
}

impl Default for PublishOptions {
    fn default() -> Self {
        Self {
            qos: QoS::AtMostOnce,
            retain: false,
        }
    }
}
