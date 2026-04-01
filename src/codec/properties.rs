#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};

use crate::codec::decode::Cursor;
use crate::codec::encode;
use crate::error::{Error, Result};

/// Subset of MQTT v5 property IDs we support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PropertyId {
    SessionExpiryInterval = 0x11,
    AssignedClientIdentifier = 0x12,
    ServerKeepAlive = 0x13,
    ReasonString = 0x1F,
    ReceiveMaximum = 0x21,
    TopicAliasMaximum = 0x22,
    MaximumQoS = 0x24,
    MaximumPacketSize = 0x27,
    UserProperty = 0x26,
}

impl PropertyId {
    fn from_u8(val: u8) -> Option<Self> {
        match val {
            0x11 => Some(PropertyId::SessionExpiryInterval),
            0x12 => Some(PropertyId::AssignedClientIdentifier),
            0x13 => Some(PropertyId::ServerKeepAlive),
            0x1F => Some(PropertyId::ReasonString),
            0x21 => Some(PropertyId::ReceiveMaximum),
            0x22 => Some(PropertyId::TopicAliasMaximum),
            0x24 => Some(PropertyId::MaximumQoS),
            0x26 => Some(PropertyId::UserProperty),
            0x27 => Some(PropertyId::MaximumPacketSize),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyValue {
    Byte(u8),
    U16(u16),
    U32(u32),
    Str(String),
    StringPair(String, String),
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

    pub fn get_byte(&self, id: PropertyId) -> Option<u8> {
        self.entries.iter().find_map(|(pid, val)| {
            if *pid == id {
                if let PropertyValue::Byte(b) = val { Some(*b) } else { None }
            } else {
                None
            }
        })
    }

    pub fn get_u16(&self, id: PropertyId) -> Option<u16> {
        self.entries.iter().find_map(|(pid, val)| {
            if *pid == id {
                if let PropertyValue::U16(v) = val { Some(*v) } else { None }
            } else {
                None
            }
        })
    }

    pub fn get_u32(&self, id: PropertyId) -> Option<u32> {
        self.entries.iter().find_map(|(pid, val)| {
            if *pid == id {
                if let PropertyValue::U32(v) = val { Some(*v) } else { None }
            } else {
                None
            }
        })
    }

    pub fn get_string(&self, id: PropertyId) -> Option<&str> {
        self.entries.iter().find_map(|(pid, val)| {
            if *pid == id {
                if let PropertyValue::Str(s) = val { Some(s.as_str()) } else { None }
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
        let mut body = Vec::new();
        for (id, val) in &self.entries {
            encode::encode_variable_int(&mut body, *id as u32)?;
            match val {
                PropertyValue::Byte(b) => body.push(*b),
                PropertyValue::U16(v) => encode::encode_u16(&mut body, *v),
                PropertyValue::U32(v) => body.extend_from_slice(&v.to_be_bytes()),
                PropertyValue::Str(s) => encode::encode_string(&mut body, s)?,
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
        self.entries.iter().map(|(id, val)| {
            let id_len = encode::variable_int_len(*id as u32);
            let val_len = match val {
                PropertyValue::Byte(_) => 1,
                PropertyValue::U16(_) => 2,
                PropertyValue::U32(_) => 4,
                PropertyValue::Str(s) => encode::string_len(s),
                PropertyValue::StringPair(k, v) => encode::string_len(k) + encode::string_len(v),
            };
            id_len + val_len
        }).sum()
    }

    /// Decode properties from a cursor. Reads the length prefix, then decodes entries.
    pub fn decode(cur: &mut Cursor<'_>) -> Result<Self> {
        let prop_len = cur.read_variable_int()? as usize;
        if prop_len == 0 {
            return Ok(Properties::new());
        }

        let start = cur.position();
        let mut props = Properties::new();

        while cur.position() - start < prop_len {
            let id_byte = cur.read_variable_int()? as u8;

            // Skip unknown property IDs by consuming their wire bytes.
            // MQTT v5 spec defines the data type for every property ID.
            let Some(id) = PropertyId::from_u8(id_byte) else {
                skip_property_value(cur, id_byte)?;
                continue;
            };

            let value = match id {
                PropertyId::MaximumQoS => PropertyValue::Byte(cur.read_u8()?),
                PropertyId::ReceiveMaximum
                | PropertyId::ServerKeepAlive
                | PropertyId::TopicAliasMaximum => PropertyValue::U16(cur.read_u16()?),
                PropertyId::SessionExpiryInterval | PropertyId::MaximumPacketSize => {
                    PropertyValue::U32(cur.read_u32()?)
                }
                PropertyId::AssignedClientIdentifier | PropertyId::ReasonString => {
                    PropertyValue::Str(cur.read_string()?)
                }
                PropertyId::UserProperty => {
                    let k = cur.read_string()?;
                    let v = cur.read_string()?;
                    PropertyValue::StringPair(k, v)
                }
            };
            props.push(id, value);
        }

        Ok(props)
    }
}

/// Skip over the value of an unrecognized property ID.
///
/// MQTT v5 spec (3.1.2.11) defines the data type for every property ID.
/// We need to consume the right number of bytes to stay in sync with the cursor.
fn skip_property_value(cur: &mut Cursor<'_>, id: u8) -> Result<()> {
    match id {
        // Byte properties
        0x01 // PayloadFormatIndicator
        | 0x17 // RequestProblemInformation
        | 0x19 // RequestResponseInformation
        | 0x24 // MaximumQoS
        | 0x25 // RetainAvailable
        | 0x28 // WildcardSubscriptionAvailable
        | 0x29 // SubscriptionIdentifiersAvailable
        | 0x2A // SharedSubscriptionAvailable
        => { cur.read_u8()?; }

        // Two-byte integer properties
        | 0x13 // ServerKeepAlive
        | 0x21 // ReceiveMaximum
        | 0x22 // TopicAliasMaximum
        | 0x23 // TopicAlias
        => { cur.read_u16()?; }

        // Four-byte integer properties
        | 0x02 // MessageExpiryInterval
        | 0x11 // SessionExpiryInterval
        | 0x18 // WillDelayInterval
        | 0x27 // MaximumPacketSize
        => { cur.read_u32()?; }

        // Variable byte integer properties
        | 0x0B // SubscriptionIdentifier
        => { cur.read_variable_int()?; }

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
        let props = Properties::new()
            .user("traceparent", "00-abc-def-01");
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
}
