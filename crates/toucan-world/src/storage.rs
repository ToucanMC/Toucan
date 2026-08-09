//! Conversion between domain chunks and target-version Anvil chunk NBT.

use std::collections::BTreeMap;

use thiserror::Error;
use toucan_nbt::{NamedTag, Tag};

use crate::{BlockStateId, Chunk, ChunkPosition, DATA_VERSION_26_1_2, MIN_Y};

pub(crate) fn encode_chunk(chunk: &Chunk) -> NamedTag {
    let position = chunk.position();
    let sections = chunk
        .sections()
        .iter()
        .enumerate()
        .map(|(section_index, section)| {
            let mut palette = Vec::new();
            let mut indices = [0_u16; 4096];
            for local_y in 0..16_u8 {
                for z in 0..16_u8 {
                    for x in 0..16_u8 {
                        let index =
                            (usize::from(local_y) * 16 + usize::from(z)) * 16 + usize::from(x);
                        let state = section.block(x, local_y, z).unwrap_or(BlockStateId::AIR);
                        let palette_index = palette
                            .iter()
                            .position(|candidate| *candidate == state)
                            .unwrap_or_else(|| {
                                palette.push(state);
                                palette.len() - 1
                            });
                        indices[index] = palette_index as u16;
                    }
                }
            }
            let palette_tags = palette
                .iter()
                .map(|state| {
                    let mut entry = BTreeMap::new();
                    entry.insert("Name".into(), Tag::String(state.identifier().into()));
                    Tag::Compound(entry)
                })
                .collect();
            let mut block_states = BTreeMap::new();
            block_states.insert(
                "palette".into(),
                Tag::List {
                    element_type: 10,
                    values: palette_tags,
                },
            );
            if palette.len() > 1 {
                let bits = bit_width(palette.len() - 1).max(4);
                block_states.insert(
                    "data".into(),
                    Tag::LongArray(pack_values(bits, indices.iter().copied().map(u64::from))),
                );
            }
            let mut biomes = BTreeMap::new();
            biomes.insert(
                "palette".into(),
                Tag::List {
                    element_type: 8,
                    values: vec![Tag::String("minecraft:plains".into())],
                },
            );
            let mut section_tag = BTreeMap::new();
            section_tag.insert("Y".into(), Tag::Byte(section_index as i8 - 4));
            section_tag.insert("block_states".into(), Tag::Compound(block_states));
            section_tag.insert("biomes".into(), Tag::Compound(biomes));
            Tag::Compound(section_tag)
        })
        .collect();

    let mut heightmaps = BTreeMap::new();
    let heights = column_heights(chunk);
    for name in ["MOTION_BLOCKING", "WORLD_SURFACE"] {
        heightmaps.insert(
            name.into(),
            Tag::LongArray(pack_values(9, heights.iter().copied().map(u64::from))),
        );
    }
    let empty_compounds = || Tag::List {
        element_type: 10,
        values: Vec::new(),
    };
    let mut root = BTreeMap::new();
    root.insert("DataVersion".into(), Tag::Int(DATA_VERSION_26_1_2));
    root.insert("xPos".into(), Tag::Int(position.x));
    root.insert("zPos".into(), Tag::Int(position.z));
    root.insert("yPos".into(), Tag::Int(-4));
    root.insert("Status".into(), Tag::String("minecraft:full".into()));
    root.insert("LastUpdate".into(), Tag::Long(0));
    root.insert("InhabitedTime".into(), Tag::Long(0));
    root.insert("isLightOn".into(), Tag::Byte(1));
    root.insert(
        "sections".into(),
        Tag::List {
            element_type: 10,
            values: sections,
        },
    );
    root.insert("Heightmaps".into(), Tag::Compound(heightmaps));
    root.insert("block_entities".into(), empty_compounds());
    root.insert("block_ticks".into(), empty_compounds());
    root.insert("fluid_ticks".into(), empty_compounds());
    NamedTag {
        name: String::new(),
        value: Tag::Compound(root),
    }
}

