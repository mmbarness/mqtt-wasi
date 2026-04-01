#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::codec::encode::encode_fixed_header;
use crate::codec::types::{DisconnectPacket, PacketType};
use crate::error::Result;

pub const PINGREQ_BYTES: [u8; 2] = [0xC0, 0x00];
pub const PINGRESP_BYTES: [u8; 2] = [0xD0, 0x00];

impl DisconnectPacket {
    pub fn encode(&self) -> Result<Vec<u8>> {
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
        let reason_code = if body.is_empty() { 0x00 } else { body[0] };
        Ok(DisconnectPacket { reason_code })
    }
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
}
