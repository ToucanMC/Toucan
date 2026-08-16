use std::collections::{HashSet, VecDeque};
use std::sync::OnceLock;

use toucan_registry::{BlockStateId, RegistryError, vanilla_registries};

use crate::{BlockChange, BlockPosition, MIN_Y, WORLD_HEIGHT, World, WorldError};

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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
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

    pub(crate) const fn tick_delay(self) -> u64 {
        match self {
            Self::Water => 5,
            Self::Lava => 30,
        }
    }

    const fn drop_off(self) -> u8 {
        match self {
            Self::Water => 1,
            Self::Lava => 2,
        }
    }

    const fn slope_find_distance(self) -> usize {
        match self {
            Self::Water => 4,
            Self::Lava => 2,
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

pub(crate) fn ticks_after_change(
    world: &World,
    position: BlockPosition,
) -> Result<Vec<(BlockPosition, FluidKind, u64)>, WorldError> {
    let mut ticks = Vec::new();
    for (dx, dy, dz) in NEIGHBOR_DIRECTIONS {
        let candidate = offset(position, dx, dy, dz);
        let Some(state) = world.loaded_block(candidate)? else {
            continue;
        };
        if let Some(fluid) = fluid_state(state)? {
            ticks.push((candidate, fluid.kind(), fluid.kind().tick_delay()));
        }
    }
    Ok(ticks)
}

pub(crate) fn update_at(
    world: &World,
    position: BlockPosition,
) -> Result<Vec<BlockChange>, WorldError> {
    let Some(current) = world.loaded_block(position)? else {
        return Ok(Vec::new());
    };
    let registry = fluid_registry()?;
    let descriptor = registry.descriptor(current)?;
    let Some(mut fluid) = fluid_state(current)? else {
        return Ok(Vec::new());
    };
    let mut changes = Vec::new();

    if fluid.kind() == FluidKind::Lava && touches_water_for_mixing(world, position)? {
        let mixed = mixing_block(fluid.is_source())?;
        world.set_block(position, mixed)?;
        return Ok(vec![BlockChange {
            position,
            state: mixed,
        }]);
    }

    if matches!(descriptor, FluidDescriptor::Standalone { .. }) && !fluid.is_source() {
        let desired = recompute_flow(world, position, fluid.kind())?;
        let next = desired
            .map(|raw_level| registry.state(fluid.kind(), raw_level))
            .unwrap_or(BlockStateId::AIR);
        if next != current {
            world.set_block(position, next)?;
            changes.push(BlockChange {
                position,
                state: next,
            });
        }
        let Some(next_fluid) = fluid_state(next)? else {
            return Ok(changes);
        };
        fluid = next_fluid;
    }

    spread_from(world, position, fluid, &mut changes)?;
    Ok(changes)
}

fn recompute_flow(
    world: &World,
    position: BlockPosition,
    kind: FluidKind,
) -> Result<Option<u8>, WorldError> {
    if let Some(above) = fluid_at(world, offset(position, 0, 1, 0))?
        && above.kind() == kind
    {
        return Ok(Some(8 + above.level().min(7)));
    }

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
        return Ok(Some(0));
    }
    Ok(nearest.and_then(|level| {
        let next = level.saturating_add(kind.drop_off());
        (next <= 7).then_some(next)
    }))
}

fn spread_from(
    world: &World,
    position: BlockPosition,
    fluid: FluidState,
    changes: &mut Vec<BlockChange>,
) -> Result<(), WorldError> {
    let below = offset(position, 0, -1, 0);
    let falling_level = 8 + fluid.level().min(7);
    if place_fluid(world, below, fluid.kind(), falling_level, changes)? {
        if horizontal_source_count(world, position, fluid.kind())? >= 3 {
            spread_to_sides(world, position, fluid, changes)?;
        }
        return Ok(());
    }
    spread_to_sides(world, position, fluid, changes)
}

fn spread_to_sides(
    world: &World,
    position: BlockPosition,
    fluid: FluidState,
    changes: &mut Vec<BlockChange>,
) -> Result<(), WorldError> {
    let level = fluid.level().saturating_add(fluid.kind().drop_off());
    if level > 7 {
        return Ok(());
    }

    let mut candidates = Vec::new();
    let mut best_distance = usize::MAX;
    for (dx, dz) in HORIZONTAL_DIRECTIONS {
        let target = offset(position, dx, 0, dz);
        if replacement_for_fluid(world, target, fluid.kind(), level)?.is_none() {
            continue;
        }
        let distance = slope_distance(world, target, position, fluid.kind(), level)?;
        best_distance = best_distance.min(distance);
        candidates.push((target, distance));
    }
    for (target, distance) in candidates {
        if distance == best_distance {
            place_fluid(world, target, fluid.kind(), level, changes)?;
        }
    }
    Ok(())
}

fn slope_distance(
    world: &World,
    start: BlockPosition,
    source: BlockPosition,
    kind: FluidKind,
    level: u8,
) -> Result<usize, WorldError> {
    let maximum = kind.slope_find_distance();
    let mut visited = HashSet::from([source, start]);
    let mut queue = VecDeque::from([(start, 0_usize)]);
    while let Some((position, distance)) = queue.pop_front() {
        if replacement_for_fluid(world, offset(position, 0, -1, 0), kind, 8 + level)?.is_some() {
            return Ok(distance);
        }
        if distance >= maximum {
            continue;
        }
        for (dx, dz) in HORIZONTAL_DIRECTIONS {
            let next = offset(position, dx, 0, dz);
            if visited.insert(next) && flow_passable(world, next, kind)? {
                queue.push_back((next, distance + 1));
            }
        }
    }
    Ok(usize::MAX)
}

fn place_fluid(
    world: &World,
    position: BlockPosition,
    kind: FluidKind,
    raw_level: u8,
    changes: &mut Vec<BlockChange>,
) -> Result<bool, WorldError> {
    let Some(next) = replacement_for_fluid(world, position, kind, raw_level)? else {
        return Ok(false);
    };
    world.set_block(position, next)?;
    changes.push(BlockChange {
        position,
        state: next,
    });
    Ok(true)
}

fn replacement_for_fluid(
    world: &World,
    position: BlockPosition,
    kind: FluidKind,
    raw_level: u8,
) -> Result<Option<BlockStateId>, WorldError> {
    let Some(current) = world.loaded_block(position)? else {
        return Ok(None);
    };
    let registry = fluid_registry()?;
    let descriptor = registry.descriptor(current)?;
    if let Some(existing) = fluid_state(current)? {
        if existing.kind() != kind {
            let mixed = match (kind, existing.kind()) {
                (FluidKind::Water, FluidKind::Lava) => mixing_block(existing.is_source())?,
                (FluidKind::Lava, FluidKind::Water) if raw_level >= 8 => {
                    default_state("minecraft:stone")?
                }
                (FluidKind::Lava, FluidKind::Water) => default_state("minecraft:cobblestone")?,
                _ => return Ok(None),
            };
            return Ok(Some(mixed));
        }
        if existing.is_contained() || existing.is_source() {
            return Ok(None);
        }
        let proposed = fluid_state(registry.state(kind, raw_level))?.expect("known fluid state");
        let stronger = proposed.level() < existing.level()
            || (proposed.is_falling() && !existing.is_falling());
        return Ok(stronger.then_some(registry.state(kind, raw_level)));
    }
    match descriptor {
        FluidDescriptor::Waterlogged {
            wet: false,
            counterpart,
        } if kind == FluidKind::Water => Ok(Some(counterpart)),
        FluidDescriptor::Waterlogged { .. } | FluidDescriptor::Standalone { .. } => Ok(None),
        FluidDescriptor::None => {
            let registries = vanilla_registries()?;
            let block = registries.block(registries.state(current)?.block())?;
            Ok(block
                .replaceable()
                .then_some(registry.state(kind, raw_level)))
        }
    }
}

fn touches_water_for_mixing(world: &World, position: BlockPosition) -> Result<bool, WorldError> {
    for (dx, dy, dz) in [(0, 1, 0), (0, 0, -1), (0, 0, 1), (-1, 0, 0), (1, 0, 0)] {
        if fluid_at(world, offset(position, dx, dy, dz))?
            .is_some_and(|fluid| fluid.kind() == FluidKind::Water)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn mixing_block(lava_source: bool) -> Result<BlockStateId, RegistryError> {
    default_state(if lava_source {
        "minecraft:obsidian"
    } else {
        "minecraft:cobblestone"
    })
}

fn default_state(name: &str) -> Result<BlockStateId, RegistryError> {
    Ok(vanilla_registries()?.block_by_name(name)?.default_state())
}

fn flow_passable(
    world: &World,
    position: BlockPosition,
    kind: FluidKind,
) -> Result<bool, WorldError> {
    let Some(current) = world.loaded_block(position)? else {
        return Ok(false);
    };
    let descriptor = fluid_registry()?.descriptor(current)?;
    if let Some(fluid) = fluid_state(current)? {
        return Ok(fluid.kind() == kind);
    }
    Ok(match descriptor {
        FluidDescriptor::Waterlogged { wet: false, .. } => kind == FluidKind::Water,
        FluidDescriptor::Waterlogged { wet: true, .. } => kind == FluidKind::Water,
        FluidDescriptor::Standalone { .. } => false,
        FluidDescriptor::None => {
            let registries = vanilla_registries()?;
            registries
                .block(registries.state(current)?.block())?
                .replaceable()
        }
    })
}

fn horizontal_source_count(
    world: &World,
    position: BlockPosition,
    kind: FluidKind,
) -> Result<u8, WorldError> {
    let mut count = 0;
    for (dx, dz) in HORIZONTAL_DIRECTIONS {
        if fluid_at(world, offset(position, dx, 0, dz))?
            .is_some_and(|fluid| fluid.kind() == kind && fluid.is_source())
        {
            count += 1;
        }
    }
    Ok(count)
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
    use crate::{BlockPosition, BlockStateId, GeneratorKind, World};
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
    fn water_uses_vanilla_tick_delay_and_waterlogs_blocks() -> Result<(), Box<dyn Error>> {
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
        for _ in 0..4 {
            assert!(world.tick_block_updates(64)?.is_empty());
        }
        let changes = world.tick_block_updates(64)?;
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
        for _ in 0..5 {
            world.tick_block_updates(64)?;
        }
        assert!(
            fluid_state(world.block(fence_position)?)?.is_some_and(|fluid| fluid.is_contained())
        );
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn water_falls_before_spreading_sideways() -> Result<(), Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "toucan-world-fluid-fall-test-{}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let source = BlockPosition { x: 8, y: 66, z: 8 };
        world.set_block(source, source_fluid_state(FluidKind::Water)?)?;
        for _ in 0..5 {
            world.tick_block_updates(64)?;
        }

        let below = fluid_state(world.block(BlockPosition { x: 8, y: 65, z: 8 })?)?
            .ok_or("falling water")?;
        assert!(below.is_falling());
        for position in [
            BlockPosition { x: 8, y: 66, z: 7 },
            BlockPosition { x: 8, y: 66, z: 9 },
            BlockPosition { x: 7, y: 66, z: 8 },
            BlockPosition { x: 9, y: 66, z: 8 },
        ] {
            assert_eq!(world.block(position)?, BlockStateId::AIR);
        }
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn water_prefers_the_nearest_downhill_route() -> Result<(), Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "toucan-world-fluid-slope-test-{}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        world.set_block(BlockPosition { x: 10, y: 63, z: 8 }, BlockStateId::AIR)?;
        world.set_block(
            BlockPosition { x: 8, y: 64, z: 8 },
            source_fluid_state(FluidKind::Water)?,
        )?;
        for _ in 0..5 {
            world.tick_block_updates(64)?;
        }

        assert!(fluid_state(world.block(BlockPosition { x: 9, y: 64, z: 8 })?)?.is_some());
        for position in [
            BlockPosition { x: 8, y: 64, z: 7 },
            BlockPosition { x: 8, y: 64, z: 9 },
            BlockPosition { x: 7, y: 64, z: 8 },
        ] {
            assert_eq!(world.block(position)?, BlockStateId::AIR);
        }
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn unsupported_flow_decays_on_the_next_water_tick() -> Result<(), Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "toucan-world-fluid-decay-test-{}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let source = BlockPosition { x: 8, y: 64, z: 8 };
        let flow = BlockPosition { x: 9, y: 64, z: 8 };
        world.set_block(source, source_fluid_state(FluidKind::Water)?)?;
        for _ in 0..5 {
            world.tick_block_updates(64)?;
        }
        assert!(fluid_state(world.block(flow)?)?.is_some());

        world.set_block(source, BlockStateId::AIR)?;
        for _ in 0..4 {
            world.tick_block_updates(64)?;
            assert!(fluid_state(world.block(flow)?)?.is_some());
        }
        world.tick_block_updates(64)?;
        assert_eq!(world.block(flow)?, BlockStateId::AIR);
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn overworld_lava_uses_its_thirty_tick_rate_and_two_level_drop_off()
    -> Result<(), Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "toucan-world-lava-rate-test-{}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let source = BlockPosition { x: 8, y: 64, z: 8 };
        let flow = BlockPosition { x: 9, y: 64, z: 8 };
        world.set_block(source, source_fluid_state(FluidKind::Lava)?)?;
        for _ in 0..29 {
            assert!(world.tick_block_updates(64)?.is_empty());
        }
        assert_eq!(world.block(flow)?, BlockStateId::AIR);

        assert!(!world.tick_block_updates(64)?.is_empty());
        let lava = fluid_state(world.block(flow)?)?.ok_or("flowing lava")?;
        assert_eq!(lava.kind(), FluidKind::Lava);
        assert_eq!(lava.level(), 2);
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn water_and_lava_create_obsidian_cobblestone_and_stone() -> Result<(), Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "toucan-world-fluid-mixing-test-{}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let registries = vanilla_registries()?;
        let water = source_fluid_state(FluidKind::Water)?;
        let lava = source_fluid_state(FluidKind::Lava)?;
        let obsidian = registries
            .block_by_name("minecraft:obsidian")?
            .default_state();
        let cobblestone = registries
            .block_by_name("minecraft:cobblestone")?
            .default_state();
        let stone = registries.block_by_name("minecraft:stone")?.default_state();

        let water_position = BlockPosition { x: 4, y: 64, z: 4 };
        let source_lava = BlockPosition { x: 5, y: 64, z: 4 };
        world.set_block(water_position, water)?;
        world.set_block(source_lava, lava)?;
        for _ in 0..5 {
            world.tick_block_updates(128)?;
        }
        assert_eq!(world.block(source_lava)?, obsidian);

        let flowing_lava = BlockPosition { x: 9, y: 64, z: 4 };
        world.set_block(
            flowing_lava,
            registries.resolve_state("minecraft:lava", [("level", "2")])?,
        )?;
        world.set_block(BlockPosition { x: 8, y: 64, z: 4 }, water)?;
        for _ in 0..5 {
            world.tick_block_updates(128)?;
        }
        assert_eq!(world.block(flowing_lava)?, cobblestone);

        let falling_lava = BlockPosition { x: 12, y: 66, z: 4 };
        let below = BlockPosition { x: 12, y: 65, z: 4 };
        world.set_block(below, water)?;
        world.set_block(falling_lava, lava)?;
        for _ in 0..30 {
            world.tick_block_updates(128)?;
        }
        assert_eq!(world.block(below)?, stone);
        assert_eq!(world.block(falling_lava)?, lava);

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

        for _ in 0..5 {
            world.tick_block_updates(64)?;
        }

        assert_eq!(world.loaded_chunk_count()?, 1);
        std::fs::remove_dir_all(path)?;
        Ok(())
    }
}
