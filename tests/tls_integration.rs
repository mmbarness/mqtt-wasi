#[cfg(feature = "tls")]
mod tls_tests {
    #[cfg(feature = "async-client")]
    use mqtt_wasi::AsyncMqttClient;
    use mqtt_wasi::{ConnectOptions, MqttClient, PublishOptions, QoS, TlsTransport};
    use std::time::Duration;

    struct TlsBroker {
        addr: String,
        user: String,
        pass: String,
    }

    fn broker() -> Option<TlsBroker> {
        dotenvy::dotenv().ok();
        Some(TlsBroker {
            addr: std::env::var("MQTT_TLS_ADDR").ok()?,
            user: std::env::var("MQTT_TLS_USER").ok()?,
            pass: std::env::var("MQTT_TLS_PASS").ok()?,
        })
    }

    fn skip() {
        eprintln!("MQTT_TLS_ADDR/MQTT_TLS_USER/MQTT_TLS_PASS not set; skipping");
    }

    fn unique(prefix: &str) -> String {
        format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
    }

    fn options(config: &TlsBroker, prefix: &str) -> ConnectOptions {
        ConnectOptions::new(unique(prefix))
            .with_credentials(&config.user, config.pass.as_bytes())
            .with_keep_alive(10)
            .with_connect_timeout(Duration::from_secs(10))
            .with_ack_timeout(Duration::from_secs(5))
    }

    #[test]
    fn tls_connect_and_disconnect() {
        let Some(config) = broker() else {
            return skip();
        };
        let tls =
            TlsTransport::connect_with_timeout(&config.addr, Duration::from_secs(10)).unwrap();
        let client = MqttClient::connect_with(tls, options(&config, "tls-connect")).unwrap();
        client.disconnect().unwrap();
    }

    #[test]
    fn tls_publish_and_subscribe_raw_bytes() {
        let Some(config) = broker() else {
            return skip();
        };
        let topic = format!("mqtt-wasi/test/tls/{}", unique("topic"));

        let subscriber_transport = TlsTransport::connect(&config.addr).unwrap();
        let mut subscriber =
            MqttClient::connect_with(subscriber_transport, options(&config, "tls-subscriber"))
                .unwrap();
        subscriber.subscribe(&topic, QoS::AtMostOnce).unwrap();

        let publisher_transport = TlsTransport::connect(&config.addr).unwrap();
        let mut publisher =
            MqttClient::connect_with(publisher_transport, options(&config, "tls-publisher"))
                .unwrap();
        publisher
            .publish(&topic, b"tls bytes", PublishOptions::default())
            .unwrap();
        publisher.disconnect().unwrap();

        let message = subscriber.recv().unwrap().expect("expected a PUBLISH");
        assert_eq!(message.topic, topic);
        assert_eq!(message.payload, b"tls bytes");
        subscriber.disconnect().unwrap();
    }

    #[cfg(feature = "async-client")]
    #[tokio::test(flavor = "current_thread")]
    async fn tls_async_client_uses_explicit_driver() {
        let Some(config) = broker() else {
            return skip();
        };
        let transport = TlsTransport::connect(&config.addr).unwrap();
        let (client, connection) =
            AsyncMqttClient::connect_with(transport, options(&config, "tls-async")).unwrap();
        let driver = tokio::spawn(connection.run());

        client.disconnect().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), driver)
            .await
            .expect("TLS driver did not stop")
            .expect("TLS driver task panicked")
            .expect("TLS driver failed");
    }
}
