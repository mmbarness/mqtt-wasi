use mqtt_wasi::{ConnectOptions, MqttClient, QoS, TraceContext};
use serde::{Deserialize, Serialize};
use std::thread;
use std::time::Duration;

const BROKER: &str = "127.0.0.1:1883";

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct SensorReading {
    celsius: f64,
    device_id: String,
}

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

#[test]
fn connect_and_disconnect() {
    let client = match try_connect("test-connect") {
        Some(c) => c,
        None => return,
    };
    client.disconnect().unwrap();
}

#[test]
fn publish_qos0() {
    let mut client = match try_connect("test-pub-qos0") {
        Some(c) => c,
        None => return,
    };
    let reading = SensorReading {
        celsius: 22.5,
        device_id: "sensor-1".into(),
    };
    client.publish("test/temperature", &reading).unwrap();
    client.disconnect().unwrap();
}

#[test]
fn publish_qos1() {
    let mut client = match try_connect("test-pub-qos1") {
        Some(c) => c,
        None => return,
    };
    let reading = SensorReading {
        celsius: 23.1,
        device_id: "sensor-2".into(),
    };
    client.publish_qos1("test/temperature", &reading).unwrap();
    client.disconnect().unwrap();
}

#[test]
fn subscribe_and_receive() {
    let mut sub = match try_connect("test-sub") {
        Some(c) => c,
        None => return,
    };
    sub.subscribe_raw("test/roundtrip", QoS::AtMostOnce).unwrap();

    thread::sleep(Duration::from_millis(100));

    let mut publ = MqttClient::connect(BROKER, ConnectOptions::new("test-pub")).unwrap();
    let reading = SensorReading {
        celsius: 19.8,
        device_id: "sensor-3".into(),
    };
    publ.publish("test/roundtrip", &reading).unwrap();
    publ.disconnect().unwrap();

    let msg = sub.recv_raw().unwrap().expect("should receive a message");
    assert_eq!(msg.topic, "test/roundtrip");

    let payload: SensorReading = serde_json::from_slice(&msg.payload).unwrap();
    assert_eq!(payload.celsius, 19.8);
    assert_eq!(payload.device_id, "sensor-3");

    sub.disconnect().unwrap();
}

#[test]
fn trace_context_propagation() {
    let mut sub = match try_connect("test-trace-sub") {
        Some(c) => c,
        None => return,
    };
    sub.subscribe_raw("test/traced", QoS::AtMostOnce).unwrap();
    thread::sleep(Duration::from_millis(100));

    let mut publ = MqttClient::connect(BROKER, ConnectOptions::new("test-trace-pub")).unwrap();
    let trace = TraceContext::new_root([0xAA; 16], [0xBB; 8]);
    publ.publish_traced("test/traced", &"hello", &trace).unwrap();
    publ.disconnect().unwrap();

    let msg = sub.recv_raw().unwrap().expect("should receive traced message");
    let extracted = TraceContext::from_properties(&msg.properties).unwrap();
    assert_eq!(extracted.trace_id, [0xAA; 16]);
    assert_eq!(extracted.span_id, [0xBB; 8]);

    sub.disconnect().unwrap();
}
