//! Chunk coordinates and compact section-owned block state.

use std::array;

use crate::BlockStateId;

/// Lowest buildable Y coordinate in the protocol-775 overworld.
pub const MIN_Y: i32 = -64;
/// Vertical overworld size in blocks.
pub const WORLD_HEIGHT: i32 = 384;
/// Number of 16-block-high sections in one overworld chunk.
pub const SECTION_COUNT: usize = 24;
const SECTION_EDGE: usize = 16;
const SECTION_VOLUME: usize = SECTION_EDGE * SECTION_EDGE * SECTION_EDGE;

/// Integer chunk coordinates in one dimension.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChunkPosition {
    /// East/west chunk coordinate.
    pub x: i32,
    /// North/south chunk coordinate.
    pub z: i32,
}

impl ChunkPosition {
    /// Returns the chunk containing the supplied block coordinates.
    #[must_use]
    pub const fn from_block(x: i32, z: i32) -> Self {
        Self {
            x: x.div_euclid(16),
            z: z.div_euclid(16),
        }
    }

    /// Returns the world-space X coordinate of this chunk's west edge.
    #[must_use]
    pub const fn min_block_x(self) -> i32 {
        self.x * 16
    }

    /// Returns the world-space Z coordinate of this chunk's north edge.
    #[must_use]
    pub const fn min_block_z(self) -> i32 {
        self.z * 16
    }
}

/// One 16 cubed block section.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChunkSection {
    blocks: Box<[BlockStateId; SECTION_VOLUME]>,
    non_air_blocks: u16,
}

impl ChunkSection {
    fn empty() -> Self {
        Self {
            blocks: Box::new([BlockStateId::AIR; SECTION_VOLUME]),
            non_air_blocks: 0,
        }
    }

    /// Returns a block using section-local coordinates.
    #[must_use]
    pub fn block(&self, x: u8, y: u8, z: u8) -> Option<BlockStateId> {
        if x >= 16 || y >= 16 || z >= 16 {
            return None;
        }
        Some(self.blocks[section_index(x, y, z)])
    }

    /// Returns the number of non-air cells in this section.
    #[must_use]
    pub const fn non_air_blocks(&self) -> u16 {
        self.non_air_blocks
    }

    fn set_block(&mut self, x: u8, y: u8, z: u8, state: BlockStateId) {
        let index = section_index(x, y, z);
        let previous = self.blocks[index];
        if previous == BlockStateId::AIR && state != BlockStateId::AIR {
            self.non_air_blocks += 1;
        } else if previous != BlockStateId::AIR && state == BlockStateId::AIR {
            self.non_air_blocks -= 1;
        }
        self.blocks[index] = state;
    }
}

/// Authoritative world-domain state for one overworld chunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Chunk {
    position: ChunkPosition,
    sections: [ChunkSection; SECTION_COUNT],
}

impl Chunk {
    /// Creates an all-air chunk at `position`.
    #[must_use]
    pub fn empty(position: ChunkPosition) -> Self {
        Self {
            position,
            sections: array::from_fn(|_| ChunkSection::empty()),
        }
    }

    /// Returns this chunk's coordinates.
    #[must_use]
    pub const fn position(&self) -> ChunkPosition {
        self.position
    }

    /// Returns all vertical sections from Y=-64 upward.
    #[must_use]
    pub const fn sections(&self) -> &[ChunkSection; SECTION_COUNT] {
        &self.sections
    }

    /// Returns a block using chunk-local X/Z and world-space Y.
    #[must_use]
    pub fn block(&self, x: u8, y: i32, z: u8) -> Option<BlockStateId> {
        if x >= 16 || z >= 16 {
            return None;
        }
        let (section, local_y) = vertical_indices(y)?;
        self.sections[section].block(x, local_y, z)
    }

    /// Replaces a block using chunk-local X/Z and world-space Y.
    ///
    /// Returns `false` when a coordinate is outside this chunk's build bounds.
    pub fn set_block(&mut self, x: u8, y: i32, z: u8, state: BlockStateId) -> bool {
        if x >= 16 || z >= 16 {
            return false;
        }
        let Some((section, local_y)) = vertical_indices(y) else {
            return false;
        };
        self.sections[section].set_block(x, local_y, z, state);
        true
    }

    /// Returns the highest non-air block in a local column.
    #[must_use]
    pub fn surface_y(&self, x: u8, z: u8) -> Option<i32> {
        (MIN_Y..MIN_Y + WORLD_HEIGHT).rev().find(|&y| {
            self.block(x, y, z)
                .is_some_and(|state| state != BlockStateId::AIR)
        })
    }
}

const fn section_index(x: u8, y: u8, z: u8) -> usize {
    (y as usize * 16 + z as usize) * 16 + x as usize
}

fn vertical_indices(y: i32) -> Option<(usize, u8)> {
    let relative = y.checked_sub(MIN_Y)?;
    if !(0..WORLD_HEIGHT).contains(&relative) {
        return None;
    }
    Some(((relative / 16) as usize, (relative % 16) as u8))
}

#[cfg(test)]
mod tests {
    use super::{Chunk, ChunkPosition, MIN_Y};
    use crate::BlockStateId;

    #[test]
    fn negative_block_coordinates_use_euclidean_chunks() {
        assert_eq!(
            ChunkPosition::from_block(-1, -17),
            ChunkPosition { x: -1, z: -2 }
        );
    }

    #[test]
    fn block_mutation_tracks_section_population() {
        let mut chunk = Chunk::empty(ChunkPosition { x: 0, z: 0 });
        assert!(chunk.set_block(15, MIN_Y, 15, BlockStateId::STONE));
        assert_eq!(chunk.block(15, MIN_Y, 15), Some(BlockStateId::STONE));
        assert_eq!(chunk.sections()[0].non_air_blocks(), 1);
        assert_eq!(chunk.surface_y(15, 15), Some(MIN_Y));
        assert!(!chunk.set_block(16, 64, 0, BlockStateId::STONE));
    }
}
