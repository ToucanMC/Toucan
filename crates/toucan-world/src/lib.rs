mod block_update;
mod chunk;
mod fluid;
mod generator;
mod neighbor;
mod storage;
mod tick;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub use chunk::{Chunk, ChunkPosition, ChunkSection, MIN_Y, SECTION_COUNT, WORLD_HEIGHT};
pub use fluid::{
    FluidKind, FluidState, block_after_break, fluid_state, source_fluid_state, with_waterlogged,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockChange {
    pub position: BlockPosition,
    pub state: BlockStateId,
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
    chunks: Mutex<ChunkCache>,
    tick_scheduler: Mutex<tick::TickScheduler>,
    regions: RegionStore,
    max_loaded_chunks: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChunkTicket {
    PlayerView,
    ScheduledTick,
    Dirty,
    Temporary,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ChunkEncodingKey {
    pub position: ChunkPosition,
    pub revision: u64,
    pub protocol_version: i32,
}

#[derive(Default)]
struct ChunkTickets {
    player_views: usize,
    scheduled_tick: bool,
    dirty: bool,
    saving: bool,
    temporary: usize,
}

impl ChunkTickets {
    fn is_evictable(&self) -> bool {
        self.player_views == 0
            && !self.scheduled_tick
            && !self.dirty
            && !self.saving
            && self.temporary == 0
    }

    fn count(&self, ticket: ChunkTicket) -> usize {
        match ticket {
            ChunkTicket::PlayerView => self.player_views,
            ChunkTicket::ScheduledTick => usize::from(self.scheduled_tick),
            ChunkTicket::Dirty => usize::from(self.dirty || self.saving),
            ChunkTicket::Temporary => self.temporary,
        }
    }
}

struct CachedChunk {
    chunk: Arc<Chunk>,
    raw: Option<storage::RawChunkDocument>,
    tickets: ChunkTickets,
    last_access: u64,
}

#[derive(Default)]
struct ChunkCache {
    entries: HashMap<ChunkPosition, CachedChunk>,
    access_clock: u64,
}

impl ChunkCache {
    fn touch(&mut self, position: ChunkPosition) -> Option<Arc<Chunk>> {
        self.access_clock = self.access_clock.wrapping_add(1);
        let entry = self.entries.get_mut(&position)?;
        entry.last_access = self.access_clock;
        Some(Arc::clone(&entry.chunk))
    }

    fn evict_lru(&mut self) -> Option<ChunkPosition> {
        let position = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.tickets.is_evictable())
            .min_by_key(|(position, entry)| (entry.last_access, position.x, position.z))
            .map(|(position, _)| *position)?;
        self.entries.remove(&position);
        Some(position)
    }
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
            chunks: Mutex::new(ChunkCache::default()),
            tick_scheduler: Mutex::new(tick::TickScheduler::default()),
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
            .lock()
            .map_err(|_| WorldError::LockPoisoned)?
            .touch(position)
        {
            return Ok(chunk);
        }

        let stored = self.regions.read_chunk(region_position(position))?;
        let (generated, ticks, raw) = match stored {
            Some(document) => {
                let decoded = storage::decode_chunk(position, &document)?;
                (Arc::new(decoded.chunk), decoded.ticks, Some(decoded.raw))
            }
            None => (
                Arc::new(self.generator.generate(self.metadata.seed, position)),
                Vec::new(),
                None,
            ),
        };
        let mut chunks = self.chunks.lock().map_err(|_| WorldError::LockPoisoned)?;
        if let Some(chunk) = chunks.touch(position) {
            return Ok(chunk);
        }
        if chunks.entries.len() >= self.max_loaded_chunks && chunks.evict_lru().is_none() {
            return Err(WorldError::ChunkCapacity {
                limit: self.max_loaded_chunks,
            });
        }
        chunks.access_clock = chunks.access_clock.wrapping_add(1);
        let last_access = chunks.access_clock;
        chunks.entries.insert(
            position,
            CachedChunk {
                chunk: Arc::clone(&generated),
                raw,
                tickets: ChunkTickets {
                    scheduled_tick: !ticks.is_empty(),
                    ..ChunkTickets::default()
                },
                last_access,
            },
        );
        drop(chunks);
        if !ticks.is_empty() {
            let mut scheduler = self
                .tick_scheduler
                .lock()
                .map_err(|_| WorldError::LockPoisoned)?;
            for tick in ticks {
                scheduler.restore(tick);
            }
        }
        Ok(generated)
    }

    pub fn block(&self, position: BlockPosition) -> Result<BlockStateId, WorldError> {
        let chunk = self.chunk(ChunkPosition::from_block(position.x, position.z))?;
        if let Some(state) = chunk.block(
            position.x.rem_euclid(16) as u8,
            position.y,
            position.z.rem_euclid(16) as u8,
        ) {
            return Ok(state);
        }
        if let Some(section_y) = chunk.opaque_section_y_at(position.y) {
            return Err(WorldError::OpaqueChunkSection {
                chunk: chunk.position(),
                section_y,
            });
        }
        Err(WorldError::InvalidBlockPosition(position))
    }

    pub fn loaded_block_value(&self, position: BlockPosition) -> Result<BlockStateId, WorldError> {
        self.loaded_block(position)?.ok_or_else(|| {
            WorldError::ChunkNotLoaded(ChunkPosition::from_block(position.x, position.z))
        })
    }

    pub fn retain_chunk(
        &self,
        position: ChunkPosition,
        ticket: ChunkTicket,
    ) -> Result<Arc<Chunk>, WorldError> {
        let chunk = self.chunk(position)?;
        let mut cache = self.chunks.lock().map_err(|_| WorldError::LockPoisoned)?;
        let entry = cache
            .entries
            .get_mut(&position)
            .ok_or(WorldError::MissingLoadedChunk(position))?;
        match ticket {
            ChunkTicket::PlayerView => entry.tickets.player_views += 1,
            ChunkTicket::Temporary => entry.tickets.temporary += 1,
            ChunkTicket::ScheduledTick | ChunkTicket::Dirty => {
                return Err(WorldError::ManagedChunkTicket(ticket));
            }
        }
        Ok(chunk)
    }

    pub fn release_chunk(
        &self,
        position: ChunkPosition,
        ticket: ChunkTicket,
    ) -> Result<(), WorldError> {
        let mut cache = self.chunks.lock().map_err(|_| WorldError::LockPoisoned)?;
        let Some(entry) = cache.entries.get_mut(&position) else {
            return Ok(());
        };
        let count = match ticket {
            ChunkTicket::PlayerView => &mut entry.tickets.player_views,
            ChunkTicket::Temporary => &mut entry.tickets.temporary,
            ChunkTicket::ScheduledTick | ChunkTicket::Dirty => {
                return Err(WorldError::ManagedChunkTicket(ticket));
            }
        };
        *count = count.saturating_sub(1);
        Ok(())
    }

    pub fn chunk_ticket_count(
        &self,
        position: ChunkPosition,
        ticket: ChunkTicket,
    ) -> Result<usize, WorldError> {
        Ok(self
            .chunks
            .lock()
            .map_err(|_| WorldError::LockPoisoned)?
            .entries
            .get(&position)
            .map_or(0, |entry| entry.tickets.count(ticket)))
    }

    pub fn chunk_encoding_key(
        &self,
        position: ChunkPosition,
        protocol_version: i32,
    ) -> Result<Option<ChunkEncodingKey>, WorldError> {
        Ok(self
            .chunks
            .lock()
            .map_err(|_| WorldError::LockPoisoned)?
            .entries
            .get(&position)
            .map(|entry| ChunkEncodingKey {
                position,
                revision: entry.chunk.revision(),
                protocol_version,
            }))
    }

    pub fn evict_unused_chunks(&self) -> Result<usize, WorldError> {
        let mut cache = self.chunks.lock().map_err(|_| WorldError::LockPoisoned)?;
        let mut evicted = 0;
        while cache.evict_lru().is_some() {
            evicted += 1;
        }
        Ok(evicted)
    }

    pub(crate) fn loaded_block(
        &self,
        position: BlockPosition,
    ) -> Result<Option<BlockStateId>, WorldError> {
        if !(MIN_Y..MIN_Y + WORLD_HEIGHT).contains(&position.y) {
            return Ok(None);
        }
        let chunk_position = ChunkPosition::from_block(position.x, position.z);
        let chunks = self.chunks.lock().map_err(|_| WorldError::LockPoisoned)?;
        let Some(entry) = chunks.entries.get(&chunk_position) else {
            return Ok(None);
        };
        if let Some(section_y) = entry.chunk.opaque_section_y_at(position.y) {
            return Err(WorldError::OpaqueChunkSection {
                chunk: chunk_position,
                section_y,
            });
        }
        Ok(entry.chunk.block(
            position.x.rem_euclid(16) as u8,
            position.y,
            position.z.rem_euclid(16) as u8,
        ))
    }

    pub fn set_block(
        &self,
        position: BlockPosition,
        state: BlockStateId,
    ) -> Result<BlockStateId, WorldError> {
        let chunk_position = ChunkPosition::from_block(position.x, position.z);
        let _ = self.chunk(chunk_position)?;
        self.set_loaded_block(position, state)
    }

    pub fn set_loaded_block(
        &self,
        position: BlockPosition,
        state: BlockStateId,
    ) -> Result<BlockStateId, WorldError> {
        let chunk_position = ChunkPosition::from_block(position.x, position.z);
        let chunk = self
            .chunks
            .lock()
            .map_err(|_| WorldError::LockPoisoned)?
            .touch(chunk_position)
            .ok_or(WorldError::ChunkNotLoaded(chunk_position))?;
        let x = position.x.rem_euclid(16) as u8;
        let z = position.z.rem_euclid(16) as u8;
        let previous = match chunk.block(x, position.y, z) {
            Some(previous) => previous,
            None => {
                if let Some(section_y) = chunk.opaque_section_y_at(position.y) {
                    return Err(WorldError::OpaqueChunkSection {
                        chunk: chunk_position,
                        section_y,
                    });
                }
                return Err(WorldError::InvalidBlockPosition(position));
            }
        };
        if !chunk.set_block(x, position.y, z, state) {
            return Err(WorldError::InvalidBlockPosition(position));
        }
        if previous != state {
            self.mark_dirty([chunk_position])?;
            let fluid_ticks = fluid::ticks_after_change(self, position)?;
            let block_ticks = block_update::ticks_after_change(self, position)?;
            let mut scheduled_chunks = HashSet::from([chunk_position]);
            let mut scheduler = self
                .tick_scheduler
                .lock()
                .map_err(|_| WorldError::LockPoisoned)?;
            for (position, kind, delay) in fluid_ticks {
                scheduled_chunks.insert(ChunkPosition::from_block(position.x, position.z));
                scheduler.schedule(
                    tick::ScheduledTick {
                        position,
                        target: tick::TickTarget::Fluid(kind),
                    },
                    delay,
                );
            }
            for (position, block, delay) in block_ticks {
                scheduled_chunks.insert(ChunkPosition::from_block(position.x, position.z));
                scheduler.schedule(
                    tick::ScheduledTick {
                        position,
                        target: tick::TickTarget::Block(block),
                    },
                    delay,
                );
            }
            drop(scheduler);
            for scheduled in &scheduled_chunks {
                if !self.is_chunk_loaded(*scheduled)? {
                    return Err(WorldError::ChunkNotLoaded(*scheduled));
                }
            }
            self.mark_dirty(scheduled_chunks.iter().copied())?;
            self.refresh_scheduled_tickets(&scheduled_chunks)?;
        }
        Ok(previous)
    }

    pub fn tick_block_updates(
        &self,
        maximum_updates: usize,
    ) -> Result<Vec<BlockChange>, WorldError> {
        if maximum_updates == 0 {
            return Ok(Vec::new());
        }
        let scheduled = self
            .tick_scheduler
            .lock()
            .map_err(|_| WorldError::LockPoisoned)?
            .advance(maximum_updates);
        let affected_chunks = scheduled
            .iter()
            .map(|tick| ChunkPosition::from_block(tick.position.x, tick.position.z))
            .collect::<HashSet<_>>();
        self.mark_dirty(affected_chunks.iter().copied())?;
        let updates = (|| {
            let mut changes = Vec::new();
            for scheduled in scheduled {
                match scheduled.target {
                    tick::TickTarget::Fluid(expected) => {
                        let Some(state) = self.loaded_block(scheduled.position)? else {
                            continue;
                        };
                        if !fluid_state(state)?.is_some_and(|fluid| fluid.kind() == expected) {
                            continue;
                        }
                        changes.extend(fluid::update_at(self, scheduled.position)?);
                    }
                    tick::TickTarget::Block(expected) => {
                        changes.extend(block_update::update_at(self, scheduled.position, expected)?)
                    }
                }
            }
            Ok::<_, WorldError>(changes)
        })();
        let ticket_result = self.refresh_scheduled_tickets(&affected_chunks);
        match updates {
            Ok(changes) => {
                ticket_result?;
                Ok(changes)
            }
            Err(error) => {
                let _ = ticket_result;
                Err(error)
            }
        }
    }

    pub fn save_dirty(&self) -> Result<usize, WorldError> {
        let snapshots = {
            let mut cache = self.chunks.lock().map_err(|_| WorldError::LockPoisoned)?;
            let mut positions = cache
                .entries
                .iter()
                .filter(|(_, entry)| entry.tickets.dirty)
                .map(|(position, _)| *position)
                .collect::<Vec<_>>();
            positions.sort_unstable();
            positions
                .into_iter()
                .map(|position| {
                    let entry = cache
                        .entries
                        .get_mut(&position)
                        .ok_or(WorldError::MissingDirtyChunk(position))?;
                    entry.tickets.dirty = false;
                    entry.tickets.saving = true;
                    Ok((position, Arc::clone(&entry.chunk), entry.raw.clone()))
                })
                .collect::<Result<Vec<_>, WorldError>>()?
        };
        if snapshots.is_empty() {
            return Ok(0);
        }

        let positions = snapshots
            .iter()
            .map(|(position, _, _)| *position)
            .collect::<Vec<_>>();
        let encoded = (|| {
            let mut regions = BTreeMap::<(i32, i32), Vec<(ChunkPosition, u64, NamedTag)>>::new();
            for (position, chunk, raw) in snapshots {
                let ticks = self.stored_ticks(position)?;
                let mut stable = None;
                for _ in 0..8 {
                    let before = chunk.revision();
                    let document = storage::encode_chunk(&chunk, &ticks, raw.as_ref())?;
                    let after = chunk.revision();
                    if before == after {
                        stable = Some((after, document));
                        break;
                    }
                }
                let (revision, document) =
                    stable.ok_or(WorldError::ChunkChangedDuringSave(position))?;
                regions
                    .entry((position.x.div_euclid(32), position.z.div_euclid(32)))
                    .or_default()
                    .push((position, revision, document));
            }
            Ok::<_, WorldError>(regions)
        })();
        let regions = match encoded {
            Ok(regions) => regions,
            Err(error) => {
                self.finish_save_failure(&positions)?;
                return Err(error);
            }
        };
        let region_groups = regions.into_values().collect::<Vec<_>>();
        for group in &region_groups {
            let documents = group
                .iter()
                .map(|(position, _, document)| (region_position(*position), document))
                .collect::<Vec<_>>();
            if let Err(error) = self.regions.write_chunks(&documents) {
                self.finish_save_failure(&positions)?;
                return Err(error.into());
            }
        }
        let mut cache = self.chunks.lock().map_err(|_| WorldError::LockPoisoned)?;
        for (position, revision, document) in region_groups.into_iter().flatten() {
            let entry = cache
                .entries
                .get_mut(&position)
                .ok_or(WorldError::MissingDirtyChunk(position))?;
            entry.tickets.saving = false;
            entry.raw = Some(storage::RawChunkDocument::new(document));
            if entry.chunk.revision() != revision {
                entry.tickets.dirty = true;
            }
        }
        Ok(positions.len())
    }

    pub fn dirty_chunk_count(&self) -> Result<usize, WorldError> {
        Ok(self
            .chunks
            .lock()
            .map_err(|_| WorldError::LockPoisoned)?
            .entries
            .values()
            .filter(|entry| entry.tickets.dirty)
            .count())
    }

    pub fn loaded_chunk_count(&self) -> Result<usize, WorldError> {
        Ok(self
            .chunks
            .lock()
            .map_err(|_| WorldError::LockPoisoned)?
            .entries
            .len())
    }

    pub fn is_chunk_loaded(&self, position: ChunkPosition) -> Result<bool, WorldError> {
        Ok(self
            .chunks
            .lock()
            .map_err(|_| WorldError::LockPoisoned)?
            .entries
            .contains_key(&position))
    }

    fn mark_dirty(
        &self,
        positions: impl IntoIterator<Item = ChunkPosition>,
    ) -> Result<(), WorldError> {
        let mut cache = self.chunks.lock().map_err(|_| WorldError::LockPoisoned)?;
        for position in positions {
            let entry = cache
                .entries
                .get_mut(&position)
                .ok_or(WorldError::MissingLoadedChunk(position))?;
            entry.tickets.dirty = true;
        }
        Ok(())
    }

    fn refresh_scheduled_tickets(
        &self,
        positions: &HashSet<ChunkPosition>,
    ) -> Result<(), WorldError> {
        if positions.is_empty() {
            return Ok(());
        }
        let scheduler = self
            .tick_scheduler
            .lock()
            .map_err(|_| WorldError::LockPoisoned)?;
        let pending = positions
            .iter()
            .map(|position| (*position, scheduler.has_pending(*position)))
            .collect::<Vec<_>>();
        drop(scheduler);
        let mut cache = self.chunks.lock().map_err(|_| WorldError::LockPoisoned)?;
        for (position, has_pending) in pending {
            if let Some(entry) = cache.entries.get_mut(&position) {
                entry.tickets.scheduled_tick = has_pending;
            }
        }
        Ok(())
    }

    fn finish_save_failure(&self, positions: &[ChunkPosition]) -> Result<(), WorldError> {
        let mut cache = self.chunks.lock().map_err(|_| WorldError::LockPoisoned)?;
        for position in positions {
            if let Some(entry) = cache.entries.get_mut(position) {
                entry.tickets.saving = false;
                entry.tickets.dirty = true;
            }
        }
        Ok(())
    }

    fn stored_ticks(&self, position: ChunkPosition) -> Result<Vec<tick::StoredTick>, WorldError> {
        Ok(self
            .tick_scheduler
            .lock()
            .map_err(|_| WorldError::LockPoisoned)?
            .snapshot(position))
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
    #[error("chunk {0:?} is absent from the loaded chunk cache")]
    MissingLoadedChunk(ChunkPosition),
    #[error("chunk {0:?} is not loaded; use an explicit blocking chunk load first")]
    ChunkNotLoaded(ChunkPosition),
    #[error("chunk {chunk:?} section Y={section_y} uses an unsupported opaque block palette")]
    OpaqueChunkSection { chunk: ChunkPosition, section_y: i8 },
    #[error("chunk {0:?} kept changing while a stable save snapshot was prepared")]
    ChunkChangedDuringSave(ChunkPosition),
    #[error("chunk {0:?} kept changing while a stable protocol payload was prepared")]
    ChunkChangedDuringEncoding(ChunkPosition),
    #[error("chunk ticket {0:?} is managed internally")]
    ManagedChunkTicket(ChunkTicket),
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
    use toucan_registry::vanilla_registries;

    use super::{
        BlockPosition, BlockStateId, Chunk, ChunkPosition, ChunkTicket, GeneratorKind,
        LevelMetadata, World, WorldError, region_position,
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

    #[test]
    fn player_and_temporary_tickets_control_lru_eviction() -> Result<(), WorldError> {
        let path = temporary_world("tickets");
        let world = World::open_or_create(&path, 1, GeneratorKind::Flat, 42)?;
        let first = ChunkPosition { x: 0, z: 0 };
        world.retain_chunk(first, ChunkTicket::PlayerView)?;
        assert_eq!(world.chunk_ticket_count(first, ChunkTicket::PlayerView)?, 1);
        assert!(matches!(
            world.chunk(ChunkPosition { x: 1, z: 0 }),
            Err(WorldError::ChunkCapacity { limit: 1 })
        ));
        assert_eq!(world.evict_unused_chunks()?, 0);
        world.release_chunk(first, ChunkTicket::PlayerView)?;
        world.retain_chunk(first, ChunkTicket::Temporary)?;
        assert_eq!(world.evict_unused_chunks()?, 0);
        world.release_chunk(first, ChunkTicket::Temporary)?;
        assert_eq!(world.evict_unused_chunks()?, 1);
        let _ = fs::remove_dir_all(path);
        Ok(())
    }

    #[test]
    fn eviction_uses_deterministic_least_recently_used_order() -> Result<(), WorldError> {
        let path = temporary_world("lru");
        let world = World::open_or_create(&path, 2, GeneratorKind::Flat, 42)?;
        let oldest = ChunkPosition { x: 0, z: 0 };
        let newest = ChunkPosition { x: 1, z: 0 };
        world.chunk(oldest)?;
        world.chunk(newest)?;
        world.chunk(oldest)?;
        world.chunk(ChunkPosition { x: 2, z: 0 })?;
        assert!(world.is_chunk_loaded(oldest)?);
        assert!(!world.is_chunk_loaded(newest)?);
        let _ = fs::remove_dir_all(path);
        Ok(())
    }

    #[test]
    fn scheduled_tick_ticket_survives_save_and_executes_before_eviction()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = temporary_world("scheduled-ticket");
        let world = World::open_or_create(&path, 1, GeneratorKind::Flat, 42)?;
        let position = BlockPosition { x: 8, y: 66, z: 8 };
        let chunk = ChunkPosition::from_block(position.x, position.z);
        let sand = vanilla_registries()?
            .block_by_name("minecraft:sand")?
            .default_state();
        world.set_block(position, sand)?;
        assert_eq!(world.save_dirty()?, 1);
        assert_eq!(world.chunk_ticket_count(chunk, ChunkTicket::Dirty)?, 0);
        assert_eq!(
            world.chunk_ticket_count(chunk, ChunkTicket::ScheduledTick)?,
            1
        );
        assert_eq!(world.evict_unused_chunks()?, 0);
        assert!(world.tick_block_updates(64)?.is_empty());
        assert!(!world.tick_block_updates(64)?.is_empty());
        assert_eq!(world.save_dirty()?, 1);
        assert!(world.is_chunk_loaded(chunk)?);
        let _ = fs::remove_dir_all(path);
        Ok(())
    }

    #[test]
    fn block_mutation_preserves_unowned_nested_vanilla_nbt()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = temporary_world("raw-preservation");
        let position = ChunkPosition { x: 0, z: 0 };
        let world = World::open_or_create(&path, 4, GeneratorKind::Flat, 42)?;
        let chunk = Chunk::empty(position);
        let mut document = super::storage::encode_chunk(&chunk, &[], None)?;
        let root = compound_mut(&mut document.value);

        let mut item = BTreeMap::new();
        item.insert("id".into(), Tag::String("minecraft:diamond".into()));
        let mut block_entity = BTreeMap::new();
        block_entity.insert("id".into(), Tag::String("minecraft:chest".into()));
        block_entity.insert(
            "Items".into(),
            Tag::List {
                element_type: 10,
                values: vec![Tag::Compound(item)],
            },
        );
        let block_entities = Tag::List {
            element_type: 10,
            values: vec![Tag::Compound(block_entity)],
        };
        root.insert("block_entities".into(), block_entities.clone());

        let mut starts = BTreeMap::new();
        starts.insert(
            "minecraft:village".into(),
            Tag::Compound(BTreeMap::from([
                ("id".into(), Tag::String("minecraft:village".into())),
                ("references".into(), Tag::LongArray(vec![1, 2, 3])),
            ])),
        );
        let structures = Tag::Compound(BTreeMap::from([("starts".into(), Tag::Compound(starts))]));
        root.insert("structures".into(), structures.clone());

        let unknown = Tag::Compound(BTreeMap::from([
            ("future_flag".into(), Tag::Byte(1)),
            (
                "nested".into(),
                Tag::List {
                    element_type: 8,
                    values: vec![Tag::String("alpha".into()), Tag::String("beta".into())],
                },
            ),
        ]));
        root.insert("ToucanUnknownFixture".into(), unknown.clone());

        let future_heightmap = Tag::LongArray(vec![11, 12, 13]);
        match root.get_mut("Heightmaps") {
            Some(Tag::Compound(heightmaps)) => {
                heightmaps.insert("FUTURE_HEIGHTMAP".into(), future_heightmap.clone());
            }
            _ => return Err("missing heightmaps fixture".into()),
        }

        let sections = match root.get_mut("sections") {
            Some(Tag::List { values, .. }) => values,
            _ => return Err("missing sections fixture".into()),
        };
        let first_section = compound_mut(&mut sections[0]);
        let future_block_state_data = Tag::IntArray(vec![3, 1, 4]);
        match first_section.get_mut("block_states") {
            Some(Tag::Compound(block_states)) => {
                block_states.insert(
                    "future_block_state_data".into(),
                    future_block_state_data.clone(),
                );
                let palette = match block_states.get_mut("palette") {
                    Some(Tag::List { values, .. }) => values,
                    _ => return Err("missing palette fixture".into()),
                };
                compound_mut(&mut palette[0])
                    .insert("Name".into(), Tag::String("example:future_block".into()));
            }
            _ => return Err("missing block states fixture".into()),
        }
        let opaque_block_states = first_section
            .get("block_states")
            .cloned()
            .ok_or("missing opaque block states fixture")?;
        let biomes = Tag::Compound(BTreeMap::from([
            (
                "palette".into(),
                Tag::List {
                    element_type: 8,
                    values: vec![Tag::String("minecraft:desert".into())],
                },
            ),
            ("future_biome_data".into(), Tag::LongArray(vec![7, 8, 9])),
        ]));
        first_section.insert("biomes".into(), biomes.clone());

        world
            .regions
            .write_chunks(&[(region_position(position), &document)])?;
        world.chunk(position)?;
        assert!(matches!(
            world.block(BlockPosition {
                x: 0,
                y: crate::MIN_Y,
                z: 0
            }),
            Err(WorldError::OpaqueChunkSection {
                chunk: ChunkPosition { x: 0, z: 0 },
                section_y: -4
            })
        ));
        assert!(matches!(
            world.set_loaded_block(
                BlockPosition {
                    x: 0,
                    y: crate::MIN_Y,
                    z: 0
                },
                BlockStateId::STONE
            ),
            Err(WorldError::OpaqueChunkSection { .. })
        ));
        world.set_loaded_block(BlockPosition { x: 1, y: 64, z: 1 }, BlockStateId::STONE)?;
        world.save_dirty()?;
        let saved = world
            .regions
            .read_chunk(region_position(position))?
            .ok_or("saved chunk missing")?;
        let saved_root = match &saved.value {
            Tag::Compound(root) => root,
            _ => return Err("saved root not compound".into()),
        };
        assert_eq!(saved_root.get("block_entities"), Some(&block_entities));
        assert_eq!(saved_root.get("structures"), Some(&structures));
        assert_eq!(saved_root.get("ToucanUnknownFixture"), Some(&unknown));
        assert_eq!(
            saved_root
                .get("Heightmaps")
                .and_then(|heightmaps| heightmaps.get("FUTURE_HEIGHTMAP")),
            Some(&future_heightmap)
        );
        let saved_sections = match saved_root.get("sections") {
            Some(Tag::List { values, .. }) => values,
            _ => return Err("saved sections missing".into()),
        };
        let saved_first_section = compound_ref(&saved_sections[0]);
        assert_eq!(saved_first_section.get("biomes"), Some(&biomes));
        assert_eq!(
            saved_first_section.get("block_states"),
            Some(&opaque_block_states)
        );
        assert_eq!(
            saved_first_section
                .get("block_states")
                .and_then(|states| states.get("future_block_state_data")),
            Some(&future_block_state_data)
        );
        let _ = fs::remove_dir_all(path);
        Ok(())
    }

    fn compound_mut(tag: &mut Tag) -> &mut BTreeMap<String, Tag> {
        match tag {
            Tag::Compound(value) => value,
            _ => panic!("expected compound fixture"),
        }
    }

    fn compound_ref(tag: &Tag) -> &BTreeMap<String, Tag> {
        match tag {
            Tag::Compound(value) => value,
            _ => panic!("expected compound fixture"),
        }
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
            changes.extend(
                reopened
                    .tick_block_updates(64)
                    .expect("run restored fluid ticks"),
            );
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
