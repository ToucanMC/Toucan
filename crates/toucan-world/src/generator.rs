use crate::{BlockStateId, Chunk, ChunkPosition};

pub trait ChunkGenerator: Send + Sync {
    fn identifier(&self) -> &'static str;

    fn generate(&self, seed: i64, position: ChunkPosition) -> Chunk;

    fn surface_y(&self, seed: i64, block_x: i32, block_z: i32) -> i32;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FlatGenerator;

impl FlatGenerator {
    pub const SURFACE_Y: i32 = 63;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TerrainGenerator;

impl ChunkGenerator for TerrainGenerator {
    fn identifier(&self) -> &'static str {
        "toucan:terrain"
    }

    fn generate(&self, seed: i64, position: ChunkPosition) -> Chunk {
        let mut chunk = Chunk::empty(position);
        for z in 0..16_u8 {
            for x in 0..16_u8 {
                let world_x = position.min_block_x() + i32::from(x);
                let world_z = position.min_block_z() + i32::from(z);
                let surface = self.surface_y(seed, world_x, world_z);
                for y in crate::MIN_Y..=surface {
                    let state = if y == surface {
                        BlockStateId::GRASS_BLOCK
                    } else if y >= surface - 3 {
                        BlockStateId::DIRT
                    } else {
                        stone_variant(seed, world_x, y, world_z)
                    };
                    let changed = chunk.set_block(x, y, z, state);
                    debug_assert!(changed);
                }
            }
        }
        chunk
    }

    fn surface_y(&self, seed: i64, block_x: i32, block_z: i32) -> i32 {
        let origin = terrain_noise(seed, 0, 0);
        let height = 63.0 + (terrain_noise(seed, block_x, block_z) - origin) * 18.0;
        (height.round() as i32).clamp(48, 96)
    }
}

fn terrain_noise(seed: i64, x: i32, z: i32) -> f64 {
    value_noise(seed, x, z, 64) * 0.65
        + value_noise(seed ^ 0x5deece66d, x, z, 24) * 0.25
        + value_noise(seed ^ 0x0b1d_5eed, x, z, 10) * 0.10
}

fn value_noise(seed: i64, x: i32, z: i32, scale: i32) -> f64 {
    let cell_x = x.div_euclid(scale);
    let cell_z = z.div_euclid(scale);
    let local_x = f64::from(x.rem_euclid(scale)) / f64::from(scale);
    let local_z = f64::from(z.rem_euclid(scale)) / f64::from(scale);
    let smooth_x = local_x * local_x * (3.0 - 2.0 * local_x);
    let smooth_z = local_z * local_z * (3.0 - 2.0 * local_z);
    let north = lerp(
        hash_noise(seed, cell_x, cell_z),
        hash_noise(seed, cell_x + 1, cell_z),
        smooth_x,
    );
    let south = lerp(
        hash_noise(seed, cell_x, cell_z + 1),
        hash_noise(seed, cell_x + 1, cell_z + 1),
        smooth_x,
    );
    lerp(north, south, smooth_z)
}

fn hash_noise(seed: i64, x: i32, z: i32) -> f64 {
    let mut value = seed as u64
        ^ (x as u32 as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ (z as u32 as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^= value >> 31;
    (value >> 11) as f64 / ((1_u64 << 53) - 1) as f64 * 2.0 - 1.0
}

fn stone_variant(seed: i64, x: i32, y: i32, z: i32) -> BlockStateId {
    match hash3(seed, x, y, z) % 29 {
        0 => BlockStateId::GRANITE,
        1 => BlockStateId::DIORITE,
        2 => BlockStateId::ANDESITE,
        _ => BlockStateId::STONE,
    }
}

fn hash3(seed: i64, x: i32, y: i32, z: i32) -> u64 {
    let mut value = seed as u64;
    value ^= (x as u32 as u64).wrapping_mul(0x9e37_79b9);
    value ^= (y as u32 as u64).wrapping_mul(0x85eb_ca6b);
    value ^= (z as u32 as u64).wrapping_mul(0xc2b2_ae35);
    value ^= value >> 33;
    value = value.wrapping_mul(0xff51_afd7_ed55_8ccd);
    value ^ (value >> 33)
}

fn lerp(start: f64, end: f64, amount: f64) -> f64 {
    start + (end - start) * amount
}

impl ChunkGenerator for FlatGenerator {
    fn identifier(&self) -> &'static str {
        "toucan:flat"
    }

    fn generate(&self, _seed: i64, position: ChunkPosition) -> Chunk {
        let mut chunk = Chunk::empty(position);
        for y in crate::MIN_Y..=Self::SURFACE_Y {
            for z in 0..16 {
                for x in 0..16 {
                    let changed = chunk.set_block(x, y, z, BlockStateId::STONE);
                    debug_assert!(changed);
                }
            }
        }
        chunk
    }

    fn surface_y(&self, _seed: i64, _block_x: i32, _block_z: i32) -> i32 {
        Self::SURFACE_Y
    }
}

#[cfg(test)]
mod tests {
    use super::{ChunkGenerator, FlatGenerator, TerrainGenerator};
    use crate::{BlockStateId, ChunkPosition};

    #[test]
    fn flat_generation_is_deterministic_across_chunk_boundaries() {
        let generator = FlatGenerator;
        let first = generator.generate(42, ChunkPosition { x: -1, z: 0 });
        let second = generator.generate(42, ChunkPosition { x: 0, z: 0 });
        assert_eq!(first.block(15, 63, 8), Some(BlockStateId::STONE));
        assert_eq!(second.block(0, 63, 8), Some(BlockStateId::STONE));
        assert_eq!(first.block(15, 64, 8), Some(BlockStateId::AIR));
        assert_eq!(first, generator.generate(42, ChunkPosition { x: -1, z: 0 }));
    }

    #[test]
    fn terrain_is_deterministic_continuous_and_layered() {
        let generator = TerrainGenerator;
        assert_eq!(generator.surface_y(42, 0, 0), 63);
        let left = generator.generate(42, ChunkPosition { x: -1, z: 0 });
        let right = generator.generate(42, ChunkPosition { x: 0, z: 0 });
        let left_surface = left.surface_y(15, 8).unwrap_or_default();
        let right_surface = right.surface_y(0, 8).unwrap_or_default();
        assert!((left_surface - right_surface).abs() <= 2);
        assert_eq!(
            right.block(0, right_surface, 8),
            Some(BlockStateId::GRASS_BLOCK)
        );
        assert_eq!(
            right.block(0, right_surface - 1, 8),
            Some(BlockStateId::DIRT)
        );
        assert_eq!(right, generator.generate(42, ChunkPosition { x: 0, z: 0 }));
    }
}
