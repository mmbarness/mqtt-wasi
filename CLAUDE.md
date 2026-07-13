# Repository guidance

`mqtt-wasi` is a public, bytes-first MQTT v5 client for native Rust and
`wasm32-wasip2`. Treat the `0.2` API as general-purpose infrastructure: do not
add application envelopes, fixed topic prefixes, broker bridge rewrites, or a
required serialization format.

## Build and test

The minimum supported Rust version is 1.94.

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features --all-targets
cargo check --locked --lib --no-default-features --target wasm32-unknown-unknown
cargo build --locked --lib --target wasm32-wasip2
cargo build --locked --lib --target wasm32-wasip2 --features tls
RUSTDOCFLAGS="-D warnings" cargo doc --locked --all-features --no-deps
cargo package --locked
```

The standalone examples are separate packages, not a Cargo workspace:

```bash
for manifest in examples/*/Cargo.toml; do
  cargo build --manifest-path "$manifest"
  cargo build --manifest-path "$manifest" --target wasm32-wasip2
done
```

Local integration tests expect Mosquitto at `127.0.0.1:1883`. They skip when it
is absent unless `MQTT_TEST_REQUIRED=1`; CI always sets that flag so a broken
handshake cannot masquerade as a skip. Optional external TLS tests use only
`MQTT_TLS_ADDR`, `MQTT_TLS_USER`, and `MQTT_TLS_PASS` and are not part of CI.

## Architecture

- `codec/` is the `no_std` plus `alloc` protocol core. It owns packet types,
  properties, and deterministic encoding/decoding.
- `frame.rs` incrementally assembles bounded MQTT frames and preserves partial
  reads.
- `transport.rs` defines ordered read/write behavior. TCP is the default; TLS
  is optional.
- `client.rs` is the blocking `MqttClient<T: Transport>`. ACK waits dispatch
  unrelated inbound PUBLISH packets into a bounded FIFO.
- `async_client.rs` contains the cloneable `AsyncMqttClient` handle and the
  sole-owner `MqttConnection<T>` driver.
- `request_response.rs` is an optional standard MQTT v5 helper based on
  Response Topic and Correlation Data.
- `trace.rs` maps W3C Trace Context version 00 to MQTT User Properties.

`PublishPacket` is the single received-message type across blocking `recv`, the
blocking iterator, async `Event::Publish`, and request/response results. Raw
bytes are the primary payload API. The optional `serde` feature implies `std`
and adds only convenience publishing.

## Async invariants

`AsyncMqttClient::connect` and `connect_with` are intentionally synchronous:
DNS, TCP/TLS, and MQTT handshake work completes before they return
`(AsyncMqttClient, MqttConnection<T>)`. All subsequent client operations are
async, and `MqttConnection::run` must be supervised continuously.

Only the driver touches the transport, advances keepalive, assigns packet IDs,
matches ACKs, and dispatches events. Never add socket I/O or blocking sleeps to
an operation future. Queues and queued bytes stay bounded. A full event queue
terminates the driver with `Error::QueueFull("event")`; silently dropping an
inbound PUBLISH is not acceptable.

Request/response callers should use one stable, ACL-scoped response topic per
client. The driver caches those subscriptions and limits distinct topics by the
pending-operation bound. Version 0.2 intentionally has no automatic eviction
or unsubscribe, so per-request response topics eventually backpressure.

## Reliability and security constraints

- The crate root forbids unsafe code.
- Check peer-declared packet length before allocating the complete frame.
- Preserve partial frames across timeout and WouldBlock boundaries.
- Enforce connect, ACK, request, packet, queue-count, and queued-byte limits.
- Do not reuse an in-flight packet identifier or leak a waiter after
  cancellation.
- Do not log credentials or payloads by default.
- Correlation Data is not an authorization credential; broker ACLs still apply.
- Trace and span identifiers must be non-zero. `TraceContext::new_root` and
  `child` therefore return `Option`.

## Feature contract and scope

Default features are `std` and `async-client`. `request-response`, `serde`, and
`tls` are opt-in. `--no-default-features` is the genuine `no_std` codec/types
contract; do not imply that clients or Serde work without `std`.

The implemented protocol subset is QoS 0/1. QoS 2, reconnect/resubscribe,
persistent sessions, offline queues, topic aliases, will messages, AUTH, and a
broker are out of scope for `0.2`.

TLS is experimental. It uses Mozilla roots and the alpha
`rustls-rustcrypto` provider, whose graph currently contains a second
`rustls-webpki` 0.102.x line. Keep timeout/address parsing tests and WASIp2 TLS
compilation in CI. `TlsTransport::connect` uses a shared ten-second TCP budget;
the platform's synchronous DNS resolver cannot itself be interrupted.

See [DESIGN.md](DESIGN.md) for the complete v0.2 design contract and
[CHANGELOG.md](CHANGELOG.md) for release history.
