# mqtt-wasi

Runtime-light MQTT v5 for native Rust and `wasm32-wasip2`.

`mqtt-wasi` provides a small bytes-first protocol core, a blocking client, and
an optional async client with an explicit connection driver. It is intended for
applications that need MQTT without a multi-threaded runtime or native library
dependency, including Wasmtime guests with WASI sockets.

Version `0.2` is a deliberate API break from the application-specific `0.1`
prototype. See [DESIGN.md](DESIGN.md) for the ownership model and non-goals.

## What it supports

- MQTT v5 CONNECT/CONNACK, PUBLISH/PUBACK, SUBSCRIBE/SUBACK,
  UNSUBSCRIBE/UNSUBACK, PINGREQ/PINGRESP, and DISCONNECT.
- QoS 0 and QoS 1 publish and subscribe flows.
- Raw byte payloads and a supported subset of MQTT v5 properties as the
  primary API. Defined but unsupported property IDs are skipped when their
  standard wire type is known, except Topic Alias, which is rejected because
  the client advertises an alias maximum of zero.
- Blocking and async clients over a common protocol implementation.
- Standard MQTT v5 request/response using Response Topic and Correlation Data.
- Bounded packets and queues, operation deadlines, and idle keepalive.
- `no_std` plus `alloc` for the codec and protocol types.
- Optional JSON publishing, W3C Trace Context helpers, and experimental TLS.

It does not currently implement QoS 2, automatic reconnect/resubscribe,
persistent sessions or offline queues, topic aliases, will messages, AUTH, or a
broker.

## Features

| Feature | Default | Purpose |
| --- | --- | --- |
| `std` | yes | Blocking client, sockets, transport, and time-based limits |
| `async-client` | yes | Cloneable async handle plus the Tokio connection driver |
| `request-response` | no | Response Topic and Correlation Data request helper |
| `serde` | no | `publish_json` convenience methods; implies `std` |
| `tls` | no | Experimental rustls/RustCrypto transport |

```toml
[dependencies]
mqtt-wasi = "0.2"

# Opt in only where needed:
# mqtt-wasi = { version = "0.2", features = ["request-response"] }
# mqtt-wasi = { version = "0.2", features = ["serde", "tls"] }

# Async binaries using the examples below also need Tokio's runtime and macros:
# tokio = { version = "1", features = ["rt", "macros"] }
```

## Blocking client

```rust
use mqtt_wasi::{ConnectOptions, MqttClient, PublishOptions, QoS};

fn main() -> Result<(), mqtt_wasi::Error> {
    let mut client = MqttClient::connect(
        "127.0.0.1:1883",
        ConnectOptions::new("example-client"),
    )?;

    client.subscribe("sensors/#", QoS::AtLeastOnce)?;
    client.publish(
        "sensors/temperature",
        br#"{"celsius":22.5}"#,
        PublishOptions::default().with_qos(QoS::AtLeastOnce),
    )?;

    if let Some(message) = client.recv()? {
        println!("{}: {:?}", message.topic, message.payload);
    }
    client.disconnect()
}
```

`recv()` and the `incoming()` iterator return `PublishPacket`, the same message
type used by the async API. While a blocking operation waits for an ACK, other
incoming publishes are placed in a bounded FIFO rather than discarded.

With the `serde` feature, `MqttClient::publish_json` and
`AsyncMqttClient::publish_json` serialize one value before publishing. Receive
payloads remain bytes so callers retain explicit control of their wire format.

## Async client

The async API separates a cheap, cloneable command handle from the one future
that owns the transport:

```rust
use mqtt_wasi::{AsyncMqttClient, ConnectOptions, Event, PublishOptions, QoS};
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // This call is intentionally synchronous: DNS resolution, TCP connection,
    // and the MQTT handshake complete before the driver is returned.
    let (mut client, connection) = AsyncMqttClient::connect(
        "127.0.0.1:1883",
        ConnectOptions::new("async-example"),
    )?;
    let driver = tokio::spawn(connection.run());

    client.subscribe("events/#", QoS::AtLeastOnce).await?;
    client
        .publish(
            "events/new",
            b"opaque bytes",
            PublishOptions::default(),
        )
        .await?;

    if let Some(Event::Publish(message)) =
        tokio::time::timeout(Duration::from_secs(5), client.next_event()).await?
    {
        println!("{}: {:?}", message.topic, message.payload);
    }

    client.disconnect().await?;
    driver.await??;
    Ok(())
}
```

`MqttConnection::run` must run continuously. It owns all reads and writes,
keepalive, ACK matching, deadlines, and packet dispatch; individual operation
futures never pump or sleep on the socket.

Commands, outbound bytes, and events are bounded. If the event queue fills, the
driver returns `Error::QueueFull("event")` and closes the connection rather than
silently dropping an inbound PUBLISH. Supervise the driver as connection state,
and drain events at a rate appropriate for the configured capacity.

