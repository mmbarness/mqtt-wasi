#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};

use crate::codec::properties::Properties;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedHeader {
    pub packet_type: PacketType,
    pub flags: u8,
    pub remaining_length: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectPacket {
    pub protocol_version: u8,
    pub clean_start: bool,
    pub keep_alive: u16,
    pub client_id: String,
    pub username: Option<String>,
    pub password: Option<Vec<u8>>,
    pub properties: Properties,
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
