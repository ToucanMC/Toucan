mod configuration;
mod vanilla;

use std::fmt;

pub use configuration::{ConfigurationPacket, configuration_packets};
use thiserror::Error;
pub use vanilla::{
    BiomeDefinition, BlockDefinition, BlockProperty, BlockState, CollisionCategory, ItemDefinition,
    PropertyKind, Registries, RegistryCounts, vanilla_registries,
};

pub const MINECRAFT_VERSION: &str = "26.1.2";
pub const PROTOCOL_VERSION: i32 = 775;

macro_rules! registry_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u16);

        impl $name {
            #[must_use]
            pub const fn raw(self) -> u16 {
                self.0
            }

            pub(crate) const fn from_generated(raw: u16) -> Self {
                Self(raw)
            }
        }
    };
}

registry_id!(BlockId);
registry_id!(BlockStateId);
registry_id!(ItemId);
registry_id!(BiomeId);

// Frequently used bootstrap states remain named here. All other states are resolved from data.
// Values are global block-state IDs in the bundled protocol-775 registry.
impl BlockStateId {
    pub const AIR: Self = Self(0);
    pub const STONE: Self = Self(1);
    pub const GRANITE: Self = Self(2);
    pub const DIORITE: Self = Self(4);
    pub const ANDESITE: Self = Self(6);
    pub const GRASS_BLOCK: Self = Self(9);
    pub const DIRT: Self = Self(10);
    pub const OAK_PLANKS: Self = Self(15);
    pub const OAK_LOG_X: Self = Self(136);
    pub const OAK_LOG: Self = Self(137);
    pub const OAK_LOG_Z: Self = Self(138);
    pub const DEEPSLATE: Self = Self(27924);
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NamespacedId(Box<str>);

impl NamespacedId {
    pub fn parse(value: &str) -> Result<Self, RegistryError> {
        let Some((namespace, path)) = value.split_once(':') else {
            return Err(RegistryError::InvalidNamespacedId(value.to_owned()));
        };
        let valid_namespace = !namespace.is_empty()
            && namespace.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.-".contains(&byte)
            });
        let valid_path = !path.is_empty()
            && path.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_./-".contains(&byte)
            });
        if !valid_namespace || !valid_path {
            return Err(RegistryError::InvalidNamespacedId(value.to_owned()));
        }
        Ok(Self(value.into()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NamespacedId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RegistryKey {
    Blocks,
    Items,
    Biomes,
}

impl RegistryKey {
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::Blocks => "minecraft:block",
            Self::Items => "minecraft:item",
            Self::Biomes => "minecraft:worldgen/biome",
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RegistryError {
    #[error("invalid embedded 26.1.2 configuration fixture: {0}")]
    InvalidConfigurationFixture(String),
    #[error("invalid bundled 26.1.2 registry data: {0}")]
    InvalidGeneratedData(String),
    #[error("invalid namespaced identifier `{0}`")]
    InvalidNamespacedId(String),
    #[error("unknown block `{0}`")]
    UnknownBlock(String),
    #[error("unknown block registry ID {0}")]
    UnknownBlockId(u16),
    #[error("unknown item registry ID {0}")]
    UnknownItemId(u16),
    #[error("invalid item protocol ID {0}")]
    InvalidItemProtocolId(i32),
    #[error("unknown item `{0}`")]
    UnknownItem(String),
    #[error("unknown biome registry ID {0}")]
    UnknownBiomeId(u16),
    #[error("unknown biome `{0}`")]
    UnknownBiome(String),
    #[error("unknown block-state ID {0}")]
    UnknownBlockStateId(u16),
    #[error("block `{block}` has no property `{property}`")]
    UnknownProperty { block: String, property: String },
    #[error("property `{property}` on block `{block}` does not accept `{value}`")]
    InvalidPropertyValue {
        block: String,
        property: String,
        value: String,
    },
    #[error("block `{block}` is missing property `{property}`")]
    MissingProperty { block: String, property: String },
    #[error("properties do not form a valid state of block `{0}`")]
    InvalidStateCombination(String),
}