pub(crate) fn decode_chunk(
    expected: ChunkPosition,
    document: &NamedTag,
) -> Result<Chunk, ChunkStorageError> {
    let root = compound(&document.value, "root")?;
    for (name, expected_value) in [("xPos", expected.x), ("zPos", expected.z)] {
        if let Some(actual) = root.get(name).and_then(Tag::as_i32)
            && actual != expected_value
        {
            return Err(ChunkStorageError::CoordinateMismatch {
                field: name,
                expected: expected_value,
                actual,
            });
        }
    }
    let sections = match root.get("sections") {
        Some(Tag::List {
            element_type: 10,
            values,
        }) => values,
        _ => return Err(ChunkStorageError::Missing("sections")),
    };
    let mut chunk = Chunk::empty(expected);
    for section in sections {
        let section = compound(section, "section")?;
        let section_y = match section.get("Y") {
            Some(Tag::Byte(value)) => i32::from(*value),
            _ => return Err(ChunkStorageError::Missing("sections[].Y")),
        };
        if !(-4..20).contains(&section_y) {
            continue;
        }
        let block_states = compound(
            section
                .get("block_states")
                .ok_or(ChunkStorageError::Missing("sections[].block_states"))?,
            "block_states",
        )?;
        let palette = match block_states.get("palette") {
            Some(Tag::List {
                element_type: 10,
                values,
            }) if !values.is_empty() => values
                .iter()
                .map(|entry| {
                    let entry = compound(entry, "block palette entry")?;
                    let name = entry
                        .get("Name")
                        .and_then(Tag::as_str)
                        .ok_or(ChunkStorageError::Missing("block palette Name"))?;
                    BlockStateId::from_identifier(name)
                        .ok_or_else(|| ChunkStorageError::UnsupportedBlock(name.to_owned()))
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err(ChunkStorageError::Missing("block_states.palette")),
        };
        let indices = if palette.len() == 1 {
            vec![0_usize; 4096]
        } else {
            let bits = bit_width(palette.len() - 1).max(4);
            let data = match block_states.get("data") {
                Some(Tag::LongArray(values)) => values,
                _ => return Err(ChunkStorageError::Missing("block_states.data")),
            };
            unpack_values(data, bits, 4096, palette.len())?
        };
        for (index, palette_index) in indices.into_iter().enumerate() {
            let local_y = (index / 256) as u8;
            let z = ((index / 16) % 16) as u8;
            let x = (index % 16) as u8;
            let world_y = section_y * 16 + i32::from(local_y);
            if !chunk.set_block(x, world_y, z, palette[palette_index]) {
                return Err(ChunkStorageError::InvalidSection(section_y));
            }
        }
    }
    Ok(chunk)
}

fn compound<'a>(
    tag: &'a Tag,
    field: &'static str,
) -> Result<&'a BTreeMap<String, Tag>, ChunkStorageError> {
    match tag {
        Tag::Compound(values) => Ok(values),
        _ => Err(ChunkStorageError::WrongType(field)),
    }
}

fn column_heights(chunk: &Chunk) -> [u16; 256] {
    let mut values = [0_u16; 256];
    for z in 0..16_u8 {
        for x in 0..16_u8 {
            values[usize::from(z) * 16 + usize::from(x)] =
                chunk.surface_y(x, z).map_or(0, |y| (y + 1 - MIN_Y) as u16);
        }
    }
    values
}

fn pack_values(bits: u8, values: impl Iterator<Item = u64>) -> Vec<i64> {
    let entries_per_long = 64 / usize::from(bits);
    let values = values.collect::<Vec<_>>();
    values
        .chunks(entries_per_long)
        .map(|group| {
            let mut packed = 0_u64;
            for (index, value) in group.iter().enumerate() {
                packed |= value << (index * usize::from(bits));
            }
            packed as i64
        })
        .collect()
}

fn unpack_values(
    data: &[i64],
    bits: u8,
    count: usize,
    palette_len: usize,
) -> Result<Vec<usize>, ChunkStorageError> {
    let entries_per_long = 64 / usize::from(bits);
    let expected_longs = count.div_ceil(entries_per_long);
    if data.len() != expected_longs {
        return Err(ChunkStorageError::PackedLength {
            actual: data.len(),
            expected: expected_longs,
        });
    }
    let mask = (1_u64 << bits) - 1;
    (0..count)
        .map(|index| {
            let value = ((data[index / entries_per_long] as u64
                >> ((index % entries_per_long) * usize::from(bits)))
                & mask) as usize;
            if value >= palette_len {
                Err(ChunkStorageError::PaletteIndex {
                    index: value,
                    palette_len,
                })
            } else {
                Ok(value)
            }
        })
        .collect()
}

fn bit_width(value: usize) -> u8 {
    (usize::BITS - value.leading_zeros()) as u8
}

/// Chunk NBT field or palette incompatibility.
#[derive(Debug, Error)]
pub enum ChunkStorageError {
    /// A required field was absent.
    #[error("chunk NBT is missing {0}")]
    Missing(&'static str),
    /// A field had the wrong NBT type.
    #[error("chunk NBT field {0} has the wrong type")]
    WrongType(&'static str),
    /// Stored coordinates did not match the region slot.
    #[error("chunk {field} is {actual}; expected {expected}")]
    CoordinateMismatch {
        /// Coordinate field name.
        field: &'static str,
        /// Region-slot coordinate.
        expected: i32,
        /// NBT coordinate.
        actual: i32,
    },
    /// Section coordinate was invalid.
    #[error("invalid chunk section Y={0}")]
    InvalidSection(i32),
    /// A block is not yet representable in Toucan's stable registry.
    #[error("unsupported stored block state {0}")]
    UnsupportedBlock(String),
    /// Packed palette storage had the wrong number of longs.
    #[error("packed palette has {actual} longs; expected {expected}")]
    PackedLength {
        /// Observed count.
        actual: usize,
        /// Required count.
        expected: usize,
    },
    /// Packed palette index exceeded the palette.
    #[error("palette index {index} exceeds palette length {palette_len}")]
    PaletteIndex {
        /// Decoded palette index.
        index: usize,
        /// Palette entry count.
        palette_len: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::{decode_chunk, encode_chunk};
    use crate::{BlockStateId, Chunk, ChunkPosition};

    #[test]
    fn domain_chunk_round_trips_through_anvil_nbt() {
        let position = ChunkPosition { x: -3, z: 7 };
        let mut chunk = Chunk::empty(position);
        assert!(chunk.set_block(1, 64, 2, BlockStateId::GRASS_BLOCK));
        assert!(chunk.set_block(2, 64, 2, BlockStateId::OAK_PLANKS));
        let decoded = decode_chunk(position, &encode_chunk(&chunk))
            .unwrap_or_else(|error| panic!("chunk should round trip: {error}"));
        assert_eq!(decoded, chunk);
    }
}
