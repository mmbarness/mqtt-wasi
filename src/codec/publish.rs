#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::codec::decode::Cursor;
use crate::codec::encode::{self, encode_fixed_header};
use crate::codec::properties::Properties;
use crate::codec::types::{PacketType, PubAckPacket, PublishPacket, QoS};
use crate::error::Result;

impl PublishPacket {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut body = Vec::new();

        // Variable header: topic name
        encode::encode_string(&mut body, &self.topic)?;

        // Packet ID (only for QoS 1)
        if let Some(id) = self.packet_id {
            encode::encode_u16(&mut body, id);
        }

        // Properties (v5)
        self.properties.encode(&mut body)?;

        // Payload
        body.extend_from_slice(&self.payload);

        // Fixed header flags: DUP(3) QoS(2-1) RETAIN(0)
        let mut flags: u8 = 0;
        if self.dup {
            flags |= 0x08;
        }
        flags |= (self.qos as u8) << 1;
        if self.retain {
            flags |= 0x01;
        }

        let mut packet = Vec::new();
        encode_fixed_header(&mut packet, PacketType::Publish, flags, body.len() as u32)?;
        packet.extend_from_slice(&body);
        Ok(packet)
    }

    pub fn decode(flags: u8, body: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(body);

        let dup = (flags & 0x08) != 0;
        let qos = QoS::from_u8((flags >> 1) & 0x03)?;
        let retain = (flags & 0x01) != 0;

        let topic = cur.read_string()?;
        let packet_id = if qos != QoS::AtMostOnce {
            Some(cur.read_u16()?)
        } else {
            None
        };

        let properties = Properties::decode(&mut cur)?;
        let payload = cur.remaining_bytes().to_vec();

        Ok(PublishPacket {
            topic,
            packet_id,
            payload,
            qos,
            retain,
            dup,
            properties,
        })
    }
}

impl PubAckPacket {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut packet = Vec::new();

        // Optimization: if reason is success, remaining length = 2 (just packet ID)
        if self.reason_code == 0x00 {
            encode_fixed_header(&mut packet, PacketType::PubAck, 0, 2)?;
            encode::encode_u16(&mut packet, self.packet_id);
        } else {
            encode_fixed_header(&mut packet, PacketType::PubAck, 0, 3)?;
            encode::encode_u16(&mut packet, self.packet_id);
            packet.push(self.reason_code);
        }
        Ok(packet)
    }

    pub fn decode(body: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(body);
        let packet_id = cur.read_u16()?;
        let reason_code = if cur.remaining() > 0 { cur.read_u8()? } else { 0x00 };
        Ok(PubAckPacket {
            packet_id,
            reason_code,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::decode::decode_fixed_header;

    #[test]
    fn publish_qos0_round_trip() {
        let pkt = PublishPacket {
            topic: String::from("test/topic"),
            packet_id: None,
            payload: b"hello".to_vec(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
        };
        let bytes = pkt.encode().unwrap();
        let (header, hdr_len) = decode_fixed_header(&bytes).unwrap();
        assert_eq!(header.packet_type, PacketType::Publish);

        let decoded = PublishPacket::decode(header.flags, &bytes[hdr_len..]).unwrap();
        assert_eq!(decoded.topic, "test/topic");
        assert_eq!(decoded.payload, b"hello");
        assert_eq!(decoded.qos, QoS::AtMostOnce);
        assert_eq!(decoded.packet_id, None);
    }

    #[test]
    fn publish_qos1_round_trip() {
        let pkt = PublishPacket {
            topic: String::from("a/b"),
            packet_id: Some(42),
            payload: b"data".to_vec(),
            qos: QoS::AtLeastOnce,
            retain: true,
            dup: false,
            properties: Properties::new(),
        };
        let bytes = pkt.encode().unwrap();
        let (header, hdr_len) = decode_fixed_header(&bytes).unwrap();

        let decoded = PublishPacket::decode(header.flags, &bytes[hdr_len..]).unwrap();
        assert_eq!(decoded.topic, "a/b");
        assert_eq!(decoded.packet_id, Some(42));
        assert_eq!(decoded.qos, QoS::AtLeastOnce);
        assert!(decoded.retain);
    }

    #[test]
    fn puback_round_trip() {
        let pkt = PubAckPacket {
            packet_id: 7,
            reason_code: 0x00,
        };
        let bytes = pkt.encode().unwrap();
        let (header, hdr_len) = decode_fixed_header(&bytes).unwrap();
        assert_eq!(header.packet_type, PacketType::PubAck);

        let decoded = PubAckPacket::decode(&bytes[hdr_len..]).unwrap();
        assert_eq!(decoded.packet_id, 7);
        assert_eq!(decoded.reason_code, 0x00);
    }
}
