#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::codec::decode::Cursor;
use crate::codec::encode::{self, encode_fixed_header};
use crate::codec::properties::{Properties, PropertyContext};
use crate::codec::topic::validate_topic_filter;
#[cfg(any(feature = "std", test))]
use crate::codec::types::QoS;
use crate::codec::types::{
    PacketType, SubAckPacket, SubscribePacket, UnsubAckPacket, UnsubscribePacket,
};
#[cfg(any(feature = "std", test))]
use crate::error::Error;
use crate::error::Result;

#[cfg(any(feature = "std", test))]
pub(crate) fn validate_suback_codes(reason_codes: &[u8], requested_qos: &[QoS]) -> Result<()> {
    if reason_codes.len() != requested_qos.len() {
        return Err(Error::MalformedPacket("ack reason code count mismatch"));
    }
    for (&granted, &requested) in reason_codes.iter().zip(requested_qos) {
        if granted >= 0x80 {
            return Err(Error::AckRejected {
                packet: "SUBACK",
                reason_code: granted,
            });
        }
        if granted == 2 || granted > requested as u8 {
            return Err(Error::MalformedPacket(
                "SUBACK granted unsupported or unrequested QoS",
            ));
        }
    }
    Ok(())
}

impl SubscribePacket {
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.packet_id == 0 {
            return Err(crate::error::Error::MalformedPacket(
                "SUBSCRIBE packet identifier is zero",
            ));
        }
        if self.filters.is_empty() {
            return Err(crate::error::Error::MalformedPacket(
                "SUBSCRIBE has no filters",
            ));
        }
        let mut body = Vec::new();

        // Variable header: packet ID
        encode::encode_u16(&mut body, self.packet_id);

        // Properties
        self.properties
            .encode_for(&mut body, PropertyContext::Subscribe)?;

        // Payload: topic filter + subscription options (QoS byte)
        for (filter, qos) in &self.filters {
            validate_topic_filter(filter)?;
            encode::encode_string(&mut body, filter)?;
            body.push(*qos as u8);
        }

        // Fixed header — SUBSCRIBE flags must be 0x02 per spec
        let mut packet = Vec::new();
        encode_fixed_header(&mut packet, PacketType::Subscribe, 0x02, body.len() as u32)?;
        packet.extend_from_slice(&body);
        Ok(packet)
    }
}

impl SubAckPacket {
    pub fn decode(body: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(body);
        let packet_id = cur.read_u16()?;
        if packet_id == 0 {
            return Err(crate::error::Error::MalformedPacket(
                "SUBACK packet identifier is zero",
            ));
        }
        let _properties = Properties::decode_for(&mut cur, PropertyContext::SubAck)?;
        let reason_codes = cur.remaining_bytes().to_vec();
        if reason_codes.is_empty() {
            return Err(crate::error::Error::MalformedPacket(
                "SUBACK has no reason codes",
            ));
        }
        if let Some(invalid) = reason_codes
            .iter()
            .find(|code| !is_suback_reason_code(**code))
        {
            return Err(crate::error::Error::InvalidReasonCode(*invalid));
        }
        Ok(SubAckPacket {
            packet_id,
            reason_codes,
        })
    }
}

impl UnsubscribePacket {
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.packet_id == 0 {
            return Err(crate::error::Error::MalformedPacket(
                "UNSUBSCRIBE packet identifier is zero",
            ));
        }
        if self.filters.is_empty() {
            return Err(crate::error::Error::MalformedPacket(
                "UNSUBSCRIBE has no filters",
            ));
        }
        let mut body = Vec::new();

        encode::encode_u16(&mut body, self.packet_id);
        self.properties
            .encode_for(&mut body, PropertyContext::Unsubscribe)?;

        for filter in &self.filters {
            validate_topic_filter(filter)?;
            encode::encode_string(&mut body, filter)?;
        }

        // UNSUBSCRIBE flags must be 0x02 per spec
        let mut packet = Vec::new();
        encode_fixed_header(
            &mut packet,
            PacketType::Unsubscribe,
            0x02,
            body.len() as u32,
        )?;
        packet.extend_from_slice(&body);
        Ok(packet)
    }
}

impl UnsubAckPacket {
    pub fn decode(body: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(body);
        let packet_id = cur.read_u16()?;
        if packet_id == 0 {
            return Err(crate::error::Error::MalformedPacket(
                "UNSUBACK packet identifier is zero",
            ));
        }
        let _properties = Properties::decode_for(&mut cur, PropertyContext::UnsubAck)?;
        let reason_codes = cur.remaining_bytes().to_vec();
        if reason_codes.is_empty() {
            return Err(crate::error::Error::MalformedPacket(
                "UNSUBACK has no reason codes",
            ));
        }
        if let Some(invalid) = reason_codes
            .iter()
            .find(|code| !is_unsuback_reason_code(**code))
        {
            return Err(crate::error::Error::InvalidReasonCode(*invalid));
        }
        Ok(UnsubAckPacket {
            packet_id,
            reason_codes,
        })
    }
}

fn is_suback_reason_code(code: u8) -> bool {
    matches!(
        code,
        0x00 | 0x01 | 0x02 | 0x80 | 0x83 | 0x87 | 0x8F | 0x91 | 0x97 | 0x9E | 0xA1 | 0xA2
    )
}