## Request/response

Enable `request-response` to publish opaque bytes with the standard MQTT v5
Response Topic and Correlation Data properties:

```rust
use mqtt_wasi::{AsyncMqttClient, Error, PublishPacket, RequestOptions};
use std::time::Duration;

async fn search(client: &AsyncMqttClient) -> Result<PublishPacket, Error> {
    client.request(
        "services/search",
        b"query bytes",
        RequestOptions::new("clients/example/replies")
            .with_timeout(Duration::from_secs(10)),
    )
    .await
}
```

Use one stable, ACL-scoped response topic per client. The `0.2` driver caches
response subscriptions and caps distinct cached topics at the pending-operation
bound; it does not automatically unsubscribe or evict them. A fresh topic for
every request will therefore eventually produce backpressure.

The helper generates opaque correlation bytes unless the caller supplies them,
matches only replies on the configured topic carrying those bytes, and returns
a `PublishPacket`. It defines no JSON envelope, method field, retry policy, or
topic naming convention. Correlation Data is a dispatch key, not an
authorization credential.

## Resource limits and deadlines

`ConnectOptions` makes the important bounds explicit:

- connect and acknowledgement timeouts;
- maximum inbound packet size, also advertised in CONNECT;
- maximum incoming/event and command counts;
- maximum queued outbound bytes;
- async driver poll interval.

The broker's Maximum Packet Size and Receive Maximum are honored for outbound
traffic. Packet lengths are rejected before allocating their complete declared
payload. Long-lived receive loops intentionally have no message deadline; use
application-level cancellation where needed.

## TLS

The `tls` feature provides `TlsTransport` backed by rustls, Mozilla root
certificates, and `rustls-rustcrypto`:

```rust
use mqtt_wasi::{ConnectOptions, MqttClient, TlsTransport};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let transport = TlsTransport::connect("broker.example.com:8883")?;
    let client = MqttClient::connect_with(
        transport,
        ConnectOptions::new("secure-client"),
    )?;
    client.disconnect()?;
    Ok(())
}
```

`TlsTransport::connect` gives all resolved TCP endpoints one shared ten-second
budget; timeout-aware and custom-`ClientConfig` variants are also available.
Bracketed IPv6 addresses are supported. The platform DNS resolver is
synchronous and cannot itself be interrupted by `std::net`, although its elapsed
time consumes the shared budget after it returns.

TLS is experimental in `0.2`. The pure-Rust provider glue is still
`0.0.2-alpha`, and it currently brings a second `rustls-webpki` 0.102.x line
alongside rustls's 0.103.x line. Consumers with audited TLS requirements should
review the resolved dependency graph and advisories for their lockfile. TLS
does not replace broker ACLs or application authorization.

## Trace Context

`TraceContext` maps W3C `traceparent` and `tracestate` values to MQTT User
Properties without an OpenTelemetry dependency:

```rust
use mqtt_wasi::codec::properties::Properties;
use mqtt_wasi::TraceContext;

let trace = TraceContext::new_root([0xaa; 16], [0xbb; 8])
    .expect("trace and span identifiers must be non-zero");
let mut properties = Properties::new();
trace.inject(&mut properties);
```

`new_root` and `child` return `None` for all-zero trace or span identifiers.
Parsing supports W3C version `00` and rejects the forbidden `ff` version and
malformed identifiers.

## WASI and `no_std`

Build the library or a standalone example for WASIp2:

```bash
rustup target add wasm32-wasip2
cargo build --target wasm32-wasip2 --release
cargo build --target wasm32-wasip2 --release --features tls
```

Runnable guests require a WASIp2 runtime with socket support. The project is
tested with Wasmtime; other runtimes are not part of the `0.2` compatibility
claim.

The codec and protocol types build with `no_std` plus `alloc`:

```bash
rustup target add wasm32-unknown-unknown
cargo check --lib --no-default-features --target wasm32-unknown-unknown
```

Clients, transports, timers, and the `serde` convenience feature require
`std`.

## Testing

Unit tests do not require a broker. Local integration tests use Mosquitto on
`127.0.0.1:1883`; they skip when it is absent for developer convenience. CI
sets `MQTT_TEST_REQUIRED=1`, turning any broker connection or handshake failure
into a test failure.

```bash
mosquitto -p 1883
MQTT_TEST_REQUIRED=1 cargo test --all-features -- --test-threads=1
```

Optional external TLS tests use `MQTT_TLS_ADDR`, `MQTT_TLS_USER`, and
`MQTT_TLS_PASS`. They skip when those variables are absent and are never used by
CI. See [.env.example](.env.example).

## License

Licensed under either [Apache License 2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT), at your option.
