//! Bytes-first publish/subscribe over the experimental TLS transport.
//!
//! Native:
//!   MQTT_TLS_ADDR=broker.example.com:8883 MQTT_TLS_USER=user \
//!     MQTT_TLS_PASS=pass cargo run
//!
//! WASI:
//!   cargo build --target wasm32-wasip2 --release
//!   MQTT_TLS_ADDR=broker.example.com:8883 MQTT_TLS_USER=user MQTT_TLS_PASS=pass \
//!     wasmtime run -S inherit-network,allow-ip-name-lookup,inherit-env \
//!     target/wasm32-wasip2/release/tls-pubsub.wasm

use mqtt_wasi::{ConnectOptions, MqttClient, PublishOptions, QoS, TlsTransport};

fn main() {
    let addr = std::env::var("MQTT_TLS_ADDR").expect("MQTT_TLS_ADDR required (host:port)");
    let user = std::env::var("MQTT_TLS_USER").expect("MQTT_TLS_USER required");
    let pass = std::env::var("MQTT_TLS_PASS").expect("MQTT_TLS_PASS required");
    let topic = "mqtt-wasi/example/tls-sensors";

    let options = |client_id: &str| {
        ConnectOptions::new(client_id)
            .with_keep_alive(30)
            .with_credentials(&user, pass.as_bytes())
    };

    let subscriber_transport = TlsTransport::connect(&addr).expect("subscriber TLS connect");
    let mut subscriber =
        MqttClient::connect_with(subscriber_transport, options("tls-example-subscriber"))
            .expect("subscriber MQTT connect");
    subscriber
        .subscribe(topic, QoS::AtLeastOnce)
        .expect("subscribe");

    let publisher_transport = TlsTransport::connect(&addr).expect("publisher TLS connect");
    let mut publisher =
        MqttClient::connect_with(publisher_transport, options("tls-example-publisher"))
            .expect("publisher MQTT connect");
    publisher
        .publish(
            topic,
            br#"{"device_id":"sensor-1","celsius":22.5}"#,
            PublishOptions::default().with_qos(QoS::AtLeastOnce),
        )
        .expect("publish");
    publisher.disconnect().expect("publisher disconnect");

    let message = subscriber.recv().expect("receive").expect("broker closed");
    println!(
        "{}: {}",
        message.topic,
        String::from_utf8_lossy(&message.payload)
    );
    subscriber.disconnect().expect("subscriber disconnect");
}
