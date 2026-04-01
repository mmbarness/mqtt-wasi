#[cfg(feature = "tls")]
mod tls_tests {
    use mqtt_wasi::{AsyncMqttClient, ConnectOptions, MqttClient, TlsTransport};
    use serde_json::json;

    fn hivemq_config() -> Option<(String, String, String)> {
        dotenvy::dotenv().ok();
        let addr = std::env::var("HIVEMQ_ADDR").ok()?;
        let user = std::env::var("HIVEMQ_USER").ok()?;
        let pass = std::env::var("HIVEMQ_PASS").ok()?;
        Some((addr, user, pass))
    }

    fn skip() {
        eprintln!("HIVEMQ_ADDR/HIVEMQ_USER/HIVEMQ_PASS not set — skipping");
    }

    #[test]
    fn tls_connect_and_disconnect() {
        let (addr, user, pass) = match hivemq_config() {
            Some(c) => c,
            None => return skip(),
        };
        let tls = TlsTransport::connect(&addr).unwrap();
        let client = MqttClient::connect_with(
            tls,
            ConnectOptions::new("mqtt-wasi-tls-test")
                .with_credentials(&user, pass.as_bytes().to_vec()),
        )
        .unwrap();
        client.disconnect().unwrap();
    }

    #[test]
    fn tls_publish_and_subscribe() {
        let (addr, user, pass) = match hivemq_config() {
            Some(c) => c,
            None => return skip(),
        };

        let tls_sub = TlsTransport::connect(&addr).unwrap();
        let mut sub = MqttClient::connect_with(
            tls_sub,
            ConnectOptions::new("mqtt-wasi-tls-sub")
                .with_credentials(&user, pass.as_bytes().to_vec())
                .with_keep_alive(10),
        )
        .unwrap();
        sub.subscribe_raw("mqtt-wasi/tls-test", mqtt_wasi::QoS::AtMostOnce)
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(200));

        let tls_pub = TlsTransport::connect(&addr).unwrap();
        let mut publ = MqttClient::connect_with(
            tls_pub,
            ConnectOptions::new("mqtt-wasi-tls-pub")
                .with_credentials(&user, pass.as_bytes().to_vec()),
        )
        .unwrap();
        publ.publish("mqtt-wasi/tls-test", &json!({"tls": true}))
            .unwrap();
        publ.disconnect().unwrap();

        let msg = sub.recv_raw().unwrap().unwrap();
        assert_eq!(msg.topic, "mqtt-wasi/tls-test");
        let payload: serde_json::Value = serde_json::from_slice(&msg.payload).unwrap();
        assert_eq!(payload["tls"], true);

        sub.disconnect().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tls_async_request_reply() {
        let (addr, user, pass) = match hivemq_config() {
            Some(c) => c,
            None => return skip(),
        };

        // Mock responder over TLS (sync, background thread)
        let addr2 = addr.clone();
        let user2 = user.clone();
        let pass2 = pass.clone();
        let responder = std::thread::spawn(move || {
            let tls = TlsTransport::connect(&addr2).unwrap();
            let mut client = MqttClient::connect_with(
                tls,
                ConnectOptions::new("mqtt-wasi-tls-responder")
                    .with_credentials(&user2, pass2.as_bytes().to_vec())
                    .with_keep_alive(10),
            )
            .unwrap();

            client
                .subscribe_raw("mqtt-wasi/tls-async-test", mqtt_wasi::QoS::AtMostOnce)
                .unwrap();

            let msg = client.recv_raw().unwrap().unwrap();
            let envelope: serde_json::Value =
                serde_json::from_slice(&msg.payload).unwrap();
            let reply_to = envelope["replyTo"].as_str().unwrap().to_string();
            let correlation_id = envelope["correlationId"].as_str().unwrap().to_string();

            let reply = json!({
                "correlationId": correlation_id,
                "result": { "ok": true, "data": { "tls_async": true } }
            });
            let reply_bytes = serde_json::to_vec(&reply).unwrap();
            client
                .publish_raw(
                    &reply_to,
                    &reply_bytes,
                    mqtt_wasi::QoS::AtMostOnce,
                    false,
                    Default::default(),
                )
                .unwrap();

            client.disconnect().unwrap();
        });

        std::thread::sleep(std::time::Duration::from_millis(300));

        // Async client over TLS
        let tls = TlsTransport::connect(&addr).unwrap();
        let client = AsyncMqttClient::connect_with(
            tls,
            ConnectOptions::new("mqtt-wasi-tls-async-req")
                .with_credentials(&user, pass.as_bytes().to_vec())
                .with_keep_alive(10),
        )
        .await
        .unwrap();

        let result = client
            .request("mqtt-wasi/tls-async-test", &json!({"prompt": "hello tls"}))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(result["data"]["tls_async"], true);

        client.disconnect().unwrap();
        responder.join().unwrap();
    }
}
