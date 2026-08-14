use bytes::Bytes;
use uuid::Uuid;

use crate::{BlockPosition, PacketWriter, ProtocolError};

const OVERWORLD: &str = "minecraft:overworld";
const WORLD_MIN_SECTION: i32 = -4;
const WORLD_SECTION_COUNT: i32 = 24;
const LIGHT_SECTION_COUNT: usize = 26;
const AIR_STATE_ID: i32 = 0;
const STONE_STATE_ID: i32 = 1;
const PLAYER_MODEL_CUSTOMIZATION_INDEX: u8 = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolItemStack {
    pub item_id: i32,
    pub count: u8,
}

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

#[must_use]
pub fn encode_view_distance(radius: u8) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(i32::from(radius));
    writer.into_bytes()
}

#[must_use]
pub fn encode_view_center(x: i32, z: i32) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(x);
    writer.write_var_i32(z);
    writer.into_bytes()
}

#[must_use]
pub fn encode_set_held_slot(slot: u8) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(i32::from(slot));
    writer.into_bytes()
}

pub fn encode_spawn_position(position: BlockPosition) -> Result<Bytes, ProtocolError> {
    let mut writer = PacketWriter::new();
    writer.write_string(OVERWORLD)?;
    writer.write_block_position(position);
    writer.write_f32(0.0);
    writer.write_f32(0.0);
    Ok(writer.into_bytes())
}

#[must_use]
pub fn encode_initial_player_position(
    teleport_id: i32,
    x: f64,
    y: f64,
    z: f64,
    yaw: f32,
    pitch: f32,
) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(teleport_id);
    for value in [x, y, z, 0.0, 0.0, 0.0] {
        writer.write_f64(value);
    }
    writer.write_f32(yaw);
    writer.write_f32(pitch);
    writer.write_i32(0);
    writer.into_bytes()
}

#[must_use]
pub fn encode_game_event(event: u8, value: f32) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_u8(event);
    writer.write_f32(value);
    writer.into_bytes()
}

#[must_use]
pub fn encode_block_changed_ack(sequence: i32) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(sequence);
    writer.into_bytes()
}

#[must_use]
pub fn encode_block_update(position: BlockPosition, protocol_state_id: i32) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_block_position(position);
    writer.write_var_i32(protocol_state_id);
    writer.into_bytes()
}

#[must_use]
pub fn encode_forget_level_chunk(chunk_x: i32, chunk_z: i32) -> Bytes {
    let packed = u64::from(chunk_x as u32) | (u64::from(chunk_z as u32) << 32);
    let mut writer = PacketWriter::new();
    writer.write_i64(packed as i64);
    writer.into_bytes()
}

#[must_use]
pub fn encode_chunk(
    chunk_x: i32,
    chunk_z: i32,
    biome_id: i32,
    mut block_state: impl FnMut(u8, i32, u8) -> i32,
) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_i32(chunk_x);
    writer.write_i32(chunk_z);
    write_heightmaps(&mut writer, &mut block_state);

    let mut sections = PacketWriter::new();
    for section_y in WORLD_MIN_SECTION..WORLD_MIN_SECTION + WORLD_SECTION_COUNT {
        write_section(&mut sections, section_y, biome_id, &mut block_state);
    }
    let sections = sections.into_bytes();
    writer.write_var_i32(sections.len() as i32);
    writer.write_bytes(&sections);
    writer.write_var_i32(0);
    write_light(&mut writer);
    writer.into_bytes()
}

#[must_use]
pub fn encode_flat_chunk(chunk_x: i32, chunk_z: i32, biome_id: i32) -> Bytes {
    encode_chunk(chunk_x, chunk_z, biome_id, |_, y, _| {
        if y <= 63 {
            STONE_STATE_ID
        } else {
            AIR_STATE_ID
        }
    })
}

#[must_use]
pub fn encode_chunk_batch_finished(count: i32) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(count);
    writer.into_bytes()
}

#[must_use]
pub fn encode_container_set_slot(
    state_id: i32,
    slot: i16,
    count: u8,
    item_id: Option<i32>,
) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(0);
    writer.write_var_i32(state_id);
    writer.write_i16(slot);
    writer.write_var_i32(i32::from(count));
    if count > 0 {
        writer.write_var_i32(item_id.unwrap_or_default());
        writer.write_var_i32(0);
        writer.write_var_i32(0);
    }
    writer.into_bytes()
}

