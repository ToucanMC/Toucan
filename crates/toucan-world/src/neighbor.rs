use toucan_registry::{
    BlockDefinition, BlockStateId, CollisionCategory, Registries, RegistryError, vanilla_registries,
};

use crate::{BlockChange, BlockPosition, World, WorldError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HorizontalDirection {
    North,
    South,
    West,
    East,
}

impl HorizontalDirection {
    const ALL: [Self; 4] = [Self::North, Self::South, Self::West, Self::East];

    const fn name(self) -> &'static str {
        match self {
            Self::North => "north",
            Self::South => "south",
            Self::West => "west",
            Self::East => "east",
        }
    }

    const fn step(self) -> (i32, i32) {
        match self {
            Self::North => (0, -1),
            Self::South => (0, 1),
            Self::West => (-1, 0),
            Self::East => (1, 0),
        }
    }

    const fn opposite(self) -> Self {
        match self {
            Self::North => Self::South,
            Self::South => Self::North,
            Self::West => Self::East,
            Self::East => Self::West,
        }
    }

    const fn counter_clockwise(self) -> Self {
        match self {
            Self::North => Self::West,
            Self::South => Self::East,
            Self::West => Self::South,
            Self::East => Self::North,
        }
    }

    const fn axis(self) -> u8 {
        match self {
            Self::North | Self::South => 0,
            Self::West | Self::East => 1,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "north" => Some(Self::North),
            "south" => Some(Self::South),
            "west" => Some(Self::West),
            "east" => Some(Self::East),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
struct StairInfo {
    facing: HorizontalDirection,
    half: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionKind {
    Fence,
    Wall,
}

pub(crate) fn update_at(
    world: &World,
    position: BlockPosition,
) -> Result<Vec<BlockChange>, WorldError> {
    let Some(current) = world.loaded_block(position)? else {
        return Ok(Vec::new());
    };
    let registries = vanilla_registries()?;
    let block = registries.block(registries.state(current)?.block())?;
    let updated = if let Some(kind) = connection_kind(block) {
        connection_state(registries, world, position, current, kind)?
    } else if stair_info(registries, current)?.is_some() {
        let shape = stair_shape(registries, world, position, current)?;
        registries.with_property(current, "shape", shape)?
    } else {
        current
    };
    if updated == current {
        return Ok(Vec::new());
    }
    world.set_block(position, updated)?;
    Ok(vec![BlockChange {
        position,
        state: updated,
    }])
}

fn connection_state(
    registries: &Registries,
    world: &World,
    position: BlockPosition,
    current: BlockStateId,
    kind: ConnectionKind,
) -> Result<BlockStateId, WorldError> {
    let mut updated = current;
    let mut connected = [false; 4];
    for (index, direction) in HorizontalDirection::ALL.into_iter().enumerate() {
        connected[index] = connects_to(registries, world, position, kind, direction)?;
        let value = match (kind, connected[index]) {
            (ConnectionKind::Fence, true) => "true",
            (ConnectionKind::Fence, false) => "false",
            (ConnectionKind::Wall, true) => "low",
            (ConnectionKind::Wall, false) => "none",
        };
        updated = registries.with_property(updated, direction.name(), value)?;
    }
    if kind == ConnectionKind::Wall {
        let [north, south, west, east] = connected;
        let straight = (north && south && !west && !east) || (west && east && !north && !south);
        updated =
            registries.with_property(updated, "up", if straight { "false" } else { "true" })?;
    }
    Ok(updated)
}

fn connection_kind(block: &BlockDefinition) -> Option<ConnectionKind> {
    let name = block.name().as_str();
    if name.ends_with("_fence")
        && HorizontalDirection::ALL
            .into_iter()
            .all(|direction| supports(block, direction.name(), "true"))
    {
        Some(ConnectionKind::Fence)
    } else if name.ends_with("_wall")
        && HorizontalDirection::ALL
            .into_iter()
            .all(|direction| supports(block, direction.name(), "low"))
        && supports(block, "up", "true")
    {
        Some(ConnectionKind::Wall)
    } else {
        None
    }
}

fn connects_to(
    registries: &Registries,
    world: &World,
    position: BlockPosition,
    kind: ConnectionKind,
    direction: HorizontalDirection,
) -> Result<bool, WorldError> {
    let (dx, dz) = direction.step();
    let Some(neighbor_state) = world.loaded_block(offset(position, dx, 0, dz))? else {
        return Ok(false);
    };
    let neighbor = registries.block(registries.state(neighbor_state)?.block())?;
    let neighbor_name = neighbor.name().as_str();
    if neighbor.collision() == CollisionCategory::FullCube {
        return Ok(true);
    }
    if neighbor_name.ends_with("_fence_gate") {
        let facing = property(registries, neighbor_state, "facing")?
            .as_deref()
            .and_then(HorizontalDirection::parse);
        return Ok(facing.is_some_and(|facing| facing.axis() != direction.axis()));
    }
    Ok(match kind {
        ConnectionKind::Fence => neighbor_name.ends_with("_fence"),
        ConnectionKind::Wall => neighbor_name.ends_with("_wall"),
    })
}

fn supports(block: &BlockDefinition, property_name: &str, value: &str) -> bool {
    block
        .properties()
        .iter()
        .find(|property| property.name() == property_name)
        .is_some_and(|property| property.values().iter().any(|candidate| candidate == value))
}

fn property(
    registries: &Registries,
    state: BlockStateId,
    name: &str,
) -> Result<Option<String>, RegistryError> {
    Ok(registries
        .state_properties(state)?
        .into_iter()
        .find_map(|(candidate, value)| (candidate == name).then_some(value)))
}

fn stair_info(
    registries: &Registries,
    state: BlockStateId,
) -> Result<Option<StairInfo>, RegistryError> {
    let block = registries.block(registries.state(state)?.block())?;
    if !block.name().as_str().ends_with("_stairs") {
        return Ok(None);
    }
    let facing = property(registries, state, "facing")?
        .as_deref()
        .and_then(HorizontalDirection::parse);
    let half = property(registries, state, "half")?;
    Ok(match (facing, half.as_deref()) {
        (Some(facing), Some("top")) => Some(StairInfo {
            facing,
            half: "top",
        }),
        (Some(facing), Some("bottom")) => Some(StairInfo {
            facing,
            half: "bottom",
        }),
        _ => None,
    })
}

fn stair_shape(
    registries: &Registries,
    world: &World,
    position: BlockPosition,
    state: BlockStateId,
) -> Result<&'static str, WorldError> {
    let Some(stair) = stair_info(registries, state)? else {
        return Ok("straight");
    };
    let (front_x, front_z) = stair.facing.step();
    if let Some(front) = stair_info_at(registries, world, offset(position, front_x, 0, front_z))?
        && front.half == stair.half
        && front.facing.axis() != stair.facing.axis()
        && different_stair(registries, world, position, stair, front.facing.opposite())?
    {
        return Ok(if front.facing == stair.facing.counter_clockwise() {
            "outer_left"
        } else {
            "outer_right"
        });
    }

    let (back_x, back_z) = stair.facing.opposite().step();
    if let Some(back) = stair_info_at(registries, world, offset(position, back_x, 0, back_z))?
        && back.half == stair.half
        && back.facing.axis() != stair.facing.axis()
        && different_stair(registries, world, position, stair, back.facing)?
    {
        return Ok(if back.facing == stair.facing.counter_clockwise() {
            "inner_left"
        } else {
            "inner_right"
        });
    }
    Ok("straight")
}

fn different_stair(
    registries: &Registries,
    world: &World,
    position: BlockPosition,
    current: StairInfo,
    direction: HorizontalDirection,
) -> Result<bool, WorldError> {
    let (dx, dz) = direction.step();
    let adjacent = stair_info_at(registries, world, offset(position, dx, 0, dz))?;
    Ok(adjacent
        .is_none_or(|adjacent| adjacent.facing != current.facing || adjacent.half != current.half))
}

fn stair_info_at(
    registries: &Registries,
    world: &World,
    position: BlockPosition,
) -> Result<Option<StairInfo>, WorldError> {
    let Some(state) = world.loaded_block(position)? else {
        return Ok(None);
    };
    Ok(stair_info(registries, state)?)
}

fn offset(position: BlockPosition, dx: i32, dy: i32, dz: i32) -> BlockPosition {
    BlockPosition {
        x: position.x.saturating_add(dx),
        y: position.y.saturating_add(dy),
        z: position.z.saturating_add(dz),
    }
}
