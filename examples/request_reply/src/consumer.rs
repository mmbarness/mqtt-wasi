//! Echo consumers for the request_reply example.
//!
//! Spawns 3 threads, each subscribing to a topic and echoing replies.
//! Run this natively before running the WASM requester.
//!
//! Usage:
//!   MQTT_ADDR=... cargo run --bin consumer

use mqtt_wasi::{ConnectOptions, MqttClient, QoS};
use std::thread;

fn main() {
    let addr = std::env::var("MQTT_ADDR").unwrap_or_else(|_| "127.0.0.1:1883".into());

    let topics = [
        ("mqtt-wasi/rpc/double", "double"),
        ("mqtt-wasi/rpc/greet", "greet"),
        ("mqtt-wasi/rpc/reverse", "reverse"),
    ];

    println!("starting 3 consumers on {addr}...");

    let handles: Vec<_> = topics
        .iter()
        .enumerate()
        .map(|(i, (topic, name))| {
            let addr = addr.clone();
            let topic = topic.to_string();
            let name = name.to_string();
            thread::spawn(move || {
                echo_consumer(&addr, &format!("consumer-{i}"), &topic, &name);
            })
        })
        .collect();

    println!("consumers ready — waiting for requests (ctrl-c to stop)");

    for h in handles {
        h.join().ok();
    }
}

fn echo_consumer(addr: &str, client_id: &str, topic: &str, name: &str) {
    let mut opts = ConnectOptions::new(client_id).with_keep_alive(30);
    if let (Ok(u), Ok(p)) = (std::env::var("MQTT_USER"), std::env::var("MQTT_PASS")) {
        opts = opts.with_credentials(u, p.as_bytes());
    }

    let mut client = MqttClient::connect(addr, opts).expect("consumer connect");
    client.subscribe_raw(topic, QoS::AtMostOnce).expect("subscribe");
    println!("  [{name}] subscribed to {topic}");

    // Serve one request then exit
    let msg = client.recv_raw().expect("recv").expect("no msg");
    let req: serde_json::Value = serde_json::from_slice(&msg.payload).expect("parse");

    let reply_to = req["replyTo"].as_str().expect("replyTo");
    let correlation_id = req["correlationId"].as_str().expect("correlationId");
    let params = &req["params"];

    let data = if name == "double" {
        let v = params["value"].as_i64().unwrap_or(0);
        serde_json::json!({"result": v * 2})
    } else if name == "greet" {
        let n = params["name"].as_str().unwrap_or("?");
        serde_json::json!({"message": format!("Hello, {n}!")})
    } else {
        let text = params["text"].as_str().unwrap_or("");
        let reversed: String = text.chars().rev().collect();
        serde_json::json!({"reversed": reversed})
    };

    let reply = serde_json::json!({
        "correlationId": correlation_id,
        "result": {"ok": true, "data": data}
    });
    let bytes = serde_json::to_vec(&reply).expect("serialize");
    client
        .publish_raw(reply_to, &bytes, QoS::AtMostOnce, false, Default::default())
        .expect("reply");

    println!("  [{name}] replied");
    client.disconnect().ok();
}
