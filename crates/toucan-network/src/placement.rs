use thiserror::Error;
use toucan_registry::{
    BlockDefinition, BlockStateId, Registries, RegistryError, vanilla_registries,
};
use toucan_world::{BlockPosition, World, WorldError};

#[derive(Clone, Copy, Debug)]
pub(crate) struct PlacementContext {
    pub clicked_face: i32,
    pub cursor: [f32; 3],
    pub player_yaw: f32,
    pub player_pitch: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BlockChange {
    pub position: BlockPosition,
    pub state: BlockStateId,
}

#[derive(Debug)]
pub(crate) struct PlacementPlan {
    pub occupied: Vec<BlockChange>,
    pub refresh_stairs: bool,
}

#[derive(Debug, Error)]
pub(crate) enum PlacementError {
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    World(#[from] WorldError),
}

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

pub(crate) fn plan_placement(
    default_state: BlockStateId,
    target: BlockPosition,
    context: PlacementContext,
) -> Result<Option<PlacementPlan>, PlacementError> {
    let registries = vanilla_registries()?;
    let state = registries.state(default_state)?;
    let block = registries.block(state.block())?;
    let name = block.name().as_str();
    let player_facing = horizontal_direction(context.player_yaw);

    if name.ends_with("_stairs") {
        let mut placed = default_state;
        placed = set_supported(registries, block, placed, "facing", player_facing.name())?;
        let half = if context.clicked_face == 0
            || (context.clicked_face != 1 && context.cursor[1] > 0.5)
        {
            "top"
        } else {
            "bottom"
        };
        placed = set_supported(registries, block, placed, "half", half)?;
        placed = set_supported(registries, block, placed, "shape", "straight")?;
        return Ok(Some(single(target, placed, true)));
    }

    if name.ends_with("_door") && !name.ends_with("_trapdoor") {
        let hinge = door_hinge(player_facing, context.cursor);
        let lower = configure_door(
            registries,
            block,
            default_state,
            player_facing,
            "lower",
            hinge,
        )?;
        let upper = configure_door(
            registries,
            block,
            default_state,
            player_facing,
            "upper",
            hinge,
        )?;
        return Ok(Some(PlacementPlan {
            occupied: vec![
                BlockChange {
                    position: target,
                    state: lower,
                },
                BlockChange {
                    position: offset(target, 0, 1, 0),
                    state: upper,
                },
            ],
            refresh_stairs: false,
        }));
    }

    if name.ends_with("_trapdoor") {
        let facing = horizontal_face(context.clicked_face).unwrap_or(player_facing.opposite());
        let half = if context.clicked_face == 0
            || (context.clicked_face != 1 && context.cursor[1] > 0.5)
        {
            "top"
        } else {
            "bottom"
        };
        let mut placed = default_state;
        placed = set_supported(registries, block, placed, "facing", facing.name())?;
        placed = set_supported(registries, block, placed, "half", half)?;
        placed = set_supported(registries, block, placed, "open", "false")?;
        placed = set_supported(registries, block, placed, "powered", "false")?;
        return Ok(Some(single(target, placed, false)));
    }

    if name.ends_with("_hanging_sign") {
        return plan_sign(registries, name, target, context, true);
    }

    if name.ends_with("_sign") {
        return plan_sign(registries, name, target, context, false);
    }

    if name.ends_with("_bed") {
        let mut foot = default_state;
        foot = set_supported(registries, block, foot, "facing", player_facing.name())?;
        foot = set_supported(registries, block, foot, "part", "foot")?;
        foot = set_supported(registries, block, foot, "occupied", "false")?;
        let mut head = foot;
        head = set_supported(registries, block, head, "part", "head")?;
        let (dx, dz) = player_facing.step();
        return Ok(Some(PlacementPlan {
            occupied: vec![
                BlockChange {
                    position: target,
                    state: foot,
                },
                BlockChange {
                    position: offset(target, dx, 0, dz),
                    state: head,
                },
            ],
            refresh_stairs: false,
        }));
    }

    if supports(block, "half", "lower") && supports(block, "half", "upper") {
        let lower = set_supported(registries, block, default_state, "half", "lower")?;
        let upper = set_supported(registries, block, default_state, "half", "upper")?;
        return Ok(Some(PlacementPlan {
            occupied: vec![
                BlockChange {
                    position: target,
                    state: lower,
                },
                BlockChange {
                    position: offset(target, 0, 1, 0),
                    state: upper,
                },
            ],
            refresh_stairs: false,
        }));
    }

    let mut placed = default_state;
    let axis = match context.clicked_face {
        0 | 1 => "y",
        2 | 3 => "z",
        4 | 5 => "x",
        _ => "y",
    };
    placed = set_supported(registries, block, placed, "axis", axis)?;

    if supports_all_directions(block) {
        let facing = nearest_placement_facing(context);
        placed = set_supported(registries, block, placed, "facing", facing)?;
    } else if supports_horizontal_facing(block) {
        let facing = if uses_clicked_face(name) {
            horizontal_face(context.clicked_face).unwrap_or(player_facing.opposite())
        } else if faces_with_player(name) {
            player_facing
        } else {
            player_facing.opposite()
        };
        placed = set_supported(registries, block, placed, "facing", facing.name())?;
    }

    Ok(Some(single(target, placed, false)))
}

pub(crate) fn refresh_stair_shapes(
    world: &World,
    placed_at: BlockPosition,
) -> Result<Vec<BlockChange>, PlacementError> {
    let mut positions = Vec::with_capacity(5);
    positions.push(placed_at);
    for direction in HorizontalDirection::ALL {
        let (dx, dz) = direction.step();
        positions.push(offset(placed_at, dx, 0, dz));
    }

    let registries = vanilla_registries()?;
    let mut changes = Vec::new();
    for position in positions {
        let current = world.block(position)?;
        if stair_info(registries, current)?.is_none() {
            continue;
        }
        let shape = stair_shape(registries, world, position, current)?;
        let updated = registries.with_property(current, "shape", shape)?;
        if updated != current {
            world.set_block(position, updated)?;
            changes.push(BlockChange {
                position,
                state: updated,
            });
        }
    }
    Ok(changes)
}

pub(crate) fn companion_for_break(
    world: &World,
    position: BlockPosition,
    state: BlockStateId,
) -> Result<Option<BlockPosition>, PlacementError> {
    let registries = vanilla_registries()?;
    let block = registries.block(registries.state(state)?.block())?;
    let name = block.name().as_str();

    let candidate = if name.ends_with("_door") && !name.ends_with("_trapdoor") {
        match property(registries, state, "half")?.as_deref() {
            Some("lower") => Some(offset(position, 0, 1, 0)),
            Some("upper") => Some(offset(position, 0, -1, 0)),
            _ => None,
        }
    } else if name.ends_with("_bed") {
        let facing = property(registries, state, "facing")?
            .as_deref()
            .and_then(HorizontalDirection::parse);
        match (property(registries, state, "part")?.as_deref(), facing) {
            (Some("foot"), Some(direction)) => {
                let (dx, dz) = direction.step();
                Some(offset(position, dx, 0, dz))
            }
            (Some("head"), Some(direction)) => {
                let (dx, dz) = direction.opposite().step();
                Some(offset(position, dx, 0, dz))
            }
            _ => None,
        }
    } else if supports(block, "half", "lower") && supports(block, "half", "upper") {
        match property(registries, state, "half")?.as_deref() {
            Some("lower") => Some(offset(position, 0, 1, 0)),
            Some("upper") => Some(offset(position, 0, -1, 0)),
            _ => None,
        }
    } else {
        None
    };

    let Some(candidate) = candidate else {
        return Ok(None);
    };
    let companion = world.block(candidate)?;
    Ok((registries.state(companion)?.block() == block.id()).then_some(candidate))
}

fn plan_sign(
    registries: &Registries,
    name: &str,
    target: BlockPosition,
    context: PlacementContext,
    hanging: bool,
) -> Result<Option<PlacementPlan>, PlacementError> {
    if let Some(facing) = horizontal_face(context.clicked_face) {
        let wall_name = if hanging {
            name.replace("_hanging_sign", "_wall_hanging_sign")
        } else {
            name.replace("_sign", "_wall_sign")
        };
        let wall = registries.block_by_name(&wall_name)?;
        let placed = set_supported(
            registries,
            wall,
            wall.default_state(),
            "facing",
            facing.name(),
        )?;
        return Ok(Some(single(target, placed, false)));
    }

    if (!hanging && context.clicked_face != 1) || (hanging && context.clicked_face == 1) {
        return Ok(None);
    }
    let block = registries.block_by_name(name)?;
    let rotation = rotation_segment(context.player_yaw).to_string();
    let mut placed = set_supported(
        registries,
        block,
        block.default_state(),
        "rotation",
        &rotation,
    )?;
    if hanging {
        placed = set_supported(registries, block, placed, "attached", "false")?;
    }
    Ok(Some(single(target, placed, false)))
}

fn configure_door(
    registries: &Registries,
    block: &BlockDefinition,
    state: BlockStateId,
    facing: HorizontalDirection,
    half: &str,
    hinge: &str,
) -> Result<BlockStateId, RegistryError> {
    let mut state = set_supported(registries, block, state, "facing", facing.name())?;
    state = set_supported(registries, block, state, "half", half)?;
    state = set_supported(registries, block, state, "hinge", hinge)?;
    state = set_supported(registries, block, state, "open", "false")?;
    set_supported(registries, block, state, "powered", "false")
}

fn single(position: BlockPosition, state: BlockStateId, refresh_stairs: bool) -> PlacementPlan {
    PlacementPlan {
        occupied: vec![BlockChange { position, state }],
        refresh_stairs,
    }
}

fn set_supported(
    registries: &Registries,
    block: &BlockDefinition,
    state: BlockStateId,
    property_name: &str,
    value: &str,
) -> Result<BlockStateId, RegistryError> {
    if supports(block, property_name, value) {
        registries.with_property(state, property_name, value)
    } else {
        Ok(state)
    }
}

fn supports(block: &BlockDefinition, property_name: &str, value: &str) -> bool {
    block
        .properties()
        .iter()
        .find(|property| property.name() == property_name)
        .is_some_and(|property| property.values().iter().any(|candidate| candidate == value))
}

fn supports_horizontal_facing(block: &BlockDefinition) -> bool {
    HorizontalDirection::ALL
        .into_iter()
        .all(|direction| supports(block, "facing", direction.name()))
}

fn supports_all_directions(block: &BlockDefinition) -> bool {
    supports_horizontal_facing(block)
        && supports(block, "facing", "up")
        && supports(block, "facing", "down")
}

fn uses_clicked_face(name: &str) -> bool {
    name.ends_with("_wall_sign")
        || name.ends_with("_wall_hanging_sign")
        || name.ends_with("_wall_skull")
        || name.ends_with("_wall_head")
        || name == "minecraft:ladder"
}

fn faces_with_player(name: &str) -> bool {
    name.ends_with("_fence_gate") || name.ends_with("_door") || name.ends_with("_trapdoor")
}

fn nearest_placement_facing(context: PlacementContext) -> &'static str {
    if context.player_pitch > 45.0 {
        "up"
    } else if context.player_pitch < -45.0 {
        "down"
    } else {
        horizontal_direction(context.player_yaw).opposite().name()
    }
}

fn horizontal_direction(yaw: f32) -> HorizontalDirection {
    match ((yaw.rem_euclid(360.0) / 90.0 + 0.5).floor() as u8) % 4 {
        0 => HorizontalDirection::South,
        1 => HorizontalDirection::West,
        2 => HorizontalDirection::North,
        3 => HorizontalDirection::East,
        _ => unreachable!(),
    }
}

fn horizontal_face(face: i32) -> Option<HorizontalDirection> {
    match face {
        2 => Some(HorizontalDirection::North),
        3 => Some(HorizontalDirection::South),
        4 => Some(HorizontalDirection::West),
        5 => Some(HorizontalDirection::East),
        _ => None,
    }
}

fn rotation_segment(yaw: f32) -> u8 {
    (((yaw + 180.0).rem_euclid(360.0) / 22.5 + 0.5).floor() as u8) % 16
}

fn door_hinge(facing: HorizontalDirection, cursor: [f32; 3]) -> &'static str {
    let right = match facing {
        HorizontalDirection::North => cursor[0] > 0.5,
        HorizontalDirection::South => cursor[0] < 0.5,
        HorizontalDirection::West => cursor[2] < 0.5,
        HorizontalDirection::East => cursor[2] > 0.5,
    };
    if right { "right" } else { "left" }
}

