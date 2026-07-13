#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};

use crate::codec::decode::Cursor;
use crate::codec::encode;
use crate::codec::topic::validate_topic_name;
use crate::error::{Error, Result};

/// Subset of MQTT v5 property IDs we support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PropertyId {
    ResponseTopic = 0x08,
    CorrelationData = 0x09,
    SessionExpiryInterval = 0x11,
    AssignedClientIdentifier = 0x12,
    ServerKeepAlive = 0x13,
    ReasonString = 0x1F,
    ReceiveMaximum = 0x21,
    TopicAliasMaximum = 0x22,
    MaximumQoS = 0x24,
    RetainAvailable = 0x25,
    MaximumPacketSize = 0x27,
    WildcardSubscriptionAvailable = 0x28,
    UserProperty = 0x26,
    SharedSubscriptionAvailable = 0x2A,
}

impl PropertyId {
    fn from_u8(val: u8) -> Option<Self> {
        match val {
            0x08 => Some(PropertyId::ResponseTopic),
            0x09 => Some(PropertyId::CorrelationData),
            0x11 => Some(PropertyId::SessionExpiryInterval),
            0x12 => Some(PropertyId::AssignedClientIdentifier),
            0x13 => Some(PropertyId::ServerKeepAlive),
            0x1F => Some(PropertyId::ReasonString),
            0x21 => Some(PropertyId::ReceiveMaximum),
            0x22 => Some(PropertyId::TopicAliasMaximum),
            0x24 => Some(PropertyId::MaximumQoS),
            0x25 => Some(PropertyId::RetainAvailable),
            0x26 => Some(PropertyId::UserProperty),
            0x27 => Some(PropertyId::MaximumPacketSize),
            0x28 => Some(PropertyId::WildcardSubscriptionAvailable),
            0x2A => Some(PropertyId::SharedSubscriptionAvailable),
            _ => None,
        }
    }
}

/// A typed MQTT v5 property value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyValue {
    Byte(u8),
    U16(u16),
    U32(u32),
    Str(String),
    Binary(Vec<u8>),
    StringPair(String, String),
}

/// MQTT packet type whose property set is being encoded or decoded.
///
/// The public [`Properties::encode`] and [`Properties::decode`] methods retain
/// their structural behavior for callers working with standalone property
/// sections. Packet codecs use this context to enforce MQTT's per-packet
/// property allowlists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PropertyContext {
    Connect,
    ConnAck,
    Publish,
    PubAck,
    Subscribe,
    SubAck,
    Unsubscribe,
    UnsubAck,
    Disconnect,
}

impl PropertyContext {
    fn allows(self, id: u8) -> bool {
        match self {
            Self::Connect => matches!(id, 0x11 | 0x21 | 0x22 | 0x26 | 0x27),
            Self::ConnAck => matches!(
                id,
                0x11 | 0x12
                    | 0x13
                    | 0x15
                    | 0x16
                    | 0x1A
                    | 0x1C
                    | 0x1F
                    | 0x21
                    | 0x22
                    | 0x24
                    | 0x25
                    | 0x26
                    | 0x27
                    | 0x28
                    | 0x29
                    | 0x2A
            ),
            Self::Publish => {
                matches!(id, 0x01 | 0x02 | 0x03 | 0x08 | 0x09 | 0x0B | 0x23 | 0x26)
            }
            Self::PubAck | Self::SubAck | Self::UnsubAck => matches!(id, 0x1F | 0x26),
            // Subscription Identifier is legal on the wire, but is not part of
            // the typed outbound API yet.
            Self::Subscribe => matches!(id, 0x0B | 0x26),
            Self::Unsubscribe => id == 0x26,
            // This context is server-to-client. Session Expiry Interval is only
            // legal in the client-to-server DISCONNECT direction.
            Self::Disconnect => matches!(id, 0x1C | 0x1F | 0x26),
        }
    }
}

/// Flat list of properties. Linear scan is fine for the typical 0-5 items.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Properties {
    entries: Vec<(PropertyId, PropertyValue)>,
}

