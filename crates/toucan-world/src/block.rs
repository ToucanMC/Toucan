#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BlockStateId(u16);

impl BlockStateId {
    pub const AIR: Self = Self(0);
    pub const STONE: Self = Self(1);
    pub const GRANITE: Self = Self(2);
    pub const DIORITE: Self = Self(3);
    pub const ANDESITE: Self = Self(4);
    pub const GRASS_BLOCK: Self = Self(5);
    pub const DIRT: Self = Self(6);
    pub const COBBLESTONE: Self = Self(7);
    pub const OAK_PLANKS: Self = Self(8);
    pub const SPRUCE_PLANKS: Self = Self(9);
    pub const BIRCH_PLANKS: Self = Self(10);
    pub const JUNGLE_PLANKS: Self = Self(11);
    pub const ACACIA_PLANKS: Self = Self(12);
    pub const CHERRY_PLANKS: Self = Self(13);
    pub const DARK_OAK_PLANKS: Self = Self(14);
    pub const PALE_OAK_PLANKS: Self = Self(15);
    pub const SAND: Self = Self(16);
    pub const GRAVEL: Self = Self(17);
    pub const GLASS: Self = Self(18);
    pub const BRICKS: Self = Self(19);
    pub const STONE_BRICKS: Self = Self(20);
    pub const OBSIDIAN: Self = Self(21);
    pub const NETHERRACK: Self = Self(22);
    pub const END_STONE: Self = Self(23);
    pub const WHITE_WOOL: Self = Self(24);
    pub const RED_WOOL: Self = Self(25);
    pub const BLUE_WOOL: Self = Self(26);
    pub const GOLD_BLOCK: Self = Self(27);
    pub const IRON_BLOCK: Self = Self(28);
    pub const DIAMOND_BLOCK: Self = Self(29);
    pub const EMERALD_BLOCK: Self = Self(30);
    pub const DEEPSLATE: Self = Self(31);
    pub const OAK_LOG: Self = Self(32);
    pub const SPRUCE_LOG: Self = Self(33);
    pub const COAL_BLOCK: Self = Self(34);
    pub const COPPER_BLOCK: Self = Self(35);
    pub const LAPIS_BLOCK: Self = Self(36);
    pub const REDSTONE_BLOCK: Self = Self(37);
    pub const NETHER_BRICKS: Self = Self(38);
    pub const QUARTZ_BLOCK: Self = Self(39);
    pub const TERRACOTTA: Self = Self(40);
    pub const WHITE_CONCRETE: Self = Self(41);
    pub const ORANGE_WOOL: Self = Self(42);
    pub const MAGENTA_WOOL: Self = Self(43);
    pub const LIGHT_BLUE_WOOL: Self = Self(44);
    pub const YELLOW_WOOL: Self = Self(45);
    pub const LIME_WOOL: Self = Self(46);
    pub const PINK_WOOL: Self = Self(47);
    pub const GRAY_WOOL: Self = Self(48);
    pub const LIGHT_GRAY_WOOL: Self = Self(49);
    pub const CYAN_WOOL: Self = Self(50);
    pub const PURPLE_WOOL: Self = Self(51);
    pub const BROWN_WOOL: Self = Self(52);
    pub const GREEN_WOOL: Self = Self(53);
    pub const BLACK_WOOL: Self = Self(54);
    pub const ORANGE_CONCRETE: Self = Self(55);
    pub const MAGENTA_CONCRETE: Self = Self(56);
    pub const LIGHT_BLUE_CONCRETE: Self = Self(57);
    pub const YELLOW_CONCRETE: Self = Self(58);
    pub const LIME_CONCRETE: Self = Self(59);
    pub const PINK_CONCRETE: Self = Self(60);
    pub const GRAY_CONCRETE: Self = Self(61);
    pub const LIGHT_GRAY_CONCRETE: Self = Self(62);
    pub const CYAN_CONCRETE: Self = Self(63);
    pub const PURPLE_CONCRETE: Self = Self(64);
    pub const BLUE_CONCRETE: Self = Self(65);
    pub const BROWN_CONCRETE: Self = Self(66);
    pub const GREEN_CONCRETE: Self = Self(67);
    pub const RED_CONCRETE: Self = Self(68);
    pub const BLACK_CONCRETE: Self = Self(69);
    pub const OAK_LOG_X: Self = Self(70);
    pub const OAK_LOG_Z: Self = Self(71);
    pub const SPRUCE_LOG_X: Self = Self(72);
    pub const SPRUCE_LOG_Z: Self = Self(73);
    pub const COAL_ORE: Self = Self(74);
    pub const IRON_ORE: Self = Self(75);
    pub const COPPER_ORE: Self = Self(76);
    pub const GOLD_ORE: Self = Self(77);
    pub const LAPIS_ORE: Self = Self(78);
    pub const DIAMOND_ORE: Self = Self(79);
    pub const EMERALD_ORE: Self = Self(80);
    pub const CLAY: Self = Self(81);
    pub const GLOWSTONE: Self = Self(82);
    pub const MAGMA_BLOCK: Self = Self(83);

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
            31 => "minecraft:deepslate",
            32 => "minecraft:oak_log",
            33 => "minecraft:spruce_log",
            34 => "minecraft:coal_block",
            35 => "minecraft:copper_block",
            36 => "minecraft:lapis_block",
            37 => "minecraft:redstone_block",
            38 => "minecraft:nether_bricks",
            39 => "minecraft:quartz_block",
            40 => "minecraft:terracotta",
            41 => "minecraft:white_concrete",
            42 => "minecraft:orange_wool",
            43 => "minecraft:magenta_wool",
            44 => "minecraft:light_blue_wool",
            45 => "minecraft:yellow_wool",
            46 => "minecraft:lime_wool",
            47 => "minecraft:pink_wool",
            48 => "minecraft:gray_wool",
            49 => "minecraft:light_gray_wool",
            50 => "minecraft:cyan_wool",
            51 => "minecraft:purple_wool",
            52 => "minecraft:brown_wool",
            53 => "minecraft:green_wool",
            54 => "minecraft:black_wool",
            55 => "minecraft:orange_concrete",
            56 => "minecraft:magenta_concrete",
            57 => "minecraft:light_blue_concrete",
            58 => "minecraft:yellow_concrete",
            59 => "minecraft:lime_concrete",
            60 => "minecraft:pink_concrete",
            61 => "minecraft:gray_concrete",
            62 => "minecraft:light_gray_concrete",
            63 => "minecraft:cyan_concrete",
            64 => "minecraft:purple_concrete",
            65 => "minecraft:blue_concrete",
            66 => "minecraft:brown_concrete",
            67 => "minecraft:green_concrete",
            68 => "minecraft:red_concrete",
            69 => "minecraft:black_concrete",
            70 | 71 => "minecraft:oak_log",
            72 | 73 => "minecraft:spruce_log",
            74 => "minecraft:coal_ore",
            75 => "minecraft:iron_ore",
            76 => "minecraft:copper_ore",
            77 => "minecraft:gold_ore",
            78 => "minecraft:lapis_ore",
            79 => "minecraft:diamond_ore",
            80 => "minecraft:emerald_ore",
            81 => "minecraft:clay",
            82 => "minecraft:glowstone",
            83 => "minecraft:magma_block",
            _ => "toucan:unknown_block_state",
        }
    }

    #[must_use]
    pub const fn raw(self) -> u16 {
        self.0
    }

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
            "minecraft:deepslate" => Self::DEEPSLATE,
            "minecraft:oak_log" => Self::OAK_LOG,
            "minecraft:spruce_log" => Self::SPRUCE_LOG,
            "minecraft:coal_block" => Self::COAL_BLOCK,
            "minecraft:copper_block" => Self::COPPER_BLOCK,
            "minecraft:lapis_block" => Self::LAPIS_BLOCK,
            "minecraft:redstone_block" => Self::REDSTONE_BLOCK,
            "minecraft:nether_bricks" => Self::NETHER_BRICKS,
            "minecraft:quartz_block" => Self::QUARTZ_BLOCK,
            "minecraft:terracotta" => Self::TERRACOTTA,
            "minecraft:white_concrete" => Self::WHITE_CONCRETE,
            "minecraft:orange_wool" => Self::ORANGE_WOOL,
            "minecraft:magenta_wool" => Self::MAGENTA_WOOL,
            "minecraft:light_blue_wool" => Self::LIGHT_BLUE_WOOL,
            "minecraft:yellow_wool" => Self::YELLOW_WOOL,
            "minecraft:lime_wool" => Self::LIME_WOOL,
            "minecraft:pink_wool" => Self::PINK_WOOL,
            "minecraft:gray_wool" => Self::GRAY_WOOL,
            "minecraft:light_gray_wool" => Self::LIGHT_GRAY_WOOL,
            "minecraft:cyan_wool" => Self::CYAN_WOOL,
            "minecraft:purple_wool" => Self::PURPLE_WOOL,
            "minecraft:brown_wool" => Self::BROWN_WOOL,
            "minecraft:green_wool" => Self::GREEN_WOOL,
            "minecraft:black_wool" => Self::BLACK_WOOL,
            "minecraft:orange_concrete" => Self::ORANGE_CONCRETE,
            "minecraft:magenta_concrete" => Self::MAGENTA_CONCRETE,
            "minecraft:light_blue_concrete" => Self::LIGHT_BLUE_CONCRETE,
            "minecraft:yellow_concrete" => Self::YELLOW_CONCRETE,
            "minecraft:lime_concrete" => Self::LIME_CONCRETE,
            "minecraft:pink_concrete" => Self::PINK_CONCRETE,
            "minecraft:gray_concrete" => Self::GRAY_CONCRETE,
            "minecraft:light_gray_concrete" => Self::LIGHT_GRAY_CONCRETE,
            "minecraft:cyan_concrete" => Self::CYAN_CONCRETE,
            "minecraft:purple_concrete" => Self::PURPLE_CONCRETE,
            "minecraft:blue_concrete" => Self::BLUE_CONCRETE,
            "minecraft:brown_concrete" => Self::BROWN_CONCRETE,
            "minecraft:green_concrete" => Self::GREEN_CONCRETE,
            "minecraft:red_concrete" => Self::RED_CONCRETE,
            "minecraft:black_concrete" => Self::BLACK_CONCRETE,
            "minecraft:coal_ore" => Self::COAL_ORE,
            "minecraft:iron_ore" => Self::IRON_ORE,
            "minecraft:copper_ore" => Self::COPPER_ORE,
            "minecraft:gold_ore" => Self::GOLD_ORE,
            "minecraft:lapis_ore" => Self::LAPIS_ORE,
            "minecraft:diamond_ore" => Self::DIAMOND_ORE,
            "minecraft:emerald_ore" => Self::EMERALD_ORE,
            "minecraft:clay" => Self::CLAY,
            "minecraft:glowstone" => Self::GLOWSTONE,
            "minecraft:magma_block" => Self::MAGMA_BLOCK,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn axis(self) -> Option<&'static str> {
        match self {
            Self::OAK_LOG_X | Self::SPRUCE_LOG_X => Some("x"),
            Self::OAK_LOG | Self::SPRUCE_LOG => Some("y"),
            Self::OAK_LOG_Z | Self::SPRUCE_LOG_Z => Some("z"),
            _ => None,
        }
    }

    #[must_use]
    pub fn from_identifier_and_axis(identifier: &str, axis: Option<&str>) -> Option<Self> {
        match (identifier, axis) {
            ("minecraft:oak_log", Some("x")) => Some(Self::OAK_LOG_X),
            ("minecraft:oak_log", Some("z")) => Some(Self::OAK_LOG_Z),
            ("minecraft:spruce_log", Some("x")) => Some(Self::SPRUCE_LOG_X),
            ("minecraft:spruce_log", Some("z")) => Some(Self::SPRUCE_LOG_Z),
            (_, None | Some("y")) => Self::from_identifier(identifier),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BlockStateId;

    #[test]
    fn every_supported_handle_round_trips_through_its_identifier() {
        for raw in 0..=83 {
            let state = BlockStateId(raw);
            assert_eq!(
                BlockStateId::from_identifier_and_axis(state.identifier(), state.axis()),
                Some(state)
            );
        }
    }
}
