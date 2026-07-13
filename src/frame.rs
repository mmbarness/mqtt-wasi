use crate::codec::decode::decode_fixed_header;
use crate::codec::types::{FixedHeader, Packet};
use crate::error::{Error, Result};

/// Largest possible MQTT control packet: a five-byte fixed header plus the
/// maximum Remaining Length value.
pub const MQTT_MAX_PACKET_SIZE: usize = 268_435_460;

/// Incremental MQTT frame parser for non-blocking reads.
///
/// Accumulates bytes via `push()` and yields complete packets via
/// `try_decode()`. Handles partial reads gracefully — call `push()`
/// with whatever bytes are available, then `try_decode()` in a loop
/// until it returns `Ok(None)`.
pub struct FrameReader {
    buf: Vec<u8>,
    state: FrameState,
    max_packet_size: usize,
    overflow_size: Option<usize>,
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
        Self::with_max_packet_size(MQTT_MAX_PACKET_SIZE)
    }

    /// Create a reader that rejects complete packets larger than `max` bytes.
    ///
    /// The limit includes the MQTT fixed header. Callers should keep individual
    /// `push` chunks at or below [`FrameReader::remaining_capacity`] when using
    /// a small custom limit.
    pub fn with_max_packet_size(max: usize) -> Self {
        Self {
            buf: Vec::with_capacity(4096),
            state: FrameState::ReadingHeader,
            max_packet_size: max,
            overflow_size: None,
        }
    }

    /// Feed raw bytes from the socket into the parser.
    pub fn push(&mut self, data: &[u8]) {
        let available = self.remaining_capacity();
        if data.len() > available {
            self.buf.extend_from_slice(&data[..available]);
            self.overflow_size = Some(self.buf.len().saturating_add(data.len() - available));
        } else {
            self.buf.extend_from_slice(data);
        }
    }

    /// Maximum number of bytes that can be pushed without exceeding the
    /// configured bound for the packet currently being assembled.
    pub fn remaining_capacity(&self) -> usize {
        let target = match self.state {
            FrameState::ReadingHeader => self.max_packet_size,
            FrameState::ReadingBody { total_len, .. } => total_len,
        };
        target.saturating_sub(self.buf.len())
    }

    pub fn max_packet_size(&self) -> usize {
        self.max_packet_size
    }

    /// Try to extract the next complete packet.
    ///
    /// Returns `Ok(Some(packet))` if a complete packet was decoded,
    /// `Ok(None)` if more data is needed, or `Err` if malformed.
    pub fn try_decode(&mut self) -> Result<Option<Packet>> {
        if let Some(size) = self.overflow_size {
            return Err(Error::PacketTooLarge {
                size,
                max: self.max_packet_size,
            });
        }

        loop {
            match self.state {
                FrameState::ReadingHeader => {
                    if self.buf.is_empty() {
                        return Ok(None);
                    }
                    match decode_fixed_header(&self.buf) {
                        Ok((header, header_len)) => {
                            let total_len = header_len
                                .checked_add(header.remaining_length as usize)
                                .ok_or(Error::MalformedPacket("packet length overflow"))?;
                            if total_len > self.max_packet_size {
                                return Err(Error::PacketTooLarge {
                                    size: total_len,
                                    max: self.max_packet_size,
                                });
                            }
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

impl Default for FrameReader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::properties::Properties;
    use crate::codec::types::*;

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

    #[test]
    fn rejects_packet_as_soon_as_header_exceeds_limit() {
        let mut reader = FrameReader::with_max_packet_size(8);
        // Two-byte header plus ten body bytes.
        reader.push(&[0x30, 0x0A]);

        assert!(matches!(
            reader.try_decode(),
            Err(Error::PacketTooLarge { size: 12, max: 8 })
        ));
    }

    #[test]
    fn capacity_prevents_next_frame_from_overflowing_a_split_packet() {
        let first = PublishPacket {
            topic: String::from("first"),
            packet_id: None,
            payload: b"one".to_vec(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
        }
        .encode()
        .unwrap();
        let second = PublishPacket {
            topic: String::from("second"),
            packet_id: None,
            payload: b"two".to_vec(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
        }
        .encode()
        .unwrap();

        let split = first.len() - 2;
        let mut reader = FrameReader::with_max_packet_size(32);
        reader.push(&first[..split]);
        assert!(reader.try_decode().unwrap().is_none());
        assert_eq!(reader.remaining_capacity(), 2);

        // A socket reader must honor remaining_capacity, leaving packet two in
        // the transport until packet one has been decoded.
        let mut available = Vec::from(&first[split..]);
        available.extend_from_slice(&second);
        let read_len = reader.remaining_capacity().min(available.len());
        reader.push(&available[..read_len]);
        let packet = reader.try_decode().unwrap().unwrap();
        assert!(matches!(packet, Packet::Publish(publish) if publish.topic == "first"));

        reader.push(&available[read_len..]);
        let packet = reader.try_decode().unwrap().unwrap();
        assert!(matches!(packet, Packet::Publish(publish) if publish.topic == "second"));
    }
}
