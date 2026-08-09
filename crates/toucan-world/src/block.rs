//! Stable block-state handles used by world-domain chunks.

/// A stable index into Toucan's internal block-state table.
///
/// This is deliberately not a Minecraft protocol state ID. Version-specific
/// conversion belongs at the networking/protocol boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BlockStateId(u16);

impl BlockStateId {
    /// Air, used for empty cells and sections.
    pub const AIR: Self = Self(0);
    /// The default solid block used by the alpha flat generator.
    pub const STONE: Self = Self(1);
    /// Natural stone variant.
    pub const GRANITE: Self = Self(2);
    /// Natural stone variant.
    pub const DIORITE: Self = Self(3);
    /// Natural stone variant.
    pub const ANDESITE: Self = Self(4);
    /// Unsnowed grass surface.
    pub const GRASS_BLOCK: Self = Self(5);
    /// Natural dirt.
    pub const DIRT: Self = Self(6);
    /// Basic cobblestone building block.
    pub const COBBLESTONE: Self = Self(7);
    /// Oak plank building block.
    pub const OAK_PLANKS: Self = Self(8);
    /// Spruce plank building block.
    pub const SPRUCE_PLANKS: Self = Self(9);
    /// Birch plank building block.
    pub const BIRCH_PLANKS: Self = Self(10);
    /// Jungle plank building block.
    pub const JUNGLE_PLANKS: Self = Self(11);
    /// Acacia plank building block.
    pub const ACACIA_PLANKS: Self = Self(12);
    /// Cherry plank building block.
    pub const CHERRY_PLANKS: Self = Self(13);
    /// Dark oak plank building block.
    pub const DARK_OAK_PLANKS: Self = Self(14);
    /// Pale oak plank building block.
    pub const PALE_OAK_PLANKS: Self = Self(15);
    /// Sand building block.
    pub const SAND: Self = Self(16);
    /// Gravel building block.
    pub const GRAVEL: Self = Self(17);
    /// Glass building block.
    pub const GLASS: Self = Self(18);
    /// Brick building block.
    pub const BRICKS: Self = Self(19);
    /// Stone brick building block.
    pub const STONE_BRICKS: Self = Self(20);
    /// Obsidian building block.
    pub const OBSIDIAN: Self = Self(21);
    /// Netherrack building block.
    pub const NETHERRACK: Self = Self(22);
    /// End stone building block.
    pub const END_STONE: Self = Self(23);
    /// White wool building block.
    pub const WHITE_WOOL: Self = Self(24);
    /// Red wool building block.
    pub const RED_WOOL: Self = Self(25);
    /// Blue wool building block.
    pub const BLUE_WOOL: Self = Self(26);
    /// Gold storage block.
    pub const GOLD_BLOCK: Self = Self(27);
    /// Iron storage block.
    pub const IRON_BLOCK: Self = Self(28);
    /// Diamond storage block.
    pub const DIAMOND_BLOCK: Self = Self(29);
    /// Emerald storage block.
    pub const EMERALD_BLOCK: Self = Self(30);

    /// Returns the stable namespaced identifier for this state.
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self.0 {
            0 => "minecraft:air",
            1 => "minecraft:stone",
            2 => "minecraft:granite",
            3 => "minecraft:diorite",
            4 => "minecraft:andesite",
            5 => "minecraft:grass_block",
            6 => "minecraft:dirt",
            7 => "minecraft:cobblestone",
            8 => "minecraft:oak_planks",
            9 => "minecraft:spruce_planks",
            10 => "minecraft:birch_planks",
            11 => "minecraft:jungle_planks",
            12 => "minecraft:acacia_planks",
            13 => "minecraft:cherry_planks",
            14 => "minecraft:dark_oak_planks",
            15 => "minecraft:pale_oak_planks",
            16 => "minecraft:sand",
            17 => "minecraft:gravel",
            18 => "minecraft:glass",
            19 => "minecraft:bricks",
            20 => "minecraft:stone_bricks",
            21 => "minecraft:obsidian",
            22 => "minecraft:netherrack",
            23 => "minecraft:end_stone",
            24 => "minecraft:white_wool",
            25 => "minecraft:red_wool",
            26 => "minecraft:blue_wool",
            27 => "minecraft:gold_block",
            28 => "minecraft:iron_block",
            29 => "minecraft:diamond_block",
            30 => "minecraft:emerald_block",
            _ => "toucan:unknown_block_state",
        }
    }

    /// Returns the internal numeric handle for compact storage and diagnostics.
    #[must_use]
    pub const fn raw(self) -> u16 {
        self.0
    }

    /// Resolves one supported namespaced identifier to its stable handle.
    #[must_use]
    pub fn from_identifier(identifier: &str) -> Option<Self> {
        Some(match identifier {
            "minecraft:air" => Self::AIR,
            "minecraft:stone" => Self::STONE,
            "minecraft:granite" => Self::GRANITE,
            "minecraft:diorite" => Self::DIORITE,
            "minecraft:andesite" => Self::ANDESITE,
            "minecraft:grass_block" => Self::GRASS_BLOCK,
            "minecraft:dirt" => Self::DIRT,
            "minecraft:cobblestone" => Self::COBBLESTONE,
            "minecraft:oak_planks" => Self::OAK_PLANKS,
            "minecraft:spruce_planks" => Self::SPRUCE_PLANKS,
            "minecraft:birch_planks" => Self::BIRCH_PLANKS,
            "minecraft:jungle_planks" => Self::JUNGLE_PLANKS,
            "minecraft:acacia_planks" => Self::ACACIA_PLANKS,
            "minecraft:cherry_planks" => Self::CHERRY_PLANKS,
            "minecraft:dark_oak_planks" => Self::DARK_OAK_PLANKS,
            "minecraft:pale_oak_planks" => Self::PALE_OAK_PLANKS,
            "minecraft:sand" => Self::SAND,
            "minecraft:gravel" => Self::GRAVEL,
            "minecraft:glass" => Self::GLASS,
            "minecraft:bricks" => Self::BRICKS,
            "minecraft:stone_bricks" => Self::STONE_BRICKS,
            "minecraft:obsidian" => Self::OBSIDIAN,
            "minecraft:netherrack" => Self::NETHERRACK,
            "minecraft:end_stone" => Self::END_STONE,
            "minecraft:white_wool" => Self::WHITE_WOOL,
            "minecraft:red_wool" => Self::RED_WOOL,
            "minecraft:blue_wool" => Self::BLUE_WOOL,
            "minecraft:gold_block" => Self::GOLD_BLOCK,
            "minecraft:iron_block" => Self::IRON_BLOCK,
            "minecraft:diamond_block" => Self::DIAMOND_BLOCK,
            "minecraft:emerald_block" => Self::EMERALD_BLOCK,
            _ => return None,
        })
    }
}
