use bytes::Bytes;

use crate::{BlockPosition, PacketWriter, ProtocolError};

const OVERWORLD: &str = "minecraft:overworld";
const WORLD_MIN_SECTION: i32 = -4;
const WORLD_SECTION_COUNT: i32 = 24;
const LIGHT_SECTION_COUNT: usize = 26;
const AIR_STATE_ID: i32 = 0;
const STONE_STATE_ID: i32 = 1;
const BIOME_ID: i32 = 0;

/// Encodes protocol 775's initial Play Login packet for one overworld.
#[allow(clippy::too_many_arguments)]
pub fn encode_play_login(
    entity_id: i32,
    max_players: u32,
    view_distance: u8,
    simulation_distance: u8,
    game_mode: u8,
) -> Result<Bytes, ProtocolError> {
    let mut writer = PacketWriter::new();
    writer.write_i32(entity_id);
    writer.write_bool(false);
    writer.write_var_i32(1);
    writer.write_string(OVERWORLD)?;
    writer.write_var_i32(max_players.min(i32::MAX as u32) as i32);
    writer.write_var_i32(i32::from(view_distance));
    writer.write_var_i32(i32::from(simulation_distance));
    writer.write_bool(false);
    writer.write_bool(true);
    writer.write_bool(false);

    // CommonPlayerSpawnInfo. DimensionType is a registry holder ID.
    writer.write_var_i32(0);
    writer.write_string(OVERWORLD)?;
    writer.write_i64(0);
    writer.write_u8(game_mode);
    writer.write_u8(u8::MAX);
    writer.write_bool(false);
    writer.write_bool(true);
    writer.write_bool(false);
    writer.write_var_i32(0);
    writer.write_var_i32(63);
    writer.write_bool(false);
    Ok(writer.into_bytes())
}

/// Encodes non-flying creative-style abilities suitable for the initial spawn.
#[must_use]
pub fn encode_player_abilities(game_mode: u8) -> Bytes {
    let mut writer = PacketWriter::new();
    let flags = match game_mode {
        1 => 0x0d,
        3 => 0x07,
        _ => 0,
    };
    writer.write_u8(flags);
    writer.write_f32(0.05);
    writer.write_f32(0.1);
    writer.into_bytes()
}

/// Encodes the client chunk-cache radius.
#[must_use]
pub fn encode_view_distance(radius: u8) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(i32::from(radius));
    writer.into_bytes()
}

/// Encodes the chunk-cache center.
#[must_use]
pub fn encode_view_center(x: i32, z: i32) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(x);
    writer.write_var_i32(z);
    writer.into_bytes()
}

/// Encodes the default overworld spawn position and rotation.
pub fn encode_spawn_position(position: BlockPosition) -> Result<Bytes, ProtocolError> {
    let mut writer = PacketWriter::new();
    writer.write_string(OVERWORLD)?;
    writer.write_block_position(position);
    writer.write_f32(0.0);
    writer.write_f32(0.0);
    Ok(writer.into_bytes())
}

/// Encodes an absolute position synchronization with no relative flags.
#[must_use]
pub fn encode_initial_player_position(teleport_id: i32, x: f64, y: f64, z: f64) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(teleport_id);
    for value in [x, y, z, 0.0, 0.0, 0.0] {
        writer.write_f64(value);
    }
    writer.write_f32(0.0);
    writer.write_f32(0.0);
    writer.write_i32(0);
    writer.into_bytes()
}

/// Encodes a one-byte game event and its float value.
#[must_use]
pub fn encode_game_event(event: u8, value: f32) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_u8(event);
    writer.write_f32(value);
    writer.into_bytes()
}

/// Encodes one target-version flat chunk with a stone floor at Y=63.
#[must_use]
pub fn encode_flat_chunk(chunk_x: i32, chunk_z: i32) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_i32(chunk_x);
    writer.write_i32(chunk_z);
    write_heightmaps(&mut writer);

    let mut sections = PacketWriter::new();
    for section_y in WORLD_MIN_SECTION..WORLD_MIN_SECTION + WORLD_SECTION_COUNT {
        if section_y == 3 {
            write_ground_section(&mut sections);
        } else {
            write_empty_section(&mut sections);
        }
    }
    let sections = sections.into_bytes();
    writer.write_var_i32(sections.len() as i32);
    writer.write_bytes(&sections);
    writer.write_var_i32(0); // block entities
    write_light(&mut writer);
    writer.into_bytes()
}

/// Encodes the chunk count carried by Chunk Batch Finished.
#[must_use]
pub fn encode_chunk_batch_finished(count: i32) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(count);
    writer.into_bytes()
}

fn write_heightmaps(writer: &mut PacketWriter) {
    writer.write_var_i32(2);
    for heightmap_type in [1, 4] {
        writer.write_var_i32(heightmap_type);
        writer.write_var_i32(37);
        let mut packed = 0_u64;
        for index in 0..7 {
            packed |= 128_u64 << (index * 9);
        }
        for _ in 0..36 {
            writer.write_i64(packed as i64);
        }
        writer.write_i64((128_u64 | (128_u64 << 9) | (128_u64 << 18) | (128_u64 << 27)) as i64);
    }
}

fn write_empty_section(writer: &mut PacketWriter) {
    writer.write_i16(0);
    writer.write_i16(0);
    writer.write_u8(0);
    writer.write_var_i32(AIR_STATE_ID);
    writer.write_u8(0);
    writer.write_var_i32(BIOME_ID);
}

fn write_ground_section(writer: &mut PacketWriter) {
    writer.write_i16(256);
    writer.write_i16(0);
    writer.write_u8(4);
    writer.write_var_i32(2);
    writer.write_var_i32(AIR_STATE_ID);
    writer.write_var_i32(STONE_STATE_ID);
    for index in 0..256 {
        let value = if index >= 240 {
            0x1111_1111_1111_1111_u64
        } else {
            0
        };
        writer.write_i64(value as i64);
    }
    writer.write_u8(0);
    writer.write_var_i32(BIOME_ID);
}

fn write_light(writer: &mut PacketWriter) {
    let all_sections = (1_u64 << LIGHT_SECTION_COUNT) - 1;
    writer.write_var_i32(1);
    writer.write_i64(all_sections as i64);
    writer.write_var_i32(0);
    writer.write_var_i32(0);
    writer.write_var_i32(1);
    writer.write_i64(all_sections as i64);
    writer.write_var_i32(LIGHT_SECTION_COUNT as i32);
    let full_sky = [0xff_u8; 2048];
    for _ in 0..LIGHT_SECTION_COUNT {
        writer.write_var_i32(full_sky.len() as i32);
        writer.write_bytes(&full_sky);
    }
    writer.write_var_i32(0);
}

#[cfg(test)]
mod tests {
    use super::{encode_flat_chunk, encode_initial_player_position, encode_play_login};
    use crate::PacketReader;

    #[test]
    fn login_and_position_match_target_field_lengths() {
        let login = encode_play_login(1, 20, 2, 2, 1).unwrap_or_default();
        assert!(!login.is_empty());
        assert_eq!(encode_initial_player_position(1, 0.5, 65.0, 0.5).len(), 61);
    }

    #[test]
    fn flat_chunk_has_coordinates_and_bounded_section_data() {
        let chunk = encode_flat_chunk(-2, 3);
        let mut reader = PacketReader::new(&chunk);
        assert_eq!(reader.read_i32(), Ok(-2));
        assert_eq!(reader.read_i32(), Ok(3));
        assert!(chunk.len() < 64 * 1024);
    }
}
