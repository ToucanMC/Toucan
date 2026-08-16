use std::array;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::BlockStateId;

pub const MIN_Y: i32 = -64;
pub const WORLD_HEIGHT: i32 = 384;
pub const SECTION_COUNT: usize = 24;
const SECTION_EDGE: usize = 16;
const SECTION_VOLUME: usize = SECTION_EDGE * SECTION_EDGE * SECTION_EDGE;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChunkPosition {
    pub x: i32,
    pub z: i32,
}

impl ChunkPosition {
    #[must_use]
    pub const fn from_block(x: i32, z: i32) -> Self {
        Self {
            x: x.div_euclid(16),
            z: z.div_euclid(16),
        }
    }

    #[must_use]
    pub const fn min_block_x(self) -> i32 {
        self.x * 16
    }

    #[must_use]
    pub const fn min_block_z(self) -> i32 {
        self.z * 16
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChunkSection {
    blocks: Option<Box<[BlockStateId; SECTION_VOLUME]>>,
    non_air_blocks: u16,
}

impl ChunkSection {
    fn empty() -> Self {
        Self {
            blocks: Some(Box::new([BlockStateId::AIR; SECTION_VOLUME])),
            non_air_blocks: 0,
        }
    }

    fn opaque() -> Self {
        Self {
            blocks: None,
            non_air_blocks: 0,
        }
    }

    #[must_use]
    pub fn block(&self, x: u8, y: u8, z: u8) -> Option<BlockStateId> {
        if x >= 16 || y >= 16 || z >= 16 {
            return None;
        }
        self.blocks
            .as_ref()
            .map(|blocks| blocks[section_index(x, y, z)])
    }

    #[must_use]
    pub const fn non_air_blocks(&self) -> Option<u16> {
        if self.blocks.is_some() {
            Some(self.non_air_blocks)
        } else {
            None
        }
    }

    #[must_use]
    pub const fn is_opaque(&self) -> bool {
        self.blocks.is_none()
    }

    fn set_block(&mut self, x: u8, y: u8, z: u8, state: BlockStateId) {
        let index = section_index(x, y, z);
        let Some(blocks) = self.blocks.as_mut() else {
            return;
        };
        let previous = blocks[index];
        if previous == BlockStateId::AIR && state != BlockStateId::AIR {
            self.non_air_blocks += 1;
        } else if previous != BlockStateId::AIR && state == BlockStateId::AIR {
            self.non_air_blocks -= 1;
        }
        blocks[index] = state;
    }
}

#[derive(Debug)]
pub struct Chunk {
    position: ChunkPosition,
    sections: [RwLock<ChunkSection>; SECTION_COUNT],
    revision: AtomicU64,
}

impl PartialEq for Chunk {
    fn eq(&self, other: &Self) -> bool {
        self.position == other.position
            && (0..SECTION_COUNT).all(|index| self.section(index) == other.section(index))
    }
}

impl Eq for Chunk {}

impl Chunk {
    #[must_use]
    pub fn empty(position: ChunkPosition) -> Self {
        Self {
            position,
            sections: array::from_fn(|_| RwLock::new(ChunkSection::empty())),
            revision: AtomicU64::new(0),
        }
    }

    #[must_use]
    pub const fn position(&self) -> ChunkPosition {
        self.position
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn section(&self, index: usize) -> Option<ChunkSection> {
        self.sections.get(index).map(|section| {
            section
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        })
    }

    #[must_use]
    pub fn first_opaque_section_y(&self) -> Option<i8> {
        self.sections
            .iter()
            .enumerate()
            .find_map(|(index, section)| {
                section
                    .read()
                    .unwrap_or_else(|error| error.into_inner())
                    .is_opaque()
                    .then_some(index as i8 - 4)
            })
    }

    #[must_use]
    pub fn opaque_section_y_at(&self, y: i32) -> Option<i8> {
        let (index, _) = vertical_indices(y)?;
        self.sections[index]
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .is_opaque()
            .then_some(index as i8 - 4)
    }

    pub(crate) fn mark_section_opaque(&self, index: usize) -> bool {
        let Some(section) = self.sections.get(index) else {
            return false;
        };
        *section.write().unwrap_or_else(|error| error.into_inner()) = ChunkSection::opaque();
        true
    }

    #[must_use]
    pub fn block(&self, x: u8, y: i32, z: u8) -> Option<BlockStateId> {
        if x >= 16 || z >= 16 {
            return None;
        }
        let (section, local_y) = vertical_indices(y)?;
        self.sections[section]
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .block(x, local_y, z)
    }

    pub fn set_block(&self, x: u8, y: i32, z: u8, state: BlockStateId) -> bool {
        if x >= 16 || z >= 16 {
            return false;
        }
        let Some((section, local_y)) = vertical_indices(y) else {
            return false;
        };
        let mut section = self.sections[section]
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if section.is_opaque() {
            return false;
        }
        if section.block(x, local_y, z) == Some(state) {
            return true;
        }
        section.set_block(x, local_y, z, state);
        self.revision.fetch_add(1, Ordering::AcqRel);
        true
    }

    #[must_use]
    pub fn surface_y(&self, x: u8, z: u8) -> Option<i32> {
        if x >= 16 || z >= 16 || self.first_opaque_section_y().is_some() {
            return None;
        }
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
        let chunk = Chunk::empty(ChunkPosition { x: 0, z: 0 });
        assert!(chunk.set_block(15, MIN_Y, 15, BlockStateId::STONE));
        assert_eq!(chunk.block(15, MIN_Y, 15), Some(BlockStateId::STONE));
        assert_eq!(
            chunk.section(0).map(|section| section.non_air_blocks()),
            Some(Some(1))
        );
        assert_eq!(chunk.revision(), 1);
        assert_eq!(chunk.surface_y(15, 15), Some(MIN_Y));
        assert!(!chunk.set_block(16, 64, 0, BlockStateId::STONE));
    }

    #[test]
    fn opaque_sections_cannot_be_read_mutated_or_used_for_surfaces() {
        let chunk = Chunk::empty(ChunkPosition { x: 0, z: 0 });
        assert!(chunk.mark_section_opaque(8));
        assert_eq!(chunk.first_opaque_section_y(), Some(4));
        assert_eq!(chunk.opaque_section_y_at(64), Some(4));
        assert_eq!(chunk.block(0, 64, 0), None);
        assert!(!chunk.set_block(0, 64, 0, BlockStateId::STONE));
        assert_eq!(chunk.revision(), 0);
        assert_eq!(chunk.surface_y(0, 0), None);
    }
}
