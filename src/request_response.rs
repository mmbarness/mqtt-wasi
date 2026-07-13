//! Optional MQTT v5 request/response convenience layer.
//!
//! Requests and responses are opaque byte payloads. Routing and correlation use
//! the standard MQTT v5 Response Topic and Correlation Data properties.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::async_client::{AsyncMqttClient, RequestLifecycle};
use crate::codec::properties::Properties;
use crate::codec::types::{PublishPacket, QoS};
use crate::error::{Error, Result};

/// Configuration for one MQTT v5 request.
#[derive(Debug, Clone)]
pub struct RequestOptions {
    response_topic: String,
    qos: QoS,
    timeout: Duration,
    properties: Properties,
    correlation_data: Option<Vec<u8>>,
}

impl RequestOptions {
    /// Create request options for an exact response topic.
    ///
    /// The connection driver subscribes to this topic once and reuses that
    /// subscription for subsequent requests. Wildcard response topics are not
    /// valid because MQTT requires Response Topic to name a concrete topic.
    pub fn new(response_topic: impl Into<String>) -> Self {
        Self {
            response_topic: response_topic.into(),
            qos: QoS::AtLeastOnce,
            timeout: Duration::from_secs(30),
            properties: Properties::new(),
            correlation_data: None,
        }
    }

    pub fn with_qos(mut self, qos: QoS) -> Self {
        self.qos = qos;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Add application MQTT properties to the outgoing request.
    ///
    /// Any Response Topic or Correlation Data value in `properties` is replaced
    /// by the values owned by this request helper.
    pub fn with_properties(mut self, properties: Properties) -> Self {
        self.properties = properties;
        self
    }

    /// Supply opaque correlation bytes instead of generating a UUID.
    pub fn with_correlation_data(mut self, correlation_data: impl Into<Vec<u8>>) -> Self {
        self.correlation_data = Some(correlation_data.into());
        self
    }

    pub fn response_topic(&self) -> &str {
        &self.response_topic
    }

    pub fn qos(&self) -> QoS {
        self.qos
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    fn validate(&self) -> Result<()> {
        if self.response_topic.is_empty() {
            return Err(Error::InvalidOptions("response topic must not be empty"));
        }
        if self.response_topic.contains(['#', '+']) {
            return Err(Error::InvalidOptions(
                "response topic must not contain wildcards",
            ));
        }
        if self.response_topic.starts_with("$share/") {
            return Err(Error::InvalidOptions(
                "response topic must not use the shared-subscription prefix",
            ));
        }
        if self.timeout.is_zero() {
            return Err(Error::InvalidOptions("request timeout must be non-zero"));
        }
        if Instant::now().checked_add(self.timeout).is_none() {
            return Err(Error::InvalidOptions("request timeout is too large"));
        }
        Ok(())
    }
}

impl AsyncMqttClient {
    /// Publish a byte request and await the response carrying matching MQTT v5
    /// Correlation Data on the configured Response Topic.
    pub async fn request(
        &self,
        topic: impl Into<String>,
        payload: impl Into<Vec<u8>>,
        options: RequestOptions,
    ) -> Result<PublishPacket> {
        options.validate()?;

        let topic = topic.into();
        let payload = payload.into();
        let correlation_data = options
            .correlation_data
            .unwrap_or_else(|| uuid::Uuid::new_v4().as_bytes().to_vec());
        let mut properties = options.properties;
        properties.set_response_topic(options.response_topic.clone());
        properties.set_correlation_data(correlation_data.clone());

        let command = RequestCommand {
            topic,
            payload,
            response_topic: options.response_topic,
            correlation_data: correlation_data.clone(),
            qos: options.qos,
            timeout: options.timeout,
            properties,
        };
        let response = self.start_request(command)?;
        let mut cancellation = RequestCancellation::new(self.request_lifecycle(), correlation_data);
        let result = response.await.unwrap_or(Err(Error::ClientClosed));
        cancellation.finish();
        result
    }
}

pub(crate) struct RequestCommand {
    pub(crate) topic: String,
    pub(crate) payload: Vec<u8>,
    pub(crate) response_topic: String,
    pub(crate) correlation_data: Vec<u8>,
    pub(crate) qos: QoS,
    pub(crate) timeout: Duration,
    pub(crate) properties: Properties,
}

struct RequestCancellation {
    lifecycle: Arc<Mutex<RequestLifecycle>>,
    correlation_data: Option<Vec<u8>>,
}

impl RequestCancellation {
    fn new(lifecycle: Arc<Mutex<RequestLifecycle>>, correlation_data: Vec<u8>) -> Self {
        Self {
            lifecycle,
            correlation_data: Some(correlation_data),
        }
    }

    fn finish(&mut self) {
        if let Some(correlation_data) = self.correlation_data.take() {
            self.lifecycle
                .lock()
                .expect("request lifecycle mutex poisoned")
                .finish(&correlation_data);
        }
    }
}

impl Drop for RequestCancellation {
    fn drop(&mut self) {
        if let Some(correlation_data) = self.correlation_data.take() {
            self.lifecycle
                .lock()
                .expect("request lifecycle mutex poisoned")
                .cancel(correlation_data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_response_topic_and_timeout() {
        assert!(RequestOptions::new("responses/client-1").validate().is_ok());
        assert!(RequestOptions::new("responses/+").validate().is_err());
        assert!(RequestOptions::new("$share/workers/responses/client-1")
            .validate()
            .is_err());
        assert!(RequestOptions::new("").validate().is_err());
        assert!(RequestOptions::new("responses/client-1")
            .with_timeout(Duration::ZERO)
            .validate()
            .is_err());
        assert!(RequestOptions::new("responses/client-1")
            .with_timeout(Duration::MAX)
            .validate()
            .is_err());
    }

    #[test]
    fn caller_can_supply_opaque_correlation_data() {
        let options = RequestOptions::new("responses/client-1").with_correlation_data([0, 1, 0xff]);
        assert_eq!(options.correlation_data, Some(vec![0, 1, 0xff]));
    }
}
