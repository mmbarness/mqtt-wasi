# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test

```bash
cargo build                                    # native build
cargo build --features tls                     # with TLS support
cargo build --target wasm32-wasip2 --release   # wasm build
cargo test                                     # unit + integration tests (needs Mosquitto on localhost:1883)
cargo test --features tls                      # includes TLS tests (needs HIVEMQ_* env vars)
cargo test --test async_integration            # async request/reply tests only
cargo test --test egress_cloudamqp             # CloudAMQP live tests (needs CLOUDAMQP_URL)
```

Local Mosquitto for basic tests:
```bash
docker run -d --name mosquitto -p 1883:1883 eclipse-mosquitto:2 mosquitto -c /mosquitto-no-auth.conf
```

External broker tests read from `.env` (gitignored) or environment variables. Tests skip gracefully when env vars are missing.

## Architecture

Three layers:

- **`codec/`** — `no_std` (alloc only). Pure MQTT v5.0 wire protocol encode/decode. No I/O. Operates on `Vec<u8>` (encode) and `&[u8]` via a lightweight `Cursor` (decode). Unknown property IDs are skipped, not errored.
- **Sync client** (`client.rs`) — `MqttClient<T: Transport>`, blocking I/O, typed serde pub/sub.
- **Async client** (`async_client.rs` + `request.rs` + `frame.rs`) — `AsyncMqttClient<T: Transport>`, cooperative non-blocking I/O for concurrent request/reply via `tokio::join!`.

### Async client design

The async client uses cooperative polling over one non-blocking TCP socket. There is no I/O reactor on wasip2, so each `RequestFuture` self-wakes (`cx.waker().wake_by_ref()`) and pumps the shared socket when polled. Packets are dispatched by correlation ID to the correct pending request.

Shared state is `Rc<RefCell<SharedInner>>` — single-threaded only. Futures are `!Send`, so `tokio::join!` works but `tokio::spawn` does not (use `spawn_local`).

The CONNECT/CONNACK handshake is done in blocking mode. After CONNACK, the socket switches to non-blocking. Writes temporarily flip back to blocking to avoid `WouldBlock` on `write_all`.

### Transport trait

`transport.rs` defines `Transport` trait. `MqttClient` and `AsyncMqttClient` are generic over `T: Transport`, defaulting to `std::net::TcpStream`. `connect()` uses the default; `connect_with()` accepts a caller-provided transport (e.g. `TlsTransport`).

### TLS

`tls.rs` (feature-gated behind `tls`) provides `TlsTransport` wrapping `rustls::StreamOwned<ClientConnection, TcpStream>`. Uses `rustls-rustcrypto` (pure Rust, no C/asm) as the crypto provider — this is the only rustls provider that compiles to `wasm32-wasip2`. The glue crate is alpha (`0.0.2-alpha`) but the underlying RustCrypto primitives are mature. Mozilla root certificates via `webpki-roots`. Works with both sync and async clients via `connect_with()`. `connect_with_config()` accepts a custom `Arc<ClientConfig>` for advanced use.

### AMQP reply format

`ConnectOptions::amqp_reply_format` controls whether `replyTo` in request envelopes uses dots (AMQP routing keys) or slashes (MQTT topics). Enable for RabbitMQ MQTT plugin where the reply publisher is an AMQP consumer.

### Key constraint

Target runtime is **wasmtime** only. WasmEdge does not implement `wasi:sockets/tcp` for wasip2 yet. Non-blocking I/O (`set_nonblocking`) is verified working on wasmtime.
