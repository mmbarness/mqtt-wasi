#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::codec::decode::Cursor;
use crate::codec::encode::{self, encode_fixed_header, encode_string, encode_u16};
#[cfg(any(feature = "std", test))]
use crate::codec::properties::PropertyId;
use crate::codec::properties::{Properties, PropertyContext};
#[cfg(any(feature = "std", test))]
use crate::codec::topic::validate_topic_filter;
#[cfg(any(feature = "std", test))]
use crate::codec::types::QoS;
use crate::codec::types::{ConnAckPacket, ConnectPacket, PacketType};
#[cfg(any(feature = "std", test))]
use crate::error::Error;
use crate::error::Result;

const PROTOCOL_NAME: &str = "MQTT";
const PROTOCOL_VERSION_5: u8 = 5;

impl ConnectPacket {
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.protocol_version != PROTOCOL_VERSION_5 {
            return Err(crate::error::Error::MalformedPacket(
                "only MQTT v5 is supported",
            ));
        }
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
            self.properties
                .encode_for(&mut body, PropertyContext::Connect)?;
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

/// Validate CONNACK fields that depend on the corresponding CONNECT packet.
#[cfg(any(feature = "std", test))]
pub(crate) fn validate_connack(
    requested_client_id: &str,
    clean_start: bool,
    connack: &ConnAckPacket,
) -> Result<()> {
    if clean_start && connack.session_present {
        return Err(Error::MalformedPacket(
            "CONNACK has session present after clean start",
        ));
    }
    if requested_client_id.is_empty() && connack.session_present {
        return Err(Error::MalformedPacket(
            "CONNACK has session present for a newly assigned client identifier",
        ));
    }
    if requested_client_id.is_empty()
        && !matches!(
            connack
                .properties
                .get_string(PropertyId::AssignedClientIdentifier),
            Some(identifier) if !identifier.is_empty()
        )
    {
        return Err(Error::MalformedPacket(
            "CONNACK omitted a non-empty assigned client identifier",
        ));
    }
    Ok(())
}

/// Broker capabilities negotiated from singleton CONNACK properties.
#[cfg(any(feature = "std", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ServerCapabilities {
    pub(crate) maximum_qos: QoS,
    pub(crate) retain_available: bool,
    pub(crate) wildcard_subscriptions_available: bool,
    pub(crate) shared_subscriptions_available: bool,
}

#[cfg(any(feature = "std", test))]
impl ServerCapabilities {
    pub(crate) fn from_connack(connack: &ConnAckPacket) -> Self {
        Self {
            maximum_qos: match connack.properties.get_byte(PropertyId::MaximumQoS) {
                Some(0) => QoS::AtMostOnce,
                Some(1) | None => QoS::AtLeastOnce,
                Some(_) => unreachable!("Maximum QoS was validated while decoding properties"),
            },
            retain_available: connack
                .properties
                .get_byte(PropertyId::RetainAvailable)
                .unwrap_or(1)
                != 0,
            wildcard_subscriptions_available: connack
                .properties
                .get_byte(PropertyId::WildcardSubscriptionAvailable)
                .unwrap_or(1)
                != 0,
            shared_subscriptions_available: connack
                .properties
                .get_byte(PropertyId::SharedSubscriptionAvailable)
                .unwrap_or(1)
                != 0,
        }
    }

