use crate::codec::decode::decode_fixed_header;
use crate::codec::types::{FixedHeader, Packet};
use crate::error::{Error, Result};

/// Incremental MQTT frame parser for non-blocking reads.
///
/// Accumulates bytes via `push()` and yields complete packets via
/// `try_decode()`. Handles partial reads gracefully — call `push()`
/// with whatever bytes are available, then `try_decode()` in a loop
/// until it returns `Ok(None)`.
pub struct FrameReader {
    buf: Vec<u8>,
    state: FrameState,
}

enum FrameState {
    /// Accumulating bytes until a complete fixed header is available.
    ReadingHeader,
    /// Fixed header parsed; accumulating body bytes.
    ReadingBody {
        header: FixedHeader,
        header_len: usize,
        total_len: usize,
    },
}

impl FrameReader {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(4096),
            state: FrameState::ReadingHeader,
        }
    }

    /// Feed raw bytes from the socket into the parser.
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Try to extract the next complete packet.
    ///
    /// Returns `Ok(Some(packet))` if a complete packet was decoded,
    /// `Ok(None)` if more data is needed, or `Err` if malformed.
    pub fn try_decode(&mut self) -> Result<Option<Packet>> {
        loop {
            match self.state {
                FrameState::ReadingHeader => {
                    if self.buf.is_empty() {
                        return Ok(None);
                    }
                    match decode_fixed_header(&self.buf) {
                        Ok((header, header_len)) => {
                            let total_len = header_len + header.remaining_length as usize;
                            self.state = FrameState::ReadingBody {
                                header,
                                header_len,
                                total_len,
                            };
                            // fall through to ReadingBody
                        }
                        Err(Error::MalformedPacket("unexpected end of data")) => {
                            return Ok(None);
                        }
                        Err(e) => return Err(e),
                    }
                }
                FrameState::ReadingBody {
                    header,
                    header_len,
                    total_len,
                } => {
                    if self.buf.len() < total_len {
                        return Ok(None);
                    }
                    let body = &self.buf[header_len..total_len];
                    let packet = Packet::decode(header, body)?;
                    self.buf.drain(..total_len);
                    self.state = FrameState::ReadingHeader;
                    return Ok(Some(packet));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::types::*;
    use crate::codec::properties::Properties;

    #[test]
    fn complete_packet_in_one_push() {
        let pkt = PublishPacket {
            topic: String::from("t"),
            packet_id: None,
            payload: b"hi".to_vec(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
        };
        let bytes = pkt.encode().unwrap();

        let mut reader = FrameReader::new();
        reader.push(&bytes);
        let decoded = reader.try_decode().unwrap().unwrap();
        match decoded {
            Packet::Publish(p) => {
                assert_eq!(p.topic, "t");
                assert_eq!(p.payload, b"hi");
            }
            _ => panic!("expected Publish"),
        }
        assert!(reader.try_decode().unwrap().is_none());
    }

    #[test]
    fn partial_then_complete() {
        let pkt = PublishPacket {
            topic: String::from("test"),
            packet_id: None,
            payload: b"hello".to_vec(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
        };
        let bytes = pkt.encode().unwrap();

        let mut reader = FrameReader::new();

        // Feed one byte at a time
        for (i, &byte) in bytes.iter().enumerate() {
            reader.push(&[byte]);
            let result = reader.try_decode().unwrap();
            if i < bytes.len() - 1 {
                assert!(result.is_none(), "should not decode yet at byte {i}");
            } else {
                assert!(result.is_some(), "should decode at final byte");
            }
        }
    }

    #[test]
    fn two_packets_concatenated() {
        let pkt1 = PublishPacket {
            topic: String::from("a"),
            packet_id: None,
            payload: b"1".to_vec(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
        };
        let pkt2 = PublishPacket {
            topic: String::from("b"),
            packet_id: None,
            payload: b"2".to_vec(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
        };
        let mut bytes = pkt1.encode().unwrap();
        bytes.extend_from_slice(&pkt2.encode().unwrap());

        let mut reader = FrameReader::new();
        reader.push(&bytes);

        let d1 = reader.try_decode().unwrap().unwrap();
        let d2 = reader.try_decode().unwrap().unwrap();
        assert!(reader.try_decode().unwrap().is_none());

        match (d1, d2) {
            (Packet::Publish(p1), Packet::Publish(p2)) => {
                assert_eq!(p1.topic, "a");
                assert_eq!(p2.topic, "b");
            }
            _ => panic!("expected two Publish packets"),
        }
    }

    #[test]
    fn empty_push() {
        let mut reader = FrameReader::new();
        assert!(reader.try_decode().unwrap().is_none());
        reader.push(&[]);
        assert!(reader.try_decode().unwrap().is_none());
    }

    #[test]
    fn pingresp() {
        let mut reader = FrameReader::new();
        reader.push(&[0xD0, 0x00]); // PINGRESP
        match reader.try_decode().unwrap().unwrap() {
            Packet::PingResp => {}
            _ => panic!("expected PingResp"),
        }
    }
}
