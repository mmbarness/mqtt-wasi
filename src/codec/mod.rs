pub mod connect;
pub mod decode;
pub mod encode;
pub mod ping;
pub mod properties;
pub mod publish;
pub mod subscribe;
pub(crate) mod topic;
pub mod types;

use crate::codec::types::*;
use crate::error::{Error, Result};

impl Packet {
    /// Decode a packet from its fixed header and body bytes.
    pub fn decode(header: FixedHeader, body: &[u8]) -> Result<Self> {
        crate::codec::decode::validate_fixed_header(header)?;
        if body.len() != header.remaining_length as usize {
            return Err(Error::MalformedPacket("body length does not match header"));
        }
        match header.packet_type {
            PacketType::ConnAck => Ok(Packet::ConnAck(ConnAckPacket::decode(body)?)),
            PacketType::Publish => Ok(Packet::Publish(PublishPacket::decode(header.flags, body)?)),
            PacketType::PubAck => Ok(Packet::PubAck(PubAckPacket::decode(body)?)),
            PacketType::SubAck => Ok(Packet::SubAck(SubAckPacket::decode(body)?)),
            PacketType::UnsubAck => Ok(Packet::UnsubAck(UnsubAckPacket::decode(body)?)),
            PacketType::PingResp if body.is_empty() => Ok(Packet::PingResp),
            PacketType::PingResp => Err(Error::MalformedPacket("PINGRESP body must be empty")),
            PacketType::Disconnect => Ok(Packet::Disconnect(DisconnectPacket::decode(body)?)),
            PacketType::PingReq if body.is_empty() => Ok(Packet::PingReq),
            PacketType::PingReq => Err(Error::MalformedPacket("PINGREQ body must be empty")),
            // QoS 2 packets — not implemented but must be recognized to avoid
            // crashing the connection if a broker sends them.
            PacketType::PubRec | PacketType::PubRel | PacketType::PubComp => {
                Err(Error::UnexpectedPacket("QoS 2 not supported"))
            }
            // Enhanced authentication — not implemented.
            PacketType::Auth => Err(Error::UnexpectedPacket("AUTH not supported")),
            // Client-to-server only; we shouldn't receive them.
            PacketType::Connect | PacketType::Subscribe | PacketType::Unsubscribe => {
                Err(Error::UnexpectedPacket("client-to-server packet type"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_packets_require_empty_bodies() {
        let header = FixedHeader {
            packet_type: PacketType::PingResp,
            flags: 0,
            remaining_length: 1,
        };
        assert!(matches!(
            Packet::decode(header, &[0]),
            Err(Error::MalformedPacket("PINGRESP body must be empty"))
        ));
    }

    #[test]
    fn body_length_must_match_fixed_header() {
        let header = FixedHeader {
            packet_type: PacketType::PingResp,
            flags: 0,
            remaining_length: 0,
        };
        assert!(matches!(
            Packet::decode(header, &[0]),
            Err(Error::MalformedPacket("body length does not match header"))
        ));
    }
}
