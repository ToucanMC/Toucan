use std::collections::BTreeMap;

use thiserror::Error;
use toucan_nbt::{NamedTag, Tag};
use toucan_registry::{RegistryError, vanilla_registries};

use crate::tick::{ScheduledTick, StoredTick, TickTarget};
use crate::{
    BlockStateId, Chunk, ChunkPosition, DATA_VERSION_26_1_2, FluidKind, MIN_Y, SECTION_COUNT,
};

#[derive(Clone, Debug)]
pub(crate) struct RawChunkDocument(NamedTag);

impl RawChunkDocument {
    pub(crate) fn new(document: NamedTag) -> Self {
        Self(document)
    }

    fn document(&self) -> &NamedTag {
        &self.0
    }
}

pub(crate) struct DecodedChunk {
    pub chunk: Chunk,
    pub ticks: Vec<StoredTick>,
    pub raw: RawChunkDocument,
}

pub(crate) fn encode_chunk(
    chunk: &Chunk,
    ticks: &[StoredTick],
    original: Option<&RawChunkDocument>,
) -> Result<NamedTag, ChunkStorageError> {
    let registries = vanilla_registries()?;
    let plains = registries.biome_by_name("minecraft:plains")?;
    let position = chunk.position();
    let block_states = (0..SECTION_COUNT)
        .map(|section_index| -> Result<(i8, Tag), ChunkStorageError> {
            let section = chunk
                .section(section_index)
                .ok_or(ChunkStorageError::InvalidSection(section_index as i32 - 4))?;
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
                .map(|state| -> Result<Tag, ChunkStorageError> {
                    let state_definition = registries.state(*state)?;
                    let block = registries.block(state_definition.block())?;
                    let mut entry = BTreeMap::new();
                    entry.insert("Name".into(), Tag::String(block.name().as_str().into()));
                    let state_properties = registries.state_properties(*state)?;
                    if !state_properties.is_empty() {
                        let mut properties = BTreeMap::new();
                        for (name, value) in state_properties {
                            properties.insert(name.into(), Tag::String(value));
                        }
                        entry.insert("Properties".into(), Tag::Compound(properties));
                    }
                    Ok(Tag::Compound(entry))
                })
                .collect::<Result<Vec<_>, _>>()?;
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
            Ok((section_index as i8 - 4, Tag::Compound(block_states)))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let sections = patch_sections(original, &block_states, plains.name().as_str())?;

    let mut heightmaps = match original {
        Some(raw) => match compound(&raw.document().value, "root")?.get("Heightmaps") {
            Some(Tag::Compound(heightmaps)) => heightmaps.clone(),
            Some(_) => return Err(ChunkStorageError::WrongType("Heightmaps")),
            None => BTreeMap::new(),
        },
        None => BTreeMap::new(),
    };
    let heights = column_heights(chunk);
    for name in ["MOTION_BLOCKING", "WORLD_SURFACE"] {
        heightmaps.insert(
            name.into(),
            Tag::LongArray(pack_values(9, heights.iter().copied().map(u64::from))),
        );
    }
    let mut root = match original {
        Some(original) => compound(&original.document().value, "root")?.clone(),
        None => generated_root(position),
    };
    root.insert("xPos".into(), Tag::Int(position.x));
    root.insert("zPos".into(), Tag::Int(position.z));
    root.insert(
        "sections".into(),
        Tag::List {
            element_type: 10,
            values: sections,
        },
    );
    root.insert("Heightmaps".into(), Tag::Compound(heightmaps));
    root.insert("block_ticks".into(), encode_block_ticks(ticks)?);
    root.insert("fluid_ticks".into(), encode_fluid_ticks(ticks));
    Ok(NamedTag {
        name: String::new(),
        value: Tag::Compound(root),
    })
}

fn generated_root(position: ChunkPosition) -> BTreeMap<String, Tag> {
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
    root.insert("block_entities".into(), empty_compounds());
    root
}

fn patch_sections(
    original: Option<&RawChunkDocument>,
    block_states: &BTreeMap<i8, Tag>,
    default_biome: &str,
) -> Result<Vec<Tag>, ChunkStorageError> {
    let mut retained = BTreeMap::<i8, BTreeMap<String, Tag>>::new();
    let mut unsupported = Vec::new();
    if let Some(original) = original {
        let root = compound(&original.document().value, "root")?;
        if let Some(sections) = root.get("sections") {
            let Tag::List {
                element_type: 10,
                values,
            } = sections
            else {
                return Err(ChunkStorageError::WrongType("sections"));
            };
            for section in values {
                let section = compound(section, "section")?.clone();
                match section.get("Y") {
                    Some(Tag::Byte(y)) if block_states.contains_key(y) => {
                        retained.insert(*y, section);
                    }
                    Some(Tag::Byte(_)) => unsupported.push(Tag::Compound(section)),
                    _ => return Err(ChunkStorageError::Missing("sections[].Y")),
                }
            }
        }
    }

    let mut patched = Vec::with_capacity(block_states.len() + unsupported.len());
    for (&y, encoded_states) in block_states {
        let mut section = retained.remove(&y).unwrap_or_else(|| {
            let mut biomes = BTreeMap::new();
            biomes.insert(
                "palette".into(),
                Tag::List {
                    element_type: 8,
                    values: vec![Tag::String(default_biome.to_owned())],
                },
            );
            let mut section = BTreeMap::new();
            section.insert("Y".into(), Tag::Byte(y));
            section.insert("biomes".into(), Tag::Compound(biomes));
            section
        });
        let encoded_states = compound(encoded_states, "encoded block_states")?;
        let mut patched_states = match section.get("block_states") {
            Some(Tag::Compound(states)) => states.clone(),
            Some(_) => return Err(ChunkStorageError::WrongType("sections[].block_states")),
            None => BTreeMap::new(),
        };
        patched_states.remove("data");
        patched_states.extend(encoded_states.clone());
        section.insert("block_states".into(), Tag::Compound(patched_states));
        patched.push(Tag::Compound(section));
    }
    patched.extend(unsupported);
    Ok(patched)
}

pub(crate) fn decode_chunk(
    expected: ChunkPosition,
    document: &NamedTag,
) -> Result<DecodedChunk, ChunkStorageError> {
    let registries = vanilla_registries()?;
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
    let chunk = Chunk::empty(expected);
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
                    let properties = match entry.get("Properties") {
                        None => Vec::new(),
                        Some(Tag::Compound(properties)) => properties
                            .iter()
                            .map(|(property, value)| {
                                value
                                    .as_str()
                                    .map(|value| (property.as_str(), value))
                                    .ok_or(ChunkStorageError::WrongType(
                                        "block palette Properties[]",
                                    ))
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                        Some(_) => {
                            return Err(ChunkStorageError::WrongType("block palette Properties"));
                        }
                    };
                    registries
                        .resolve_state(name, properties)
                        .map_err(|source| ChunkStorageError::IncompatibleBlockState {
                            name: name.to_owned(),
                            source,
                        })
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
    let mut ticks = decode_fluid_ticks(root, expected)?;
    ticks.extend(decode_block_ticks(root, expected)?);
    Ok(DecodedChunk {
        chunk,
        ticks,
        raw: RawChunkDocument::new(document.clone()),
    })
}

fn encode_fluid_ticks(ticks: &[StoredTick]) -> Tag {
    let values = ticks
        .iter()
        .filter_map(|tick| {
            let TickTarget::Fluid(kind) = tick.scheduled.target else {
                return None;
            };
            let mut encoded = BTreeMap::new();
            encoded.insert("i".into(), Tag::String(kind.identifier().into()));
            encoded.insert("p".into(), Tag::Int(0));
            encoded.insert("t".into(), Tag::Int(tick.delay.min(i32::MAX as u32) as i32));
            encoded.insert("x".into(), Tag::Int(tick.scheduled.position.x));
            encoded.insert("y".into(), Tag::Int(tick.scheduled.position.y));
            encoded.insert("z".into(), Tag::Int(tick.scheduled.position.z));
            Some(Tag::Compound(encoded))
        })
        .collect();
    Tag::List {
        element_type: 10,
        values,
    }
}

fn encode_block_ticks(ticks: &[StoredTick]) -> Result<Tag, ChunkStorageError> {
    let registries = vanilla_registries()?;
    let values = ticks
        .iter()
        .filter_map(|tick| {
            let TickTarget::Block(block) = tick.scheduled.target else {
                return None;
            };
            Some((tick, block))
        })
        .map(|(tick, block)| {
            let mut encoded = BTreeMap::new();
            encoded.insert(
                "i".into(),
                Tag::String(registries.block(block)?.name().as_str().into()),
            );
            encoded.insert("p".into(), Tag::Int(0));
            encoded.insert("t".into(), Tag::Int(tick.delay.min(i32::MAX as u32) as i32));
            encoded.insert("x".into(), Tag::Int(tick.scheduled.position.x));
            encoded.insert("y".into(), Tag::Int(tick.scheduled.position.y));
            encoded.insert("z".into(), Tag::Int(tick.scheduled.position.z));
            Ok(Tag::Compound(encoded))
        })
        .collect::<Result<Vec<_>, RegistryError>>()?;
    Ok(Tag::List {
        element_type: 10,
        values,
    })
}

fn decode_fluid_ticks(
    root: &BTreeMap<String, Tag>,
    expected: ChunkPosition,
) -> Result<Vec<StoredTick>, ChunkStorageError> {
    let values = match root.get("fluid_ticks") {
        None => return Ok(Vec::new()),
        Some(Tag::List {
            element_type: 10,
            values,
        }) => values,
        Some(_) => return Err(ChunkStorageError::WrongType("fluid_ticks")),
    };
    values
        .iter()
        .map(|value| {
            let value = compound(value, "fluid_ticks[]")?;
            let identifier = value
                .get("i")
                .and_then(Tag::as_str)
                .ok_or(ChunkStorageError::Missing("fluid_ticks[].i"))?;
            let kind = match identifier {
                "minecraft:water" | "minecraft:flowing_water" | "water" | "flowing_water" => {
                    FluidKind::Water
                }
                "minecraft:lava" | "minecraft:flowing_lava" | "lava" | "flowing_lava" => {
                    FluidKind::Lava
                }
                _ => return Err(ChunkStorageError::UnknownFluid(identifier.to_owned())),
            };
            let (position, delay) = decode_tick_position(value, expected, "fluid_ticks")?;
            Ok(StoredTick {
                scheduled: ScheduledTick {
                    position,
                    target: TickTarget::Fluid(kind),
                },
                delay,
            })
        })
        .collect()
}

fn decode_block_ticks(
    root: &BTreeMap<String, Tag>,
    expected: ChunkPosition,
) -> Result<Vec<StoredTick>, ChunkStorageError> {
    let values = match root.get("block_ticks") {
        None => return Ok(Vec::new()),
        Some(Tag::List {
            element_type: 10,
            values,
        }) => values,
        Some(_) => return Err(ChunkStorageError::WrongType("block_ticks")),
    };
    let registries = vanilla_registries()?;
    values
        .iter()
        .map(|value| {
            let value = compound(value, "block_ticks[]")?;
            let identifier = value
                .get("i")
                .and_then(Tag::as_str)
                .ok_or(ChunkStorageError::Missing("block_ticks[].i"))?;
            let block = registries.block_by_name(identifier)?.id();
            let (position, delay) = decode_tick_position(value, expected, "block_ticks")?;
            Ok(StoredTick {
                scheduled: ScheduledTick {
                    position,
                    target: TickTarget::Block(block),
                },
                delay,
            })
        })
        .collect()
}

fn decode_tick_position(
    value: &BTreeMap<String, Tag>,
    expected: ChunkPosition,
    field: &'static str,
) -> Result<(crate::BlockPosition, u32), ChunkStorageError> {
    let coordinate = |name: &'static str| {
        value
            .get(name)
            .and_then(Tag::as_i32)
            .ok_or(ChunkStorageError::Missing(name))
    };
    let position = crate::BlockPosition {
        x: coordinate("x")?,
        y: coordinate("y")?,
        z: coordinate("z")?,
    };
    if ChunkPosition::from_block(position.x, position.z) != expected {
        return Err(ChunkStorageError::TickOutsideChunk { field, position });
    }
    let delay = coordinate("t")?;
    if delay < 0 {
        return Err(ChunkStorageError::InvalidTickDelay { field, delay });
    }
    Ok((position, delay as u32))
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

#[derive(Debug, Error)]
pub enum ChunkStorageError {
    #[error("chunk NBT is missing {0}")]
    Missing(&'static str),
    #[error("chunk NBT field {0} has the wrong type")]
    WrongType(&'static str),
    #[error("chunk {field} is {actual}; expected {expected}")]
    CoordinateMismatch {
        field: &'static str,
        expected: i32,
        actual: i32,
    },
    #[error("invalid chunk section Y={0}")]
    InvalidSection(i32),
    #[error("incompatible stored block state `{name}`: {source}")]
    IncompatibleBlockState {
        name: String,
        #[source]
        source: RegistryError,
    },
    #[error("packed palette has {actual} longs; expected {expected}")]
    PackedLength { actual: usize, expected: usize },
    #[error("palette index {index} exceeds palette length {palette_len}")]
    PaletteIndex { index: usize, palette_len: usize },
    #[error("unknown scheduled fluid `{0}`")]
    UnknownFluid(String),
    #[error("scheduled {field} entry at {position:?} is outside its chunk")]
    TickOutsideChunk {
        field: &'static str,
        position: crate::BlockPosition,
    },
    #[error("scheduled {field} entry has negative delay {delay}")]
    InvalidTickDelay { field: &'static str, delay: i32 },
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

#[cfg(test)]
mod tests {
    use toucan_nbt::Tag;
    use toucan_registry::{RegistryError, vanilla_registries};

    use super::{ChunkStorageError, decode_chunk, encode_chunk};
    use crate::tick::{ScheduledTick, StoredTick, TickTarget};
    use crate::{BlockPosition, BlockStateId, Chunk, ChunkPosition, FluidKind};

    #[test]
    fn domain_chunk_round_trips_through_anvil_nbt() {
        let position = ChunkPosition { x: -3, z: 7 };
        let chunk = Chunk::empty(position);
        assert!(chunk.set_block(1, 64, 2, BlockStateId::GRASS_BLOCK));
        assert!(chunk.set_block(2, 64, 2, BlockStateId::OAK_PLANKS));
        assert!(chunk.set_block(3, 64, 2, BlockStateId::OAK_LOG_X));
        let water = vanilla_registries()
            .expect("registries")
            .resolve_state("minecraft:water", [("level", "15")])
            .expect("water state");
        assert!(chunk.set_block(4, 64, 2, water));
        assert!(chunk.set_block(5, 64, 2, BlockStateId::DEEPSLATE));
        let fluid_tick = StoredTick {
            scheduled: ScheduledTick {
                position: BlockPosition {
                    x: -47,
                    y: 64,
                    z: 114,
                },
                target: TickTarget::Fluid(FluidKind::Water),
            },
            delay: 5,
        };
        let block_tick = StoredTick {
            scheduled: ScheduledTick {
                position: BlockPosition {
                    x: -46,
                    y: 65,
                    z: 114,
                },
                target: TickTarget::Block(
                    vanilla_registries()
                        .expect("registries")
                        .block_by_name("minecraft:sand")
                        .expect("sand")
                        .id(),
                ),
            },
            delay: 2,
        };
        let encoded = encode_chunk(&chunk, &[fluid_tick, block_tick], None)
            .expect("known block states should encode");
        let decoded = decode_chunk(position, &encoded)
            .unwrap_or_else(|error| panic!("chunk should round trip: {error}"));
        for y in crate::MIN_Y..crate::MIN_Y + crate::WORLD_HEIGHT {
            assert_eq!(decoded.chunk.block(1, y, 2), chunk.block(1, y, 2));
        }
        assert_eq!(decoded.ticks, vec![fluid_tick, block_tick]);
    }

    #[test]
    fn unknown_and_invalid_palette_entries_fail_without_substitution() {
        let position = ChunkPosition { x: 0, z: 0 };
        let chunk = Chunk::empty(position);
        let mut unknown = encode_chunk(&chunk, &[], None).expect("empty chunk should encode");
        set_first_palette_name(&mut unknown.value, "minecraft:not_a_real_block");
        assert!(matches!(
            decode_chunk(position, &unknown),
            Err(ChunkStorageError::IncompatibleBlockState {
                source: RegistryError::UnknownBlock(_),
                ..
            })
        ));

        let mut invalid = encode_chunk(&chunk, &[], None).expect("empty chunk should encode");
        let root = compound_mut(&mut invalid.value);
        let sections = match root.get_mut("sections") {
            Some(Tag::List { values, .. }) => values,
            _ => panic!("sections list"),
        };
        let section = compound_mut(&mut sections[0]);
        let block_states = compound_mut(section.get_mut("block_states").expect("block states"));
        let palette = match block_states.get_mut("palette") {
            Some(Tag::List { values, .. }) => values,
            _ => panic!("palette list"),
        };
        let entry = compound_mut(&mut palette[0]);
        entry.insert("Name".into(), Tag::String("minecraft:oak_log".into()));
        let mut properties = std::collections::BTreeMap::new();
        properties.insert("axis".into(), Tag::String("diagonal".into()));
        entry.insert("Properties".into(), Tag::Compound(properties));
        assert!(matches!(
            decode_chunk(position, &invalid),
            Err(ChunkStorageError::IncompatibleBlockState {
                source: RegistryError::InvalidPropertyValue { .. },
                ..
            })
        ));
    }

    fn set_first_palette_name(root: &mut Tag, name: &str) {
        let root = compound_mut(root);
        let sections = match root.get_mut("sections") {
            Some(Tag::List { values, .. }) => values,
            _ => panic!("sections list"),
        };
        let section = compound_mut(&mut sections[0]);
        let block_states = compound_mut(section.get_mut("block_states").expect("block states"));
        let palette = match block_states.get_mut("palette") {
            Some(Tag::List { values, .. }) => values,
            _ => panic!("palette list"),
        };
        compound_mut(&mut palette[0]).insert("Name".into(), Tag::String(name.into()));
    }

    fn compound_mut(tag: &mut Tag) -> &mut std::collections::BTreeMap<String, Tag> {
        match tag {
            Tag::Compound(value) => value,
            _ => panic!("compound tag"),
        }
    }
}
