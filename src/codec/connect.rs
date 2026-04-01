#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::codec::decode::Cursor;
use crate::codec::encode::{self, encode_fixed_header, encode_string, encode_u16};
use crate::codec::properties::Properties;
use crate::codec::types::{ConnAckPacket, ConnectPacket, PacketType};
use crate::error::Result;

const PROTOCOL_NAME: &str = "MQTT";
const PROTOCOL_VERSION_5: u8 = 5;

impl ConnectPacket {
    pub fn encode(&self) -> Result<Vec<u8>> {
        // Build the variable header + payload into a body buffer first,
        // then prepend the fixed header with the correct remaining length.
        let mut body = Vec::new();

        // Variable header: protocol name, version, flags, keep-alive
        encode_string(&mut body, PROTOCOL_NAME)?;
        body.push(self.protocol_version);

        // Connect flags
        let mut flags: u8 = 0;
        if self.clean_start {
            flags |= 0x02;
        }
        if self.password.is_some() {
            flags |= 0x40;
        }
        if self.username.is_some() {
            flags |= 0x80;
        }
        body.push(flags);
        encode_u16(&mut body, self.keep_alive);

        // Properties (v5)
        if self.protocol_version >= PROTOCOL_VERSION_5 {
            self.properties.encode(&mut body)?;
        }

        // Payload: client ID (always present), then optional username/password
        encode_string(&mut body, &self.client_id)?;
        if let Some(ref username) = self.username {
            encode_string(&mut body, username)?;
        }
        if let Some(ref password) = self.password {
            encode::encode_binary(&mut body, password)?;
        }

        // Fixed header
        let mut packet = Vec::new();
        encode_fixed_header(&mut packet, PacketType::Connect, 0, body.len() as u32)?;
        packet.extend_from_slice(&body);
        Ok(packet)
    }
}

impl ConnAckPacket {
    pub fn decode(body: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(body);

        // Byte 1: connect acknowledge flags (only bit 0 = session present)
        let ack_flags = cur.read_u8()?;
        let session_present = (ack_flags & 0x01) != 0;

        // Byte 2: reason code
        let reason_code = cur.read_u8()?;

        // Properties (v5) — present if there are remaining bytes
        let properties = if cur.remaining() > 0 {
            Properties::decode(&mut cur)?
        } else {
            Properties::new()
        };

        Ok(ConnAckPacket {
            session_present,
            reason_code,
            properties,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::decode::decode_fixed_header;

    #[test]
    fn connect_encode_minimal() {
        let pkt = ConnectPacket {
            protocol_version: 5,
            clean_start: true,
            keep_alive: 60,
            client_id: String::new(),
            username: None,
            password: None,
            properties: Properties::new(),
        };
        let bytes = pkt.encode().unwrap();

        // Verify fixed header
        let (header, hdr_len) = decode_fixed_header(&bytes).unwrap();
        assert_eq!(header.packet_type, PacketType::Connect);
        assert_eq!(header.flags, 0);

        // Verify protocol name
        let body = &bytes[hdr_len..];
        let mut cur = Cursor::new(body);
        assert_eq!(cur.read_string().unwrap(), "MQTT");
        assert_eq!(cur.read_u8().unwrap(), 5); // version
        assert_eq!(cur.read_u8().unwrap(), 0x02); // clean start flag
        assert_eq!(cur.read_u16().unwrap(), 60); // keep alive
    }

    #[test]
    fn connect_with_credentials() {
        let pkt = ConnectPacket {
            protocol_version: 5,
            clean_start: true,
            keep_alive: 30,
            client_id: String::from("test-client"),
            username: Some(String::from("user")),
            password: Some(b"pass".to_vec()),
            properties: Properties::new(),
        };
        let bytes = pkt.encode().unwrap();

        let (header, _) = decode_fixed_header(&bytes).unwrap();
        assert_eq!(header.packet_type, PacketType::Connect);
    }

    #[test]
    fn connack_decode_success() {
        // session_present=false, reason=0x00 (success), empty properties
        let body = [0x00, 0x00, 0x00];
        let pkt = ConnAckPacket::decode(&body).unwrap();
        assert!(!pkt.session_present);
        assert_eq!(pkt.reason_code, 0x00);
    }

    #[test]
    fn connack_decode_refused() {
        let body = [0x00, 0x87, 0x00]; // not authorized
        let pkt = ConnAckPacket::decode(&body).unwrap();
        assert_eq!(pkt.reason_code, 0x87);
    }
}
