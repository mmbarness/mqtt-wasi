//! Publish/subscribe over TLS — compiles to both native and wasm32-wasip2.
//!
//! Native:
//!   MQTT_ADDR=broker.example.com:8883 MQTT_USER=user MQTT_PASS=pass cargo run
//!
//! WASM:
//!   cargo build --target wasm32-wasip2 --release
//!   MQTT_ADDR=broker.example.com:8883 MQTT_USER=user MQTT_PASS=pass \
//!     wasmtime run -S inherit-network,allow-ip-name-lookup,inherit-env \
//!     target/wasm32-wasip2/release/tls-pubsub.wasm

use mqtt_wasi::{ConnectOptions, MqttClient, QoS, TlsTransport};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct SensorReading {
    device_id: String,
    celsius: f64,
}

fn main() {
    let addr = std::env::var("MQTT_ADDR").expect("MQTT_ADDR required (host:8883)");
    let user = std::env::var("MQTT_USER").expect("MQTT_USER required");
    let pass = std::env::var("MQTT_PASS").expect("MQTT_PASS required");
    let topic = "mqtt-wasi/example/tls-sensors";

    let opts = |id: &str| {
        ConnectOptions::new(id)
            .with_keep_alive(30)
            .with_credentials(&user, pass.as_bytes())
    };

    // Subscriber over TLS
    let tls_sub = TlsTransport::connect(&addr).expect("TLS connect (sub)");
    let mut sub = MqttClient::connect_with(tls_sub, opts("tls-example-sub")).expect("MQTT connect");
    sub.subscribe_raw(topic, QoS::AtMostOnce).expect("subscribe");
    println!("[sub] subscribed to {topic} over TLS");

    // Publisher over TLS (separate connection)
    let tls_pub = TlsTransport::connect(&addr).expect("TLS connect (pub)");
    let mut publ = MqttClient::connect_with(tls_pub, opts("tls-example-pub")).expect("MQTT connect");
    let reading = SensorReading {
        device_id: "sensor-1".into(),
        celsius: 22.5,
    };
    publ.publish(topic, &reading).expect("publish");
    println!("[pub] sent: {reading:?}");
    publ.disconnect().ok();

    // Receive
    let msg = sub.recv_raw().expect("recv").expect("no message");
    let payload: SensorReading = serde_json::from_slice(&msg.payload).expect("deserialize");
    println!("[sub] received: {payload:?}");
    sub.disconnect().ok();
}
