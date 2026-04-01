use mqtt_wasi::{AsyncMqttClient, ConnectOptions, MqttClient, QoS};
use serde_json::json;
use std::thread;
use std::time::Duration;

const BROKER: &str = "127.0.0.1:1883";

/// Try connecting to the local broker. Returns None if not running.
fn try_connect(client_id: &str) -> Option<MqttClient> {
    match MqttClient::connect(BROKER, ConnectOptions::new(client_id).with_keep_alive(10)) {
        Ok(c) => Some(c),
        Err(_) => {
            eprintln!("local broker not running at {BROKER} — skipping");
            None
        }
    }
}

async fn try_async_connect(client_id: &str) -> Option<AsyncMqttClient> {
    match AsyncMqttClient::connect(BROKER, ConnectOptions::new(client_id).with_keep_alive(10)).await {
        Ok(c) => Some(c),
        Err(_) => {
            eprintln!("local broker not running at {BROKER} — skipping");
            None
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn async_connect_disconnect() {
    let client = match try_async_connect("async-test-connect").await {
        Some(c) => c,
        None => return,
    };
    client.disconnect().unwrap();
}

/// Test the full request/reply pattern with a mock responder.
///
/// Uses a sync MqttClient in a background thread as the "mastermind"
/// that reads requests and publishes replies.
#[tokio::test(flavor = "current_thread")]
async fn single_request_reply() {
    // Start a mock responder in a background thread
    let responder = thread::spawn(|| {
        let mut client = match try_connect("mock-responder-single") {
            Some(c) => c,
            None => return,
        };

        // Subscribe to request topic
        client
            .subscribe_raw("test/request/single", QoS::AtMostOnce)
            .unwrap();

        // Wait for a request
        let msg = client.recv_raw().unwrap().unwrap();

        // Parse the request envelope to get correlationId and replyTo
        let envelope: serde_json::Value = serde_json::from_slice(&msg.payload).unwrap();
        let reply_to = envelope["replyTo"].as_str().unwrap().to_string();
        let correlation_id = envelope["correlationId"].as_str().unwrap().to_string();

        // Publish reply
        let reply = json!({
            "correlationId": correlation_id,
            "result": { "ok": true, "data": { "answer": 42 } }
        });
        let reply_bytes = serde_json::to_vec(&reply).unwrap();
        client
            .publish_raw(&reply_to, &reply_bytes, QoS::AtMostOnce, false, Default::default())
            .unwrap();

        client.disconnect().unwrap();
    });

    // Give the responder time to subscribe
    thread::sleep(Duration::from_millis(200));

    // Send an async request
    let client = match try_async_connect("async-requester-single").await {
        Some(c) => c,
        None => { responder.join().ok(); return; }
    };

    let result = client
        .request("test/request/single", &json!({"prompt": "hello"}))
        .await
        .unwrap();

    assert_eq!(result["ok"], true);
    assert_eq!(result["data"]["answer"], 42);

    client.disconnect().unwrap();
    responder.join().unwrap();
}

/// Test concurrent request/reply with tokio::join!
#[tokio::test(flavor = "current_thread")]
async fn concurrent_request_reply() {
    // Start two mock responders (one per topic)
    let responder_a = thread::spawn(|| {
        mock_responder("mock-resp-a", "test/request/a", "alpha");
    });
    let responder_b = thread::spawn(|| {
        mock_responder("mock-resp-b", "test/request/b", "bravo");
    });

    thread::sleep(Duration::from_millis(200));

    let client = match try_async_connect("async-requester-concurrent").await {
        Some(c) => c,
        None => { responder_a.join().ok(); responder_b.join().ok(); return; }
    };

    // Both requests in flight concurrently
    let (result_a, result_b) = tokio::join!(
        client.request("test/request/a", &json!({"q": "first"})),
        client.request("test/request/b", &json!({"q": "second"})),
    );

    let a = result_a.unwrap();
    let b = result_b.unwrap();
    assert_eq!(a["data"]["tag"], "alpha");
    assert_eq!(b["data"]["tag"], "bravo");

    client.disconnect().unwrap();
    responder_a.join().unwrap();
    responder_b.join().unwrap();
}

/// Helper: a sync MQTT client that subscribes to a topic, reads one
/// request, and publishes a reply with the given tag.
fn mock_responder(client_id: &str, topic: &str, tag: &str) {
    let mut client = match try_connect(client_id) {
        Some(c) => c,
        None => return,
    };

    client.subscribe_raw(topic, QoS::AtMostOnce).unwrap();

    let msg = client.recv_raw().unwrap().unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&msg.payload).unwrap();
    let reply_to = envelope["replyTo"].as_str().unwrap().to_string();
    let correlation_id = envelope["correlationId"].as_str().unwrap().to_string();

    let reply = json!({
        "correlationId": correlation_id,
        "result": { "ok": true, "data": { "tag": tag } }
    });
    let reply_bytes = serde_json::to_vec(&reply).unwrap();
    client
        .publish_raw(&reply_to, &reply_bytes, QoS::AtMostOnce, false, Default::default())
        .unwrap();

    client.disconnect().unwrap();
}