impl Properties {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, id: PropertyId, value: PropertyValue) {
        self.entries.push((id, value));
    }

    pub fn user(mut self, key: &str, value: &str) -> Self {
        self.entries.push((
            PropertyId::UserProperty,
            PropertyValue::StringPair(String::from(key), String::from(value)),
        ));
        self
    }

    /// Set the standard MQTT v5 Response Topic property.
    pub fn set_response_topic(&mut self, topic: impl Into<String>) {
        self.replace_single(PropertyId::ResponseTopic, PropertyValue::Str(topic.into()));
    }

    pub fn with_response_topic(mut self, topic: impl Into<String>) -> Self {
        self.set_response_topic(topic);
        self
    }

    pub fn response_topic(&self) -> Option<&str> {
        self.get_string(PropertyId::ResponseTopic)
    }

    /// Set the standard MQTT v5 Correlation Data property.
    pub fn set_correlation_data(&mut self, data: impl Into<Vec<u8>>) {
        self.replace_single(
            PropertyId::CorrelationData,
            PropertyValue::Binary(data.into()),
        );
    }

    pub fn with_correlation_data(mut self, data: impl Into<Vec<u8>>) -> Self {
        self.set_correlation_data(data);
        self
    }

    pub fn correlation_data(&self) -> Option<&[u8]> {
        self.get_binary(PropertyId::CorrelationData)
    }

    fn replace_single(&mut self, id: PropertyId, value: PropertyValue) {
        self.entries.retain(|(property_id, _)| *property_id != id);
        self.entries.push((id, value));
    }

    pub fn get_byte(&self, id: PropertyId) -> Option<u8> {
        self.entries.iter().find_map(|(pid, val)| {
            if *pid == id {
                if let PropertyValue::Byte(b) = val {
                    Some(*b)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    pub fn get_u16(&self, id: PropertyId) -> Option<u16> {
        self.entries.iter().find_map(|(pid, val)| {
            if *pid == id {
                if let PropertyValue::U16(v) = val {
                    Some(*v)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    pub fn get_u32(&self, id: PropertyId) -> Option<u32> {
        self.entries.iter().find_map(|(pid, val)| {
            if *pid == id {
                if let PropertyValue::U32(v) = val {
                    Some(*v)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    pub fn get_string(&self, id: PropertyId) -> Option<&str> {
        self.entries.iter().find_map(|(pid, val)| {
            if *pid == id {
                if let PropertyValue::Str(s) = val {
                    Some(s.as_str())
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    pub fn get_binary(&self, id: PropertyId) -> Option<&[u8]> {
        self.entries.iter().find_map(|(pid, val)| {
            if *pid == id {
                if let PropertyValue::Binary(bytes) = val {
                    Some(bytes.as_slice())
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    /// Iterate over all user properties.
    pub fn user_properties(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().filter_map(|(pid, val)| {
            if *pid == PropertyId::UserProperty {
                if let PropertyValue::StringPair(k, v) = val {
                    Some((k.as_str(), v.as_str()))
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Encode properties into buffer. Writes the variable-int length prefix followed by entries.
    pub fn encode(&self, buf: &mut Vec<u8>) -> Result<()> {
        self.encode_inner(buf, None)
    }

    pub(crate) fn encode_for(&self, buf: &mut Vec<u8>, context: PropertyContext) -> Result<()> {
        self.encode_inner(buf, Some(context))
    }

    fn encode_inner(&self, buf: &mut Vec<u8>, context: Option<PropertyContext>) -> Result<()> {
        let mut body = Vec::new();
        for (index, (id, val)) in self.entries.iter().enumerate() {
            if context.is_some_and(|context| !context.allows(*id as u8)) {
                return Err(Error::MalformedPacket(
                    "property is not allowed in this packet",
                ));
            }
            validate_property(*id, val)?;
            if *id != PropertyId::UserProperty
                && self.entries[..index]
                    .iter()
                    .any(|(previous, _)| previous == id)
            {
                return Err(Error::MalformedPacket("duplicate singleton property"));
            }
            encode::encode_variable_int(&mut body, *id as u32)?;
            match val {
                PropertyValue::Byte(b) => body.push(*b),
                PropertyValue::U16(v) => encode::encode_u16(&mut body, *v),
                PropertyValue::U32(v) => body.extend_from_slice(&v.to_be_bytes()),
                PropertyValue::Str(s) => encode::encode_string(&mut body, s)?,
                PropertyValue::Binary(bytes) => encode::encode_binary(&mut body, bytes)?,
                PropertyValue::StringPair(k, v) => {
                    encode::encode_string(&mut body, k)?;
                    encode::encode_string(&mut body, v)?;
                }
            }
        }
        encode::encode_variable_int(buf, body.len() as u32)?;
        buf.extend_from_slice(&body);
        Ok(())
    }

    /// Encoded byte length of the properties (including the length prefix).
    pub fn encoded_len(&self) -> usize {
        let body_len = self.body_len();
        encode::variable_int_len(body_len as u32) + body_len
    }

    fn body_len(&self) -> usize {
        self.entries
            .iter()
            .map(|(id, val)| {
                let id_len = encode::variable_int_len(*id as u32);
                let val_len = match val {
                    PropertyValue::Byte(_) => 1,
                    PropertyValue::U16(_) => 2,
                    PropertyValue::U32(_) => 4,
                    PropertyValue::Str(s) => encode::string_len(s),
                    PropertyValue::Binary(bytes) => encode::binary_len(bytes),
                    PropertyValue::StringPair(k, v) => {
                        encode::string_len(k) + encode::string_len(v)
                    }
                };
                id_len + val_len
            })
            .sum()
    }

    /// Decode properties from a cursor. Reads the length prefix, then decodes entries.
    pub fn decode(cur: &mut Cursor<'_>) -> Result<Self> {
        Self::decode_inner(cur, None)
    }

    pub(crate) fn decode_for(cur: &mut Cursor<'_>, context: PropertyContext) -> Result<Self> {
        Self::decode_inner(cur, Some(context))
    }

    fn decode_inner(cur: &mut Cursor<'_>, context: Option<PropertyContext>) -> Result<Self> {
        let prop_len = cur.read_variable_int()? as usize;
        if prop_len == 0 {
            return Ok(Properties::new());
        }
        if cur.remaining() < prop_len {
            return Err(Error::MalformedPacket("property length exceeds packet"));
        }

        let start = cur.position();
        let end = start
            .checked_add(prop_len)
            .ok_or(Error::MalformedPacket("property length overflow"))?;
        let mut props = Properties::new();
        let mut seen_ids = Vec::new();

        while cur.position() < end {
            let raw_id = cur.read_variable_int()?;
            let id_byte = u8::try_from(raw_id)
                .map_err(|_| Error::MalformedPacket("property id exceeds one byte"))?;

            if id_byte == 0x23 {
                return Err(Error::MalformedPacket("topic alias is not supported"));
            }
            if context.is_some_and(|context| !context.allows(id_byte)) {
                return Err(Error::MalformedPacket(
                    "property is not allowed in this packet",
                ));
            }
            register_property_id(&mut seen_ids, id_byte, context)?;

            // Preserve legal properties outside the typed public subset by
            // consuming and validating their wire values.
            let Some(id) = PropertyId::from_u8(id_byte) else {
                skip_property_value(cur, id_byte)?;
                if cur.position() > end {
                    return Err(Error::MalformedPacket("property exceeded declared length"));
                }
                continue;
            };

            let value = match id {
                PropertyId::MaximumQoS
                | PropertyId::RetainAvailable
                | PropertyId::WildcardSubscriptionAvailable
                | PropertyId::SharedSubscriptionAvailable => PropertyValue::Byte(cur.read_u8()?),
                PropertyId::ReceiveMaximum
                | PropertyId::ServerKeepAlive
                | PropertyId::TopicAliasMaximum => PropertyValue::U16(cur.read_u16()?),
                PropertyId::SessionExpiryInterval | PropertyId::MaximumPacketSize => {
                    PropertyValue::U32(cur.read_u32()?)
                }
                PropertyId::ResponseTopic
                | PropertyId::AssignedClientIdentifier
                | PropertyId::ReasonString => PropertyValue::Str(cur.read_string()?),
                PropertyId::CorrelationData => PropertyValue::Binary(cur.read_binary()?),
                PropertyId::UserProperty => {
                    let k = cur.read_string()?;
                    let v = cur.read_string()?;
                    PropertyValue::StringPair(k, v)
                }
            };
            if cur.position() > end {
                return Err(Error::MalformedPacket("property exceeded declared length"));
            }
            validate_property(id, &value)?;
            props.push(id, value);
        }

        if cur.position() != end {
            return Err(Error::MalformedPacket("property length mismatch"));
        }

        Ok(props)
    }
}

fn register_property_id(
    seen_ids: &mut Vec<u8>,
    id: u8,
    context: Option<PropertyContext>,
) -> Result<()> {
    let repeatable = id == PropertyId::UserProperty as u8
        || (id == 0x0B && matches!(context, None | Some(PropertyContext::Publish)));
    if !repeatable && seen_ids.contains(&id) {
        return Err(Error::MalformedPacket("duplicate singleton property"));
    }
    seen_ids.push(id);
    Ok(())
}

fn validate_property(id: PropertyId, value: &PropertyValue) -> Result<()> {
    let valid_type = matches!(
        (id, value),
        (
            PropertyId::ResponseTopic
                | PropertyId::AssignedClientIdentifier
                | PropertyId::ReasonString,
            PropertyValue::Str(_)
        ) | (PropertyId::CorrelationData, PropertyValue::Binary(_))
            | (
                PropertyId::SessionExpiryInterval | PropertyId::MaximumPacketSize,
                PropertyValue::U32(_)
            )
            | (
                PropertyId::ServerKeepAlive
                    | PropertyId::ReceiveMaximum
                    | PropertyId::TopicAliasMaximum,
                PropertyValue::U16(_)
            )
            | (
                PropertyId::MaximumQoS
                    | PropertyId::RetainAvailable
                    | PropertyId::WildcardSubscriptionAvailable
                    | PropertyId::SharedSubscriptionAvailable,
                PropertyValue::Byte(_)
            )
            | (PropertyId::UserProperty, PropertyValue::StringPair(_, _))
    );
    if !valid_type {
        return Err(Error::MalformedPacket("property value has wrong wire type"));
    }

    match (id, value) {
        (PropertyId::ResponseTopic, PropertyValue::Str(topic)) => validate_topic_name(topic),
        (PropertyId::AssignedClientIdentifier, PropertyValue::Str(identifier))
            if identifier.is_empty() =>
        {
            Err(Error::MalformedPacket(
                "assigned client identifier must be non-empty",
            ))
        }
        (PropertyId::ReceiveMaximum, PropertyValue::U16(0)) => {
            Err(Error::MalformedPacket("receive maximum must be non-zero"))
        }
        (PropertyId::MaximumPacketSize, PropertyValue::U32(0)) => Err(Error::MalformedPacket(
            "maximum packet size must be non-zero",
        )),
        (PropertyId::MaximumQoS, PropertyValue::Byte(value)) if *value > 1 => {
            Err(Error::MalformedPacket("maximum QoS exceeds one"))
        }
        (
            PropertyId::RetainAvailable
            | PropertyId::WildcardSubscriptionAvailable
            | PropertyId::SharedSubscriptionAvailable,
            PropertyValue::Byte(value),
        ) if *value > 1 => Err(Error::MalformedPacket(
            "availability property must be zero or one",
        )),
        _ => Ok(()),
    }
}

/// Skip over the value of an unrecognized property ID.
///
/// MQTT v5 spec (3.1.2.11) defines the data type for every property ID.
/// We need to consume the right number of bytes to stay in sync with the cursor.
fn skip_property_value(cur: &mut Cursor<'_>, id: u8) -> Result<()> {
    match id {
        0x01 => {
            if cur.read_u8()? > 1 {
                return Err(Error::MalformedPacket(
                    "payload format indicator must be zero or one",
                ));
            }
        }
        0x29 => {
            if cur.read_u8()? > 1 {
                return Err(Error::MalformedPacket(
                    "subscription identifiers available must be zero or one",
                ));
            }
        }

        // Byte properties
        0x17 // RequestProblemInformation
        | 0x19 // RequestResponseInformation
        | 0x24 // MaximumQoS
        => { cur.read_u8()?; }

        // Two-byte integer properties
        | 0x13 // ServerKeepAlive
        | 0x21 // ReceiveMaximum
        | 0x22 // TopicAliasMaximum
        => { cur.read_u16()?; }

        // Four-byte integer properties
        | 0x02 // MessageExpiryInterval
        | 0x11 // SessionExpiryInterval
        | 0x18 // WillDelayInterval
        | 0x27 // MaximumPacketSize
        => { cur.read_u32()?; }

        // Variable byte integer properties
        | 0x0B // SubscriptionIdentifier
        => {
            if cur.read_variable_int()? == 0 {
                return Err(Error::MalformedPacket(
                    "subscription identifier must be non-zero",
                ));
            }
        }

        // UTF-8 string properties
        | 0x03 // ContentType
        | 0x08 // ResponseTopic
        | 0x12 // AssignedClientIdentifier
        | 0x15 // AuthenticationMethod
        | 0x1A // ResponseInformation
        | 0x1C // ServerReference
        | 0x1F // ReasonString
        => { cur.read_string()?; }

        // Binary data properties (same length-prefix as strings but may contain non-UTF-8)
        | 0x09 // CorrelationData
        | 0x16 // AuthenticationData
        => { cur.read_binary()?; }

        // UTF-8 string pair
        | 0x26 // UserProperty
        => { cur.read_string()?; cur.read_string()?; }

        _ => return Err(Error::MalformedPacket("unknown property id")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_context(context: PropertyContext, entries: &[u8]) -> Result<Properties> {
        let mut encoded = Vec::new();
        encode::encode_variable_int(&mut encoded, entries.len() as u32).unwrap();
        encoded.extend_from_slice(entries);
        Properties::decode_for(&mut Cursor::new(&encoded), context)
    }

    #[test]
    fn empty_properties_round_trip() {
        let props = Properties::new();
        let mut buf = Vec::new();
        props.encode(&mut buf).unwrap();
        assert_eq!(buf, [0x00]); // just the zero-length prefix

        let mut cur = Cursor::new(&buf);
        let decoded = Properties::decode(&mut cur).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn user_property_round_trip() {
        let props = Properties::new().user("traceparent", "00-abc-def-01");
        let mut buf = Vec::new();
        props.encode(&mut buf).unwrap();

        let mut cur = Cursor::new(&buf);
        let decoded = Properties::decode(&mut cur).unwrap();
        let pairs: Vec<_> = decoded.user_properties().collect();
        assert_eq!(pairs, [("traceparent", "00-abc-def-01")]);
    }

    #[test]
    fn mixed_properties_round_trip() {
        let mut props = Properties::new();
        props.push(PropertyId::ReceiveMaximum, PropertyValue::U16(100));
        props.push(PropertyId::MaximumQoS, PropertyValue::Byte(1));

        let mut buf = Vec::new();
        props.encode(&mut buf).unwrap();

        let mut cur = Cursor::new(&buf);
        let decoded = Properties::decode(&mut cur).unwrap();
        assert_eq!(decoded.get_u16(PropertyId::ReceiveMaximum), Some(100));
        assert_eq!(decoded.get_byte(PropertyId::MaximumQoS), Some(1));
    }

    #[test]
    fn request_response_properties_round_trip() {
        let props = Properties::new()
            .with_response_topic("replies/client")
            .with_correlation_data([0x00, 0xFF, 0x42]);
        let mut buf = Vec::new();
        props.encode(&mut buf).unwrap();

        let mut cur = Cursor::new(&buf);
        let decoded = Properties::decode(&mut cur).unwrap();
        assert_eq!(decoded.response_topic(), Some("replies/client"));
        assert_eq!(decoded.correlation_data(), Some(&[0x00, 0xFF, 0x42][..]));
    }

    #[test]
    fn response_topic_must_be_a_concrete_topic_name() {
        for topic in ["", "responses/+", "responses/#"] {
            let properties = Properties::new().with_response_topic(topic);
            assert!(properties.encode(&mut Vec::new()).is_err());

            let mut body = Vec::new();
            encode::encode_variable_int(&mut body, PropertyId::ResponseTopic as u32).unwrap();
            encode::encode_string(&mut body, topic).unwrap();
            let mut encoded = Vec::new();
            encode::encode_variable_int(&mut encoded, body.len() as u32).unwrap();
            encoded.extend_from_slice(&body);
            assert!(Properties::decode(&mut Cursor::new(&encoded)).is_err());
        }
    }

    #[test]
    fn topic_alias_is_rejected_instead_of_silently_skipped() {
        let encoded = [0x03, 0x23, 0x00, 0x01];
        assert!(matches!(
            Properties::decode(&mut Cursor::new(&encoded)),
            Err(Error::MalformedPacket("topic alias is not supported"))
        ));
    }

    #[test]
    fn packet_context_rejects_properties_from_other_packet_types_on_encode() {
        let response_topic = Properties::new().with_response_topic("responses/client");
        for context in [
            PropertyContext::Connect,
            PropertyContext::Subscribe,
            PropertyContext::Unsubscribe,
        ] {
            assert!(matches!(
                response_topic.encode_for(&mut Vec::new(), context),
                Err(Error::MalformedPacket(
                    "property is not allowed in this packet"
                ))
            ));
        }

        let mut session_expiry = Properties::new();
        session_expiry.push(PropertyId::SessionExpiryInterval, PropertyValue::U32(60));
        assert!(matches!(
            session_expiry.encode_for(&mut Vec::new(), PropertyContext::Publish),
            Err(Error::MalformedPacket(
                "property is not allowed in this packet"
            ))
        ));
    }

    #[test]
    fn packet_context_rejects_properties_from_other_packet_types_on_decode() {
        let response_topic = [0x08, 0x00, 0x01, b'r'];
        for context in [
            PropertyContext::ConnAck,
            PropertyContext::PubAck,
            PropertyContext::SubAck,
            PropertyContext::UnsubAck,
        ] {
            assert!(matches!(
                decode_context(context, &response_topic),
                Err(Error::MalformedPacket(
                    "property is not allowed in this packet"
                ))
            ));
        }

        let session_expiry = [0x11, 0x00, 0x00, 0x00, 0x01];
        for context in [PropertyContext::Publish, PropertyContext::Disconnect] {
            assert!(matches!(
                decode_context(context, &session_expiry),
                Err(Error::MalformedPacket(
                    "property is not allowed in this packet"
                ))
            ));
        }
    }

    #[test]
    fn context_decode_preserves_legal_unsupported_properties() {
        let publish = [
            0x01, 0x01, // Payload Format Indicator
            0x02, 0x00, 0x00, 0x00, 0x05, // Message Expiry Interval
            0x03, 0x00, 0x01, b'j', // Content Type
            0x0B, 0x01, // Subscription Identifier (repeatable in PUBLISH)
            0x0B, 0x02,
        ];
        assert!(decode_context(PropertyContext::Publish, &publish).is_ok());

        let connack = [
            0x15, 0x00, 0x01, b'm', // Authentication Method
            0x16, 0x00, 0x01, 0xFF, // Authentication Data
            0x1A, 0x00, 0x01, b'i', // Response Information
            0x1C, 0x00, 0x01, b's', // Server Reference
            0x29, 0x01, // Subscription Identifiers Available
        ];
        assert!(decode_context(PropertyContext::ConnAck, &connack).is_ok());

        let server_reference = [0x1C, 0x00, 0x01, b's'];
        assert!(decode_context(PropertyContext::Disconnect, &server_reference).is_ok());

        let invalid_utf8_server_reference = [0x1C, 0x00, 0x01, 0xFF];
        assert!(matches!(
            decode_context(PropertyContext::Disconnect, &invalid_utf8_server_reference),
            Err(Error::MalformedPacket("invalid UTF-8"))
        ));
    }

    #[test]
    fn context_decode_validates_skipped_values_and_duplicate_rules() {
        assert!(matches!(
            decode_context(PropertyContext::Publish, &[0x01, 0x02]),
            Err(Error::MalformedPacket(
                "payload format indicator must be zero or one"
            ))
        ));
        assert!(matches!(
            decode_context(PropertyContext::Publish, &[0x0B, 0x00]),
            Err(Error::MalformedPacket(
                "subscription identifier must be non-zero"
            ))
        ));
        assert!(matches!(
            decode_context(PropertyContext::ConnAck, &[0x29, 0x02]),
            Err(Error::MalformedPacket(
                "subscription identifiers available must be zero or one"
            ))
        ));
        assert!(matches!(
            decode_context(PropertyContext::Publish, &[0x01, 0x00, 0x01, 0x01]),
            Err(Error::MalformedPacket("duplicate singleton property"))
        ));
        assert!(matches!(
            decode_context(PropertyContext::Subscribe, &[0x0B, 0x01, 0x0B, 0x02]),
            Err(Error::MalformedPacket("duplicate singleton property"))
        ));
    }

    #[test]
    fn request_response_setters_replace_singleton_properties() {
        let mut props = Properties::new().with_response_topic("old");
        props.set_response_topic("new");
        props.set_correlation_data([1]);
        props.set_correlation_data([2]);

        assert_eq!(props.response_topic(), Some("new"));
        assert_eq!(props.correlation_data(), Some(&[2][..]));
    }

    #[test]
    fn duplicate_request_response_properties_are_rejected() {
        let mut encoded = Vec::new();
        Properties::new()
            .with_response_topic("one")
            .encode(&mut encoded)
            .unwrap();
        let first_body = &encoded[1..];

        let mut duplicate = Vec::new();
        crate::codec::encode::encode_variable_int(&mut duplicate, (first_body.len() * 2) as u32)
            .unwrap();
        duplicate.extend_from_slice(first_body);
        duplicate.extend_from_slice(first_body);

        let mut cur = Cursor::new(&duplicate);
        assert!(matches!(
            Properties::decode(&mut cur),
            Err(Error::MalformedPacket("duplicate singleton property"))
        ));
    }

    #[test]
    fn oversized_property_id_is_rejected_without_truncation() {
        // Property section: ID 0x108 encoded as a variable byte integer.
        let mut cur = Cursor::new(&[0x02, 0x88, 0x02]);
        assert!(matches!(
            Properties::decode(&mut cur),
            Err(Error::MalformedPacket("property id exceeds one byte"))
        ));
    }

    #[test]
    fn encode_rejects_mismatched_property_wire_type() {
        let mut props = Properties::new();
        props.push(PropertyId::ResponseTopic, PropertyValue::U32(7));

        assert!(matches!(
            props.encode(&mut Vec::new()),
            Err(Error::MalformedPacket("property value has wrong wire type"))
        ));
    }

    #[test]
    fn encode_rejects_duplicate_singleton_but_allows_user_properties() {
        let mut duplicate = Properties::new();
        duplicate.push(PropertyId::MaximumPacketSize, PropertyValue::U32(10));
        duplicate.push(PropertyId::MaximumPacketSize, PropertyValue::U32(20));
        assert!(matches!(
            duplicate.encode(&mut Vec::new()),
            Err(Error::MalformedPacket("duplicate singleton property"))
        ));

        let users = Properties::new().user("one", "1").user("two", "2");
        assert!(users.encode(&mut Vec::new()).is_ok());
    }

    #[test]
    fn zero_invalid_limits_are_rejected_on_encode_and_decode() {
        let mut receive_maximum = Properties::new();
        receive_maximum.push(PropertyId::ReceiveMaximum, PropertyValue::U16(0));
        assert!(matches!(
            receive_maximum.encode(&mut Vec::new()),
            Err(Error::MalformedPacket("receive maximum must be non-zero"))
        ));

        // Declared property length 5, MaximumPacketSize ID, four zero bytes.
        let mut cur = Cursor::new(&[0x05, 0x27, 0, 0, 0, 0]);
        assert!(matches!(
            Properties::decode(&mut cur),
            Err(Error::MalformedPacket(
                "maximum packet size must be non-zero"
            ))
        ));
    }

    #[test]
    fn property_length_cannot_overrun_declared_section() {
        // Declares a 2-byte property section, but ReasonString needs its
        // property ID plus a 2-byte string length before the value.
        let mut cur = Cursor::new(&[0x02, 0x1F, 0x00, 0x03, b'a', b'b', b'c']);
        assert!(matches!(
            Properties::decode(&mut cur),
            Err(Error::MalformedPacket("property exceeded declared length"))
        ));
    }

    #[test]
    fn property_length_cannot_exceed_remaining_packet() {
        let mut cur = Cursor::new(&[0x05, 0x24, 0x01]);
        assert!(matches!(
            Properties::decode(&mut cur),
            Err(Error::MalformedPacket("property length exceeds packet"))
        ));
    }
}
