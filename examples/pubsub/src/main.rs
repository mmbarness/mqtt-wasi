//! Publish/subscribe — compiles to both native and wasm32-wasip2.
//!
//! Native:
//!   MQTT_ADDR=127.0.0.1:1883 cargo run
//!
//! WASM:
//!   cargo build --target wasm32-wasip2 --release
//!   MQTT_ADDR=127.0.0.1:1883 wasmtime run -S inherit-network,allow-ip-name-lookup,inherit-env \
//!     target/wasm32-wasip2/release/pubsub.wasm

use mqtt_wasi::{ConnectOptions, MqttClient, QoS};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct SensorReading {
    device_id: String,
    celsius: f64,
}

fn main() {
    let addr = std::env::var("MQTT_ADDR").unwrap_or_else(|_| "127.0.0.1:1883".into());
    let user = std::env::var("MQTT_USER").ok();
    let pass = std::env::var("MQTT_PASS").ok();
    let topic = "mqtt-wasi/example/sensors";

    let opts = |id: &str| {
        let mut o = ConnectOptions::new(id).with_keep_alive(30);
        if let (Some(u), Some(p)) = (&user, &pass) {
            o = o.with_credentials(u.as_str(), p.as_bytes());
        }
        o
    };

    // Subscriber
    let mut sub = MqttClient::connect(&addr, opts("example-sub")).expect("sub connect");
    sub.subscribe_raw(topic, QoS::AtMostOnce).expect("subscribe");
    println!("[sub] subscribed to {topic}");

    // Publisher (separate connection)
    let mut publ = MqttClient::connect(&addr, opts("example-pub")).expect("pub connect");
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
