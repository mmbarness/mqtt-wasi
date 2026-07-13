use mqtt_wasi::codec::properties::Properties;
use mqtt_wasi::{ConnectOptions, MqttClient, PublishOptions, QoS, TraceContext};

const BROKER: &str = "127.0.0.1:1883";

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

fn broker_is_required() -> bool {
    std::env::var("MQTT_TEST_REQUIRED").as_deref() == Ok("1")
}

/// Connect to the local broker, or skip when it is not running.
fn try_connect(prefix: &str) -> Option<MqttClient> {
    let client_id = unique(prefix);
    match MqttClient::connect(
        BROKER,
        ConnectOptions::new(client_id)
            .with_keep_alive(10)
            .with_ack_timeout(std::time::Duration::from_secs(3)),
    ) {
        Ok(client) => Some(client),
        Err(error) => {
            if broker_is_required() {
                panic!("required local broker connection failed at {BROKER}: {error}");
            }
            eprintln!("local broker not running at {BROKER}; skipping: {error}");
            None
        }
    }
}

#[test]
fn connect_and_disconnect() {
    let Some(client) = try_connect("sync-connect") else {
        return;
    };
    client.disconnect().unwrap();
}

#[test]
fn publish_qos_zero_and_one() {
    let Some(mut client) = try_connect("sync-publish") else {
        return;
    };
    let topic = format!("mqtt-wasi/test/publish/{}", unique("topic"));

    client
        .publish(&topic, b"qos-zero", PublishOptions::default())
        .unwrap();
    client
        .publish(
            &topic,
            b"qos-one",
            PublishOptions::default().with_qos(QoS::AtLeastOnce),
        )
        .unwrap();
    client.disconnect().unwrap();
}

#[test]
fn subscribe_receives_bytes_qos_and_properties() {
    let Some(mut subscriber) = try_connect("sync-subscriber") else {
        return;
    };
    let topic = format!("mqtt-wasi/test/roundtrip/{}", unique("topic"));
    subscriber.subscribe(&topic, QoS::AtLeastOnce).unwrap();

    let mut publisher = try_connect("sync-publisher").expect("broker disappeared");
    let properties = Properties::new().user("content-type", "application/octet-stream");
    publisher
        .publish(
            &topic,
            b"opaque\0payload",
            PublishOptions::default()
                .with_qos(QoS::AtLeastOnce)
                .with_properties(properties),
        )
        .unwrap();
    publisher.disconnect().unwrap();

    let message = subscriber.recv().unwrap().expect("expected a PUBLISH");
    assert_eq!(message.topic, topic);
    assert_eq!(message.payload, b"opaque\0payload");
    assert_eq!(message.qos, QoS::AtLeastOnce);
    assert_eq!(
        message.properties.user_properties().collect::<Vec<_>>(),
        [("content-type", "application/octet-stream")]
    );
    subscriber.disconnect().unwrap();
}

#[test]
fn subscribe_and_unsubscribe() {
    let Some(mut client) = try_connect("sync-subscription") else {
        return;
    };
    let topic = format!("mqtt-wasi/test/subscription/{}", unique("topic"));

    client.subscribe(&topic, QoS::AtMostOnce).unwrap();
    client.unsubscribe(&topic).unwrap();
    client.disconnect().unwrap();
}

#[test]
fn trace_context_uses_standard_user_properties() {
    let Some(mut subscriber) = try_connect("trace-subscriber") else {
        return;
    };
    let topic = format!("mqtt-wasi/test/trace/{}", unique("topic"));
    subscriber.subscribe(&topic, QoS::AtMostOnce).unwrap();

    let mut publisher = try_connect("trace-publisher").expect("broker disappeared");
    let trace = TraceContext::new_root([0xaa; 16], [0xbb; 8])
        .expect("non-zero trace and span identifiers are valid");
    let mut properties = Properties::new();
    trace.inject(&mut properties);
    publisher
        .publish(
            &topic,
            b"traced",
            PublishOptions::default().with_properties(properties),
        )
        .unwrap();
    publisher.disconnect().unwrap();

    let message = subscriber.recv().unwrap().expect("expected a PUBLISH");
    let extracted = TraceContext::from_properties(&message.properties).unwrap();
    assert_eq!(extracted.trace_id, [0xaa; 16]);
    assert_eq!(extracted.span_id, [0xbb; 8]);
    subscriber.disconnect().unwrap();
}
