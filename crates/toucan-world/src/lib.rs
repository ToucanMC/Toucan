mod chunk;
mod fluid;
mod generator;
mod storage;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

pub use chunk::{Chunk, ChunkPosition, ChunkSection, MIN_Y, SECTION_COUNT, WORLD_HEIGHT};
pub use fluid::{
    FluidChange, FluidKind, FluidState, block_after_break, fluid_state, source_fluid_state,
    with_waterlogged,
};
pub use generator::{ChunkGenerator, FlatGenerator, TerrainGenerator};
use thiserror::Error;
use toucan_nbt::{NamedTag, NbtError, NbtLimits, Tag, from_gzip, to_gzip};
use toucan_region::{RegionChunkPosition, RegionError, RegionStore};
pub use toucan_registry::BlockStateId;
use toucan_registry::{RegistryError, vanilla_registries};

use crate::storage::ChunkStorageError;

pub const DATA_VERSION_26_1_2: i32 = 4790;
const DEFAULT_SEED: i64 = 0x544f_5543_414e;
const MAX_REGION_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeneratorKind {
    Terrain,
    Flat,
}

impl GeneratorKind {
    fn create(self) -> Box<dyn ChunkGenerator> {
        match self {
            Self::Terrain => Box::new(TerrainGenerator),
            Self::Flat => Box::new(FlatGenerator),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BlockPosition {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LevelMetadata {
    pub data_version: i32,
    pub level_name: String,
    pub spawn: BlockPosition,
    pub seed: i64,
    pub document: NamedTag,
}

impl LevelMetadata {
    pub fn load(world: impl AsRef<Path>, limits: NbtLimits) -> Result<Self, WorldError> {
        let path = world.as_ref().join("level.dat");
        let bytes = fs::read(&path).map_err(|source| WorldError::Read {
            path: path.clone(),
            source,
        })?;
        Self::from_gzip(&bytes, limits)
    }

    pub fn from_gzip(bytes: &[u8], limits: NbtLimits) -> Result<Self, WorldError> {
        let document = from_gzip(bytes, limits)?;
        Self::from_document(document)
    }

    fn generated(level_name: String, seed: i64, spawn: BlockPosition) -> Self {
        let mut version = BTreeMap::new();
        version.insert("Id".into(), Tag::Int(DATA_VERSION_26_1_2));
        version.insert("Name".into(), Tag::String("26.1.2".into()));
        version.insert("Series".into(), Tag::String("main".into()));
        version.insert("Snapshot".into(), Tag::Byte(0));

        let mut data = BTreeMap::new();
        data.insert("DataVersion".into(), Tag::Int(DATA_VERSION_26_1_2));
        data.insert("LevelName".into(), Tag::String(level_name.clone()));
        data.insert("SpawnX".into(), Tag::Int(spawn.x));
        data.insert("SpawnY".into(), Tag::Int(spawn.y));
        data.insert("SpawnZ".into(), Tag::Int(spawn.z));
        data.insert("RandomSeed".into(), Tag::Long(seed));
        data.insert("GameType".into(), Tag::Int(0));
        data.insert("Difficulty".into(), Tag::Byte(2));
        data.insert("DifficultyLocked".into(), Tag::Byte(0));
        data.insert("hardcore".into(), Tag::Byte(0));
        data.insert("allowCommands".into(), Tag::Byte(1));
        data.insert("initialized".into(), Tag::Byte(1));
        data.insert("version".into(), Tag::Int(19133));
        data.insert("Version".into(), Tag::Compound(version));

        let mut root = BTreeMap::new();
        root.insert("Data".into(), Tag::Compound(data));
        let document = NamedTag {
            name: String::new(),
            value: Tag::Compound(root),
        };
        Self {
            data_version: DATA_VERSION_26_1_2,
            level_name,
            spawn,
            seed,
            document,
        }
    }

    fn from_document(document: NamedTag) -> Result<Self, WorldError> {
        let data = compound_field(&document.value, "Data")?;
        let data_version = int_field(data, "DataVersion")?;
        let level_name = string_field(data, "LevelName")?.to_owned();
        let spawn = BlockPosition {
            x: int_field(data, "SpawnX")?,
            y: int_field(data, "SpawnY")?,
            z: int_field(data, "SpawnZ")?,
        };
        let seed = data
            .get("RandomSeed")
            .and_then(|value| match value {
                Tag::Long(seed) => Some(*seed),
                _ => None,
            })
            .unwrap_or(DEFAULT_SEED);
        Ok(Self {
            data_version,
            level_name,
            spawn,
            seed,
            document,
        })
    }

    fn save_new(&self, world: &Path, limits: NbtLimits) -> Result<(), WorldError> {
        let target = world.join("level.dat");
        let temporary = world.join("level.dat_new");
        let bytes = to_gzip(&self.document, limits)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| WorldError::Write {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| WorldError::Write {
                path: temporary.clone(),
                source,
            })?;
        drop(file);
        fs::rename(&temporary, &target).map_err(|source| WorldError::Write {
            path: target,
            source,
        })?;
        Ok(())
    }
}

pub struct World {
    path: PathBuf,
    metadata: LevelMetadata,
    generator: Box<dyn ChunkGenerator>,
    chunks: RwLock<HashMap<ChunkPosition, Arc<Chunk>>>,
    dirty_chunks: RwLock<HashSet<ChunkPosition>>,
    saving_chunks: RwLock<HashSet<ChunkPosition>>,
    fluid_scheduler: Mutex<fluid::FluidScheduler>,
    regions: RegionStore,
    max_loaded_chunks: usize,
}

impl World {
    pub fn open_or_create(
        path: impl AsRef<Path>,
        max_loaded_chunks: usize,
        generator_kind: GeneratorKind,
        configured_seed: i64,
    ) -> Result<Self, WorldError> {
        vanilla_registries()?;
        if max_loaded_chunks == 0 {
            return Err(WorldError::InvalidChunkCapacity);
        }
        let path = path.as_ref().to_owned();
        create_world_directories(&path)?;
        let limits = NbtLimits::default();
        let generator = generator_kind.create();
        let metadata = if path.join("level.dat").exists() {
            LevelMetadata::load(&path, limits)?
        } else {
            let level_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("Toucan World")
                .to_owned();
            let spawn = BlockPosition {
                x: 0,
                y: generator.surface_y(configured_seed, 0, 0) + 1,
                z: 0,
            };
            let metadata = LevelMetadata::generated(level_name, configured_seed, spawn);
            metadata.save_new(&path, limits)?;
            metadata
        };
        Ok(Self {
            regions: RegionStore::new(path.join("region"), limits, MAX_REGION_BYTES),
            path,
            metadata,
            generator,
            chunks: RwLock::new(HashMap::new()),
            dirty_chunks: RwLock::new(HashSet::new()),
            saving_chunks: RwLock::new(HashSet::new()),
            fluid_scheduler: Mutex::new(fluid::FluidScheduler::default()),
            max_loaded_chunks,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn chunk(&self, position: ChunkPosition) -> Result<Arc<Chunk>, WorldError> {
        if let Some(chunk) = self
            .chunks
            .read()
            .map_err(|_| WorldError::LockPoisoned)?
            .get(&position)
            .cloned()
        {
            return Ok(chunk);
        }

        let stored = self.regions.read_chunk(region_position(position))?;
        let (generated, fluid_ticks) = match stored {
            Some(document) => {
                let decoded = storage::decode_chunk(position, &document)?;
                (Arc::new(decoded.chunk), decoded.fluid_ticks)
            }
            None => (
                Arc::new(self.generator.generate(self.metadata.seed, position)),
                Vec::new(),
            ),
        };
        let mut chunks = self.chunks.write().map_err(|_| WorldError::LockPoisoned)?;
        if let Some(chunk) = chunks.get(&position).cloned() {
            return Ok(chunk);
        }
        if chunks.len() >= self.max_loaded_chunks {
            let dirty = self
                .dirty_chunks
                .read()
                .map_err(|_| WorldError::LockPoisoned)?;
            let saving = self
                .saving_chunks
                .read()
                .map_err(|_| WorldError::LockPoisoned)?;
            let evictable = chunks
                .keys()
                .copied()
                .find(|candidate| !dirty.contains(candidate) && !saving.contains(candidate));
            drop(saving);
            drop(dirty);
            if let Some(evictable) = evictable {
                chunks.remove(&evictable);
            } else {
                return Err(WorldError::ChunkCapacity {
                    limit: self.max_loaded_chunks,
                });
            }
        }
        chunks.insert(position, Arc::clone(&generated));
        drop(chunks);
        if !fluid_ticks.is_empty() {
            let mut scheduler = self
                .fluid_scheduler
                .lock()
                .map_err(|_| WorldError::LockPoisoned)?;
            for tick in fluid_ticks {
                scheduler.restore(tick);
            }
        }
        Ok(generated)
    }

    pub fn block(&self, position: BlockPosition) -> Result<BlockStateId, WorldError> {
        let chunk = self.chunk(ChunkPosition::from_block(position.x, position.z))?;
        chunk
            .block(
                position.x.rem_euclid(16) as u8,
                position.y,
                position.z.rem_euclid(16) as u8,
            )
            .ok_or(WorldError::InvalidBlockPosition(position))
    }

    pub(crate) fn loaded_block(
        &self,
        position: BlockPosition,
    ) -> Result<Option<BlockStateId>, WorldError> {
        if !(MIN_Y..MIN_Y + WORLD_HEIGHT).contains(&position.y) {
            return Ok(None);
        }
        let chunk_position = ChunkPosition::from_block(position.x, position.z);
        let chunks = self.chunks.read().map_err(|_| WorldError::LockPoisoned)?;
        Ok(chunks.get(&chunk_position).and_then(|chunk| {
            chunk.block(
                position.x.rem_euclid(16) as u8,
                position.y,
                position.z.rem_euclid(16) as u8,
            )
        }))
    }

    pub fn set_block(
        &self,
        position: BlockPosition,
        state: BlockStateId,
    ) -> Result<BlockStateId, WorldError> {
        let chunk_position = ChunkPosition::from_block(position.x, position.z);
        let _ = self.chunk(chunk_position)?;
        let mut chunks = self.chunks.write().map_err(|_| WorldError::LockPoisoned)?;
        let current = chunks
            .get(&chunk_position)
            .cloned()
            .ok_or(WorldError::LockPoisoned)?;
        let mut updated = (*current).clone();
        let x = position.x.rem_euclid(16) as u8;
        let z = position.z.rem_euclid(16) as u8;
        let previous = updated
            .block(x, position.y, z)
            .ok_or(WorldError::InvalidBlockPosition(position))?;
        if !updated.set_block(x, position.y, z, state) {
            return Err(WorldError::InvalidBlockPosition(position));
        }
        chunks.insert(chunk_position, Arc::new(updated));
        drop(chunks);
        if previous != state {
            self.dirty_chunks
                .write()
                .map_err(|_| WorldError::LockPoisoned)?
                .insert(chunk_position);
            let delay = fluid::update_delay_after_change(self, position, previous, state)?;
            self.fluid_scheduler
                .lock()
                .map_err(|_| WorldError::LockPoisoned)?
                .schedule_around(position, delay);
        }
        Ok(previous)
    }

    pub fn tick_fluids(&self, maximum_updates: usize) -> Result<Vec<FluidChange>, WorldError> {
        if maximum_updates == 0 {
            return Ok(Vec::new());
        }
        let positions = self
            .fluid_scheduler
            .lock()
            .map_err(|_| WorldError::LockPoisoned)?
            .advance(maximum_updates);
        let mut changes = Vec::new();
        for position in positions {
            changes.extend(fluid::update_at(self, position)?);
        }
        Ok(changes)
    }

    pub fn save_dirty(&self) -> Result<usize, WorldError> {
        let positions = {
            let mut dirty = self
                .dirty_chunks
                .write()
                .map_err(|_| WorldError::LockPoisoned)?;
            let positions = std::mem::take(&mut *dirty).into_iter().collect::<Vec<_>>();
            self.saving_chunks
                .write()
                .map_err(|_| WorldError::LockPoisoned)?
                .extend(positions.iter().copied());
            positions
        };
        let mut regions = BTreeMap::<(i32, i32), Vec<(ChunkPosition, NamedTag)>>::new();
        {
            let chunks = self.chunks.read().map_err(|_| WorldError::LockPoisoned)?;
            for position in positions.iter().copied() {
                let Some(chunk) = chunks.get(&position) else {
                    drop(chunks);
                    let mut saving = self
                        .saving_chunks
                        .write()
                        .map_err(|_| WorldError::LockPoisoned)?;
                    for position in &positions {
                        saving.remove(position);
                    }
                    drop(saving);
                    self.dirty_chunks
                        .write()
                        .map_err(|_| WorldError::LockPoisoned)?
                        .extend(positions.iter().copied());
                    return Err(WorldError::MissingDirtyChunk(position));
                };
                let fluid_ticks = self.stored_fluid_ticks(position, chunk)?;
                regions
                    .entry((position.x.div_euclid(32), position.z.div_euclid(32)))
                    .or_default()
                    .push((position, storage::encode_chunk(chunk, &fluid_ticks)?));
            }
        }
        let region_groups = regions.into_values().collect::<Vec<_>>();
        for (index, group) in region_groups.iter().enumerate() {
            let documents = group
                .iter()
                .map(|(position, document)| (region_position(*position), document))
                .collect::<Vec<_>>();
            if let Err(error) = self.regions.write_chunks(&documents) {
                let retry_positions = region_groups[index..]
                    .iter()
                    .flatten()
                    .map(|(position, _)| *position)
                    .collect::<Vec<_>>();
                let mut saving = self
                    .saving_chunks
                    .write()
                    .map_err(|_| WorldError::LockPoisoned)?;
                for position in &positions {
                    saving.remove(position);
                }
                drop(saving);
                self.dirty_chunks
                    .write()
                    .map_err(|_| WorldError::LockPoisoned)?
                    .extend(retry_positions);
                return Err(error.into());
            }
        }
        let mut saving = self
            .saving_chunks
            .write()
            .map_err(|_| WorldError::LockPoisoned)?;
        for position in &positions {
            saving.remove(position);
        }
        Ok(positions.len())
    }

    pub fn dirty_chunk_count(&self) -> Result<usize, WorldError> {
        Ok(self
            .dirty_chunks
            .read()
            .map_err(|_| WorldError::LockPoisoned)?
            .len())
    }

    pub fn loaded_chunk_count(&self) -> Result<usize, WorldError> {
        Ok(self
            .chunks
            .read()
            .map_err(|_| WorldError::LockPoisoned)?
            .len())
    }

    fn stored_fluid_ticks(
        &self,
        position: ChunkPosition,
        chunk: &Chunk,
    ) -> Result<Vec<fluid::StoredFluidTick>, WorldError> {
        let pending = self
            .fluid_scheduler
            .lock()
            .map_err(|_| WorldError::LockPoisoned)?
            .snapshot(position);
        let mut stored = Vec::new();
        for (position, delay) in pending {
            let x = position.x.rem_euclid(16) as u8;
            let z = position.z.rem_euclid(16) as u8;
            let mut kind = chunk
                .block(x, position.y, z)
                .and_then(|state| fluid_state(state).ok().flatten())
                .map(FluidState::kind);
            if kind.is_none() {
                for (dx, dy, dz) in [
                    (0, -1, 0),
                    (0, 1, 0),
                    (0, 0, -1),
                    (0, 0, 1),
                    (-1, 0, 0),
                    (1, 0, 0),
                ] {
                    let neighbor_x = position.x.saturating_add(dx);
                    let neighbor_z = position.z.saturating_add(dz);
                    if ChunkPosition::from_block(neighbor_x, neighbor_z) != chunk.position() {
                        continue;
                    }
                    kind = chunk
                        .block(
                            neighbor_x.rem_euclid(16) as u8,
                            position.y.saturating_add(dy),
                            neighbor_z.rem_euclid(16) as u8,
                        )
                        .and_then(|state| fluid_state(state).ok().flatten())
                        .map(FluidState::kind);
                    if kind.is_some() {
                        break;
                    }
                }
            }
            if let Some(kind) = kind {
                stored.push(fluid::StoredFluidTick {
                    position,
                    kind,
                    delay,
                });
            }
        }
        Ok(stored)
    }

    #[must_use]
    pub const fn metadata(&self) -> &LevelMetadata {
        &self.metadata
    }

    #[must_use]
    pub fn generator_identifier(&self) -> &'static str {
        self.generator.identifier()
    }
}

#[derive(Debug, Error)]
pub enum WorldError {
    #[error("failed to create world directory at {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read world metadata at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write world metadata at {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Nbt(#[from] NbtError),
    #[error("level.dat is missing required field Data.{0}")]
    MissingField(&'static str),
    #[error("level.dat field Data.{0} has the wrong NBT type")]
    InvalidFieldType(&'static str),
    #[error("world chunk capacity must be greater than zero")]
    InvalidChunkCapacity,
    #[error("loaded chunk capacity {limit} reached with no clean chunk available for eviction")]
    ChunkCapacity { limit: usize },
    #[error("world chunk cache lock was poisoned")]
    LockPoisoned,
    #[error("block position {0:?} is outside the supported world bounds")]
    InvalidBlockPosition(BlockPosition),
    #[error("dirty chunk {0:?} is absent from the loaded chunk cache")]
    MissingDirtyChunk(ChunkPosition),
    #[error(transparent)]
    Region(#[from] RegionError),
    #[error(transparent)]
    ChunkStorage(#[from] ChunkStorageError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

const fn region_position(position: ChunkPosition) -> RegionChunkPosition {
    RegionChunkPosition {
        x: position.x,
        z: position.z,
    }
}

fn create_world_directories(world: &Path) -> Result<(), WorldError> {
    for relative in [
        "",
        "region",
        "entities",
        "poi",
        "playerdata",
        "data",
        "datapacks",
        "DIM-1",
        "DIM1",
    ] {
        let path = world.join(relative);
        fs::create_dir_all(&path).map_err(|source| WorldError::CreateDirectory { path, source })?;
    }
    Ok(())
}

fn compound_field<'a>(
    tag: &'a Tag,
    field: &'static str,
) -> Result<&'a BTreeMap<String, Tag>, WorldError> {
    let value = tag.get(field).ok_or(WorldError::MissingField(field))?;
    match value {
        Tag::Compound(value) => Ok(value),
        _ => Err(WorldError::InvalidFieldType(field)),
    }
}

fn int_field(data: &BTreeMap<String, Tag>, field: &'static str) -> Result<i32, WorldError> {
    data.get(field)
        .ok_or(WorldError::MissingField(field))?
        .as_i32()
        .ok_or(WorldError::InvalidFieldType(field))
}

fn string_field<'a>(
    data: &'a BTreeMap<String, Tag>,
    field: &'static str,
) -> Result<&'a str, WorldError> {
    data.get(field)
        .ok_or(WorldError::MissingField(field))?
        .as_str()
        .ok_or(WorldError::InvalidFieldType(field))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Write;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use toucan_nbt::{NamedTag, NbtLimits, Tag, to_bytes};

    use super::{
        BlockPosition, BlockStateId, ChunkPosition, GeneratorKind, LevelMetadata, World, WorldError,
    };

    #[test]
    fn loads_required_metadata_and_retains_unknown_fields() {
        let mut data = BTreeMap::new();
        data.insert("DataVersion".into(), Tag::Int(4790));
        data.insert("LevelName".into(), Tag::String("Toucan Test".into()));
        data.insert("SpawnX".into(), Tag::Int(10));
        data.insert("SpawnY".into(), Tag::Int(64));
        data.insert("SpawnZ".into(), Tag::Int(-5));
        data.insert("UnknownFutureField".into(), Tag::Long(42));
        let mut root = BTreeMap::new();
        root.insert("Data".into(), Tag::Compound(data));
        let document = NamedTag {
            name: String::new(),
            value: Tag::Compound(root),
        };
        let bytes = to_bytes(&document, NbtLimits::default()).unwrap_or_default();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        assert!(encoder.write_all(&bytes).is_ok());
        let compressed = encoder.finish().unwrap_or_default();

        let metadata = LevelMetadata::from_gzip(&compressed, NbtLimits::default())
            .unwrap_or_else(|error| panic!("level.dat should load: {error}"));
        assert_eq!(metadata.data_version, 4790);
        assert_eq!(metadata.level_name, "Toucan Test");
        assert_eq!(
            metadata.spawn,
            BlockPosition {
                x: 10,
                y: 64,
                z: -5,
            }
        );
        assert_eq!(
            metadata
                .document
                .value
                .get("Data")
                .and_then(|data| data.get("UnknownFutureField")),
            Some(&Tag::Long(42))
        );
    }

    #[test]
    fn creates_vanilla_shaped_world_and_caches_generated_chunks() {
        let path = temporary_world("create");
        let world = World::open_or_create(&path, 2, GeneratorKind::Flat, 42)
            .unwrap_or_else(|error| panic!("world should be generated: {error}"));
        assert!(path.join("level.dat").is_file());
        assert!(path.join("region").is_dir());
        assert_eq!(world.metadata().spawn.y, 64);
        assert_eq!(world.generator_identifier(), "toucan:flat");

        let first = world
            .chunk(ChunkPosition { x: 0, z: 0 })
            .unwrap_or_else(|error| panic!("chunk should generate: {error}"));
        let cached = world
            .chunk(ChunkPosition { x: 0, z: 0 })
            .unwrap_or_else(|error| panic!("chunk should cache: {error}"));
        assert!(Arc::ptr_eq(&first, &cached));
        assert_eq!(first.block(0, 63, 0), Some(BlockStateId::STONE));
        assert_eq!(world.loaded_chunk_count().unwrap_or_default(), 1);
        let position = BlockPosition { x: 0, y: 63, z: 0 };
        let high_state_position = BlockPosition { x: 1, y: 63, z: 0 };
        assert_eq!(
            world
                .set_block(position, BlockStateId::AIR)
                .unwrap_or(BlockStateId::AIR),
            BlockStateId::STONE
        );
        assert_eq!(
            world.block(position).unwrap_or(BlockStateId::STONE),
            BlockStateId::AIR
        );
        assert!(
            world
                .set_block(high_state_position, BlockStateId::DEEPSLATE)
                .is_ok()
        );
        assert_eq!(world.dirty_chunk_count().unwrap_or_default(), 1);
        assert_eq!(world.save_dirty().unwrap_or_default(), 1);
        assert_eq!(world.dirty_chunk_count().unwrap_or(1), 0);

        let reopened = World::open_or_create(&path, 2, GeneratorKind::Flat, 99)
            .unwrap_or_else(|error| panic!("generated metadata should reopen: {error}"));
        assert_eq!(reopened.metadata().spawn, world.metadata().spawn);
        assert_eq!(
            reopened.block(position).unwrap_or(BlockStateId::STONE),
            BlockStateId::AIR
        );
        assert_eq!(
            reopened
                .block(high_state_position)
                .unwrap_or(BlockStateId::STONE),
            BlockStateId::DEEPSLATE
        );
        assert!(fs::remove_dir_all(&path).is_ok());
    }

    #[test]
    fn evicts_clean_chunks_at_loaded_chunk_capacity() {
        let path = temporary_world("capacity");
        let world = World::open_or_create(&path, 1, GeneratorKind::Flat, 42)
            .unwrap_or_else(|error| panic!("world should be generated: {error}"));
        let first = world.chunk(ChunkPosition { x: 0, z: 0 });
        assert!(first.is_ok());
        assert!(world.chunk(ChunkPosition { x: 1, z: 0 }).is_ok());
        assert_eq!(world.loaded_chunk_count().unwrap_or_default(), 1);
        assert!(world.chunk(ChunkPosition { x: 0, z: 0 }).is_ok());
        assert_eq!(world.loaded_chunk_count().unwrap_or_default(), 1);
        assert!(fs::remove_dir_all(&path).is_ok());
    }

    #[test]
    fn retains_dirty_chunks_when_capacity_is_exhausted() {
        let path = temporary_world("dirty-capacity");
        let world = World::open_or_create(&path, 1, GeneratorKind::Flat, 42)
            .unwrap_or_else(|error| panic!("world should be generated: {error}"));
        assert!(
            world
                .set_block(BlockPosition { x: 0, y: 63, z: 0 }, BlockStateId::AIR)
                .is_ok()
        );
        assert!(matches!(
            world.chunk(ChunkPosition { x: 1, z: 0 }),
            Err(WorldError::ChunkCapacity { limit: 1 })
        ));
        assert_eq!(world.save_dirty().unwrap_or_default(), 1);
        assert!(world.chunk(ChunkPosition { x: 1, z: 0 }).is_ok());
        assert!(fs::remove_dir_all(&path).is_ok());
    }

    #[test]
    fn saves_multiple_dirty_chunks_in_region_batches() {
        let path = temporary_world("batch-save");
        let world = World::open_or_create(&path, 3, GeneratorKind::Flat, 42)
            .unwrap_or_else(|error| panic!("world should be generated: {error}"));
        let positions = [
            BlockPosition { x: 0, y: 63, z: 0 },
            BlockPosition { x: 16, y: 63, z: 0 },
            BlockPosition {
                x: 512,
                y: 63,
                z: 0,
            },
        ];
        for position in positions {
            assert!(world.set_block(position, BlockStateId::AIR).is_ok());
        }
        assert_eq!(world.dirty_chunk_count().unwrap_or_default(), 3);
        assert_eq!(world.save_dirty().unwrap_or_default(), 3);

        let reopened = World::open_or_create(&path, 3, GeneratorKind::Flat, 99)
            .unwrap_or_else(|error| panic!("saved world should reopen: {error}"));
        for position in positions {
            assert_eq!(
                reopened.block(position).unwrap_or(BlockStateId::STONE),
                BlockStateId::AIR
            );
        }
        assert!(fs::remove_dir_all(&path).is_ok());
    }

    fn temporary_world(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "toucan-world-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn scheduled_fluid_updates_survive_world_save_and_reopen() {
        let path = std::env::temp_dir().join(format!(
            "toucan-world-fluid-persistence-test-{}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path).expect("remove stale fluid test world");
        }
        let source_position = BlockPosition { x: 0, y: 64, z: 0 };
        let flowing_position = BlockPosition { x: 1, y: 64, z: 0 };
        {
            let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)
                .expect("create fluid test world");
            let source =
                crate::source_fluid_state(crate::FluidKind::Water).expect("resolve source water");
            world
                .set_block(source_position, source)
                .expect("place source water");
            assert_eq!(world.save_dirty().expect("save source water"), 1);
        }

        let reopened = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)
            .expect("reopen fluid test world");
        reopened.block(source_position).expect("load source chunk");
        let mut changes = Vec::new();
        for _ in 0..5 {
            changes.extend(reopened.tick_fluids(64).expect("run restored fluid ticks"));
        }
        assert!(!changes.is_empty());
        let flowing = crate::fluid_state(
            reopened
                .block(flowing_position)
                .expect("read restored flow"),
        )
        .expect("resolve restored flow")
        .expect("water should flow after reload");
        assert_eq!(flowing.kind(), crate::FluidKind::Water);
        std::fs::remove_dir_all(path).expect("remove fluid test world");
    }
}
