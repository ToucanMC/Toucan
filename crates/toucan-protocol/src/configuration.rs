use bytes::Bytes;

use crate::{PacketReader, PacketWriter, ProtocolError};

/// Client preferences sent during Configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientInformation {
    /// Lowercase locale identifier.
    pub locale: String,
    /// Requested view distance byte.
    pub view_distance: i8,
    /// Chat visibility ordinal.
    pub chat_visibility: i32,
    /// Whether chat colors are enabled.
    pub chat_colors: bool,
    /// Skin-part bit mask.
    pub model_customization: u8,
    /// Main-hand ordinal.
    pub main_hand: i32,
    /// Whether client-side text filtering is enabled.
    pub text_filtering: bool,
    /// Whether the player permits server-list display.
    pub allows_listing: bool,
    /// Particle-status ordinal.
    pub particle_status: i32,
}

/// A known data-pack descriptor negotiated before registry synchronization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnownPack {
    /// Pack namespace.
    pub namespace: String,
    /// Pack identifier.
    pub id: String,
    /// Pack version string.
    pub version: String,
}

/// Bounded custom payload accepted during Configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomPayload {
    /// Plugin channel.
    pub channel: String,
    /// Channel-defined opaque bytes.
    pub data: Vec<u8>,
}

/// Decodes and validates Configuration Client Information.
pub fn decode_client_information(
    reader: &mut PacketReader<'_>,
) -> Result<ClientInformation, ProtocolError> {
    let information = ClientInformation {
        locale: reader.read_string(16, 48)?,
        view_distance: reader.read_i8()?,
        chat_visibility: read_enum(reader, "chat visibility", 3)?,
        chat_colors: reader.read_bool()?,
        model_customization: reader.read_u8()?,
        main_hand: read_enum(reader, "main hand", 2)?,
        text_filtering: reader.read_bool()?,
        allows_listing: reader.read_bool()?,
        particle_status: read_enum(reader, "particle status", 3)?,
    };
    reader.finish()?;
    Ok(information)
}

/// Decodes the serverbound known-pack response with a 64-entry bound.
pub fn decode_known_packs(reader: &mut PacketReader<'_>) -> Result<Vec<KnownPack>, ProtocolError> {
    let count = reader.read_count("known packs", 64)?;
    let mut packs = Vec::with_capacity(count);
    for _ in 0..count {
        packs.push(KnownPack {
            namespace: reader.read_string(32_767, 131_068)?,
            id: reader.read_string(32_767, 131_068)?,
            version: reader.read_string(32_767, 131_068)?,
        });
    }
    reader.finish()?;
    Ok(packs)
}

/// Decodes a plugin payload with the target client's 32,767-byte cap.
pub fn decode_custom_payload(
    reader: &mut PacketReader<'_>,
) -> Result<CustomPayload, ProtocolError> {
    let channel = reader.read_identifier(32_767)?;
    let data = reader.read_remaining("custom payload", 32_767)?.to_vec();
    Ok(CustomPayload { channel, data })
}

/// Encodes the default vanilla feature set.
pub fn encode_enabled_features() -> Result<Bytes, ProtocolError> {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(1);
    writer.write_string("minecraft:vanilla")?;
    Ok(writer.into_bytes())
}

/// Encodes an empty known-pack request, forcing complete registry transfer.
#[must_use]
pub fn encode_empty_known_packs() -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(0);
    writer.into_bytes()
}

fn read_enum(
    reader: &mut PacketReader<'_>,
    kind: &'static str,
    variants: i32,
) -> Result<i32, ProtocolError> {
    let value = reader.read_var_i32()?;
    if (0..variants).contains(&value) {
        Ok(value)
    } else {
        Err(ProtocolError::InvalidEnum { kind, value })
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_client_information, decode_known_packs, encode_enabled_features};
    use crate::{PacketReader, PacketWriter};

    #[test]
    fn client_information_fixture_decodes() {
        let mut writer = PacketWriter::new();
        assert!(writer.write_string("en_us").is_ok());
        writer.write_u8(12);
        writer.write_var_i32(0);
        writer.write_bool(true);
        writer.write_u8(0x7f);
        writer.write_var_i32(1);
        writer.write_bool(false);
        writer.write_bool(true);
        writer.write_var_i32(2);
        let bytes = writer.into_bytes();
        let mut reader = PacketReader::new(&bytes);
        let information = decode_client_information(&mut reader);
        assert_eq!(
            information.as_ref().map(|value| value.view_distance),
            Ok(12)
        );
        assert_eq!(
            information.as_ref().map(|value| value.particle_status),
            Ok(2)
        );
    }

    #[test]
    fn empty_known_pack_fixture_decodes() {
        let bytes = [0x00];
        let mut reader = PacketReader::new(&bytes);
        assert_eq!(decode_known_packs(&mut reader), Ok(Vec::new()));
    }

    #[test]
    fn enabled_features_contains_vanilla() {
        let bytes = encode_enabled_features().unwrap_or_default();
        let mut reader = PacketReader::new(&bytes);
        assert_eq!(reader.read_count("features", 16), Ok(1));
        assert_eq!(
            reader.read_identifier(64).as_deref(),
            Ok("minecraft:vanilla")
        );
        assert_eq!(reader.finish(), Ok(()));
    }
}
