use bytes::{BufMut, BytesMut};
use uuid::Uuid;

use crate::{BlockPosition, ProtocolError};

pub fn decode_var_i32(input: &mut &[u8]) -> Result<i32, ProtocolError> {
    let mut value = 0_u32;

    for index in 0..5 {
        let Some((&byte, rest)) = input.split_first() else {
            return Err(ProtocolError::UnexpectedEof);
        };
        *input = rest;

        if index == 4 && byte & 0xf0 != 0 {
            return Err(ProtocolError::MalformedVarInt);
        }
        value |= u32::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok(value as i32);
        }
    }

    Err(ProtocolError::MalformedVarInt)
}

pub fn encode_var_i32(value: i32, output: &mut impl BufMut) {
    let mut remaining = value as u32;
    loop {
        if remaining & !0x7f == 0 {
            output.put_u8(remaining as u8);
            return;
        }
        output.put_u8((remaining as u8 & 0x7f) | 0x80);
        remaining >>= 7;
    }
}

pub fn decode_var_i64(input: &mut &[u8]) -> Result<i64, ProtocolError> {
    let mut value = 0_u64;

    for index in 0..10 {
        let Some((&byte, rest)) = input.split_first() else {
            return Err(ProtocolError::UnexpectedEof);
        };
        *input = rest;

        if index == 9 && byte & 0xfe != 0 {
            return Err(ProtocolError::MalformedVarLong);
        }
        value |= u64::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok(value as i64);
        }
    }

    Err(ProtocolError::MalformedVarLong)
}

pub fn encode_var_i64(value: i64, output: &mut impl BufMut) {
    let mut remaining = value as u64;
    loop {
        if remaining & !0x7f == 0 {
            output.put_u8(remaining as u8);
            return;
        }
        output.put_u8((remaining as u8 & 0x7f) | 0x80);
        remaining >>= 7;
    }
}

#[derive(Debug)]
pub struct PacketReader<'a> {
    remaining: &'a [u8],
}