fn offset(position: BlockPosition, dx: i32, dy: i32, dz: i32) -> BlockPosition {
    BlockPosition {
        x: position.x.saturating_add(dx),
        y: position.y.saturating_add(dy),
        z: position.z.saturating_add(dz),
    }
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
) -> Result<&'static str, PlacementError> {
    let Some(stair) = stair_info(registries, state)? else {
        return Ok("straight");
    };
    let (front_x, front_z) = stair.facing.step();
    let front_position = offset(position, front_x, 0, front_z);
    if let Some(front) = stair_info(registries, world.block(front_position)?)?
        && front.half == stair.half
        && front.facing.axis() != stair.facing.axis()
        && different_stair(registries, world, position, state, front.facing.opposite())?
    {
        return Ok(if front.facing == stair.facing.counter_clockwise() {
            "outer_left"
        } else {
            "outer_right"
        });
    }

    let (back_x, back_z) = stair.facing.opposite().step();
    let back_position = offset(position, back_x, 0, back_z);
    if let Some(back) = stair_info(registries, world.block(back_position)?)?
        && back.half == stair.half
        && back.facing.axis() != stair.facing.axis()
        && different_stair(registries, world, position, state, back.facing)?
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
    state: BlockStateId,
    direction: HorizontalDirection,
) -> Result<bool, PlacementError> {
    let current = stair_info(registries, state)?.expect("caller validated stair state");
    let (dx, dz) = direction.step();
    let adjacent = stair_info(registries, world.block(offset(position, dx, 0, dz))?)?;
    Ok(adjacent
        .is_none_or(|adjacent| adjacent.facing != current.facing || adjacent.half != current.half))
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::{PlacementContext, plan_placement, property, refresh_stair_shapes};
    use toucan_registry::{BlockStateId, vanilla_registries};
    use toucan_world::{BlockPosition, GeneratorKind, World};

    const TARGET: BlockPosition = BlockPosition { x: 0, y: 64, z: 0 };

    fn context(face: i32, yaw: f32, pitch: f32, cursor_y: f32) -> PlacementContext {
        PlacementContext {
            clicked_face: face,
            cursor: [0.75, cursor_y, 0.25],
            player_yaw: yaw,
            player_pitch: pitch,
        }
    }

    fn default_state(name: &str) -> Result<BlockStateId, Box<dyn Error>> {
        Ok(vanilla_registries()?.block_by_name(name)?.default_state())
    }

    fn assert_property(
        state: BlockStateId,
        name: &str,
        expected: &str,
    ) -> Result<(), Box<dyn Error>> {
        assert_eq!(
            property(vanilla_registries()?, state, name)?.as_deref(),
            Some(expected)
        );
        Ok(())
    }

    #[test]
    fn doors_and_beds_create_paired_states() -> Result<(), Box<dyn Error>> {
        let door = plan_placement(
            default_state("minecraft:oak_door")?,
            TARGET,
            context(1, 90.0, 0.0, 0.25),
        )?
        .expect("door placement");
        assert_eq!(door.occupied.len(), 2);
        assert_eq!(door.occupied[1].position.y, TARGET.y + 1);
        assert_property(door.occupied[0].state, "facing", "west")?;
        assert_property(door.occupied[0].state, "half", "lower")?;
        assert_property(door.occupied[1].state, "half", "upper")?;
        assert_property(door.occupied[0].state, "hinge", "right")?;

        let bed = plan_placement(
            default_state("minecraft:red_bed")?,
            TARGET,
            context(1, 0.0, 0.0, 0.5),
        )?
        .expect("bed placement");
        assert_eq!(bed.occupied.len(), 2);
        assert_eq!(bed.occupied[1].position.z, TARGET.z + 1);
        assert_property(bed.occupied[0].state, "part", "foot")?;
        assert_property(bed.occupied[1].state, "part", "head")?;
        Ok(())
    }

    #[test]
    fn trapdoors_use_clicked_face_and_cursor_half() -> Result<(), Box<dyn Error>> {
        let trapdoor = plan_placement(
            default_state("minecraft:oak_trapdoor")?,
            TARGET,
            context(2, 0.0, 0.0, 0.75),
        )?
        .expect("trapdoor placement")
        .occupied[0]
            .state;
        assert_property(trapdoor, "facing", "north")?;
        assert_property(trapdoor, "half", "top")?;
        assert_property(trapdoor, "open", "false")?;
        Ok(())
    }

    #[test]
    fn signs_choose_standing_or_wall_variants() -> Result<(), Box<dyn Error>> {
        let sign = default_state("minecraft:oak_sign")?;
        let standing = plan_placement(sign, TARGET, context(1, 180.0, 0.0, 0.5))?
            .expect("standing sign placement")
            .occupied[0]
            .state;
        assert_property(standing, "rotation", "0")?;

        let wall = plan_placement(sign, TARGET, context(5, 180.0, 0.0, 0.5))?
            .expect("wall sign placement")
            .occupied[0]
            .state;
        let registries = vanilla_registries()?;
        assert_eq!(
            registries
                .block(registries.state(wall)?.block())?
                .name()
                .as_str(),
            "minecraft:oak_wall_sign"
        );
        assert_property(wall, "facing", "east")?;
        Ok(())
    }

    #[test]
    fn containers_face_player_and_pistons_use_six_directions() -> Result<(), Box<dyn Error>> {
        for name in ["minecraft:furnace", "minecraft:chest"] {
            let state = plan_placement(default_state(name)?, TARGET, context(1, 90.0, 0.0, 0.5))?
                .expect("horizontal placement")
                .occupied[0]
                .state;
            assert_property(state, "facing", "east")?;
        }

        let piston = default_state("minecraft:piston")?;
        let horizontal = plan_placement(piston, TARGET, context(1, 0.0, 0.0, 0.5))?
            .expect("horizontal piston")
            .occupied[0]
            .state;
        assert_property(horizontal, "facing", "north")?;
        let vertical = plan_placement(piston, TARGET, context(1, 0.0, 90.0, 0.5))?
            .expect("vertical piston")
            .occupied[0]
            .state;
        assert_property(vertical, "facing", "up")?;
        Ok(())
    }

    #[test]
    fn double_height_plants_create_lower_and_upper_states() -> Result<(), Box<dyn Error>> {
        let sunflower = plan_placement(
            default_state("minecraft:sunflower")?,
            TARGET,
            context(1, 0.0, 0.0, 0.5),
        )?
        .expect("sunflower placement");
        assert_eq!(sunflower.occupied.len(), 2);
        assert_property(sunflower.occupied[0].state, "half", "lower")?;
        assert_property(sunflower.occupied[1].state, "half", "upper")?;
        Ok(())
    }

    #[test]
    fn stair_neighbors_form_and_release_corners() -> Result<(), Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "toucan-placement-corner-test-{}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let stairs = default_state("minecraft:oak_stairs")?;
        let first = plan_placement(stairs, TARGET, context(1, 0.0, 0.0, 0.5))?
            .expect("first stair")
            .occupied[0]
            .state;
        let front = BlockPosition { x: 0, y: 64, z: 1 };
        let second = plan_placement(stairs, front, context(1, 270.0, 0.0, 0.5))?
            .expect("second stair")
            .occupied[0]
            .state;
        world.set_block(TARGET, first)?;
        world.set_block(front, second)?;

        refresh_stair_shapes(&world, front)?;
        let corner = property(vanilla_registries()?, world.block(TARGET)?, "shape")?
            .ok_or("missing stair shape")?;
        assert!(corner.starts_with("outer_"));

        world.set_block(front, BlockStateId::AIR)?;
        refresh_stair_shapes(&world, front)?;
        assert_property(world.block(TARGET)?, "shape", "straight")?;
        std::fs::remove_dir_all(path)?;
        Ok(())
    }
}
