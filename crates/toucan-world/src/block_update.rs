use toucan_registry::{BlockDefinition, BlockId, BlockStateId, Registries, vanilla_registries};

use crate::{BlockChange, BlockPosition, FluidKind, MIN_Y, World, WorldError, fluid_state};

const NEIGHBORS: [(i32, i32, i32); 7] = [
    (0, 0, 0),
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
    (-1, 0, 0),
    (1, 0, 0),
];

pub(crate) fn ticks_after_change(
    world: &World,
    position: BlockPosition,
) -> Result<Vec<(BlockPosition, BlockId, u64)>, WorldError> {
    let mut ticks = Vec::new();
    for (dx, dy, dz) in NEIGHBORS {
        let candidate = offset(position, dx, dy, dz);
        let Some(state) = world.loaded_block(candidate)? else {
            continue;
        };
        let registries = vanilla_registries()?;
        let block = registries.block(registries.state(state)?.block())?;
        if let Some(delay) = behavior_delay(block) {
            ticks.push((candidate, block.id(), delay));
        }
    }
    Ok(ticks)
}

pub(crate) fn update_at(
    world: &World,
    position: BlockPosition,
    expected: BlockId,
) -> Result<Vec<BlockChange>, WorldError> {
    let Some(state) = world.loaded_block(position)? else {
        return Ok(Vec::new());
    };
    let registries = vanilla_registries()?;
    let block = registries.block(registries.state(state)?.block())?;
    if block.id() != expected {
        return Ok(Vec::new());
    }
    let name = block.name().as_str();
    if let Some(companion) = paired_companion(registries, position, state, block)? {
        let valid = world
            .loaded_block(companion)?
            .is_some_and(|companion_state| {
                registries
                    .state(companion_state)
                    .is_ok_and(|state| state.block() == block.id())
            });
        if !valid {
            world.set_loaded_block(position, BlockStateId::AIR)?;
            return Ok(vec![BlockChange {
                position,
                state: BlockStateId::AIR,
            }]);
        }
    }
    if name.ends_with("_concrete_powder") && touches_water(world, position)? {
        let concrete_name = name.strip_suffix("_powder").expect("checked suffix");
        let hardened = registries.block_by_name(concrete_name)?.default_state();
        world.set_loaded_block(position, hardened)?;
        return Ok(vec![BlockChange {
            position,
            state: hardened,
        }]);
    }
    if is_falling_block(name) {
        return fall_one_block(world, position, state);
    }
    crate::neighbor::update_at(world, position)
}

fn fall_one_block(
    world: &World,
    position: BlockPosition,
    state: BlockStateId,
) -> Result<Vec<BlockChange>, WorldError> {
    if position.y <= MIN_Y {
        return Ok(Vec::new());
    }
    let below = offset(position, 0, -1, 0);
    let Some(below_state) = world.loaded_block(below)? else {
        return Ok(Vec::new());
    };
    let registries = vanilla_registries()?;
    let below_block = registries.block(registries.state(below_state)?.block())?;
    if !below_block.replaceable() {
        return Ok(Vec::new());
    }

    world.set_loaded_block(position, BlockStateId::AIR)?;
    world.set_loaded_block(below, state)?;
    Ok(vec![
        BlockChange {
            position,
            state: BlockStateId::AIR,
        },
        BlockChange {
            position: below,
            state,
        },
    ])
}

