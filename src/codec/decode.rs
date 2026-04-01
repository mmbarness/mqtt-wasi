#[cfg(not(feature = "std"))]
use alloc::{string::String, vec, vec::Vec};

use crate::codec::types::FixedHeader;
use crate::codec::types::PacketType;
use crate::error::{Error, Result};

/// Lightweight cursor over a byte slice for sequential decoding.
pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    pub fn remaining_bytes(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    fn ensure(&self, n: usize) -> Result<()> {
        if self.remaining() < n {
            Err(Error::MalformedPacket("unexpected end of data"))
        } else {
            Ok(())
        }
    }

    pub fn read_u8(&mut self) -> Result<u8> {
        self.ensure(1)?;
        let val = self.buf[self.pos];
        self.pos += 1;
        Ok(val)
    }

    pub fn read_u16(&mut self) -> Result<u16> {
        self.ensure(2)?;
        let val = u16::from_be_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(val)
    }

    pub fn read_u32(&mut self) -> Result<u32> {
        self.ensure(4)?;
        let val = u32::from_be_bytes([
            self.buf[self.pos],
            self.buf[self.pos + 1],
            self.buf[self.pos + 2],
            self.buf[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(val)
    }

    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.ensure(n)?;
        let slice = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    /// Decode a variable-length integer (1-4 bytes).
    pub fn read_variable_int(&mut self) -> Result<u32> {
        let mut value: u32 = 0;
        let mut multiplier: u32 = 1;
        loop {
            let byte = self.read_u8()?;
            value += (byte as u32 & 0x7F) * multiplier;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            multiplier *= 128;
            if multiplier > 128 * 128 * 128 {
                return Err(Error::MalformedPacket("variable int too long"));
            }
        }
    }

    /// Decode a length-prefixed UTF-8 string.
    pub fn read_string(&mut self) -> Result<String> {
        let len = self.read_u16()? as usize;
        let bytes = self.read_bytes(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| Error::MalformedPacket("invalid UTF-8"))
    }

    /// Decode length-prefixed binary data.
    pub fn read_binary(&mut self) -> Result<Vec<u8>> {
        let len = self.read_u16()? as usize;
        let bytes = self.read_bytes(len)?;
        Ok(bytes.to_vec())
    }
}

/// Decode a fixed header from raw bytes. Returns the header and total bytes consumed.
pub fn decode_fixed_header(data: &[u8]) -> Result<(FixedHeader, usize)> {
    let mut cur = Cursor::new(data);
    let first_byte = cur.read_u8()?;
    let packet_type = PacketType::from_u8(first_byte >> 4)?;
    let flags = first_byte & 0x0F;
    let remaining_length = cur.read_variable_int()?;
    Ok((
        FixedHeader {
            packet_type,
            flags,
            remaining_length,
        },
        cur.position(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_read_u8() {
        let mut cur = Cursor::new(&[0x42, 0xFF]);
        assert_eq!(cur.read_u8().unwrap(), 0x42);
        assert_eq!(cur.read_u8().unwrap(), 0xFF);
        assert!(cur.read_u8().is_err());
    }

    #[test]
    fn cursor_read_u16() {
        let mut cur = Cursor::new(&[0x00, 0x0A]);
        assert_eq!(cur.read_u16().unwrap(), 10);
    }

    #[test]
    fn cursor_variable_int() {
        // Single byte
        let mut cur = Cursor::new(&[0x7F]);
        assert_eq!(cur.read_variable_int().unwrap(), 127);

        // Two bytes
        let mut cur = Cursor::new(&[0x80, 0x01]);
        assert_eq!(cur.read_variable_int().unwrap(), 128);

        // Four bytes (max)
        let mut cur = Cursor::new(&[0xFF, 0xFF, 0xFF, 0x7F]);
        assert_eq!(cur.read_variable_int().unwrap(), 268_435_455);
    }

    #[test]
    fn cursor_read_string() {
        let mut cur = Cursor::new(&[0x00, 0x05, b'h', b'e', b'l', b'l', b'o']);
        assert_eq!(cur.read_string().unwrap(), "hello");
    }

    #[test]
    fn decode_connect_fixed_header() {
        let (header, consumed) = decode_fixed_header(&[0x10, 0x0A]).unwrap();
        assert_eq!(header.packet_type, PacketType::Connect);
        assert_eq!(header.flags, 0);
        assert_eq!(header.remaining_length, 10);
        assert_eq!(consumed, 2);
    }

    #[test]
    fn variable_int_round_trip() {
        use crate::codec::encode::encode_variable_int;

        for value in [0, 1, 127, 128, 16_383, 16_384, 2_097_151, 268_435_455] {
            let mut buf = Vec::new();
            encode_variable_int(&mut buf, value).unwrap();
            let mut cur = Cursor::new(&buf);
            assert_eq!(cur.read_variable_int().unwrap(), value);
        }
    }

    #[test]
    fn string_round_trip() {
        use crate::codec::encode::encode_string;

        let mut buf = Vec::new();
        encode_string(&mut buf, "test 🦀").unwrap();
        let mut cur = Cursor::new(&buf);
        assert_eq!(cur.read_string().unwrap(), "test 🦀");
    }
}
