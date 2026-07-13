//! Plain-byte responders for the request/response example.
//!
//! Each responder reads MQTT v5 Response Topic and Correlation Data properties,
//! then copies Correlation Data onto its response. There is no RPC envelope.

use mqtt_wasi::codec::properties::Properties;
use mqtt_wasi::{ConnectOptions, MqttClient, PublishOptions, QoS};
use std::thread;

fn main() {
    let addr = std::env::var("MQTT_ADDR").unwrap_or_else(|_| "127.0.0.1:1883".into());
    let topics = [
        ("mqtt-wasi/example/double", Operation::Double),
        ("mqtt-wasi/example/greet", Operation::Greet),
        ("mqtt-wasi/example/reverse", Operation::Reverse),
    ];
    let responder_count = topics.len();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();

    let handles: Vec<_> = topics
        .into_iter()
        .enumerate()
        .map(|(index, (topic, operation))| {
            let addr = addr.clone();
            let ready_tx = ready_tx.clone();
            thread::spawn(move || serve_once(&addr, index, topic, operation, ready_tx))
        })
        .collect();
    drop(ready_tx);

    for _ in 0..responder_count {
        ready_rx.recv().expect("responder stopped before SUBACK");
    }
    println!("responders ready on {addr}");
    for handle in handles {
        handle.join().expect("responder thread panicked");
    }
}

#[derive(Clone, Copy)]
enum Operation {
    Double,
    Greet,
    Reverse,
}

fn serve_once(
    addr: &str,
    index: usize,
    topic: &str,
    operation: Operation,
    ready: std::sync::mpsc::Sender<()>,
) {
    let mut options = ConnectOptions::new(format!("example-responder-{index}"));
    if let (Ok(user), Ok(pass)) = (std::env::var("MQTT_USER"), std::env::var("MQTT_PASS")) {
        options = options.with_credentials(user, pass.as_bytes());
    }

    let mut client = MqttClient::connect(addr, options).expect("responder connect");
    client
        .subscribe(topic, QoS::AtLeastOnce)
        .expect("subscribe");
    ready.send(()).expect("requester process stopped");

    let request = client.recv().expect("receive").expect("broker closed");
    let response_topic = request
        .properties
        .response_topic()
        .expect("request omitted Response Topic")
        .to_owned();
    let correlation_data = request
        .properties
        .correlation_data()
        .expect("request omitted Correlation Data")
        .to_vec();
    let response = transform(operation, &request.payload);
    let properties = Properties::new().with_correlation_data(correlation_data);

    client
        .publish(
            &response_topic,
            response,
            PublishOptions::default().with_properties(properties),
        )
        .expect("publish response");
    client.disconnect().expect("disconnect");
}

fn transform(operation: Operation, payload: &[u8]) -> Vec<u8> {
    let text = std::str::from_utf8(payload).expect("request payload was not UTF-8");
    match operation {
        Operation::Double => (text.parse::<i64>().expect("expected an integer") * 2)
            .to_string()
            .into_bytes(),
        Operation::Greet => format!("Hello, {text}!").into_bytes(),
        Operation::Reverse => text.chars().rev().collect::<String>().into_bytes(),
    }
}
