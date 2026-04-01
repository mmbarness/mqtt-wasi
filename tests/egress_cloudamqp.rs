//! Integration tests against a live RabbitMQ broker (CloudAMQP).
//!
//! Each test spins up its own consumer thread on the same broker — no external
//! services required beyond the MQTT broker itself. Tests the full request/reply
//! path through RabbitMQ's MQTT plugin.
//!
//! Requires CLOUDAMQP_URL env var (or .env file). Skipped if not set.
//!
//! Run:
//!   cargo test --test egress_cloudamqp -- --nocapture

use mqtt_wasi::{AsyncMqttClient, ConnectOptions, MqttClient};
use std::thread;
use std::time::Duration;

struct BrokerConfig {
    host: String,
    user: String,
    pass: String,
}

fn parse_cloudamqp() -> Option<BrokerConfig> {
    dotenvy::dotenv().ok();
    let url = std::env::var("CLOUDAMQP_URL").ok()?;
    let rest = url
        .strip_prefix("amqps://")
        .or_else(|| url.strip_prefix("amqp://"))?;
    let (userpass, hostpart) = rest.split_once('@')?;
    let (user, pass) = userpass.split_once(':')?;
    let host = hostpart.split(&['/', ':'][..]).next()?;
    Some(BrokerConfig {
        host: host.into(),
        user: user.into(),
        pass: pass.into(),
    })
}

fn connect(client_id: &str) -> Option<MqttClient> {
    let cfg = parse_cloudamqp()?;
    let addr = format!("{}:1883", cfg.host);
    let opts = ConnectOptions::new(client_id)
        .with_keep_alive(60)
        .with_credentials(&cfg.user, cfg.pass.as_bytes());
    MqttClient::connect(&addr, opts).ok()
}

async fn async_connect(client_id: &str, amqp_reply: bool) -> Option<AsyncMqttClient> {
    let cfg = parse_cloudamqp()?;
    let addr = format!("{}:1883", cfg.host);
    let opts = ConnectOptions::new(client_id)
        .with_keep_alive(60)
        .with_credentials(&cfg.user, cfg.pass.as_bytes())
        .with_amqp_reply_format(amqp_reply);
    AsyncMqttClient::connect(&addr, opts).await.ok()
}

fn skip() {
    eprintln!("CLOUDAMQP_URL not set — skipping");
}

/// Spawn a consumer thread that subscribes to `topic`, reads one request,
/// and publishes a reply with the given transform applied to params.
fn spawn_echo_consumer(
    client_id: &str,
    topic: &str,
    transform: impl Fn(serde_json::Value) -> serde_json::Value + Send + 'static,
) -> Option<thread::JoinHandle<()>> {
    let mut client = connect(client_id)?;
    let topic = topic.to_string();

    Some(thread::spawn(move || {
        client
            .subscribe_raw(&topic, mqtt_wasi::QoS::AtMostOnce)
            .expect("consumer subscribe");

        let msg = client.recv_raw().expect("consumer recv").expect("no msg");
        let req: serde_json::Value = serde_json::from_slice(&msg.payload).expect("parse request");

        let reply_to = req["replyTo"].as_str().expect("missing replyTo").to_string();
        let correlation_id = req["correlationId"].as_str().expect("missing correlationId");
        let params = req["params"].clone();

        let reply = serde_json::json!({
            "correlationId": correlation_id,
            "result": { "ok": true, "data": transform(params) }
        });
        let reply_bytes = serde_json::to_vec(&reply).expect("serialize reply");
        client
            .publish_raw(
                &reply_to,
                &reply_bytes,
                mqtt_wasi::QoS::AtMostOnce,
                false,
                Default::default(),
            )
            .expect("publish reply");

        client.disconnect().ok();
    }))
}

// ---------------------------------------------------------------------------
// Basic connectivity
// ---------------------------------------------------------------------------

#[test]
fn test_cloudamqp_connect() {
    let client = match connect("mqtt-wasi-test-conn") {
        Some(c) => c,
        None => return skip(),
    };
    client.disconnect().ok();
}

