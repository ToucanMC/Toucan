//! Vanilla-shaped world folders and authoritative generated chunk state.

mod block;
mod chunk;
mod generator;
mod storage;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub use block::BlockStateId;
pub use chunk::{Chunk, ChunkPosition, ChunkSection, MIN_Y, SECTION_COUNT, WORLD_HEIGHT};
pub use generator::{ChunkGenerator, FlatGenerator, TerrainGenerator};
use thiserror::Error;
use toucan_nbt::{NamedTag, NbtError, NbtLimits, Tag, from_gzip, to_gzip};
use toucan_region::{RegionChunkPosition, RegionError, RegionStore};

use crate::storage::ChunkStorageError;

/// Data version used by Minecraft 26.1.2 world metadata.
pub const DATA_VERSION_26_1_2: i32 = 4790;
const DEFAULT_SEED: i64 = 0x544f_5543_414e;
const MAX_REGION_BYTES: usize = 256 * 1024 * 1024;

/// Built-in deterministic world generator selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeneratorKind {
    /// Layered stone, dirt, and grass terrain.
    Terrain,
    /// Completely flat stone terrain for tests and building.
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

/// Integer block coordinates used by world-domain state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BlockPosition {
    /// East/west coordinate.
    pub x: i32,
    /// Vertical coordinate.
    pub y: i32,
    /// North/south coordinate.
    pub z: i32,
}

/// Metadata read from a vanilla world's gzip-compressed `level.dat`.
#[derive(Clone, Debug, PartialEq)]
pub struct LevelMetadata {
    /// Vanilla data-version number recorded by the generating version.
    pub data_version: i32,
    /// User-visible level name.
    pub level_name: String,
    /// Default overworld spawn.
    pub spawn: BlockPosition,
    /// Seed used by Toucan's deterministic generator.
    pub seed: i64,
    /// Complete original document, including fields Toucan does not interpret.
    pub document: NamedTag,
}

impl LevelMetadata {
    /// Loads `<world>/level.dat` with bounded gzip and NBT decoding.
    pub fn load(world: impl AsRef<Path>, limits: NbtLimits) -> Result<Self, WorldError> {
        let path = world.as_ref().join("level.dat");
        let bytes = fs::read(&path).map_err(|source| WorldError::Read {
            path: path.clone(),
            source,
        })?;
        Self::from_gzip(&bytes, limits)
    }

    /// Decodes already-read `level.dat` bytes.
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

/// Shared world service owning metadata, generation, and the bounded chunk cache.
pub struct World {
    path: PathBuf,
    metadata: LevelMetadata,
    generator: Box<dyn ChunkGenerator>,
    chunks: RwLock<HashMap<ChunkPosition, Arc<Chunk>>>,
    dirty_chunks: RwLock<HashSet<ChunkPosition>>,
    regions: RegionStore,
    max_loaded_chunks: usize,
}

impl World {
    /// Opens a world folder or creates generated metadata when none exists.
    pub fn open_or_create(
        path: impl AsRef<Path>,
        max_loaded_chunks: usize,
        generator_kind: GeneratorKind,
        configured_seed: i64,
    ) -> Result<Self, WorldError> {
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
            max_loaded_chunks,
        })
    }

    /// Returns the world folder backing this service.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns loaded or newly generated authoritative chunk state.
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
        let generated = Arc::new(match stored {
            Some(document) => storage::decode_chunk(position, &document)?,
            None => self.generator.generate(self.metadata.seed, position),
        });
        let mut chunks = self.chunks.write().map_err(|_| WorldError::LockPoisoned)?;
        if let Some(chunk) = chunks.get(&position).cloned() {
            return Ok(chunk);
        }
        if chunks.len() >= self.max_loaded_chunks {
            return Err(WorldError::ChunkCapacity {
                limit: self.max_loaded_chunks,
            });
        }
        chunks.insert(position, Arc::clone(&generated));
        Ok(generated)
    }

    /// Returns an authoritative block state at world-space coordinates.
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

    /// Replaces one authoritative block using copy-on-write chunk publication.
    ///
    /// The previous state is returned so callers can validate and broadcast the
    /// mutation and mark the owning chunk dirty for the asynchronous save path.
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
        }
        Ok(previous)
    }

    /// Saves every currently dirty chunk through crash-resistant Anvil rewrites.
    ///
    /// This method performs blocking filesystem work and must run on a blocking
    /// worker when called from an asynchronous runtime.
    pub fn save_dirty(&self) -> Result<usize, WorldError> {
        let positions = {
            let mut dirty = self
                .dirty_chunks
                .write()
                .map_err(|_| WorldError::LockPoisoned)?;
            std::mem::take(&mut *dirty).into_iter().collect::<Vec<_>>()
        };
        let mut regions = BTreeMap::<(i32, i32), Vec<(ChunkPosition, NamedTag)>>::new();
        {
            let chunks = self.chunks.read().map_err(|_| WorldError::LockPoisoned)?;
            for position in positions.iter().copied() {
                let Some(chunk) = chunks.get(&position) else {
                    drop(chunks);
                    self.dirty_chunks
                        .write()
                        .map_err(|_| WorldError::LockPoisoned)?
                        .extend(positions.iter().copied());
                    return Err(WorldError::MissingDirtyChunk(position));
                };
                regions
                    .entry((position.x.div_euclid(32), position.z.div_euclid(32)))
                    .or_default()
                    .push((position, storage::encode_chunk(chunk)));
            }
        }
        let region_groups = regions.into_values().collect::<Vec<_>>();
        for (index, group) in region_groups.iter().enumerate() {
            let documents = group
                .iter()
                .map(|(position, document)| (region_position(*position), document))
                .collect::<Vec<_>>();
            if let Err(error) = self.regions.write_chunks(&documents) {
                self.dirty_chunks
                    .write()
                    .map_err(|_| WorldError::LockPoisoned)?
                    .extend(
                        region_groups[index..]
                            .iter()
                            .flatten()
                            .map(|(position, _)| *position),
                    );
                return Err(error.into());
            }
        }
        Ok(positions.len())
    }

    /// Returns the number of modified chunks waiting for persistence.
    pub fn dirty_chunk_count(&self) -> Result<usize, WorldError> {
        Ok(self
            .dirty_chunks
            .read()
            .map_err(|_| WorldError::LockPoisoned)?
            .len())
    }

    /// Returns the number of generated chunks currently retained in memory.
    pub fn loaded_chunk_count(&self) -> Result<usize, WorldError> {
        Ok(self
            .chunks
            .read()
            .map_err(|_| WorldError::LockPoisoned)?
            .len())
    }

    /// Returns world metadata, including the authoritative spawn and seed.
    #[must_use]
    pub const fn metadata(&self) -> &LevelMetadata {
        &self.metadata
    }

    /// Returns the active generator's stable identifier.
    #[must_use]
    pub fn generator_identifier(&self) -> &'static str {
        self.generator.identifier()
    }
}

