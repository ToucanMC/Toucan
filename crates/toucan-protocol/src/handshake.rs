use crate::{PacketReader, ProtocolError};

/// Explicit Minecraft connection phases.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    /// Initial routing packet.
    Handshake,
    /// Server-list status and ping.
    Status,
    /// Authentication and login negotiation.
    Login,
    /// Registry and feature negotiation.
    Configuration,
    /// Active gameplay.
    Play,
}

impl ConnectionState {
    /// Stable diagnostic name for logs and protocol errors.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Handshake => "handshake",
            Self::Status => "status",
            Self::Login => "login",
            Self::Configuration => "configuration",
            Self::Play => "play",
        }
    }
}

/// Decoded server-bound handshake packet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Handshake {
    /// Client protocol version.
    pub protocol_version: i32,
    /// Host name or virtual-host address sent by the client.
    pub server_address: String,
    /// Requested TCP port.
    pub server_port: u16,
    /// Requested next phase.
    pub next_state: ConnectionState,
}

/// Decodes a complete handshake payload after the packet ID.
pub fn decode_handshake(reader: &mut PacketReader<'_>) -> Result<Handshake, ProtocolError> {
    let protocol_version = reader.read_var_i32()?;
    let server_address = reader.read_string(255, 765)?;
    let server_port = reader.read_u16()?;
    let next_state = match reader.read_var_i32()? {
        1 => ConnectionState::Status,
        2 => ConnectionState::Login,
        value => return Err(ProtocolError::InvalidNextState(value)),
    };
    reader.finish()?;

    Ok(Handshake {
        protocol_version,
        server_address,
        server_port,
        next_state,
    })
}

#[cfg(test)]
mod tests {
    use crate::{ConnectionState, PacketReader, PacketWriter, decode_handshake};

    #[test]
    fn decodes_status_handshake_fixture() {
        let fixture = [
            0x87, 0x06, 0x09, b'l', b'o', b'c', b'a', b'l', b'h', b'o', b's', b't', 0x63, 0xdd,
            0x01,
        ];
        let mut reader = PacketReader::new(&fixture);
        let handshake = decode_handshake(&mut reader);
        assert!(handshake.is_ok());
        let handshake = handshake.unwrap_or_else(|error| panic!("unexpected error: {error}"));
        assert_eq!(handshake.protocol_version, 775);
        assert_eq!(handshake.server_address, "localhost");
        assert_eq!(handshake.server_port, 25_565);
        assert_eq!(handshake.next_state, ConnectionState::Status);
    }

    #[test]
    fn handshake_round_trip_from_writer() {
        let mut writer = PacketWriter::new();
        writer.write_var_i32(775);
        assert!(writer.write_string("example.test").is_ok());
        writer.write_u16(25_565);
        writer.write_var_i32(1);
        let bytes = writer.into_bytes();
        let mut reader = PacketReader::new(&bytes);
        let handshake = decode_handshake(&mut reader);
        assert_eq!(
            handshake
                .as_ref()
                .map(|value| value.server_address.as_str()),
            Ok("example.test")
        );
    }
}
