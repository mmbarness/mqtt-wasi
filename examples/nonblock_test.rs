use std::io::{Read, Write};
use std::net::TcpStream;

fn main() {
    // Connect to the MQTT broker (Mosquitto on localhost)
    println!("[1] connecting...");
    let mut stream = TcpStream::connect("127.0.0.1:1883").unwrap();
    println!("[2] connected");

    // Test set_nonblocking
    println!("[3] setting nonblocking...");
    stream.set_nonblocking(true).unwrap();
    println!("[4] nonblocking set OK");

    // Try a non-blocking read (should get WouldBlock since we haven't sent anything)
    let mut buf = [0u8; 256];
    match stream.read(&mut buf) {
        Ok(n) => println!("[5] read {} bytes (unexpected)", n),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            println!("[5] got WouldBlock — non-blocking I/O works!");
        }
        Err(e) => println!("[5] read error: {} (kind: {:?})", e, e.kind()),
    }

    // Switch back to blocking, send an MQTT CONNECT, verify we get CONNACK
    stream.set_nonblocking(false).unwrap();
    println!("[6] switched back to blocking");

    // Minimal MQTT v5 CONNECT packet
    let connect: &[u8] = &[
        0x10, 0x0D, // Fixed header: CONNECT, remaining length 13
        0x00, 0x04, b'M', b'Q', b'T', b'T', // Protocol name (6 bytes)
        0x05, // Protocol version 5
        0x02, // Connect flags: clean start
        0x00, 0x3C, // Keep alive: 60s
        0x00, // Properties length: 0
        0x00, 0x00, // Client ID: empty string
    ];
    stream.write_all(connect).unwrap();
    println!("[7] sent CONNECT");

    let n = stream.read(&mut buf).unwrap();
    println!("[8] got {} bytes back", n);
    if n >= 2 && buf[0] == 0x20 {
        println!("[9] CONNACK received — reason code: 0x{:02x}", buf[3]);
    }

    // DISCONNECT
    stream.write_all(&[0xE0, 0x00]).unwrap();
    println!("[10] done");
}
