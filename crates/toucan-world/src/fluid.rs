use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use toucan_registry::{BlockStateId, RegistryError, vanilla_registries};

use crate::{BlockPosition, ChunkPosition, MIN_Y, WORLD_HEIGHT, World, WorldError};

const HORIZONTAL_DIRECTIONS: [(i32, i32); 4] = [(0, -1), (0, 1), (-1, 0), (1, 0)];
const NEIGHBOR_DIRECTIONS: [(i32, i32, i32); 7] = [
    (0, 0, 0),
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
    (-1, 0, 0),
    (1, 0, 0),
];
static FLUID_REGISTRY: OnceLock<Result<FluidRegistry, String>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FluidKind {
    Water,
    Lava,
}

impl FluidKind {
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::Water => "minecraft:water",
            Self::Lava => "minecraft:lava",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FluidState {
    kind: FluidKind,
    level: u8,
    falling: bool,
    contained: bool,
}

impl FluidState {
    #[must_use]
    pub const fn kind(self) -> FluidKind {
        self.kind
    }

    #[must_use]
    pub const fn level(self) -> u8 {
        self.level
    }

    #[must_use]
    pub const fn is_source(self) -> bool {
        self.level == 0 && !self.falling
    }

    #[must_use]
    pub const fn is_falling(self) -> bool {
        self.falling
    }

