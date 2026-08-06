use bytes::Bytes;

use crate::{PacketReader, PacketWriter, ProtocolError};

/// Encodes Login's length-prefixed JSON disconnect component.
pub fn encode_login_disconnect(reason: &str) -> Result<Bytes, ProtocolError> {
    let json = serde_json::to_string(reason)
        .map_err(|error| ProtocolError::ComponentSerialization(error.to_string()))?;
    let mut writer = PacketWriter::new();
    writer.write_string(&json)?;
    Ok(writer.into_bytes())
}

/// Encodes Configuration and Play's context-free network-NBT text component.
pub fn encode_common_disconnect(reason: &str) -> Result<Bytes, ProtocolError> {
    let encoded = modified_utf8(reason);
    let length = u16::try_from(encoded.len()).map_err(|_| ProtocolError::LengthLimit {
        kind: "text component",
        actual: encoded.len(),
        limit: usize::from(u16::MAX),
    })?;
    let mut writer = PacketWriter::new();
    writer.write_u8(8); // NBT StringTag, written as an unnamed network tag.
    writer.write_u16(length);
    writer.write_bytes(&encoded);
    Ok(writer.into_bytes())
}

/// Encodes the signed 64-bit payload shared by common keep-alive packets.
#[must_use]
pub fn encode_keep_alive(id: i64) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_i64(id);
    writer.into_bytes()
}

/// Decodes a complete common keep-alive payload.
pub fn decode_keep_alive(reader: &mut PacketReader<'_>) -> Result<i64, ProtocolError> {
    let id = reader.read_i64()?;
    reader.finish()?;
    Ok(id)
}

fn modified_utf8(value: &str) -> Vec<u8> {
    let mut output = Vec::with_capacity(value.len());
    for unit in value.encode_utf16() {
        match unit {
            0x0001..=0x007f => output.push(unit as u8),
            0x0000..=0x07ff => {
                output.push((0xc0 | (unit >> 6)) as u8);
                output.push((0x80 | (unit & 0x3f)) as u8);
            }
            _ => {
                output.push((0xe0 | (unit >> 12)) as u8);
                output.push((0x80 | ((unit >> 6) & 0x3f)) as u8);
                output.push((0x80 | (unit & 0x3f)) as u8);
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        decode_keep_alive, encode_common_disconnect, encode_keep_alive, encode_login_disconnect,
    };
    use crate::PacketReader;

    #[test]
    fn login_disconnect_is_lenient_json_component() {
        let payload = encode_login_disconnect("Toucan says \"hello\"").unwrap_or_default();
        let mut reader = PacketReader::new(&payload);
        assert_eq!(
            reader.read_string(256, 1024).as_deref(),
            Ok("\"Toucan says \\\"hello\\\"\"")
        );
        assert_eq!(reader.finish(), Ok(()));
    }

    #[test]
    fn common_disconnect_is_an_unnamed_nbt_string() {
        assert_eq!(
            encode_common_disconnect("Toucan").as_deref(),
            Ok([8, 0, 6, b'T', b'o', b'u', b'c', b'a', b'n'].as_slice())
        );
        assert_eq!(
            encode_common_disconnect("\0😀").as_deref(),
            Ok([8, 0, 8, 0xc0, 0x80, 0xed, 0xa0, 0xbd, 0xed, 0xb8, 0x80].as_slice())
        );
    }

    #[test]
    fn keep_alive_is_one_signed_long() {
        let bytes = encode_keep_alive(-42);
        let mut reader = PacketReader::new(&bytes);
        assert_eq!(decode_keep_alive(&mut reader), Ok(-42));
    }
}