#[must_use]
pub fn encode_container_set_content(
    state_id: i32,
    slots: &[Option<ProtocolItemStack>],
    carried: Option<ProtocolItemStack>,
) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(0);
    writer.write_var_i32(state_id);
    writer.write_var_i32(slots.len() as i32);
    for stack in slots {
        write_item_stack(&mut writer, *stack);
    }
    write_item_stack(&mut writer, carried);
    writer.into_bytes()
}

#[must_use]
pub fn encode_set_cursor_item(stack: Option<ProtocolItemStack>) -> Bytes {
    let mut writer = PacketWriter::new();
    write_item_stack(&mut writer, stack);
    writer.into_bytes()
}

fn write_item_stack(writer: &mut PacketWriter, stack: Option<ProtocolItemStack>) {
    let Some(stack) = stack else {
        writer.write_var_i32(0);
        return;
    };
    writer.write_var_i32(i32::from(stack.count));
    writer.write_var_i32(stack.item_id);
    writer.write_var_i32(0);
    writer.write_var_i32(0);
}

pub fn encode_player_info_add(
    uuid: Uuid,
    username: &str,
    properties: &[(&str, &str, Option<&str>)],
) -> Result<Bytes, ProtocolError> {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(1);
    writer.write_var_i32(1);
    writer.write_uuid(uuid);
    writer.write_string(username)?;
    writer.write_var_i32(properties.len() as i32);
    for (name, value, signature) in properties {
        writer.write_string(name)?;
        writer.write_string(value)?;
        writer.write_bool(signature.is_some());
        if let Some(signature) = signature {
            writer.write_string(signature)?;
        }
    }
    Ok(writer.into_bytes())
}

#[must_use]
pub fn encode_player_skin_parts(entity_id: i32, model_customization: u8) -> Bytes {
    let mut writer = PacketWriter::new();
    writer.write_var_i32(entity_id);
    writer.write_u8(PLAYER_MODEL_CUSTOMIZATION_INDEX);
    writer.write_var_i32(0);
    writer.write_u8(model_customization);
    writer.write_u8(0xff);
    writer.into_bytes()
}

fn write_heightmaps(writer: &mut PacketWriter, block_state: &mut impl FnMut(u8, i32, u8) -> i32) {
    let mut values = [0_u16; 256];
    for z in 0..16_u8 {
        for x in 0..16_u8 {
            let surface = (WORLD_MIN_SECTION * 16..(WORLD_MIN_SECTION + WORLD_SECTION_COUNT) * 16)
                .rev()
                .find(|&y| block_state(x, y, z) != AIR_STATE_ID);
            values[usize::from(z) * 16 + usize::from(x)] =
                surface.map_or(0, |y| (y + 1 - WORLD_MIN_SECTION * 16) as u16);
        }
    }

    writer.write_var_i32(2);
    for heightmap_type in [1, 4] {
        writer.write_var_i32(heightmap_type);
        write_packed_values(writer, 9, values.iter().copied().map(u64::from));
    }
}

fn write_section(
    writer: &mut PacketWriter,
    section_y: i32,
    biome_id: i32,
    block_state: &mut impl FnMut(u8, i32, u8) -> i32,
) {
    let mut states = [AIR_STATE_ID; 4096];
    let mut palette = Vec::new();
    let mut palette_indices = [0_u16; 4096];
    let mut non_air = 0_i16;
    for local_y in 0..16_u8 {
        for z in 0..16_u8 {
            for x in 0..16_u8 {
                let index = (usize::from(local_y) * 16 + usize::from(z)) * 16 + usize::from(x);
                let state = block_state(x, section_y * 16 + i32::from(local_y), z);
                states[index] = state;
                if state != AIR_STATE_ID {
                    non_air += 1;
                }
                let palette_index = palette
                    .iter()
                    .position(|candidate| *candidate == state)
                    .unwrap_or_else(|| {
                        palette.push(state);
                        palette.len() - 1
                    });
                palette_indices[index] = palette_index as u16;
            }
        }
    }

    writer.write_i16(non_air);
    writer.write_i16(0);
    if palette.len() == 1 {
        writer.write_u8(0);
        writer.write_var_i32(palette[0]);
    } else if palette.len() <= 256 {
        let bits = bit_width(palette.len() - 1).max(4);
        writer.write_u8(bits);
        writer.write_var_i32(palette.len() as i32);
        for state in palette {
            writer.write_var_i32(state);
        }
        write_fixed_packed_values(writer, bits, palette_indices.iter().copied().map(u64::from));
    } else {
        const DIRECT_BITS: u8 = 15;
        writer.write_u8(DIRECT_BITS);
        write_fixed_packed_values(
            writer,
            DIRECT_BITS,
            states.iter().copied().map(|state| state as u64),
        );
    }
    writer.write_u8(0);
    writer.write_var_i32(biome_id);
}