fn touches_water(world: &World, position: BlockPosition) -> Result<bool, WorldError> {
    for (dx, dy, dz) in &NEIGHBORS[1..] {
        let Some(state) = world.loaded_block(offset(position, *dx, *dy, *dz))? else {
            continue;
        };
        if fluid_state(state)?.is_some_and(|fluid| fluid.kind() == FluidKind::Water) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn behavior_delay(block: &BlockDefinition) -> Option<u64> {
    let name = block.name().as_str();
    if is_falling_block(name) {
        Some(2)
    } else if name.ends_with("_stairs")
        || name.ends_with("_fence")
        || name.ends_with("_wall")
        || is_paired_block(block)
    {
        Some(1)
    } else {
        None
    }
}

fn is_paired_block(block: &BlockDefinition) -> bool {
    let name = block.name().as_str();
    (name.ends_with("_door") && !name.ends_with("_trapdoor"))
        || name.ends_with("_bed")
        || (supports(block, "half", "lower") && supports(block, "half", "upper"))
}

fn paired_companion(
    registries: &Registries,
    position: BlockPosition,
    state: BlockStateId,
    block: &BlockDefinition,
) -> Result<Option<BlockPosition>, toucan_registry::RegistryError> {
    let name = block.name().as_str();
    if name.ends_with("_door") && !name.ends_with("_trapdoor")
        || (supports(block, "half", "lower") && supports(block, "half", "upper"))
    {
        return Ok(match property(registries, state, "half")?.as_deref() {
            Some("lower") => Some(offset(position, 0, 1, 0)),
            Some("upper") => Some(offset(position, 0, -1, 0)),
            _ => None,
        });
    }
    if name.ends_with("_bed") {
        let Some(facing) = property(registries, state, "facing")? else {
            return Ok(None);
        };
        let (dx, dz) = match facing.as_str() {
            "north" => (0, -1),
            "south" => (0, 1),
            "west" => (-1, 0),
            "east" => (1, 0),
            _ => return Ok(None),
        };
        return Ok(match property(registries, state, "part")?.as_deref() {
            Some("foot") => Some(offset(position, dx, 0, dz)),
            Some("head") => Some(offset(position, -dx, 0, -dz)),
            _ => None,
        });
    }
    Ok(None)
}

fn property(
    registries: &Registries,
    state: BlockStateId,
    name: &str,
) -> Result<Option<String>, toucan_registry::RegistryError> {
    Ok(registries
        .state_properties(state)?
        .into_iter()
        .find_map(|(candidate, value)| (candidate == name).then_some(value)))
}

fn supports(block: &BlockDefinition, property: &str, value: &str) -> bool {
    block
        .properties()
        .iter()
        .find(|candidate| candidate.name() == property)
        .is_some_and(|property| property.values().iter().any(|candidate| candidate == value))
}

fn is_falling_block(name: &str) -> bool {
    (name == "minecraft:sand" || (name.ends_with("_sand") && name != "minecraft:suspicious_sand"))
        || name == "minecraft:gravel"
        || name.ends_with("_concrete_powder")
        || name == "minecraft:anvil"
        || name.ends_with("_anvil")
        || name == "minecraft:dragon_egg"
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

    use toucan_registry::vanilla_registries;

    use crate::{BlockPosition, FluidKind, GeneratorKind, World, fluid_state, source_fluid_state};

    #[test]
    fn sand_falls_on_two_tick_steps_until_supported() -> Result<(), Box<dyn Error>> {
        let path = test_path("falling-sand");
        reset(&path)?;
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let sand = vanilla_registries()?
            .block_by_name("minecraft:sand")?
            .default_state();
        let top = BlockPosition { x: 8, y: 66, z: 8 };
        world.set_block(top, sand)?;

        assert!(world.tick_block_updates(64)?.is_empty());
        assert_eq!(world.block(top)?, sand);
        let first = world.tick_block_updates(64)?;
        assert_eq!(first.len(), 2);
        assert_eq!(world.block(top)?, crate::BlockStateId::AIR);
        assert_eq!(world.block(BlockPosition { x: 8, y: 65, z: 8 })?, sand);

        world.tick_block_updates(64)?;
        world.tick_block_updates(64)?;
        assert_eq!(world.block(BlockPosition { x: 8, y: 64, z: 8 })?, sand);
        world.tick_block_updates(64)?;
        world.tick_block_updates(64)?;
        assert_eq!(world.block(BlockPosition { x: 8, y: 64, z: 8 })?, sand);
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn concrete_powder_hardens_when_touching_water() -> Result<(), Box<dyn Error>> {
        let path = test_path("concrete-hardening");
        reset(&path)?;
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let registries = vanilla_registries()?;
        let powder = registries
            .block_by_name("minecraft:blue_concrete_powder")?
            .default_state();
        let concrete = registries
            .block_by_name("minecraft:blue_concrete")?
            .default_state();
        let position = BlockPosition { x: 8, y: 64, z: 8 };
        world.set_block(position, powder)?;
        world.set_block(
            BlockPosition { x: 9, y: 64, z: 8 },
            source_fluid_state(FluidKind::Water)?,
        )?;
        world.tick_block_updates(64)?;
        world.tick_block_updates(64)?;

        assert_eq!(world.block(position)?, concrete);
        assert!(fluid_state(world.block(BlockPosition { x: 9, y: 64, z: 8 })?)?.is_some());
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn fence_connections_update_from_world_neighbor_ticks() -> Result<(), Box<dyn Error>> {
        let path = test_path("neighbor-fence");
        reset(&path)?;
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let registries = vanilla_registries()?;
        let fence = registries
            .block_by_name("minecraft:oak_fence")?
            .default_state();
        let west = BlockPosition { x: 8, y: 64, z: 8 };
        let east = BlockPosition { x: 9, y: 64, z: 8 };
        world.set_block(west, fence)?;
        world.set_block(east, fence)?;
        let changes = world.tick_block_updates(64)?;
        assert_eq!(changes.len(), 2);

        let west_properties = registries.state_properties(world.block(west)?)?;
        let east_properties = registries.state_properties(world.block(east)?)?;
        assert!(west_properties.contains(&("east", "true".to_owned())));
        assert!(east_properties.contains(&("west", "true".to_owned())));
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn falling_block_tick_survives_save_and_reopen() -> Result<(), Box<dyn Error>> {
        let path = test_path("falling-persistence");
        reset(&path)?;
        let sand = vanilla_registries()?
            .block_by_name("minecraft:sand")?
            .default_state();
        let top = BlockPosition { x: 8, y: 66, z: 8 };
        {
            let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
            world.set_block(top, sand)?;
            assert_eq!(world.save_dirty()?, 1);
        }

        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        assert_eq!(world.block(top)?, sand);
        assert!(world.tick_block_updates(64)?.is_empty());
        assert_eq!(world.tick_block_updates(64)?.len(), 2);
        assert_eq!(world.block(BlockPosition { x: 8, y: 65, z: 8 })?, sand);
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn orphaned_door_half_is_removed_by_neighbor_update() -> Result<(), Box<dyn Error>> {
        let path = test_path("orphaned-door");
        reset(&path)?;
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let registries = vanilla_registries()?;
        let lower = registries.resolve_state(
            "minecraft:oak_door",
            [
                ("facing", "north"),
                ("half", "lower"),
                ("hinge", "left"),
                ("open", "false"),
                ("powered", "false"),
            ],
        )?;
        let upper = registries.resolve_state(
            "minecraft:oak_door",
            [
                ("facing", "north"),
                ("half", "upper"),
                ("hinge", "left"),
                ("open", "false"),
                ("powered", "false"),
            ],
        )?;
        let lower_position = BlockPosition { x: 8, y: 64, z: 8 };
        let upper_position = BlockPosition { x: 8, y: 65, z: 8 };
        world.set_block(lower_position, lower)?;
        world.set_block(upper_position, upper)?;
        world.tick_block_updates(64)?;
        assert_eq!(world.block(upper_position)?, upper);

        world.set_block(lower_position, crate::BlockStateId::AIR)?;
        let changes = world.tick_block_updates(64)?;
        assert_eq!(changes.len(), 1);
        assert_eq!(world.block(upper_position)?, crate::BlockStateId::AIR);
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn consumed_noop_block_tick_is_removed_from_persistence() -> Result<(), Box<dyn Error>> {
        let path = test_path("consumed-tick-persistence");
        reset(&path)?;
        let fence = vanilla_registries()?
            .block_by_name("minecraft:oak_fence")?
            .default_state();
        let position = BlockPosition { x: 8, y: 64, z: 8 };
        {
            let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
            world.set_block(position, fence)?;
            assert_eq!(world.save_dirty()?, 1);
        }
        {
            let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
            assert_eq!(world.block(position)?, fence);
            assert!(world.tick_block_updates(64)?.is_empty());
            assert_eq!(world.dirty_chunk_count()?, 1);
            assert_eq!(world.save_dirty()?, 1);
        }

        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        assert_eq!(world.block(position)?, fence);
        assert!(world.tick_block_updates(64)?.is_empty());
        assert_eq!(world.dirty_chunk_count()?, 0);
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    fn test_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("toucan-world-{name}-test-{}", std::process::id()))
    }

    fn reset(path: &std::path::Path) -> Result<(), std::io::Error> {
        if path.exists() {
            std::fs::remove_dir_all(path)?;
        }
        Ok(())
    }
}
