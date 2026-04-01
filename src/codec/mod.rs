pub mod types;
pub mod encode;
pub mod decode;
pub mod properties;
pub mod connect;
pub mod publish;
pub mod subscribe;
pub mod ping;

use crate::codec::types::*;
use crate::error::{Error, Result};

impl Packet {
    /// Decode a packet from its fixed header and body bytes.
    pub fn decode(header: FixedHeader, body: &[u8]) -> Result<Self> {
        match header.packet_type {
            PacketType::ConnAck => Ok(Packet::ConnAck(ConnAckPacket::decode(body)?)),
            PacketType::Publish => Ok(Packet::Publish(PublishPacket::decode(header.flags, body)?)),
            PacketType::PubAck => Ok(Packet::PubAck(PubAckPacket::decode(body)?)),
            PacketType::SubAck => Ok(Packet::SubAck(SubAckPacket::decode(body)?)),
            PacketType::UnsubAck => Ok(Packet::UnsubAck(UnsubAckPacket::decode(body)?)),
            PacketType::PingResp => Ok(Packet::PingResp),
            PacketType::Disconnect => Ok(Packet::Disconnect(DisconnectPacket::decode(body)?)),
            PacketType::PingReq => Ok(Packet::PingReq),
            // These are client-to-server only; we shouldn't receive them
            PacketType::Connect | PacketType::Subscribe | PacketType::Unsubscribe => {
                Err(Error::UnexpectedPacket("server-only packet type"))
            }
        }
    }
}