impl<'a> PacketReader<'a> {
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }

    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.remaining.len()
    }

    pub const fn finish(&self) -> Result<(), ProtocolError> {
        if self.remaining.is_empty() {
            Ok(())
        } else {
            Err(ProtocolError::TrailingData(self.remaining.len()))
        }
    }

    pub fn read_var_i32(&mut self) -> Result<i32, ProtocolError> {
        decode_var_i32(&mut self.remaining)
    }

    pub fn read_var_i64(&mut self) -> Result<i64, ProtocolError> {
        decode_var_i64(&mut self.remaining)
    }

    pub fn read_bool(&mut self) -> Result<bool, ProtocolError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            byte => Err(ProtocolError::InvalidBoolean(byte)),
        }
    }

    pub fn read_u8(&mut self) -> Result<u8, ProtocolError> {
        let bytes = self.take(1)?;
        Ok(bytes[0])
    }

    pub fn read_i8(&mut self) -> Result<i8, ProtocolError> {
        Ok(self.read_u8()? as i8)
    }

    pub fn read_u16(&mut self) -> Result<u16, ProtocolError> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub fn read_i16(&mut self) -> Result<i16, ProtocolError> {
        Ok(i16::from_be_bytes(self.array()?))
    }

    pub fn read_i32(&mut self) -> Result<i32, ProtocolError> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    pub fn read_i64(&mut self) -> Result<i64, ProtocolError> {
        let bytes = self.take(8)?;
        let mut array = [0_u8; 8];
        array.copy_from_slice(bytes);
        Ok(i64::from_be_bytes(array))
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ProtocolError> {
        let mut output = [0; N];
        output.copy_from_slice(self.take(N)?);
        Ok(output)
    }

    pub fn read_f32(&mut self) -> Result<f32, ProtocolError> {
        let bytes = self.take(4)?;
        let mut array = [0_u8; 4];
        array.copy_from_slice(bytes);
        Ok(f32::from_be_bytes(array))
    }

    pub fn read_f64(&mut self) -> Result<f64, ProtocolError> {
        let bytes = self.take(8)?;
        let mut array = [0_u8; 8];
        array.copy_from_slice(bytes);
        Ok(f64::from_be_bytes(array))
    }

    pub fn read_uuid(&mut self) -> Result<Uuid, ProtocolError> {
        let bytes = self.take(16)?;
        let mut array = [0_u8; 16];
        array.copy_from_slice(bytes);
        Ok(Uuid::from_bytes(array))
    }

    pub fn read_byte_array(&mut self, limit: usize) -> Result<&'a [u8], ProtocolError> {
        let length = self.read_length("byte array", limit)?;
        self.take(length)
    }

    pub fn read_count(&mut self, kind: &'static str, limit: usize) -> Result<usize, ProtocolError> {
        self.read_length(kind, limit)
    }

    pub fn read_remaining(
        &mut self,
        kind: &'static str,
        limit: usize,
    ) -> Result<&'a [u8], ProtocolError> {
        let length = self.remaining.len();
        if length > limit {
            return Err(ProtocolError::LengthLimit {
                kind,
                actual: length,
                limit,
            });
        }
        self.take(length)
    }

    pub fn read_string(
        &mut self,
        max_chars: usize,
        max_bytes: usize,
    ) -> Result<String, ProtocolError> {
        let length = self.read_length("string", max_bytes)?;
        let bytes = self.take(length)?;
        let value = std::str::from_utf8(bytes).map_err(|_| ProtocolError::InvalidUtf8)?;
        let character_count = value.chars().count();
        if character_count > max_chars {
            return Err(ProtocolError::LengthLimit {
                kind: "string characters",
                actual: character_count,
                limit: max_chars,
            });
        }
        Ok(value.to_owned())
    }

    pub fn read_identifier(&mut self, max_bytes: usize) -> Result<String, ProtocolError> {
        let value = self.read_string(max_bytes, max_bytes)?;
        if is_identifier(&value) {
            Ok(value)
        } else {
            Err(ProtocolError::InvalidIdentifier(value))
        }
    }

    pub fn read_block_position(&mut self) -> Result<BlockPosition, ProtocolError> {
        Ok(BlockPosition::unpack(self.read_i64()?))
    }

    fn read_length(&mut self, kind: &'static str, limit: usize) -> Result<usize, ProtocolError> {
        let signed = self.read_var_i32()?;
        let length = usize::try_from(signed).map_err(|_| ProtocolError::NegativeLength {
            kind,
            value: signed,
        })?;
        if length > limit {
            return Err(ProtocolError::LengthLimit {
                kind,
                actual: length,
                limit,
            });
        }
        Ok(length)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ProtocolError> {
        if self.remaining.len() < length {
            return Err(ProtocolError::UnexpectedEof);
        }
        let (value, rest) = self.remaining.split_at(length);
        self.remaining = rest;
        Ok(value)
    }
}

fn is_identifier(value: &str) -> bool {
    let Some((namespace, path)) = value.split_once(':') else {
        return false;
    };
    !namespace.is_empty()
        && !path.is_empty()
        && namespace.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.-".contains(&byte)
        })
        && path.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_./-".contains(&byte)
        })
}

#[derive(Clone, Debug, Default)]
pub struct PacketWriter {
    bytes: BytesMut,
}