    #[must_use]
    pub const fn is_contained(self) -> bool {
        self.contained
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FluidChange {
    pub position: BlockPosition,
    pub state: BlockStateId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StoredFluidTick {
    pub position: BlockPosition,
    pub kind: FluidKind,
    pub delay: u32,
}

#[derive(Clone, Copy)]
enum FluidDescriptor {
    None,
    Standalone {
        kind: FluidKind,
        raw_level: u8,
    },
    Waterlogged {
        wet: bool,
        counterpart: BlockStateId,
    },
}

struct FluidRegistry {
    descriptors: Box<[FluidDescriptor]>,
    water: [BlockStateId; 16],
    lava: [BlockStateId; 16],
}

impl FluidRegistry {
    fn state(&self, kind: FluidKind, raw_level: u8) -> BlockStateId {
        match kind {
            FluidKind::Water => self.water[usize::from(raw_level)],
            FluidKind::Lava => self.lava[usize::from(raw_level)],
        }
    }

    fn descriptor(&self, state: BlockStateId) -> Result<FluidDescriptor, RegistryError> {
        self.descriptors
            .get(usize::from(state.raw()))
            .copied()
            .ok_or(RegistryError::UnknownBlockStateId(state.raw()))
    }
}

pub fn fluid_state(state: BlockStateId) -> Result<Option<FluidState>, RegistryError> {
    let registry = fluid_registry()?;
    Ok(match registry.descriptor(state)? {
        FluidDescriptor::None | FluidDescriptor::Waterlogged { wet: false, .. } => None,
        FluidDescriptor::Standalone { kind, raw_level } => Some(FluidState {
            kind,
            level: if raw_level >= 8 {
                raw_level - 8
            } else {
                raw_level
            },
            falling: raw_level >= 8,
            contained: false,
        }),
        FluidDescriptor::Waterlogged { wet: true, .. } => Some(FluidState {
            kind: FluidKind::Water,
            level: 0,
            falling: false,
            contained: true,
        }),
    })
}

pub fn with_waterlogged(
    state: BlockStateId,
    waterlogged: bool,
) -> Result<Option<BlockStateId>, RegistryError> {
    Ok(match fluid_registry()?.descriptor(state)? {
        FluidDescriptor::Waterlogged { wet, counterpart } if wet != waterlogged => {
            Some(counterpart)
        }
        FluidDescriptor::Waterlogged { .. } => Some(state),
        FluidDescriptor::None | FluidDescriptor::Standalone { .. } => None,
    })
}

pub fn source_fluid_state(kind: FluidKind) -> Result<BlockStateId, RegistryError> {
    Ok(fluid_registry()?.state(kind, 0))
}

pub fn block_after_break(state: BlockStateId) -> Result<BlockStateId, RegistryError> {
    let registry = fluid_registry()?;
    Ok(match registry.descriptor(state)? {
        FluidDescriptor::Waterlogged { wet: true, .. } => registry.state(FluidKind::Water, 0),
        _ => BlockStateId::AIR,
    })
}

fn fluid_registry() -> Result<&'static FluidRegistry, RegistryError> {
    FLUID_REGISTRY
        .get_or_init(build_fluid_registry)
        .as_ref()
        .map_err(|error| RegistryError::InvalidGeneratedData(error.clone()))
}

fn build_fluid_registry() -> Result<FluidRegistry, String> {
    let registries = vanilla_registries().map_err(|error| error.to_string())?;
    let levels = |name: &str| -> Result<[BlockStateId; 16], String> {
        let mut states = [BlockStateId::AIR; 16];
        for (level, state) in states.iter_mut().enumerate() {
            let level = level.to_string();
            *state = registries
                .resolve_state(name, [("level", level.as_str())])
                .map_err(|error| error.to_string())?;
        }
        Ok(states)
    };
    let water = levels("minecraft:water")?;
    let lava = levels("minecraft:lava")?;
    let mut descriptors = vec![FluidDescriptor::None; registries.counts().block_states];

    for state in registries.block_states() {
        let block = registries
            .block(state.block())
            .map_err(|error| error.to_string())?;
        let properties = registries
            .state_properties(state.id())
            .map_err(|error| error.to_string())?;
        let descriptor = match block.name().as_str() {
            "minecraft:water" | "minecraft:lava" => {
                let raw_level = properties
                    .iter()
                    .find_map(|(name, value)| (name == &"level").then_some(value))
                    .ok_or_else(|| format!("fluid state {} has no level", state.id().raw()))?
                    .parse::<u8>()
                    .map_err(|error| format!("invalid fluid level: {error}"))?;
                FluidDescriptor::Standalone {
                    kind: if block.name().as_str() == "minecraft:water" {
                        FluidKind::Water
                    } else {
                        FluidKind::Lava
                    },
                    raw_level,
                }
            }
            _ => {
                if let Some(value) = properties
                    .iter()
                    .find_map(|(name, value)| (name == &"waterlogged").then_some(value))
                {
                    let wet = value == "true";
                    let counterpart = registries
                        .with_property(
                            state.id(),
                            "waterlogged",
                            if wet { "false" } else { "true" },
                        )
                        .map_err(|error| error.to_string())?;
                    FluidDescriptor::Waterlogged { wet, counterpart }
                } else {
                    FluidDescriptor::None
                }
            }
        };
        descriptors[usize::from(state.id().raw())] = descriptor;
    }

    Ok(FluidRegistry {
        descriptors: descriptors.into_boxed_slice(),
        water,
        lava,
    })
}

#[derive(Default)]
pub(crate) struct FluidScheduler {
    current_tick: u64,
    due: BTreeMap<u64, Vec<BlockPosition>>,
    scheduled: HashMap<BlockPosition, u64>,
}

impl FluidScheduler {
    pub fn schedule_around(&mut self, position: BlockPosition, delay: u64) {
        for (dx, dy, dz) in NEIGHBOR_DIRECTIONS {
            self.schedule(offset(position, dx, dy, dz), delay);
        }
    }

    pub fn schedule(&mut self, position: BlockPosition, delay: u64) {
        if !(MIN_Y..MIN_Y + WORLD_HEIGHT).contains(&position.y) {
            return;
        }
        let due = self.current_tick.saturating_add(delay.max(1));
        if self
            .scheduled
            .get(&position)
            .is_some_and(|existing| *existing <= due)
        {
            return;
        }
        self.scheduled.insert(position, due);
        self.due.entry(due).or_default().push(position);
    }

    pub fn restore(&mut self, tick: StoredFluidTick) {
        self.schedule(tick.position, u64::from(tick.delay));
    }

    pub fn advance(&mut self, maximum: usize) -> Vec<BlockPosition> {
        self.current_tick = self.current_tick.wrapping_add(1);
        let mut ready = Vec::with_capacity(maximum);
        while ready.len() < maximum {
            let Some((&due, _)) = self.due.first_key_value() else {
                break;
            };
            if due > self.current_tick {
                break;
            }
            let mut positions = self.due.remove(&due).unwrap_or_default();
            while let Some(position) = positions.pop() {
                if self.scheduled.get(&position) != Some(&due) {
                    continue;
                }
                if ready.len() == maximum {
                    self.due
                        .entry(self.current_tick + 1)
                        .or_default()
                        .push(position);
                    self.scheduled.insert(position, self.current_tick + 1);
                    continue;
                }
                self.scheduled.remove(&position);
                ready.push(position);
            }
        }
        ready
    }

    pub fn snapshot(&self, chunk: ChunkPosition) -> Vec<(BlockPosition, u32)> {
        let mut pending = self
            .scheduled
            .iter()
            .filter(|(position, _)| ChunkPosition::from_block(position.x, position.z) == chunk)
            .map(|(position, due)| {
                let delay = due
                    .saturating_sub(self.current_tick)
                    .min(u64::from(u32::MAX)) as u32;
                (*position, delay)
            })
            .collect::<Vec<_>>();
        pending.sort_unstable_by_key(|(position, _)| (position.y, position.z, position.x));
        pending
    }
}

pub(crate) fn update_at(
    world: &World,
    position: BlockPosition,
) -> Result<Option<FluidChange>, WorldError> {
    let Some(current) = world.loaded_block(position)? else {
        return Ok(None);
    };
    let registry = fluid_registry()?;
    let descriptor = registry.descriptor(current)?;
    if matches!(descriptor, FluidDescriptor::Waterlogged { wet: true, .. }) {
        return Ok(None);
    }

    let current_fluid = fluid_state(current)?;
    if current_fluid.is_some_and(FluidState::is_source) {
        return Ok(None);
    }
    let desired = desired_fluid(world, position, current_fluid.map(FluidState::kind))?;
    let next = match (descriptor, desired) {
        (
            FluidDescriptor::Waterlogged {
                wet: false,
                counterpart,
            },
            Some((FluidKind::Water, _)),
        ) => counterpart,
        (FluidDescriptor::Waterlogged { .. }, _) => return Ok(None),
        (FluidDescriptor::Standalone { .. }, None) => BlockStateId::AIR,
        (FluidDescriptor::Standalone { .. }, Some((kind, raw_level)))
        | (FluidDescriptor::None, Some((kind, raw_level))) => {
            let block =
                vanilla_registries()?.block(vanilla_registries()?.state(current)?.block())?;
            if !matches!(descriptor, FluidDescriptor::Standalone { .. }) && !block.replaceable() {
                return Ok(None);
            }
            registry.state(kind, raw_level)
        }
        (FluidDescriptor::None, None) => return Ok(None),
    };
    if next == current {
        return Ok(None);
    }
    world.set_block(position, next)?;
    Ok(Some(FluidChange {
        position,
        state: next,
    }))
}

fn desired_fluid(
    world: &World,
    position: BlockPosition,
    preferred: Option<FluidKind>,
) -> Result<Option<(FluidKind, u8)>, WorldError> {
    let above = offset(position, 0, 1, 0);
    if let Some(fluid) = fluid_at(world, above)?
        && preferred.is_none_or(|kind| kind == fluid.kind())
    {
        return Ok(Some((fluid.kind(), 8)));
    }

    let kinds = [preferred.unwrap_or(FluidKind::Water), FluidKind::Lava];
    let kind_count = if preferred.is_some() { 1 } else { 2 };
    for kind in kinds.into_iter().take(kind_count) {
        let mut source_count = 0_u8;
        let mut nearest = None::<u8>;
        for (dx, dz) in HORIZONTAL_DIRECTIONS {
            let Some(fluid) = fluid_at(world, offset(position, dx, 0, dz))? else {
                continue;
            };
            if fluid.kind() != kind {
                continue;
            }
            if fluid.is_source() {
                source_count += 1;
            }
            nearest = Some(nearest.map_or(fluid.level(), |level| level.min(fluid.level())));
        }
        if kind == FluidKind::Water && source_count >= 2 && has_solid_support(world, position)? {
            return Ok(Some((kind, 0)));
        }
        if let Some(level) = nearest
            && level < 7
        {
            return Ok(Some((kind, level + 1)));
        }
    }
    Ok(None)
}

fn has_solid_support(world: &World, position: BlockPosition) -> Result<bool, WorldError> {
    let below_position = offset(position, 0, -1, 0);
    if !(MIN_Y..MIN_Y + WORLD_HEIGHT).contains(&below_position.y) {
        return Ok(false);
    }
    let Some(below) = world.loaded_block(below_position)? else {
        return Ok(false);
    };
    let registries = vanilla_registries()?;
    let block = registries.block(registries.state(below)?.block())?;
    Ok(!block.replaceable() || fluid_state(below)?.is_some_and(FluidState::is_source))
}

fn fluid_at(world: &World, position: BlockPosition) -> Result<Option<FluidState>, WorldError> {
    if !(MIN_Y..MIN_Y + WORLD_HEIGHT).contains(&position.y) {
        return Ok(None);
    }
    let Some(state) = world.loaded_block(position)? else {
        return Ok(None);
    };
    Ok(fluid_state(state)?)
}

fn offset(position: BlockPosition, dx: i32, dy: i32, dz: i32) -> BlockPosition {
    BlockPosition {
        x: position.x.saturating_add(dx),
        y: position.y.saturating_add(dy),
        z: position.z.saturating_add(dz),
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::{FluidKind, block_after_break, fluid_state, source_fluid_state, with_waterlogged};
    use crate::{BlockPosition, GeneratorKind, World};
    use toucan_registry::vanilla_registries;

    #[test]
    fn registry_states_describe_sources_falling_fluid_and_waterlogging()
    -> Result<(), Box<dyn Error>> {
        let registries = vanilla_registries()?;
        let source = source_fluid_state(FluidKind::Water)?;
        let source_fluid = fluid_state(source)?.ok_or("source water fluid")?;
        assert!(source_fluid.is_source());
        assert!(!source_fluid.is_falling());

        let falling = registries.resolve_state("minecraft:water", [("level", "15")])?;
        let falling_fluid = fluid_state(falling)?.ok_or("falling water fluid")?;
        assert_eq!(falling_fluid.level(), 7);
        assert!(falling_fluid.is_falling());

        let stairs = registries
            .block_by_name("minecraft:oak_stairs")?
            .default_state();
        let wet = with_waterlogged(stairs, true)?.ok_or("waterlogged stair state")?;
        assert!(fluid_state(wet)?.is_some_and(|fluid| fluid.is_contained()));
        assert_eq!(block_after_break(wet)?, source);
        assert_eq!(with_waterlogged(wet, false)?, Some(stairs));
        Ok(())
    }

    #[test]
    fn scheduled_water_flows_and_waterlogs_neighboring_blocks() -> Result<(), Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "toucan-world-fluid-flow-test-{}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let source_position = BlockPosition { x: 8, y: 64, z: 8 };
        world.set_block(source_position, source_fluid_state(FluidKind::Water)?)?;
        let changes = world.tick_fluids(64)?;
        assert!(!changes.is_empty());

        for position in [
            BlockPosition { x: 8, y: 64, z: 7 },
            BlockPosition { x: 8, y: 64, z: 9 },
            BlockPosition { x: 7, y: 64, z: 8 },
            BlockPosition { x: 9, y: 64, z: 8 },
        ] {
            let fluid = fluid_state(world.block(position)?)?.ok_or("flowing water")?;
            assert_eq!(fluid.kind(), FluidKind::Water);
            assert_eq!(fluid.level(), 1);
            assert!(!fluid.is_source());
        }

        let fence_position = BlockPosition { x: 10, y: 64, z: 8 };
        let fence = vanilla_registries()?
            .block_by_name("minecraft:oak_fence")?
            .default_state();
        world.set_block(fence_position, fence)?;
        world.tick_fluids(64)?;
        assert!(
            fluid_state(world.block(fence_position)?)?.is_some_and(|fluid| fluid.is_contained())
        );
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn fluid_updates_do_not_generate_adjacent_chunks() -> Result<(), Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "toucan-world-fluid-chunk-edge-test-{}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        world.set_block(
            BlockPosition { x: 15, y: 64, z: 0 },
            source_fluid_state(FluidKind::Water)?,
        )?;
        assert_eq!(world.loaded_chunk_count()?, 1);

        world.tick_fluids(64)?;

        assert_eq!(world.loaded_chunk_count()?, 1);
        std::fs::remove_dir_all(path)?;
        Ok(())
    }
}