#[test]
fn test_cloudamqp_pubsub_roundtrip() {
    let id = &uuid::Uuid::new_v4().to_string()[..8];
    let topic = format!("test/mqtt-wasi/roundtrip-{id}");

    let mut pub_client = match connect(&format!("mw-pub-{id}")) {
        Some(c) => c,
        None => return skip(),
    };
    let mut sub_client = connect(&format!("mw-sub-{id}")).unwrap();

    sub_client
        .subscribe_raw(&topic, mqtt_wasi::QoS::AtMostOnce)
        .expect("subscribe");

    let payload = serde_json::json!({"test": true, "id": id});
    pub_client.publish(&topic, &payload).expect("publish");

    let msg = sub_client.recv_raw().expect("recv").expect("no message");
    let received: serde_json::Value = serde_json::from_slice(&msg.payload).expect("deserialize");
    assert_eq!(received["test"], true);

    pub_client.disconnect().ok();
    sub_client.disconnect().ok();
}

// ---------------------------------------------------------------------------
// Sync request/reply with self-hosted consumer
// ---------------------------------------------------------------------------

#[test]
fn test_request_reply_echo() {
    let id = &uuid::Uuid::new_v4().to_string()[..8];
    let topic = format!("test/mqtt-wasi/echo-{id}");

    // Consumer: echoes params back as data
    let consumer = match spawn_echo_consumer(
        &format!("mw-echo-consumer-{id}"),
        &topic,
        |params| params,
    ) {
        Some(h) => h,
        None => return skip(),
    };

    thread::sleep(Duration::from_millis(200));

    // Requester: manual request/reply (sync)
    let mut client = connect(&format!("mw-echo-requester-{id}")).unwrap();
    let correlation_id = uuid::Uuid::new_v4().to_string();
    let reply_topic = format!("test/mqtt-wasi/reply/{correlation_id}");

    client
        .subscribe_raw(&reply_topic, mqtt_wasi::QoS::AtMostOnce)
        .expect("subscribe reply");

    let request = serde_json::json!({
        "params": {"greeting": "hello", "n": 42},
        "correlationId": correlation_id,
        "replyTo": reply_topic,
    });
    let request_bytes = serde_json::to_vec(&request).expect("serialize");
    client
        .publish_raw(&topic, &request_bytes, mqtt_wasi::QoS::AtMostOnce, false, Default::default())
        .expect("publish request");

    let msg = client.recv_raw().expect("recv reply").expect("no reply");
    let envelope: serde_json::Value = serde_json::from_slice(&msg.payload).expect("parse reply");

    assert_eq!(envelope["correlationId"], correlation_id);
    assert_eq!(envelope["result"]["ok"], true);
    assert_eq!(envelope["result"]["data"]["greeting"], "hello");
    assert_eq!(envelope["result"]["data"]["n"], 42);

    client.disconnect().ok();
    consumer.join().unwrap();
}

#[test]
fn test_request_reply_transform() {
    let id = &uuid::Uuid::new_v4().to_string()[..8];
    let topic = format!("test/mqtt-wasi/transform-{id}");

    // Consumer: doubles the "value" field
    let consumer = match spawn_echo_consumer(
        &format!("mw-transform-consumer-{id}"),
        &topic,
        |params| {
            let v = params["value"].as_i64().unwrap_or(0);
            serde_json::json!({"doubled": v * 2})
        },
    ) {
        Some(h) => h,
        None => return skip(),
    };

    thread::sleep(Duration::from_millis(200));

    let mut client = connect(&format!("mw-transform-requester-{id}")).unwrap();
    let correlation_id = uuid::Uuid::new_v4().to_string();
    let reply_topic = format!("test/mqtt-wasi/reply/{correlation_id}");

    client
        .subscribe_raw(&reply_topic, mqtt_wasi::QoS::AtMostOnce)
        .expect("subscribe");

    let request = serde_json::json!({
        "params": {"value": 21},
        "correlationId": correlation_id,
        "replyTo": reply_topic,
    });
    let request_bytes = serde_json::to_vec(&request).expect("serialize");
    client
        .publish_raw(&topic, &request_bytes, mqtt_wasi::QoS::AtMostOnce, false, Default::default())
        .expect("publish");

    let msg = client.recv_raw().expect("recv").expect("no reply");
    let envelope: serde_json::Value = serde_json::from_slice(&msg.payload).expect("parse");

    assert_eq!(envelope["result"]["data"]["doubled"], 42);

    client.disconnect().ok();
    consumer.join().unwrap();
}

