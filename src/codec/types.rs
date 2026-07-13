#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};
use core::fmt;

use crate::codec::properties::Properties;

/// MQTT v5 control packet type (4-bit value in the fixed header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    Connect = 1,
    ConnAck = 2,
    Publish = 3,
    PubAck = 4,
    PubRec = 5,
    PubRel = 6,
    PubComp = 7,
    Subscribe = 8,
    SubAck = 9,
    Unsubscribe = 10,
    UnsubAck = 11,
    PingReq = 12,
    PingResp = 13,
    Disconnect = 14,
    Auth = 15,
}

impl PacketType {
    pub fn from_u8(val: u8) -> crate::error::Result<Self> {
        match val {
            1 => Ok(PacketType::Connect),
            2 => Ok(PacketType::ConnAck),
            3 => Ok(PacketType::Publish),
            4 => Ok(PacketType::PubAck),
            5 => Ok(PacketType::PubRec),
            6 => Ok(PacketType::PubRel),
            7 => Ok(PacketType::PubComp),
            8 => Ok(PacketType::Subscribe),
            9 => Ok(PacketType::SubAck),
            10 => Ok(PacketType::Unsubscribe),
            11 => Ok(PacketType::UnsubAck),
            12 => Ok(PacketType::PingReq),
            13 => Ok(PacketType::PingResp),
            14 => Ok(PacketType::Disconnect),
            15 => Ok(PacketType::Auth),
            _ => Err(crate::error::Error::InvalidPacketType(val)),
        }
    }
}

/// MQTT Quality of Service level. Only QoS 0 and 1 are supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum QoS {
    AtMostOnce = 0,
    AtLeastOnce = 1,
}

impl QoS {
    pub fn from_u8(val: u8) -> crate::error::Result<Self> {
        match val {
            0 => Ok(QoS::AtMostOnce),
            1 => Ok(QoS::AtLeastOnce),
            _ => Err(crate::error::Error::InvalidQoS(val)),
        }
    }
}

/// Decoded MQTT fixed header (first 2-5 bytes of every packet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedHeader {
    pub packet_type: PacketType,
    pub flags: u8,
    pub remaining_length: u32,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ConnectPacket {
    pub protocol_version: u8,
    pub clean_start: bool,
    pub keep_alive: u16,
    pub client_id: String,
    pub username: Option<String>,
    pub password: Option<Vec<u8>>,
    pub properties: Properties,
}

impl fmt::Debug for ConnectPacket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectPacket")
            .field("protocol_version", &self.protocol_version)
            .field("clean_start", &self.clean_start)
            .field("keep_alive", &self.keep_alive)
            .field("client_id", &self.client_id)
            .field("username_set", &self.username.is_some())
            .field("password_set", &self.password.is_some())
            .field("properties", &self.properties)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnAckPacket {
    pub session_present: bool,
    pub reason_code: u8,
    pub properties: Properties,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishPacket {
    pub topic: String,
    pub packet_id: Option<u16>,
    pub payload: Vec<u8>,
    pub qos: QoS,
    pub retain: bool,
    pub dup: bool,
    pub properties: Properties,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PubAckPacket {
    pub packet_id: u16,
    pub reason_code: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscribePacket {
    pub packet_id: u16,
    pub filters: Vec<(String, QoS)>,
    pub properties: Properties,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubAckPacket {
    pub packet_id: u16,
    pub reason_codes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsubscribePacket {
    pub packet_id: u16,
    pub filters: Vec<String>,
    pub properties: Properties,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsubAckPacket {
    pub packet_id: u16,
    pub reason_codes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisconnectPacket {
    pub reason_code: u8,
}

/// A decoded MQTT v5 control packet.
#[derive(Debug, Clone, PartialEq)]
pub enum Packet {
    Connect(ConnectPacket),
    ConnAck(ConnAckPacket),
    Publish(PublishPacket),
    PubAck(PubAckPacket),
    Subscribe(SubscribePacket),
    SubAck(SubAckPacket),
    Unsubscribe(UnsubscribePacket),
    UnsubAck(UnsubAckPacket),
    PingReq,
    PingResp,
    Disconnect(DisconnectPacket),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_packet_debug_redacts_credentials_directly_and_in_packet() {
        let connect = ConnectPacket {
            protocol_version: 5,
            clean_start: true,
            keep_alive: 30,
            client_id: String::from("debug-client"),
            username: Some(String::from("visible-user")),
            password: Some(b"super-secret".to_vec()),
            properties: Properties::new(),
        };

        for debug in [
            format!("{connect:?}"),
            format!("{:?}", Packet::Connect(connect)),
        ] {
            assert!(debug.contains("username_set: true"));
            assert!(debug.contains("password_set: true"));
            assert!(!debug.contains("visible-user"));
            assert!(!debug.contains("super-secret"));
            assert!(!debug.contains("[115, 117, 112"));
        }
    }
}
