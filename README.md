# mqtt-wasi

Minimal MQTT v5.0 client for Rust that compiles to `wasm32-wasip2`. No host dependencies beyond TCP sockets.

**Runtime:** [wasmtime](https://wasmtime.dev/) only. Requires `wasi:sockets/tcp` support, which WasmEdge does not yet implement for wasip2.

## Why

No existing Rust MQTT client compiles cleanly to WebAssembly. rumqttc and paho-mqtt pull in tokio's multi-threaded runtime or native TLS, but neither work in Wasm. This crate implements the MQTT wire protocol from scratch against `std::net::TcpStream`, which WASI maps automatically via `wasi:sockets/tcp`.

The same binary runs native and on wasmtime.

## Usage

### Sync client

```rust
use mqtt_wasi::{MqttClient, ConnectOptions};
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug)]
struct SensorReading {
    celsius: f64,
}

fn main() {
    let mut client = MqttClient::connect(
        "broker:1883",
        ConnectOptions::new("my-device"),
    ).unwrap();

    client.publish("sensors/temp", &SensorReading { celsius: 22.5 }).unwrap();

    for msg in client.subscribe::<SensorReading>("sensors/#").unwrap() {
        let msg = msg.unwrap();
        println!("{}: {:?}", msg.topic, msg.payload);
    }
}
```

### Async request/reply

Concurrent requests over one connection via cooperative non-blocking I/O. Works with `tokio::main(flavor = "current_thread")` on wasip2.

```rust
use mqtt_wasi::{AsyncMqttClient, ConnectOptions};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let client = AsyncMqttClient::connect("broker:1883", opts).await.unwrap();

    // These run concurrently — total time ≈ max(call1, call2), not sum
    let (r1, r2) = tokio::join!(
        client.request("egress/inference/openai/gpt-5.4-mini", &chat_req),
        client.request("egress/serpapi", &search_req),
    );

    client.disconnect().unwrap();
}
```

Each `request()` subscribes to a reply topic with a correlation ID, publishes the request, and waits for the correlated reply. Multiple requests multiplex over one non-blocking socket.

### TLS

Optional, feature-gated behind `tls`. The default is plaintext TCP on port 1883 — typical for VPC deployments where the broker terminates TLS at the edge. Enable `tls` when connecting directly to a broker over the public internet (port 8883).

Uses [rustls](https://github.com/rustls/rustls) with [rustls-rustcrypto](https://github.com/RustCrypto/rustls-rustcrypto) (pure Rust crypto, no C dependencies) so TLS compiles to `wasm32-wasip2`. Mozilla root certificates included via [webpki-roots](https://github.com/rustls/webpki-roots).

```toml
[dependencies]
mqtt-wasi = { version = "0.1", features = ["tls"] }
```

```rust
use mqtt_wasi::{TlsTransport, MqttClient, AsyncMqttClient, ConnectOptions};

let tls = TlsTransport::connect("broker.example.com:8883").unwrap();
let client = MqttClient::connect_with(tls, opts).unwrap();

// Also works with the async client
let tls = TlsTransport::connect("broker.example.com:8883").unwrap();
let client = AsyncMqttClient::connect_with(tls, opts).await.unwrap();
```

### Trace Context

W3C Trace Context propagation via MQTT v5 User Properties. No OpenTelemetry SDK required.

```rust
use mqtt_wasi::TraceContext;

let trace = TraceContext::new_root(trace_id_bytes, span_id_bytes);
client.publish_traced("sensors/temp", &reading, &trace).unwrap();

// On the receiving side
let msg = client.recv_raw().unwrap().unwrap();
if let Some(trace) = TraceContext::from_properties(&msg.properties) {
    println!("trace: {trace}");
}
```

## Building for WASI

```bash
cargo build --target wasm32-wasip2 --release
```

Binary sizes (wasm32-wasip2, with serde_json, LTO, `opt-level = "z"`, `panic = "abort"`):

| Example | Size |
|---------|------|
| `pubsub` (sync client) | ~220 KB |
| `request_reply` (async client + tokio) | ~270 KB |
| `tls_pubsub` (sync client + TLS) | ~1.1 MB |

Run with wasmtime:

```bash
wasmtime run -S inherit-network,allow-ip-name-lookup your_app.wasm
```

## Design

- **Protocol layer** (`codec/`) is `no_std` compatible (alloc only). No `bytes` crate, no derive macros, no `hashbrown`. Encodes to `Vec<u8>`, decodes from `&[u8]` via a lightweight `Cursor`.
- **Sync client** (`client.rs`). Blocking `TcpStream` with read timeouts for keep-alive.
- **Async client** (`async_client.rs`). Cooperative non-blocking I/O over one socket. Each `request()` Future pumps the shared socket when polled, dispatching packets by correlation ID. Uses `Rc<RefCell<...>>` (single-threaded, `!Send`). Works with `tokio::join!` but not `tokio::spawn` (use `spawn_local`).
- **Frame reader** (`frame.rs`). Incremental MQTT frame parser for partial non-blocking reads.
- **TLS** (`tls.rs`, feature-gated). `TlsTransport` wraps rustls `StreamOwned` and implements `Transport`. Uses [`rustls-rustcrypto`](https://github.com/RustCrypto/rustls-rustcrypto) (pure Rust, no C dependencies) so TLS compiles to Wasm. The underlying RustCrypto crates are mature; the rustls glue layer is alpha but covers all standard TLS 1.2/1.3 cipher suites.
- **Properties** stored as `Vec<(PropertyId, PropertyValue)>`. linear scan beats hashing for the typical 0-5 items per packet. Unknown property IDs are skipped rather than erroring.
- **Trace context** is pure string formatting per W3C Trace Context Level 1.

### Features

| Feature | Default | Description |
|---------|---------|-------------|
| `std` | yes | Sync/async clients, transport layer |
| `tls` | no | TLS via rustls + rustls-rustcrypto (pure Rust, Wasm-compatible) |

### What's supported

- CONNECT / CONNACK, PUBLISH / PUBACK (QoS 0 and 1), SUBSCRIBE / SUBACK, UNSUBSCRIBE / UNSUBACK, PINGREQ / PINGRESP, DISCONNECT
- MQTT v5 properties (subset, unknown IDs skipped)
- Async request/reply with correlation IDs and concurrent multiplexing
- W3C Trace Context propagation
- TLS (feature-gated)
- AMQP-compatible reply routing for RabbitMQ MQTT plugin

### Not yet

QoS 2, AUTH packet, will messages, topic aliases, session resumption, auto-reconnect.

## Environment

Tests that require external brokers read credentials from environment variables. Copy `.env.example` or set:

```bash
CLOUDAMQP_URL=amqps://user:pass@host/vhost   # CloudAMQP egress tests
HIVEMQ_ADDR=host:8883                          # TLS integration tests
HIVEMQ_USER=...
HIVEMQ_PASS=...
```

Tests skip gracefully when env vars are not set.

## License

MIT OR Apache-2.0