impl PacketWriter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write_var_i32(&mut self, value: i32) {
        encode_var_i32(value, &mut self.bytes);
    }

    pub fn write_var_i64(&mut self, value: i64) {
        encode_var_i64(value, &mut self.bytes);
    }

    pub fn write_bool(&mut self, value: bool) {
        self.bytes.put_u8(u8::from(value));
    }

    pub fn write_u8(&mut self, value: u8) {
        self.bytes.put_u8(value);
    }

    pub fn write_u16(&mut self, value: u16) {
        self.bytes.put_u16(value);
    }

    pub fn write_i16(&mut self, value: i16) {
        self.bytes.put_i16(value);
    }

    pub fn write_i32(&mut self, value: i32) {
        self.bytes.put_i32(value);
    }

    pub fn write_i64(&mut self, value: i64) {
        self.bytes.put_i64(value);
    }

    pub fn write_f32(&mut self, value: f32) {
        self.bytes.put_f32(value);
    }

    pub fn write_f64(&mut self, value: f64) {
        self.bytes.put_f64(value);
    }

    pub fn write_uuid(&mut self, value: Uuid) {
        self.bytes.extend_from_slice(value.as_bytes());
    }

    pub fn write_string(&mut self, value: &str) -> Result<(), ProtocolError> {
        let length = i32::try_from(value.len()).map_err(|_| ProtocolError::LengthLimit {
            kind: "string",
            actual: value.len(),
            limit: i32::MAX as usize,
        })?;
        self.write_var_i32(length);
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }

    pub fn write_block_position(&mut self, position: BlockPosition) {
        self.write_i64(position.pack());
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    #[must_use]
    pub fn into_bytes(self) -> bytes::Bytes {
        self.bytes.freeze()
    }
}

#[cfg(test)]
mod tests {
    use bytes::BytesMut;

    use super::{
        PacketReader, PacketWriter, decode_var_i32, decode_var_i64, encode_var_i32, encode_var_i64,
    };
    use crate::ProtocolError;

    #[test]
    fn varint_boundaries_round_trip() {
        for value in [i32::MIN, -1, 0, 1, 127, 128, 255, 2_097_151, i32::MAX] {
            let mut encoded = BytesMut::new();
            encode_var_i32(value, &mut encoded);
            let mut input = encoded.as_ref();
            assert_eq!(decode_var_i32(&mut input), Ok(value));
            assert!(input.is_empty());
        }
    }

    #[test]
    fn varlong_boundaries_round_trip() {
        for value in [i64::MIN, -1, 0, 1, 127, 128, i64::MAX] {
            let mut encoded = BytesMut::new();
            encode_var_i64(value, &mut encoded);
            let mut input = encoded.as_ref();
            assert_eq!(decode_var_i64(&mut input), Ok(value));
            assert!(input.is_empty());
        }
    }

    #[test]
    fn rejects_malformed_varints() {
        let mut too_wide = [0xff, 0xff, 0xff, 0xff, 0x10].as_slice();
        assert_eq!(
            decode_var_i32(&mut too_wide),
            Err(ProtocolError::MalformedVarInt)
        );

        let mut unterminated = [0x80, 0x80, 0x80, 0x80, 0x80].as_slice();
        assert_eq!(
            decode_var_i32(&mut unterminated),
            Err(ProtocolError::MalformedVarInt)
        );
    }

    #[test]
    fn validates_string_lengths_and_utf8() {
        let mut writer = PacketWriter::new();
        assert!(writer.write_string("toucan").is_ok());
        let bytes = writer.into_bytes();
        let mut reader = PacketReader::new(&bytes);
        assert_eq!(reader.read_string(6, 6).as_deref(), Ok("toucan"));

        let overlong = [0x04, b'b', b'i', b'r', b'd'];
        let mut reader = PacketReader::new(&overlong);
        assert!(matches!(
            reader.read_string(3, 4),
            Err(ProtocolError::LengthLimit {
                kind: "string characters",
                ..
            })
        ));

        let invalid = [0x02, 0xc3, 0x28];
        let mut reader = PacketReader::new(&invalid);
        assert_eq!(reader.read_string(2, 2), Err(ProtocolError::InvalidUtf8));
    }

    #[test]
    fn validates_identifiers() {
        let mut writer = PacketWriter::new();
        assert!(writer.write_string("minecraft:stone").is_ok());
        let bytes = writer.into_bytes();
        let mut reader = PacketReader::new(&bytes);
        assert_eq!(reader.read_identifier(64).as_deref(), Ok("minecraft:stone"));
    }
}
