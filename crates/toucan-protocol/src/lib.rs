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
    ProtocolItemStack, encode_block_changed_ack, encode_block_update, encode_chunk,
    encode_chunk_batch_finished, encode_container_set_content, encode_container_set_slot,
    encode_flat_chunk, encode_forget_level_chunk, encode_game_event,
    encode_initial_player_position, encode_play_login, encode_player_abilities,
    encode_player_info_add, encode_player_skin_parts, encode_set_cursor_item,
    encode_spawn_position, encode_view_center, encode_view_distance,
};
pub use position::BlockPosition;

use thiserror::Error;

pub const PROTOCOL_VERSION: i32 = 775;
pub const MINECRAFT_VERSION: &str = "26.1.2";

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProtocolError {
    #[error("unexpected end of packet")]
    UnexpectedEof,
    #[error("malformed VarInt")]
    MalformedVarInt,
    #[error("malformed VarLong")]
    MalformedVarLong,
    #[error("negative {kind} length {value}")]
    NegativeLength { kind: &'static str, value: i32 },
    #[error("{kind} length {actual} exceeds limit {limit}")]
    LengthLimit {
        kind: &'static str,
        actual: usize,
        limit: usize,
    },
    #[error("string is not valid UTF-8")]
    InvalidUtf8,
    #[error("invalid namespaced identifier `{0}`")]
    InvalidIdentifier(String),
    #[error("invalid boolean byte {0}")]
    InvalidBoolean(u8),
    #[error("packet has {0} trailing bytes")]
    TrailingData(usize),
    #[error("packet ID {packet_id:#x} is invalid in {state} state")]
    UnexpectedPacket { state: &'static str, packet_id: i32 },
    #[error("invalid next connection state {0}")]
    InvalidNextState(i32),
    #[error("unsupported protocol {received}; Toucan requires {supported}")]
    UnsupportedProtocol { received: i32, supported: i32 },
    #[error("invalid {kind} value {value}")]
    InvalidEnum { kind: &'static str, value: i32 },
    #[error("uncompressed packet length {actual} is not below compression threshold {threshold}")]
    UncompressedAboveThreshold { actual: usize, threshold: usize },
    #[error("compressed packet length {declared} is below compression threshold {threshold}")]
    CompressionBelowThreshold { declared: usize, threshold: usize },
    #[error("packet decompression failed: {0}")]
    Decompression(String),
    #[error("decompressed packet length {actual} does not match declared length {declared}")]
    DecompressedLengthMismatch { actual: usize, declared: usize },
    #[error("compressed packet has {0} trailing bytes")]
    TrailingCompressedData(usize),
    #[error("text component serialization failed: {0}")]
    ComponentSerialization(String),
}
