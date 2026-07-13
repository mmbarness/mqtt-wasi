# Changelog

All notable changes to this project are documented in this file. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-07-13

This is the breaking `0.2.0` redesign. The `0.1.x` API should be treated as a
prototype rather than a compatibility boundary.

### Added

- A public, payload-agnostic MQTT v5 client architecture for native Rust and
  `wasm32-wasip2`.
- A dedicated connection driver that owns network I/O, keepalive, packet
  dispatch, acknowledgement tracking, and bounded queues independently of
  request futures.
- Configurable packet-size, queue, connection, acknowledgement, and request
  limits.
- Optional request/response support based on MQTT v5 Response Topic and
  Correlation Data properties.
- One `PublishPacket` receive type across blocking, async event, and
  request/response APIs.
- Bounded TLS TCP connection variants with bracketed IPv6 address support.
- CI coverage for formatting, Clippy, feature combinations, `no_std`, native
  Mosquitto integration, WASIp2, examples, documentation, and packaging.

### Changed

- Raw byte payloads are now the primary API; Serde and JSON support are opt-in.
- Async clients communicate with one connection driver instead of polling and
  sleeping on a shared socket from each request future.
- Incoming publishes are queued while synchronous operations wait for ACKs.
- TLS remains optional and experimental while its pure-Rust provider matures.
- The minimum supported Rust version is 1.94.
- Unsafe code is forbidden at the crate root.

### Removed

- Application-specific egress topics, hard-coded action fields, JSON request
  envelopes, and broker-bridge topic rewriting from the core client.
- Application-specific request/response behavior from the default feature set.

## [0.1.1] - 2026-05-11

### Fixed

- Preserved partial packets across socket timeouts.
- Corrected keepalive behavior and negative acknowledgement handling.
- Reduced busy spinning and fixed subscription and request-cancellation leaks.

The `v0.1.1` source tag retained `version = "0.1.0"` in `Cargo.toml`; no
separate `0.1.1` crate was published.

## [0.1.0] - 2026-04-01

### Added

- Initial MQTT v5 codec, synchronous client, cooperative request/reply client,
  WASIp2 support, trace-context helpers, and experimental TLS transport.

[Unreleased]: https://github.com/mmbarness/mqtt-wasi/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/mmbarness/mqtt-wasi/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/mmbarness/mqtt-wasi/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/mmbarness/mqtt-wasi/releases/tag/v0.1.0
