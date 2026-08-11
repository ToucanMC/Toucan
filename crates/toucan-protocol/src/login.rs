use bytes::Bytes;
use uuid::Uuid;

use crate::{PacketReader, PacketWriter, ProtocolError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoginStart {
    pub username: String,
    pub profile_id: Uuid,
}

pub fn decode_login_start(reader: &mut PacketReader<'_>) -> Result<LoginStart, ProtocolError> {
    let username = reader.read_string(16, 48)?;
    let profile_id = reader.read_uuid()?;
    reader.finish()?;
    Ok(LoginStart {
        username,
        profile_id,
    })
}

pub fn encode_login_success(username: &str, uuid: Uuid) -> Result<Bytes, ProtocolError> {
    let mut writer = PacketWriter::new();
    writer.write_uuid(uuid);
    writer.write_string(username)?;
    writer.write_var_i32(0);
    Ok(writer.into_bytes())
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{decode_login_start, encode_login_success};
    use crate::{PacketReader, PacketWriter};

    #[test]
    fn login_start_fixture_round_trips() {
        let uuid = Uuid::from_u128(0x1234_5678_90ab_cdef_1234_5678_90ab_cdef);
        let mut payload = PacketWriter::new();
        assert!(payload.write_string("ToucanTest").is_ok());
        payload.write_uuid(uuid);
        let bytes = payload.into_bytes();
        let mut reader = PacketReader::new(&bytes);
        let login = decode_login_start(&mut reader);
        assert_eq!(login.as_ref().map(|value| value.profile_id), Ok(uuid));
        assert_eq!(
            login.as_ref().map(|value| value.username.as_str()),
            Ok("ToucanTest")
        );
    }

    #[test]
    fn login_success_is_uuid_name_and_empty_properties() {
        let uuid = Uuid::from_u128(0x1234_5678_90ab_cdef_1234_5678_90ab_cdef);
        let bytes = encode_login_success("ToucanTest", uuid).unwrap_or_default();
        let mut reader = PacketReader::new(&bytes);
        assert_eq!(reader.read_uuid(), Ok(uuid));
        assert_eq!(reader.read_string(16, 48).as_deref(), Ok("ToucanTest"));
        assert_eq!(reader.read_count("profile properties", 16), Ok(0));
        assert_eq!(reader.finish(), Ok(()));
    }
}