    pub(crate) fn validate_publish(&self, qos: QoS, retain: bool) -> Result<()> {
        if qos as u8 > self.maximum_qos as u8 {
            return Err(Error::InvalidOptions(
                "broker does not support QoS 1 publishing",
            ));
        }
        if retain && !self.retain_available {
            return Err(Error::InvalidOptions(
                "broker does not support retained publishing",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_subscription_filter(&self, filter: &str) -> Result<()> {
        validate_topic_filter(filter)?;
        if filter.starts_with("$share/") && !self.shared_subscriptions_available {
            return Err(Error::InvalidOptions(
                "broker does not support shared subscriptions",
            ));
        }
        if filter.contains(['#', '+']) && !self.wildcard_subscriptions_available {
            return Err(Error::InvalidOptions(
                "broker does not support wildcard subscriptions",
            ));
        }
        Ok(())
    }
}

#[cfg(any(feature = "std", test))]
impl Default for ServerCapabilities {
    fn default() -> Self {
        Self {
            maximum_qos: QoS::AtLeastOnce,
            retain_available: true,
            wildcard_subscriptions_available: true,
            shared_subscriptions_available: true,
        }
    }
}

impl ConnAckPacket {
    pub fn decode(body: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(body);

        // Byte 1: connect acknowledge flags (only bit 0 = session present)
        let ack_flags = cur.read_u8()?;
        if ack_flags & 0xFE != 0 {
            return Err(crate::error::Error::MalformedPacket(
                "CONNACK has reserved acknowledge flags",
            ));
        }
        let session_present = (ack_flags & 0x01) != 0;

        // Byte 2: reason code
        let reason_code = cur.read_u8()?;
        if !is_connack_reason_code(reason_code) {
            return Err(crate::error::Error::InvalidReasonCode(reason_code));
        }
        if session_present && reason_code != 0x00 {
            return Err(crate::error::Error::MalformedPacket(
                "refused CONNACK has session present",
            ));
        }

        // MQTT v5 always includes Property Length, including a zero value.
        let properties = Properties::decode_for(&mut cur, PropertyContext::ConnAck)?;
        if cur.remaining() != 0 {
            return Err(crate::error::Error::MalformedPacket(
                "trailing CONNACK bytes",
            ));
        }

        Ok(ConnAckPacket {
            session_present,
            reason_code,
            properties,
        })
    }
}

fn is_connack_reason_code(code: u8) -> bool {
    matches!(
        code,
        0x00 | 0x80
            | 0x81
            | 0x82
            | 0x83
            | 0x84
            | 0x85
            | 0x86
            | 0x87
            | 0x88
            | 0x89
            | 0x8A
            | 0x8C
            | 0x90
            | 0x95
            | 0x97
            | 0x99
            | 0x9A
            | 0x9B
            | 0x9C
            | 0x9D
            | 0x9F
    )
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
    fn mqtt_v5_encodes_empty_client_identifier_without_clean_start() {
        let packet = ConnectPacket {
            protocol_version: 5,
            clean_start: false,
            keep_alive: 30,
            client_id: String::new(),
            username: None,
            password: None,
            properties: Properties::new(),
        };

        assert!(packet.encode().is_ok());
    }

    #[test]
    fn connect_rejects_properties_from_other_packet_types() {
        let packet = ConnectPacket {
            protocol_version: 5,
            clean_start: true,
            keep_alive: 30,
            client_id: String::from("client"),
            username: None,
            password: None,
            properties: Properties::new().with_response_topic("responses/client"),
        };

        assert!(matches!(
            packet.encode(),
            Err(Error::MalformedPacket(
                "property is not allowed in this packet"
            ))
        ));
    }

    #[test]
    fn connack_validation_uses_connect_context() {
        let mut assigned = Properties::new();
        assigned.push(
            PropertyId::AssignedClientIdentifier,
            crate::codec::properties::PropertyValue::Str(String::from("assigned-1")),
        );
        let assigned = ConnAckPacket {
            session_present: false,
            reason_code: 0,
            properties: assigned,
        };
        assert!(validate_connack("", false, &assigned).is_ok());

        let missing_assignment = ConnAckPacket {
            session_present: false,
            reason_code: 0,
            properties: Properties::new(),
        };
        assert!(matches!(
            validate_connack("", false, &missing_assignment),
            Err(crate::error::Error::MalformedPacket(
                "CONNACK omitted a non-empty assigned client identifier"
            ))
        ));

        let resumed_after_clean_start = ConnAckPacket {
            session_present: true,
            reason_code: 0,
            properties: Properties::new(),
        };
        assert!(matches!(
            validate_connack("requested", true, &resumed_after_clean_start),
            Err(crate::error::Error::MalformedPacket(
                "CONNACK has session present after clean start"
            ))
        ));
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
    fn connack_capabilities_default_parse_and_validate() {
        let default_packet = ConnAckPacket::decode(&[0x00, 0x00, 0x00]).unwrap();
        assert_eq!(
            ServerCapabilities::from_connack(&default_packet),
            ServerCapabilities::default()
        );

        let restricted_packet = ConnAckPacket::decode(&[
            0x00, 0x00, 0x08, // flags, reason, property length
            0x24, 0x00, // Maximum QoS = 0
            0x25, 0x00, // Retain Available = false
            0x28, 0x00, // Wildcard Subscription Available = false
            0x2A, 0x00, // Shared Subscription Available = false
        ])
        .unwrap();
        assert_eq!(
            ServerCapabilities::from_connack(&restricted_packet),
            ServerCapabilities {
                maximum_qos: QoS::AtMostOnce,
                retain_available: false,
                wildcard_subscriptions_available: false,
                shared_subscriptions_available: false,
            }
        );

        assert!(matches!(
            ConnAckPacket::decode(&[0x00, 0x00, 0x02, 0x24, 0x02]),
            Err(Error::MalformedPacket("maximum QoS exceeds one"))
        ));
        for property_id in [0x25, 0x28, 0x2A] {
            assert!(matches!(
                ConnAckPacket::decode(&[0x00, 0x00, 0x02, property_id, 0x02]),
                Err(Error::MalformedPacket(
                    "availability property must be zero or one"
                ))
            ));
        }
    }

    #[test]
    fn connack_decode_refused() {
        let body = [0x00, 0x87, 0x00]; // not authorized
        let pkt = ConnAckPacket::decode(&body).unwrap();
        assert_eq!(pkt.reason_code, 0x87);
    }

    #[test]
    fn connack_rejects_reserved_flags_and_trailing_bytes() {
        assert!(matches!(
            ConnAckPacket::decode(&[0x02, 0x00, 0x00]),
            Err(crate::error::Error::MalformedPacket(
                "CONNACK has reserved acknowledge flags"
            ))
        ));
        assert!(matches!(
            ConnAckPacket::decode(&[0x00, 0x00, 0x00, 0x00]),
            Err(crate::error::Error::MalformedPacket(
                "trailing CONNACK bytes"
            ))
        ));
        assert!(matches!(
            ConnAckPacket::decode(&[0x00, 0x00]),
            Err(crate::error::Error::MalformedPacket(
                "unexpected end of data"
            ))
        ));
        assert!(matches!(
            ConnAckPacket::decode(&[0x00, 0x00, 0x04, 0x08, 0x00, 0x01, b'r']),
            Err(crate::error::Error::MalformedPacket(
                "property is not allowed in this packet"
            ))
        ));
    }

    #[test]
    fn refused_connack_cannot_resume_a_session() {
        assert!(matches!(
            ConnAckPacket::decode(&[0x01, 0x87, 0x00]),
            Err(crate::error::Error::MalformedPacket(
                "refused CONNACK has session present"
            ))
        ));
    }

    #[test]
    fn connack_rejects_reserved_reason_code() {
        assert!(matches!(
            ConnAckPacket::decode(&[0x00, 0x03, 0x00]),
            Err(crate::error::Error::InvalidReasonCode(0x03))
        ));
    }
}
