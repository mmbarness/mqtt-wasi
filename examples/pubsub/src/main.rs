//! Bytes-first publish/subscribe for native Rust and `wasm32-wasip2`.
//!
//! Native:
//!   MQTT_ADDR=127.0.0.1:1883 cargo run
//!
//! WASI:
//!   cargo build --target wasm32-wasip2 --release
//!   MQTT_ADDR=127.0.0.1:1883 wasmtime run \
//!     -S inherit-network,allow-ip-name-lookup,inherit-env \
//!     target/wasm32-wasip2/release/pubsub.wasm

use mqtt_wasi::{ConnectOptions, MqttClient, PublishOptions, QoS};

fn main() {
    let addr = std::env::var("MQTT_ADDR").unwrap_or_else(|_| "127.0.0.1:1883".into());
    let user = std::env::var("MQTT_USER").ok();
    let pass = std::env::var("MQTT_PASS").ok();
    let topic = "mqtt-wasi/example/sensors";

    let options = |client_id: &str| {
        let mut options = ConnectOptions::new(client_id).with_keep_alive(30);
        if let (Some(user), Some(pass)) = (&user, &pass) {
            options = options.with_credentials(user.as_str(), pass.as_bytes());
        }
        options
    };

    let mut subscriber =
        MqttClient::connect(&addr, options("example-subscriber")).expect("subscriber connect");
    subscriber
        .subscribe(topic, QoS::AtLeastOnce)
        .expect("subscribe");
    println!("subscriber ready on {topic}");

    let mut publisher =
        MqttClient::connect(&addr, options("example-publisher")).expect("publisher connect");
    let payload = br#"{"device_id":"sensor-1","celsius":22.5}"#;
    publisher
        .publish(
            topic,
            payload,
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
