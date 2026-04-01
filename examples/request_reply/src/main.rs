//! Async request/reply — 3 concurrent requests over one connection.
//!
//! Sends requests to 3 topics and awaits replies via tokio::join!,
//! all multiplexed over a single socket. Compiles to wasm32-wasip2.
//!
//! Requires consumers on the broker (run the companion consumer first):
//!   cargo run --bin consumer    # native — spawns 3 echo consumers
//!
//! Then run this example:
//!   Native:
//!     MQTT_ADDR=... cargo run --bin request-reply
//!
//!   WASM:
//!     cargo build --target wasm32-wasip2 --release --bin request-reply
//!     MQTT_ADDR=... wasmtime run -S inherit-network,allow-ip-name-lookup,inherit-env \
//!       target/wasm32-wasip2/release/request-reply.wasm

use mqtt_wasi::{AsyncMqttClient, ConnectOptions};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let addr = std::env::var("MQTT_ADDR").unwrap_or_else(|_| "127.0.0.1:1883".into());

    let mut opts = ConnectOptions::new("rpc-requester").with_keep_alive(30);
    if let (Ok(u), Ok(p)) = (std::env::var("MQTT_USER"), std::env::var("MQTT_PASS")) {
        opts = opts.with_credentials(u, p.as_bytes());
    }

    let client = AsyncMqttClient::connect(&addr, opts)
        .await
        .expect("connect");

    println!("connected to {addr}");
    let start = std::time::Instant::now();

    let (r1, r2, r3) = tokio::join!(
        client.request("mqtt-wasi/rpc/double", &serde_json::json!({"value": 21})),
        client.request("mqtt-wasi/rpc/greet", &serde_json::json!({"name": "world"})),
        client.request("mqtt-wasi/rpc/reverse", &serde_json::json!({"text": "hello"})),
    );

    let elapsed = start.elapsed();

    println!("double:  {}", r1.expect("double failed"));
    println!("greet:   {}", r2.expect("greet failed"));
    println!("reverse: {}", r3.expect("reverse failed"));
    println!("\n3 concurrent requests in {elapsed:.2?}");

    client.disconnect().ok();
}