fn is_unsuback_reason_code(code: u8) -> bool {
    matches!(code, 0x00 | 0x11 | 0x80 | 0x83 | 0x87 | 0x8F | 0x91)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::decode::decode_fixed_header;
    use crate::codec::properties::PropertyValue;
    use crate::codec::types::QoS;
    #[cfg(not(feature = "std"))]
    use alloc::{string::String, vec};

    #[test]
    fn subscribe_encode() {
        let pkt = SubscribePacket {
            packet_id: 1,
            filters: vec![(String::from("test/#"), QoS::AtLeastOnce)],
            properties: Properties::new(),
        };
        let bytes = pkt.encode().unwrap();
        let (header, _) = decode_fixed_header(&bytes).unwrap();
        assert_eq!(header.packet_type, PacketType::Subscribe);
        assert_eq!(header.flags, 0x02);
    }

    #[test]
    fn suback_decode() {
        // packet_id=1, empty properties, two reason codes (0x00, 0x01)
        let body = [0x00, 0x01, 0x00, 0x00, 0x01];
        let pkt = SubAckPacket::decode(&body).unwrap();
        assert_eq!(pkt.packet_id, 1);
        assert_eq!(pkt.reason_codes, [0x00, 0x01]);
    }

    #[test]
    fn unsubscribe_encode() {
        let pkt = UnsubscribePacket {
            packet_id: 2,
            filters: vec![String::from("test/#")],
            properties: Properties::new(),
        };
        let bytes = pkt.encode().unwrap();
        let (header, _) = decode_fixed_header(&bytes).unwrap();
        assert_eq!(header.packet_type, PacketType::Unsubscribe);
        assert_eq!(header.flags, 0x02);
    }

    #[test]
    fn subscription_packets_accept_shared_filters_and_reject_malformed_filters() {
        let shared = String::from("$share/workers/events/+");
        assert!(SubscribePacket {
            packet_id: 1,
            filters: vec![(shared.clone(), QoS::AtLeastOnce)],
            properties: Properties::new(),
        }
        .encode()
        .is_ok());
        assert!(UnsubscribePacket {
            packet_id: 2,
            filters: vec![shared],
            properties: Properties::new(),
        }
        .encode()
        .is_ok());

        for filter in ["", "events/#/new", "events/+new", "$share//events"] {
            assert!(SubscribePacket {
                packet_id: 1,
                filters: vec![(String::from(filter), QoS::AtMostOnce)],
                properties: Properties::new(),
            }
            .encode()
            .is_err());
            assert!(UnsubscribePacket {
                packet_id: 2,
                filters: vec![String::from(filter)],
                properties: Properties::new(),
            }
            .encode()
            .is_err());
        }
    }

    #[test]
    fn subscription_packets_enforce_their_property_contexts() {
        let response_topic = Properties::new().with_response_topic("responses/client");
        assert!(matches!(
            SubscribePacket {
                packet_id: 1,
                filters: vec![(String::from("events/#"), QoS::AtMostOnce)],
                properties: response_topic.clone(),
            }
            .encode(),
            Err(Error::MalformedPacket(
                "property is not allowed in this packet"
            ))
        ));
        assert!(matches!(
            UnsubscribePacket {
                packet_id: 2,
                filters: vec![String::from("events/#")],
                properties: response_topic,
            }
            .encode(),
            Err(Error::MalformedPacket(
                "property is not allowed in this packet"
            ))
        ));

        let response_topic_section = [0x00, 0x01, 0x04, 0x08, 0x00, 0x01, b'r', 0x00];
        assert!(matches!(
            SubAckPacket::decode(&response_topic_section),
            Err(Error::MalformedPacket(
                "property is not allowed in this packet"
            ))
        ));
        assert!(matches!(
            UnsubAckPacket::decode(&response_topic_section),
            Err(Error::MalformedPacket(
                "property is not allowed in this packet"
            ))
        ));

        let mut reason = Properties::new();
        reason.push(
            crate::codec::properties::PropertyId::ReasonString,
            PropertyValue::Str(String::from("ok")),
        );
        assert!(reason
            .encode_for(&mut Vec::new(), PropertyContext::SubAck)
            .is_ok());
    }

    #[test]
    fn unsuback_decode() {
        let body = [0x00, 0x02, 0x00, 0x00];
        let pkt = UnsubAckPacket::decode(&body).unwrap();
        assert_eq!(pkt.packet_id, 2);
        assert_eq!(pkt.reason_codes, [0x00]);
    }

    #[test]
    fn subscription_packets_reject_zero_identifiers_and_empty_payloads() {
        let subscribe = SubscribePacket {
            packet_id: 0,
            filters: vec![(String::from("test/#"), QoS::AtMostOnce)],
            properties: Properties::new(),
        };
        assert!(subscribe.encode().is_err());
        assert!(SubAckPacket::decode(&[0, 0, 0, 0]).is_err());
        assert!(SubAckPacket::decode(&[0, 1, 0]).is_err());
        assert!(UnsubAckPacket::decode(&[0, 1, 0]).is_err());
    }

    #[test]
    fn acknowledgement_packets_reject_reserved_reason_codes() {
        assert!(matches!(
            SubAckPacket::decode(&[0, 1, 0, 0x03]),
            Err(crate::error::Error::InvalidReasonCode(0x03))
        ));
        assert!(matches!(
            UnsubAckPacket::decode(&[0, 1, 0, 0x03]),
            Err(crate::error::Error::InvalidReasonCode(0x03))
        ));
    }

    #[test]
    fn suback_grants_cannot_exceed_requested_or_supported_qos() {
        assert!(validate_suback_codes(&[0], &[QoS::AtMostOnce]).is_ok());
        assert!(validate_suback_codes(&[1], &[QoS::AtLeastOnce]).is_ok());
        assert!(matches!(
            validate_suback_codes(&[1], &[QoS::AtMostOnce]),
            Err(Error::MalformedPacket(
                "SUBACK granted unsupported or unrequested QoS"
            ))
        ));
        assert!(matches!(
            validate_suback_codes(&[2], &[QoS::AtLeastOnce]),
            Err(Error::MalformedPacket(
                "SUBACK granted unsupported or unrequested QoS"
            ))
        ));
    }
}