// ---------------------------------------------------------------------------
// Async request/reply with self-hosted consumers
// ---------------------------------------------------------------------------

/// Spawn a consumer for the async client's request/reply pattern.
/// The async client wraps in a RequestEnvelope, so the consumer reads that format.
fn spawn_async_echo_consumer(
    client_id: &str,
    topic: &str,
) -> Option<thread::JoinHandle<()>> {
    spawn_echo_consumer(client_id, topic, |params| {
        serde_json::json!({"echo": params})
    })
}

#[tokio::test(flavor = "current_thread")]
async fn test_async_request_reply_single() {
    let id = &uuid::Uuid::new_v4().to_string()[..8];
    let topic = format!("test/mqtt-wasi/async-single-{id}");

    let consumer = match spawn_async_echo_consumer(
        &format!("mw-async-consumer-{id}"),
        &topic,
    ) {
        Some(h) => h,
        None => return skip(),
    };

    thread::sleep(Duration::from_millis(200));

    // amqp_reply_format=false since consumer is also an MQTT client
    let client = match async_connect(&format!("mw-async-requester-{id}"), false).await {
        Some(c) => c,
        None => { consumer.join().ok(); return skip(); }
    };

    let result = client
        .request(&topic, &serde_json::json!({"hello": "world"}))
        .await
        .expect("request failed");

    assert_eq!(result["ok"], true);
    assert_eq!(result["data"]["echo"]["hello"], "world");

    client.disconnect().ok();
    consumer.join().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn test_async_concurrent_requests() {
    let id = &uuid::Uuid::new_v4().to_string()[..8];
    let topic_a = format!("test/mqtt-wasi/concurrent-a-{id}");
    let topic_b = format!("test/mqtt-wasi/concurrent-b-{id}");
    let topic_c = format!("test/mqtt-wasi/concurrent-c-{id}");

    // 3 consumers, one per topic
    let consumer_a = match spawn_echo_consumer(
        &format!("mw-conc-a-{id}"), &topic_a,
        |p| serde_json::json!({"tag": "alpha", "input": p}),
    ) {
        Some(h) => h,
        None => return skip(),
    };
    let consumer_b = spawn_echo_consumer(
        &format!("mw-conc-b-{id}"), &topic_b,
        |p| serde_json::json!({"tag": "bravo", "input": p}),
    ).unwrap();
    let consumer_c = spawn_echo_consumer(
        &format!("mw-conc-c-{id}"), &topic_c,
        |p| serde_json::json!({"tag": "charlie", "input": p}),
    ).unwrap();

    thread::sleep(Duration::from_millis(300));

    let client = match async_connect(&format!("mw-conc-req-{id}"), false).await {
        Some(c) => c,
        None => {
            consumer_a.join().ok(); consumer_b.join().ok(); consumer_c.join().ok();
            return skip();
        }
    };

    let start = std::time::Instant::now();

    let (ra, rb, rc) = tokio::join!(
        client.request(&topic_a, &serde_json::json!({"n": 1})),
        client.request(&topic_b, &serde_json::json!({"n": 2})),
        client.request(&topic_c, &serde_json::json!({"n": 3})),
    );

    let elapsed = start.elapsed();

    let a = ra.expect("request a failed");
    let b = rb.expect("request b failed");
    let c = rc.expect("request c failed");

    assert_eq!(a["data"]["tag"], "alpha");
    assert_eq!(b["data"]["tag"], "bravo");
    assert_eq!(c["data"]["tag"], "charlie");
    assert_eq!(a["data"]["input"]["n"], 1);
    assert_eq!(b["data"]["input"]["n"], 2);
    assert_eq!(c["data"]["input"]["n"], 3);

    eprintln!("[concurrent] 3 requests completed in {elapsed:.2?} (parallel over 1 connection)");

    client.disconnect().ok();
    consumer_a.join().unwrap();
    consumer_b.join().unwrap();
    consumer_c.join().unwrap();
}
