//! Versioned Minecraft protocol primitives and packet framing.
//!
//! Protocol packet types deliberately stop at this crate's boundary. Validated
//! gameplay actions will live in domain crates rather than reusing wire types.

mod codec;
mod common;
mod compression;
mod configuration;
mod frame;
mod handshake;
mod login;
pub mod packet_id;
mod play;
mod position;

pub use codec::{
    PacketReader, PacketWriter, decode_var_i32, decode_var_i64, encode_var_i32, encode_var_i64,
};
pub use common::{
    decode_keep_alive, encode_common_disconnect, encode_keep_alive, encode_login_disconnect,
};
pub use compression::{decode_compressed_packet, encode_compressed_packet};
pub use configuration::{
    ClientInformation, CustomPayload, KnownPack, decode_client_information, decode_custom_payload,
    decode_known_packs, encode_empty_known_packs, encode_enabled_features,
};
pub use frame::{FrameDecoder, encode_packet};
pub use handshake::{ConnectionState, Handshake, decode_handshake};
pub use login::{LoginStart, decode_login_start, encode_login_success};
pub use play::{
    encode_block_changed_ack, encode_block_update, encode_chunk, encode_chunk_batch_finished,
    encode_flat_chunk, encode_forget_level_chunk, encode_game_event,
    encode_initial_player_position, encode_play_login, encode_player_abilities,
    encode_spawn_position, encode_view_center, encode_view_distance,
};
pub use position::BlockPosition;

use thiserror::Error;

/// Minecraft Java Edition 26.1.2 protocol number.
pub const PROTOCOL_VERSION: i32 = 775;
/// Human-readable target version advertised in the server list.
pub const MINECRAFT_VERSION: &str = "26.1.2";

/// Defensive protocol codec failure.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProtocolError {
    /// More bytes are required to finish the current value.
    #[error("unexpected end of packet")]
    UnexpectedEof,
    /// A VarInt used too many bytes or carried bits outside its integer width.
    #[error("malformed VarInt")]
    MalformedVarInt,
    /// A VarLong used too many bytes or carried bits outside its integer width.
    #[error("malformed VarLong")]
    MalformedVarLong,
    /// A signed length was negative.
    #[error("negative {kind} length {value}")]
    NegativeLength {
        /// Name of the bounded field.
        kind: &'static str,
        /// Received length.
        value: i32,
    },
    /// A length-prefixed value exceeded its configured or protocol limit.
    #[error("{kind} length {actual} exceeds limit {limit}")]
    LengthLimit {
        /// Name of the bounded field.
        kind: &'static str,
        /// Received length.
        actual: usize,
        /// Maximum accepted length.
        limit: usize,
    },
    /// A string was not valid UTF-8.
    #[error("string is not valid UTF-8")]
    InvalidUtf8,
    /// An identifier did not use the required namespaced syntax.
    #[error("invalid namespaced identifier `{0}`")]
    InvalidIdentifier(String),
    /// A boolean contained a byte other than zero or one.
    #[error("invalid boolean byte {0}")]
    InvalidBoolean(u8),
    /// A packet contained bytes after its declared fields.
    #[error("packet has {0} trailing bytes")]
    TrailingData(usize),
    /// A packet ID is invalid for the current connection state.
    #[error("packet ID {packet_id:#x} is invalid in {state} state")]
    UnexpectedPacket {
        /// Current connection state.
        state: &'static str,
        /// Received packet ID.
        packet_id: i32,
    },
    /// A numeric state discriminator was unknown.
    #[error("invalid next connection state {0}")]
    InvalidNextState(i32),
    /// The login handshake used a different protocol version.
    #[error("unsupported protocol {received}; Toucan requires {supported}")]
    UnsupportedProtocol {
        /// Client-supplied protocol.
        received: i32,
        /// Toucan's only implemented protocol.
        supported: i32,
    },
    /// A numeric enum discriminator was outside its protocol-defined range.
    #[error("invalid {kind} value {value}")]
    InvalidEnum {
        /// Enum field name.
        kind: &'static str,
        /// Received discriminator.
        value: i32,
    },
    /// An uncompressed packet was large enough that it should have been compressed.
    #[error("uncompressed packet length {actual} is not below compression threshold {threshold}")]
    UncompressedAboveThreshold {
        /// Uncompressed packet size.
        actual: usize,
        /// Negotiated threshold.
        threshold: usize,
    },
    /// A compressed packet declared a size smaller than the negotiated threshold.
    #[error("compressed packet length {declared} is below compression threshold {threshold}")]
    CompressionBelowThreshold {
        /// Declared uncompressed size.
        declared: usize,
        /// Negotiated threshold.
        threshold: usize,
    },
    /// Zlib decompression failed.
    #[error("packet decompression failed: {0}")]
    Decompression(String),
    /// Zlib output did not match its declared length.
    #[error("decompressed packet length {actual} does not match declared length {declared}")]
    DecompressedLengthMismatch {
        /// Actual output bytes.
        actual: usize,
        /// Length declared in the packet.
        declared: usize,
    },
    /// The compressed stream ended before all packet bytes were consumed.
    #[error("compressed packet has {0} trailing bytes")]
    TrailingCompressedData(usize),
    /// A text component could not be serialized for its connection state.
    #[error("text component serialization failed: {0}")]
    ComponentSerialization(String),
}
