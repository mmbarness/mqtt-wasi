#[cfg(not(feature = "std"))]
use alloc::{string::String, vec, vec::Vec};

use crate::codec::decode::Cursor;
use crate::codec::encode::{self, encode_fixed_header};
use crate::codec::properties::Properties;
use crate::codec::types::{
    PacketType, SubAckPacket, SubscribePacket, UnsubAckPacket, UnsubscribePacket,
};
use crate::error::Result;

impl SubscribePacket {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut body = Vec::new();

        // Variable header: packet ID
        encode::encode_u16(&mut body, self.packet_id);

        // Properties
        self.properties.encode(&mut body)?;

        // Payload: topic filter + subscription options (QoS byte)
        for (filter, qos) in &self.filters {
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
        let _properties = Properties::decode(&mut cur)?;
        let reason_codes = cur.remaining_bytes().to_vec();
        Ok(SubAckPacket {
            packet_id,
            reason_codes,
        })
    }
}

impl UnsubscribePacket {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut body = Vec::new();

        encode::encode_u16(&mut body, self.packet_id);
        self.properties.encode(&mut body)?;

        for filter in &self.filters {
            encode::encode_string(&mut body, filter)?;
        }

        // UNSUBSCRIBE flags must be 0x02 per spec
        let mut packet = Vec::new();
        encode_fixed_header(&mut packet, PacketType::Unsubscribe, 0x02, body.len() as u32)?;
        packet.extend_from_slice(&body);
        Ok(packet)
    }
}

impl UnsubAckPacket {
    pub fn decode(body: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(body);
        let packet_id = cur.read_u16()?;
        let _properties = Properties::decode(&mut cur)?;
        let reason_codes = cur.remaining_bytes().to_vec();
        Ok(UnsubAckPacket {
            packet_id,
            reason_codes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::decode::decode_fixed_header;
    use crate::codec::types::QoS;

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
    fn unsuback_decode() {
        let body = [0x00, 0x02, 0x00, 0x00];
        let pkt = UnsubAckPacket::decode(&body).unwrap();
        assert_eq!(pkt.packet_id, 2);
        assert_eq!(pkt.reason_codes, [0x00]);
    }
}
