#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::codec::types::PacketType;
use crate::error::{Error, Result};

/// Maximum value for an MQTT variable-length integer.
const VARIABLE_INT_MAX: u32 = 268_435_455;

/// Encode a variable-length integer (MQTT spec 1.5.5).
pub fn encode_variable_int(buf: &mut Vec<u8>, mut value: u32) -> Result<()> {
    if value > VARIABLE_INT_MAX {
        return Err(Error::PacketTooLarge);
    }
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value > 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if value == 0 {
            break;
        }
    }
    Ok(())
}

/// Byte length of a variable-length integer encoding.
pub fn variable_int_len(value: u32) -> usize {
    match value {
        0..=127 => 1,
        128..=16_383 => 2,
        16_384..=2_097_151 => 3,
        _ => 4,
    }
}

/// Encode a UTF-8 string with 2-byte length prefix.
pub fn encode_string(buf: &mut Vec<u8>, s: &str) -> Result<()> {
    let len = s.len();
    if len > 65_535 {
        return Err(Error::StringTooLong(len));
    }
    buf.extend_from_slice(&(len as u16).to_be_bytes());
    buf.extend_from_slice(s.as_bytes());
    Ok(())
}

/// Encode binary data with 2-byte length prefix.
pub fn encode_binary(buf: &mut Vec<u8>, data: &[u8]) -> Result<()> {
    let len = data.len();
    if len > 65_535 {
        return Err(Error::StringTooLong(len));
    }
    buf.extend_from_slice(&(len as u16).to_be_bytes());
    buf.extend_from_slice(data);
    Ok(())
}

/// Encode a u16 in big-endian.
pub fn encode_u16(buf: &mut Vec<u8>, val: u16) {
    buf.extend_from_slice(&val.to_be_bytes());
}

/// Encode the fixed header: type+flags byte, then remaining length.
pub fn encode_fixed_header(
    buf: &mut Vec<u8>,
    packet_type: PacketType,
    flags: u8,
    remaining_length: u32,
) -> Result<()> {
    let type_byte = ((packet_type as u8) << 4) | (flags & 0x0F);
    buf.push(type_byte);
    encode_variable_int(buf, remaining_length)
}

/// Encoded byte length of an MQTT string (2 + len).
pub fn string_len(s: &str) -> usize {
    2 + s.len()
}

/// Encoded byte length of binary data (2 + len).
pub fn binary_len(data: &[u8]) -> usize {
    2 + data.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_int_single_byte() {
        let mut buf = Vec::new();
        encode_variable_int(&mut buf, 0).unwrap();
        assert_eq!(buf, [0x00]);

        buf.clear();
        encode_variable_int(&mut buf, 127).unwrap();
        assert_eq!(buf, [0x7F]);
    }

    #[test]
    fn variable_int_two_bytes() {
        let mut buf = Vec::new();
        encode_variable_int(&mut buf, 128).unwrap();
        assert_eq!(buf, [0x80, 0x01]);

        buf.clear();
        encode_variable_int(&mut buf, 16_383).unwrap();
        assert_eq!(buf, [0xFF, 0x7F]);
    }

    #[test]
    fn variable_int_four_bytes() {
        let mut buf = Vec::new();
        encode_variable_int(&mut buf, VARIABLE_INT_MAX).unwrap();
        assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0x7F]);
    }

    #[test]
    fn variable_int_too_large() {
        let mut buf = Vec::new();
        assert!(encode_variable_int(&mut buf, VARIABLE_INT_MAX + 1).is_err());
    }

    #[test]
    fn string_encoding() {
        let mut buf = Vec::new();
        encode_string(&mut buf, "hello").unwrap();
        assert_eq!(buf, [0x00, 0x05, b'h', b'e', b'l', b'l', b'o']);
    }

    #[test]
    fn empty_string() {
        let mut buf = Vec::new();
        encode_string(&mut buf, "").unwrap();
        assert_eq!(buf, [0x00, 0x00]);
    }

    #[test]
    fn binary_encoding() {
        let mut buf = Vec::new();
        encode_binary(&mut buf, &[0xDE, 0xAD]).unwrap();
        assert_eq!(buf, [0x00, 0x02, 0xDE, 0xAD]);
    }

    #[test]
    fn fixed_header_encoding() {
        let mut buf = Vec::new();
        encode_fixed_header(&mut buf, PacketType::Connect, 0, 10).unwrap();
        assert_eq!(buf, [0x10, 0x0A]);
    }
}
