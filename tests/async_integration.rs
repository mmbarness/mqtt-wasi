#![cfg(feature = "async-client")]

use mqtt_wasi::codec::properties::Properties;
#[cfg(feature = "request-response")]
use mqtt_wasi::RequestOptions;
use mqtt_wasi::{AsyncMqttClient, ConnectOptions, Error, Event, MqttClient, PublishOptions, QoS};
use std::thread;
use std::time::Duration;
use tokio::task::JoinHandle;

const BROKER: &str = "127.0.0.1:1883";

type Driver = JoinHandle<Result<(), Error>>;

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

fn broker_is_required() -> bool {
    std::env::var("MQTT_TEST_REQUIRED").as_deref() == Ok("1")
}

fn options(prefix: &str) -> ConnectOptions {
    ConnectOptions::new(unique(prefix))
        .with_keep_alive(10)
        .with_ack_timeout(Duration::from_secs(3))
        .with_poll_interval(Duration::from_millis(1))
}

fn try_async_connect(prefix: &str) -> Option<(AsyncMqttClient, Driver)> {
    match AsyncMqttClient::connect(BROKER, options(prefix)) {
        Ok((client, connection)) => Some((client, tokio::spawn(connection.run()))),
        Err(error) => {
            if broker_is_required() {
                panic!("required local broker connection failed at {BROKER}: {error}");
            }
            eprintln!("local broker not running at {BROKER}; skipping: {error}");
            None
        }
    }
}

