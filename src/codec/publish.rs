#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::codec::decode::Cursor;
use crate::codec::encode::{self, encode_fixed_header};
use crate::codec::properties::{Properties, PropertyContext};
use crate::codec::topic::validate_topic_name;
use crate::codec::types::{PacketType, PubAckPacket, PublishPacket, QoS};
use crate::error::Result;

impl PublishPacket {
    pub fn encode(&self) -> Result<Vec<u8>> {
        match (self.qos, self.packet_id) {
            (QoS::AtMostOnce, None) => {}
            (QoS::AtMostOnce, Some(_)) => {
                return Err(crate::error::Error::MalformedPacket(
                    "QoS 0 PUBLISH has packet identifier",
                ));
            }
            (QoS::AtLeastOnce, Some(0) | None) => {
                return Err(crate::error::Error::MalformedPacket(
                    "QoS 1 PUBLISH needs non-zero packet identifier",
                ));
            }
            (QoS::AtLeastOnce, Some(_)) => {}
        }
        let mut body = Vec::new();

        // Variable header: topic name
        validate_topic_name(&self.topic)?;
        encode::encode_string(&mut body, &self.topic)?;

        // Packet ID (only for QoS 1)
        if let Some(id) = self.packet_id {
            encode::encode_u16(&mut body, id);
        }

        // Properties (v5)
        self.properties
            .encode_for(&mut body, PropertyContext::Publish)?;

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
        validate_topic_name(&topic)?;
        let packet_id = if qos != QoS::AtMostOnce {
            let packet_id = cur.read_u16()?;
            if packet_id == 0 {
                return Err(crate::error::Error::MalformedPacket(
                    "PUBLISH packet identifier is zero",
                ));
            }
            Some(packet_id)
        } else {
            None
        };

        let properties = Properties::decode_for(&mut cur, PropertyContext::Publish)?;
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
        if self.packet_id == 0 {
            return Err(crate::error::Error::MalformedPacket(
                "PUBACK packet identifier is zero",
            ));
        }
        if !is_puback_reason_code(self.reason_code) {
            return Err(crate::error::Error::InvalidReasonCode(self.reason_code));
        }
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
        if packet_id == 0 {
            return Err(crate::error::Error::MalformedPacket(
                "PUBACK packet identifier is zero",
            ));
        }
        let reason_code = if cur.remaining() > 0 {
            cur.read_u8()?
        } else {
            0x00
        };
        if !is_puback_reason_code(reason_code) {
            return Err(crate::error::Error::InvalidReasonCode(reason_code));
        }
        if cur.remaining() > 0 {
            Properties::decode_for(&mut cur, PropertyContext::PubAck)?;
        }
        if cur.remaining() != 0 {
            return Err(crate::error::Error::MalformedPacket(
                "trailing PUBACK bytes",
            ));
        }
        Ok(PubAckPacket {
            packet_id,
            reason_code,
        })
    }
}

fn is_puback_reason_code(code: u8) -> bool {
    matches!(
        code,
        0x00 | 0x10 | 0x80 | 0x83 | 0x87 | 0x90 | 0x91 | 0x97 | 0x99
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::decode::decode_fixed_header;
    use crate::codec::properties::{PropertyId, PropertyValue};

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
    fn publish_rejects_empty_and_wildcard_topic_names_on_encode_and_decode() {
        for topic in ["", "events/#", "events/+"] {
            let packet = PublishPacket {
                topic: String::from(topic),
                packet_id: None,
                payload: Vec::new(),
                qos: QoS::AtMostOnce,
                retain: false,
                dup: false,
                properties: Properties::new(),
            };
            assert!(packet.encode().is_err());

            let mut body = Vec::new();
            encode::encode_string(&mut body, topic).unwrap();
            Properties::new().encode(&mut body).unwrap();
            assert!(PublishPacket::decode(0, &body).is_err());
        }
    }

    #[test]
    fn publish_rejects_topic_alias_even_with_a_nonempty_topic() {
        let mut body = Vec::new();
        encode::encode_string(&mut body, "events/new").unwrap();
        body.extend_from_slice(&[0x03, 0x23, 0x00, 0x01]);

        assert!(matches!(
            PublishPacket::decode(0, &body),
            Err(crate::error::Error::MalformedPacket(
                "topic alias is not supported"
            ))
        ));
    }

    #[test]
    fn publish_enforces_its_property_context_on_encode_and_decode() {
        let mut properties = Properties::new();
        properties.push(PropertyId::SessionExpiryInterval, PropertyValue::U32(60));
        let packet = PublishPacket {
            topic: String::from("events/new"),
            packet_id: None,
            payload: Vec::new(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties,
        };
        assert!(matches!(
            packet.encode(),
            Err(crate::error::Error::MalformedPacket(
                "property is not allowed in this packet"
            ))
        ));

        let mut body = Vec::new();
        encode::encode_string(&mut body, "events/new").unwrap();
        body.extend_from_slice(&[0x05, 0x11, 0x00, 0x00, 0x00, 0x3C]);
        assert!(matches!(
            PublishPacket::decode(0, &body),
            Err(crate::error::Error::MalformedPacket(
                "property is not allowed in this packet"
            ))
        ));
    }

    #[test]
    fn publish_skips_legal_unsupported_properties_without_consuming_payload() {
        let mut body = Vec::new();
        encode::encode_string(&mut body, "events/new").unwrap();
        body.extend_from_slice(&[
            0x0B, // property length
            0x01, 0x01, // Payload Format Indicator
            0x02, 0x00, 0x00, 0x00, 0x05, // Message Expiry Interval
            0x03, 0x00, 0x01, b'j', // Content Type
        ]);
        body.extend_from_slice(b"payload");

        let packet = PublishPacket::decode(0, &body).unwrap();
        assert_eq!(packet.payload, b"payload");
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

    #[test]
    fn publish_and_puback_reject_zero_packet_identifiers() {
        let publish = PublishPacket {
            topic: String::from("test/topic"),
            packet_id: Some(0),
            payload: Vec::new(),
            qos: QoS::AtLeastOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
        };
        assert!(publish.encode().is_err());
        assert!(PublishPacket::decode(0x02, &[0, 1, b't', 0, 0, 0]).is_err());
        assert!(PubAckPacket::decode(&[0, 0]).is_err());
    }

    #[test]
    fn puback_accepts_properties_but_rejects_trailing_bytes() {
        // packet ID 7, success, one Reason String property "x".
        let valid = [0x00, 0x07, 0x00, 0x04, 0x1F, 0x00, 0x01, b'x'];
        assert!(PubAckPacket::decode(&valid).is_ok());

        // Empty property section followed by an impossible trailing byte.
        let trailing = [0x00, 0x07, 0x00, 0x00, 0xFF];
        assert!(matches!(
            PubAckPacket::decode(&trailing),
            Err(crate::error::Error::MalformedPacket(
                "trailing PUBACK bytes"
            ))
        ));

        let wrong_context = [0x00, 0x07, 0x00, 0x04, 0x08, 0x00, 0x01, b'r'];
        assert!(matches!(
            PubAckPacket::decode(&wrong_context),
            Err(crate::error::Error::MalformedPacket(
                "property is not allowed in this packet"
            ))
        ));
    }

    #[test]
    fn puback_rejects_reserved_reason_code() {
        assert!(matches!(
            PubAckPacket::decode(&[0, 7, 0x03]),
            Err(crate::error::Error::InvalidReasonCode(0x03))
        ));
    }
}