/// World-folder or chunk-service failure.
#[derive(Debug, Error)]
pub enum WorldError {
    /// A directory in the vanilla world layout could not be created.
    #[error("failed to create world directory at {path}: {source}")]
    CreateDirectory {
        /// Attempted directory path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// `level.dat` could not be read.
    #[error("failed to read world metadata at {path}: {source}")]
    Read {
        /// Attempted file path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// Generated world metadata could not be committed.
    #[error("failed to write world metadata at {path}: {source}")]
    Write {
        /// Attempted file path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The compressed NBT document was malformed or exceeded a limit.
    #[error(transparent)]
    Nbt(#[from] NbtError),
    /// Required vanilla metadata was absent.
    #[error("level.dat is missing required field Data.{0}")]
    MissingField(&'static str),
    /// Required metadata used another NBT type.
    #[error("level.dat field Data.{0} has the wrong NBT type")]
    InvalidFieldType(&'static str),
    /// The configured in-memory chunk capacity was zero.
    #[error("world chunk capacity must be greater than zero")]
    InvalidChunkCapacity,
    /// Generation was refused because the bounded cache is full.
    #[error("loaded chunk capacity {limit} reached")]
    ChunkCapacity {
        /// Configured cache limit.
        limit: usize,
    },
    /// A previous panic poisoned the chunk cache lock.
    #[error("world chunk cache lock was poisoned")]
    LockPoisoned,
    /// A block coordinate was outside the supported overworld build bounds.
    #[error("block position {0:?} is outside the supported world bounds")]
    InvalidBlockPosition(BlockPosition),
    /// A dirty marker referred to a chunk absent from the authoritative cache.
    #[error("dirty chunk {0:?} is absent from the loaded chunk cache")]
    MissingDirtyChunk(ChunkPosition),
    /// Anvil region I/O or validation failed.
    #[error(transparent)]
    Region(#[from] RegionError),
    /// Stored chunk NBT could not be represented safely.
    #[error(transparent)]
    ChunkStorage(#[from] ChunkStorageError),
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
        assert!(fs::remove_dir_all(&path).is_ok());
    }

    #[test]
    fn enforces_loaded_chunk_capacity() {
        let path = temporary_world("capacity");
        let world = World::open_or_create(&path, 1, GeneratorKind::Flat, 42)
            .unwrap_or_else(|error| panic!("world should be generated: {error}"));
        assert!(world.chunk(ChunkPosition { x: 0, z: 0 }).is_ok());
        assert!(matches!(
            world.chunk(ChunkPosition { x: 1, z: 0 }),
            Err(WorldError::ChunkCapacity { limit: 1 })
        ));
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
}