fn try_sync_connect(prefix: &str) -> Option<MqttClient> {
    match MqttClient::connect(BROKER, options(prefix)) {
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

async fn disconnect(client: AsyncMqttClient, driver: Driver) {
    client.disconnect().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), driver)
        .await
        .expect("connection driver did not stop")
        .expect("connection driver task panicked")
        .expect("connection driver failed");
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_connection_driver_connects_and_disconnects() {
    let Some((client, driver)) = try_async_connect("async-connect") else {
        return;
    };
    disconnect(client, driver).await;
}

#[tokio::test(flavor = "current_thread")]
async fn async_publish_subscribe_roundtrip() {
    let Some((mut subscriber, subscriber_driver)) = try_async_connect("async-subscriber") else {
        return;
    };
    let topic = format!("mqtt-wasi/test/async-roundtrip/{}", unique("topic"));
    let subscription = subscriber
        .subscribe(&topic, QoS::AtLeastOnce)
        .await
        .unwrap();
    assert_eq!(subscription.reason_codes, [QoS::AtLeastOnce as u8]);

    let (publisher, publisher_driver) =
        try_async_connect("async-publisher").expect("broker disappeared");
    let acknowledgement = publisher
        .publish(
            &topic,
            b"async bytes",
            PublishOptions::default().with_qos(QoS::AtLeastOnce),
        )
        .await
        .unwrap();
    assert!(acknowledgement.packet_id.is_some());
    assert_eq!(acknowledgement.reason_code, Some(0));

    let event = tokio::time::timeout(Duration::from_secs(3), subscriber.next_event())
        .await
        .expect("timed out waiting for PUBLISH")
        .expect("event stream closed");
    let Event::Publish(message) = event else {
        panic!("expected PUBLISH event, got {event:?}");
    };
    assert_eq!(message.topic, topic);
    assert_eq!(message.payload, b"async bytes");
    assert_eq!(message.qos, QoS::AtLeastOnce);

    disconnect(publisher, publisher_driver).await;
    disconnect(subscriber, subscriber_driver).await;
}

#[cfg(feature = "request-response")]
#[tokio::test(flavor = "current_thread")]
async fn request_response_uses_standard_mqtt_properties_and_raw_bytes() {
    let Some(probe) = try_sync_connect("request-probe") else {
        return;
    };
    probe.disconnect().unwrap();

    let request_topic = format!("mqtt-wasi/test/request/{}", unique("topic"));
    let response_topic = format!("mqtt-wasi/test/response/{}", unique("topic"));
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let responder_topic = request_topic.clone();

    let responder = thread::spawn(move || {
        let mut client = try_sync_connect("standards-responder").expect("broker disappeared");
        client
            .subscribe(&responder_topic, QoS::AtLeastOnce)
            .unwrap();
        ready_tx.send(()).unwrap();

        for _ in 0..2 {
            let request = client.recv().unwrap().expect("expected request PUBLISH");
            let reply_to = request
                .properties
                .response_topic()
                .expect("request omitted MQTT Response Topic")
                .to_owned();
            let correlation_data = request
                .properties
                .correlation_data()
                .expect("request omitted MQTT Correlation Data")
                .to_vec();

            let mut payload = b"reply:".to_vec();
            payload.extend_from_slice(&request.payload);
            let properties = Properties::new().with_correlation_data(correlation_data.clone());
            client
                .publish(
                    &reply_to,
                    payload,
                    PublishOptions::default().with_properties(properties),
                )
                .unwrap();
        }
        client.disconnect().unwrap();
    });

    ready_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("responder did not subscribe");
    let (client, driver) = try_async_connect("standards-requester").expect("broker disappeared");

    let first = client.request(
        &request_topic,
        b"alpha",
        RequestOptions::new(&response_topic)
            .with_qos(QoS::AtLeastOnce)
            .with_timeout(Duration::from_secs(3))
            .with_correlation_data(b"alpha-correlation"),
    );
    let second = client.request(
        &request_topic,
        b"bravo",
        RequestOptions::new(&response_topic)
            .with_qos(QoS::AtLeastOnce)
            .with_timeout(Duration::from_secs(3))
            .with_correlation_data(b"bravo-correlation"),
    );
    let (first, second) = tokio::join!(first, second);
    let first = first.unwrap();
    let second = second.unwrap();

    assert_eq!(first.payload, b"reply:alpha");
    assert_eq!(
        first.properties.correlation_data(),
        Some(&b"alpha-correlation"[..])
    );
    assert_eq!(second.payload, b"reply:bravo");
    assert_eq!(
        second.properties.correlation_data(),
        Some(&b"bravo-correlation"[..])
    );

    disconnect(client, driver).await;
    responder.join().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn full_event_queue_terminates_the_driver() {
    let Some((mut subscriber, driver)) =
        (match AsyncMqttClient::connect(BROKER, options("bounded-events").with_event_capacity(1)) {
            Ok((client, connection)) => Some((client, tokio::spawn(connection.run()))),
            Err(error) => {
                if broker_is_required() {
                    panic!("required local broker connection failed at {BROKER}: {error}");
                }
                eprintln!("local broker not running at {BROKER}; skipping: {error}");
                None
            }
        })
    else {
        return;
    };
    let topic = format!("mqtt-wasi/test/bounded-events/{}", unique("topic"));
    subscriber.subscribe(&topic, QoS::AtMostOnce).await.unwrap();

    // The current-thread runtime cannot poll the driver while these blocking
    // publishes run, ensuring both PUBLISH packets are waiting together.
    let mut publisher = try_sync_connect("bounded-publisher").expect("broker disappeared");
    publisher
        .publish(&topic, b"one", PublishOptions::default())
        .unwrap();
    publisher
        .publish(&topic, b"two", PublishOptions::default())
        .unwrap();
    publisher.disconnect().unwrap();

    let error = tokio::time::timeout(Duration::from_secs(3), driver)
        .await
        .expect("driver did not enforce the event bound")
        .expect("driver task panicked")
        .expect_err("driver silently dropped an inbound PUBLISH");
    assert!(matches!(error, Error::QueueFull("event")));

    // One event remains available; the second caused the terminal error.
    assert!(matches!(
        subscriber.next_event().await,
        Some(Event::Publish(_))
    ));
}