fn write_packed_values(writer: &mut PacketWriter, bits: u8, values: impl Iterator<Item = u64>) {
    let entries_per_long = 64 / usize::from(bits);
    let values = values.collect::<Vec<_>>();
    let long_count = values.len().div_ceil(entries_per_long);
    writer.write_var_i32(long_count as i32);
    for group in values.chunks(entries_per_long) {
        let mut packed = 0_u64;
        for (index, value) in group.iter().enumerate() {
            packed |= value << (index * usize::from(bits));
        }
        writer.write_i64(packed as i64);
    }
}

fn write_fixed_packed_values(
    writer: &mut PacketWriter,
    bits: u8,
    values: impl Iterator<Item = u64>,
) {
    let entries_per_long = 64 / usize::from(bits);
    let values = values.collect::<Vec<_>>();
    for group in values.chunks(entries_per_long) {
        let mut packed = 0_u64;
        for (index, value) in group.iter().enumerate() {
            packed |= value << (index * usize::from(bits));
        }
        writer.write_i64(packed as i64);
    }
}

fn bit_width(value: usize) -> u8 {
    (usize::BITS - value.leading_zeros()) as u8
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
    use super::{
        WORLD_SECTION_COUNT, encode_chunk, encode_flat_chunk, encode_initial_player_position,
        encode_play_login, encode_player_info_add, encode_player_skin_parts,
    };
    use crate::PacketReader;
    use uuid::Uuid;

    #[test]
    fn login_and_position_match_target_field_lengths() {
        let login = encode_play_login(1, 20, 2, 2, 1).unwrap_or_default();
        assert!(!login.is_empty());
        assert_eq!(
            encode_initial_player_position(1, 0.5, 65.0, 0.5, 90.0, 12.5).len(),
            61
        );
    }

    #[test]
    fn player_info_add_carries_signed_texture_property() {
        let uuid = Uuid::from_u128(42);
        let packet = encode_player_info_add(
            uuid,
            "ToucanTest",
            &[("textures", "base64-value", Some("signed-value"))],
        )
        .unwrap_or_default();
        let mut reader = PacketReader::new(&packet);
        assert_eq!(reader.read_var_i32(), Ok(1));
        assert_eq!(reader.read_var_i32(), Ok(1));
        assert_eq!(reader.read_uuid(), Ok(uuid));
        assert_eq!(reader.read_string(16, 16).as_deref(), Ok("ToucanTest"));
        assert_eq!(reader.read_var_i32(), Ok(1));
        assert_eq!(reader.read_string(32, 32).as_deref(), Ok("textures"));
        assert_eq!(reader.read_string(128, 128).as_deref(), Ok("base64-value"));
        assert_eq!(reader.read_bool(), Ok(true));
        assert_eq!(reader.read_string(128, 128).as_deref(), Ok("signed-value"));
        assert_eq!(reader.finish(), Ok(()));
    }

    #[test]
    fn player_skin_parts_metadata_enables_all_outer_layers() {
        let packet = encode_player_skin_parts(1, 0x7f);
        let mut reader = PacketReader::new(&packet);
        assert_eq!(reader.read_var_i32(), Ok(1));
        assert_eq!(reader.read_u8(), Ok(16));
        assert_eq!(reader.read_var_i32(), Ok(0));
        assert_eq!(reader.read_u8(), Ok(0x7f));
        assert_eq!(reader.read_u8(), Ok(0xff));
        assert_eq!(reader.finish(), Ok(()));
    }

    #[test]
    fn flat_chunk_has_coordinates_and_bounded_section_data() {
        let chunk = encode_flat_chunk(-2, 3, 40);
        let mut reader = PacketReader::new(&chunk);
        assert_eq!(reader.read_i32(), Ok(-2));
        assert_eq!(reader.read_i32(), Ok(3));
        assert!(chunk.len() < 64 * 1024);
    }

    #[test]
    fn chunk_heightmaps_and_palettes_follow_supplied_blocks() {
        let chunk = encode_chunk(4, -3, 40, |x, y, z| {
            if y <= 63 + i32::from((x + z) % 2) {
                1
            } else {
                0
            }
        });
        let mut reader = PacketReader::new(&chunk);
        assert_eq!(reader.read_i32(), Ok(4));
        assert_eq!(reader.read_i32(), Ok(-3));
        assert_eq!(reader.read_count("heightmaps", 2), Ok(2));
        for _ in 0..2 {
            let _ = reader.read_var_i32();
            let longs = reader.read_count("heightmap longs", 64).unwrap_or_default();
            for _ in 0..longs {
                let _ = reader.read_i64();
            }
        }
        let section_bytes = reader.read_byte_array(64 * 1024).unwrap_or_default();
        let mut sections = PacketReader::new(section_bytes);
        let mut multi_value_sections = 0;
        for _ in 0..WORLD_SECTION_COUNT {
            let _ = sections.read_i16();
            let _ = sections.read_i16();
            let bits = sections.read_u8().unwrap_or_default();
            if bits == 0 {
                let _ = sections.read_var_i32();
            } else {
                multi_value_sections += 1;
                if bits <= 8 {
                    let palette_entries = sections
                        .read_count("block palette entries", 256)
                        .unwrap_or_default();
                    for _ in 0..palette_entries {
                        let _ = sections.read_var_i32();
                    }
                }
                let packed_longs = 4096_usize.div_ceil(64 / usize::from(bits));
                for _ in 0..packed_longs {
                    let _ = sections.read_i64();
                }
            }
            assert_eq!(sections.read_u8(), Ok(0));
            assert_eq!(sections.read_var_i32(), Ok(40));
        }
        assert_eq!(sections.finish(), Ok(()));
        assert_eq!(multi_value_sections, 1);
        assert!(chunk.len() < 64 * 1024);
    }

    #[test]
    fn chunk_palette_transitions_and_high_global_states_are_encoded() {
        for (unique_states, expected_bits) in [(16, 4), (17, 5), (256, 8), (257, 15)] {
            let chunk = encode_chunk(0, 0, 40, |x, y, z| {
                if (-64..-48).contains(&y) {
                    let index = ((y + 64) * 256 + i32::from(z) * 16 + i32::from(x)) as usize;
                    if unique_states == 257 && index % unique_states == 256 {
                        29_872
                    } else {
                        (index % unique_states) as i32
                    }
                } else {
                    0
                }
            });
            let mut packet = PacketReader::new(&chunk);
            let _ = packet.read_i32();
            let _ = packet.read_i32();
            let heightmaps = packet.read_count("heightmaps", 2).unwrap_or_default();
            for _ in 0..heightmaps {
                let _ = packet.read_var_i32();
                let longs = packet.read_count("heightmap longs", 64).unwrap_or_default();
                for _ in 0..longs {
                    let _ = packet.read_i64();
                }
            }
            let section_bytes = packet.read_byte_array(128 * 1024).unwrap_or_default();
            let mut section = PacketReader::new(section_bytes);
            let _ = section.read_i16();
            let _ = section.read_i16();
            assert_eq!(section.read_u8(), Ok(expected_bits));
            if expected_bits <= 8 {
                assert_eq!(section.read_count("palette", 256), Ok(unique_states));
            }
        }
    }

    #[test]
    fn container_content_encodes_slots_and_carried_stack() {
        let slots = [
            None,
            Some(super::ProtocolItemStack {
                item_id: 36,
                count: 12,
            }),
        ];
        let packet = super::encode_container_set_content(
            7,
            &slots,
            Some(super::ProtocolItemStack {
                item_id: 1,
                count: 1,
            }),
        );
        let mut reader = PacketReader::new(&packet);
        assert_eq!(reader.read_var_i32(), Ok(0));
        assert_eq!(reader.read_var_i32(), Ok(7));
        assert_eq!(reader.read_var_i32(), Ok(2));
        assert_eq!(reader.read_var_i32(), Ok(0));
        for expected in [12, 36, 0, 0, 1, 1, 0, 0] {
            assert_eq!(reader.read_var_i32(), Ok(expected));
        }
        assert_eq!(reader.finish(), Ok(()));
    }

    #[test]
    fn cursor_item_uses_the_optional_item_stack_codec() {
        let packet = super::encode_set_cursor_item(Some(super::ProtocolItemStack {
            item_id: 36,
            count: 5,
        }));
        let mut reader = PacketReader::new(&packet);
        for expected in [5, 36, 0, 0] {
            assert_eq!(reader.read_var_i32(), Ok(expected));
        }
        assert_eq!(reader.finish(), Ok(()));
        assert_eq!(super::encode_set_cursor_item(None).as_ref(), &[0]);
    }

    #[test]
    fn held_slot_is_one_varint() {
        assert_eq!(super::encode_set_held_slot(8).as_ref(), &[8]);
    }
}
