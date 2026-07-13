#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::codec::decode::Cursor;
use crate::codec::encode::encode_fixed_header;
use crate::codec::properties::{Properties, PropertyContext};
use crate::codec::types::{DisconnectPacket, PacketType};
use crate::error::Result;

pub const PINGREQ_BYTES: [u8; 2] = [0xC0, 0x00];
pub const PINGRESP_BYTES: [u8; 2] = [0xD0, 0x00];

impl DisconnectPacket {
    pub fn encode(&self) -> Result<Vec<u8>> {
        if !is_disconnect_reason_code(self.reason_code) {
            return Err(crate::error::Error::InvalidReasonCode(self.reason_code));
        }
        let mut packet = Vec::new();
        if self.reason_code == 0x00 {
            // Normal disconnect: empty body is valid
            encode_fixed_header(&mut packet, PacketType::Disconnect, 0, 0)?;
        } else {
            encode_fixed_header(&mut packet, PacketType::Disconnect, 0, 1)?;
            packet.push(self.reason_code);
        }
        Ok(packet)
    }

    pub fn decode(body: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(body);
        let reason_code = if body.is_empty() {
            0x00
        } else {
            cur.read_u8()?
        };
        if !is_disconnect_reason_code(reason_code) {
            return Err(crate::error::Error::InvalidReasonCode(reason_code));
        }
        if cur.remaining() > 0 {
            Properties::decode_for(&mut cur, PropertyContext::Disconnect)?;
        }
        if cur.remaining() != 0 {
            return Err(crate::error::Error::MalformedPacket(
                "trailing DISCONNECT bytes",
            ));
        }
        Ok(DisconnectPacket { reason_code })
    }
}

fn is_disconnect_reason_code(code: u8) -> bool {
    matches!(
        code,
        0x00 | 0x04
            | 0x80
            | 0x81
            | 0x82
            | 0x83
            | 0x87
            | 0x89
            | 0x8B
            | 0x8D
            | 0x8E
            | 0x8F
            | 0x90
            | 0x93
            | 0x94
            | 0x95
            | 0x96
            | 0x97
            | 0x98
            | 0x99
            | 0x9A
            | 0x9B
            | 0x9C
            | 0x9D
            | 0x9E
            | 0x9F
            | 0xA0
            | 0xA1
            | 0xA2
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::decode::decode_fixed_header;

    #[test]
    fn pingreq_bytes() {
        let (header, consumed) = decode_fixed_header(&PINGREQ_BYTES).unwrap();
        assert_eq!(header.packet_type, PacketType::PingReq);
        assert_eq!(header.remaining_length, 0);
        assert_eq!(consumed, 2);
    }

    #[test]
    fn pingresp_bytes() {
        let (header, consumed) = decode_fixed_header(&PINGRESP_BYTES).unwrap();
        assert_eq!(header.packet_type, PacketType::PingResp);
        assert_eq!(header.remaining_length, 0);
        assert_eq!(consumed, 2);
    }

    #[test]
    fn disconnect_normal() {
        let pkt = DisconnectPacket { reason_code: 0x00 };
        let bytes = pkt.encode().unwrap();
        assert_eq!(bytes, [0xE0, 0x00]);

        let (_header, hdr_len) = decode_fixed_header(&bytes).unwrap();
        let decoded = DisconnectPacket::decode(&bytes[hdr_len..]).unwrap();
        assert_eq!(decoded.reason_code, 0x00);
    }

    #[test]
    fn disconnect_with_reason() {
        let pkt = DisconnectPacket { reason_code: 0x04 };
        let bytes = pkt.encode().unwrap();

        let (_, hdr_len) = decode_fixed_header(&bytes).unwrap();
        let decoded = DisconnectPacket::decode(&bytes[hdr_len..]).unwrap();
        assert_eq!(decoded.reason_code, 0x04);
    }

    #[test]
    fn disconnect_accepts_properties_but_rejects_trailing_bytes() {
        // Reason 0x80 and one Reason String property "x".
        let valid = [0x80, 0x04, 0x1F, 0x00, 0x01, b'x'];
        let packet = DisconnectPacket::decode(&valid).unwrap();
        assert_eq!(packet.reason_code, 0x80);

        // Empty property section followed by an impossible trailing byte.
        assert!(matches!(
            DisconnectPacket::decode(&[0x80, 0x00, 0xFF]),
            Err(crate::error::Error::MalformedPacket(
                "trailing DISCONNECT bytes"
            ))
        ));

        // Server Reference is legal but intentionally not retained by the API.
        assert!(DisconnectPacket::decode(&[0x80, 0x04, 0x1C, 0x00, 0x01, b's']).is_ok());

        // Session Expiry Interval is not legal in a server-to-client DISCONNECT.
        assert!(matches!(
            DisconnectPacket::decode(&[0x80, 0x05, 0x11, 0x00, 0x00, 0x00, 0x01]),
            Err(crate::error::Error::MalformedPacket(
                "property is not allowed in this packet"
            ))
        ));
    }

    #[test]
    fn disconnect_rejects_reserved_reason_code() {
        assert!(matches!(
            DisconnectPacket::decode(&[0x03]),
            Err(crate::error::Error::InvalidReasonCode(0x03))
        ));
    }
}
