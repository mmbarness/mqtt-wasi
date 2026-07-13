//! Standards-based MQTT v5 request/response over one async connection.
//!
//! Run the companion consumer first:
//!   MQTT_ADDR=127.0.0.1:1883 cargo run --bin consumer
//!
//! Then run this requester natively:
//!   MQTT_ADDR=127.0.0.1:1883 cargo run --bin request-reply
//!
//! Or with Wasmtime:
//!   cargo build --target wasm32-wasip2 --release --bin request-reply
//!   MQTT_ADDR=127.0.0.1:1883 wasmtime run \
//!     -S inherit-network,allow-ip-name-lookup,inherit-env \
//!     target/wasm32-wasip2/release/request-reply.wasm

use mqtt_wasi::{AsyncMqttClient, ConnectOptions, RequestOptions};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let addr = std::env::var("MQTT_ADDR").unwrap_or_else(|_| "127.0.0.1:1883".into());
    let client_id = std::env::var("MQTT_CLIENT_ID").unwrap_or_else(|_| "example-requester".into());
    // Use one stable, ACL-scoped response topic per client. The v0.2 driver
    // caches response subscriptions and intentionally does not evict them.
    let response_topic = format!("mqtt-wasi/example/responses/{client_id}");

    let mut options = ConnectOptions::new(&client_id).with_keep_alive(30);
    if let (Ok(user), Ok(pass)) = (std::env::var("MQTT_USER"), std::env::var("MQTT_PASS")) {
        options = options.with_credentials(user, pass.as_bytes());
    }

    // DNS/TCP/MQTT handshake is intentionally synchronous. Only operations
    // after this point are async.
    let (client, connection) = AsyncMqttClient::connect(&addr, options).expect("connect");
    let driver = tokio::spawn(connection.run());
    println!("connected to {addr}");

    let (double, greet, reverse) = tokio::join!(
        client.request(
            "mqtt-wasi/example/double",
            b"21",
            RequestOptions::new(&response_topic),
        ),
        client.request(
            "mqtt-wasi/example/greet",
            b"world",
            RequestOptions::new(&response_topic),
        ),
        client.request(
            "mqtt-wasi/example/reverse",
            b"hello",
            RequestOptions::new(&response_topic),
        ),
    );

    println!("double:  {}", text(double.expect("double request")));
    println!("greet:   {}", text(greet.expect("greet request")));
    println!("reverse: {}", text(reverse.expect("reverse request")));

    client.disconnect().await.expect("disconnect");
    driver
        .await
        .expect("driver task panicked")
        .expect("driver failed");
}

fn text(message: mqtt_wasi::PublishPacket) -> String {
    String::from_utf8(message.payload).expect("consumer returned non-UTF-8 bytes")
}
