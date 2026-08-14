use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::OnceLock;

use flate2::read::GzDecoder;
use serde::Deserialize;

use crate::{
    BiomeId, BlockId, BlockStateId, ItemId, MINECRAFT_VERSION, NamespacedId, PROTOCOL_VERSION,
    RegistryError,
};

const MAX_REGISTRY_BYTES: u64 = 16 * 1024 * 1024;
static VANILLA_REGISTRIES: OnceLock<Result<Registries, String>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollisionCategory {
    Empty,
    FullCube,
    Complex,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PropertyKind {
    Boolean,
    Integer { min: i32, max: i32 },
    Enum { values: Box<[Box<str>]> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockProperty {
    name: Box<str>,
    kind: PropertyKind,
}

impl BlockProperty {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn kind(&self) -> &PropertyKind {
        &self.kind
    }

    #[must_use]
    pub fn values(&self) -> Vec<String> {
        match &self.kind {
            PropertyKind::Boolean => vec!["true".to_owned(), "false".to_owned()],
            PropertyKind::Integer { min, max } => {
                (*min..=*max).map(|value| value.to_string()).collect()
            }
            PropertyKind::Enum { values } => values.iter().map(ToString::to_string).collect(),
        }
    }

    fn value_index(&self, value: &str) -> Option<u16> {
        match &self.kind {
            PropertyKind::Boolean => match value {
                "true" => Some(0),
                "false" => Some(1),
                _ => None,
            },
            PropertyKind::Integer { min, max } => value
                .parse::<i32>()
                .ok()
                .filter(|parsed| (*min..=*max).contains(parsed))
                .and_then(|parsed| u16::try_from(parsed - min).ok()),
            PropertyKind::Enum { values } => values
                .iter()
                .position(|candidate| candidate.as_ref() == value)
                .and_then(|index| u16::try_from(index).ok()),
        }
    }

    fn value_name(&self, index: u16) -> Option<&str> {
        match &self.kind {
            PropertyKind::Boolean => ["true", "false"].get(usize::from(index)).copied(),
            PropertyKind::Integer { min, max } => {
                let value = min + i32::from(index);
                (*min..=*max).contains(&value).then_some("")
            }
            PropertyKind::Enum { values } => values.get(usize::from(index)).map(AsRef::as_ref),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BlockDefinition {
    id: BlockId,
    name: NamespacedId,
    default_state: BlockStateId,
    states: Box<[BlockStateId]>,
    properties: Box<[BlockProperty]>,
    state_lookup: HashMap<Box<[u16]>, BlockStateId>,
    item: Option<ItemId>,
    collision: CollisionCategory,
    replaceable: bool,
    hardness: Option<f32>,
}

impl BlockDefinition {
    #[must_use]
    pub const fn id(&self) -> BlockId {
        self.id
    }

    #[must_use]
    pub const fn name(&self) -> &NamespacedId {
        &self.name
    }

    #[must_use]
    pub const fn default_state(&self) -> BlockStateId {
        self.default_state
    }

    #[must_use]
    pub const fn states(&self) -> &[BlockStateId] {
        &self.states
    }

    #[must_use]
    pub const fn properties(&self) -> &[BlockProperty] {
        &self.properties
    }

    #[must_use]
    pub const fn item(&self) -> Option<ItemId> {
        self.item
    }

    #[must_use]
    pub const fn collision(&self) -> CollisionCategory {
        self.collision
    }

    #[must_use]
    pub const fn replaceable(&self) -> bool {
        self.replaceable
    }

    #[must_use]
    pub const fn hardness(&self) -> Option<f32> {
        self.hardness
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockState {
    id: BlockStateId,
    block: BlockId,
    values: Box<[u16]>,
}

impl BlockState {
    #[must_use]
    pub const fn id(&self) -> BlockStateId {
        self.id
    }

    #[must_use]
    pub const fn block(&self) -> BlockId {
        self.block
    }
}

#[derive(Clone, Debug)]
pub struct ItemDefinition {
    id: ItemId,
    name: NamespacedId,
    max_stack_size: u8,
    block: Option<BlockId>,
}

impl ItemDefinition {
    #[must_use]
    pub const fn id(&self) -> ItemId {
        self.id
    }

    #[must_use]
    pub const fn name(&self) -> &NamespacedId {
        &self.name
    }

    #[must_use]
    pub const fn max_stack_size(&self) -> u8 {
        self.max_stack_size
    }

    #[must_use]
    pub const fn block(&self) -> Option<BlockId> {
        self.block
    }
}

#[derive(Clone, Debug)]
pub struct BiomeDefinition {
    id: BiomeId,
    name: NamespacedId,
}

impl BiomeDefinition {
    #[must_use]
    pub const fn id(&self) -> BiomeId {
        self.id
    }

    #[must_use]
    pub const fn name(&self) -> &NamespacedId {
        &self.name
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegistryCounts {
    pub blocks: usize,
    pub block_states: usize,
    pub items: usize,
    pub biomes: usize,
}

#[derive(Debug)]
pub struct Registries {
    blocks: Vec<BlockDefinition>,
    block_states: Vec<BlockState>,
    items: Vec<ItemDefinition>,
    biomes: Vec<BiomeDefinition>,
    blocks_by_name: HashMap<Box<str>, BlockId>,
    items_by_name: HashMap<Box<str>, ItemId>,
    biomes_by_name: HashMap<Box<str>, BiomeId>,
}

impl Registries {
    #[must_use]
    pub fn counts(&self) -> RegistryCounts {
        RegistryCounts {
            blocks: self.blocks.len(),
            block_states: self.block_states.len(),
            items: self.items.len(),
            biomes: self.biomes.len(),
        }
    }

    pub fn block(&self, id: BlockId) -> Result<&BlockDefinition, RegistryError> {
        self.blocks
            .get(usize::from(id.raw()))
            .ok_or(RegistryError::UnknownBlockId(id.raw()))
    }

    pub fn block_by_name(&self, name: &str) -> Result<&BlockDefinition, RegistryError> {
        let id = self
            .blocks_by_name
            .get(name)
            .copied()
            .ok_or_else(|| RegistryError::UnknownBlock(name.to_owned()))?;
        self.block(id)
    }

    pub fn state(&self, id: BlockStateId) -> Result<&BlockState, RegistryError> {
        self.block_states
            .get(usize::from(id.raw()))
            .ok_or(RegistryError::UnknownBlockStateId(id.raw()))
    }

    pub fn item(&self, id: ItemId) -> Result<&ItemDefinition, RegistryError> {
        self.items
            .get(usize::from(id.raw()))
            .ok_or(RegistryError::UnknownItemId(id.raw()))
    }

    pub fn item_by_protocol_id(&self, id: i32) -> Result<&ItemDefinition, RegistryError> {
        let id = u16::try_from(id).map_err(|_| RegistryError::InvalidItemProtocolId(id))?;
        self.item(ItemId::from_generated(id))
    }

    #[must_use]
    pub fn blocks(&self) -> &[BlockDefinition] {
        &self.blocks
    }

    #[must_use]
    pub fn block_states(&self) -> &[BlockState] {
        &self.block_states
    }

    #[must_use]
    pub fn items(&self) -> &[ItemDefinition] {
        &self.items
    }

    #[must_use]
    pub fn biomes(&self) -> &[BiomeDefinition] {
        &self.biomes
    }

    pub fn item_by_name(&self, name: &str) -> Result<&ItemDefinition, RegistryError> {
        let id = self
            .items_by_name
            .get(name)
            .copied()
            .ok_or_else(|| RegistryError::UnknownItem(name.to_owned()))?;
        self.item(id)
    }

    pub fn biome(&self, id: BiomeId) -> Result<&BiomeDefinition, RegistryError> {
        self.biomes
            .get(usize::from(id.raw()))
            .ok_or(RegistryError::UnknownBiomeId(id.raw()))
    }

    pub fn biome_by_name(&self, name: &str) -> Result<&BiomeDefinition, RegistryError> {
        let id = self
            .biomes_by_name
            .get(name)
            .copied()
            .ok_or_else(|| RegistryError::UnknownBiome(name.to_owned()))?;
        self.biome(id)
    }

    pub fn resolve_state<'a>(
        &self,
        block_name: &str,
        properties: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Result<BlockStateId, RegistryError> {
        let block = self.block_by_name(block_name)?;
        let mut values = vec![None; block.properties.len()];
        for (name, value) in properties {
            let Some(index) = block
                .properties
                .iter()
                .position(|property| property.name() == name)
            else {
                return Err(RegistryError::UnknownProperty {
                    block: block_name.to_owned(),
                    property: name.to_owned(),
                });
            };
            let Some(value_index) = block.properties[index].value_index(value) else {
                return Err(RegistryError::InvalidPropertyValue {
                    block: block_name.to_owned(),
                    property: name.to_owned(),
                    value: value.to_owned(),
                });
            };
            if values[index].replace(value_index).is_some() {
                return Err(RegistryError::InvalidStateCombination(
                    block_name.to_owned(),
                ));
            }
        }
        let values = values
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                value.ok_or_else(|| RegistryError::MissingProperty {
                    block: block_name.to_owned(),
                    property: block.properties[index].name().to_owned(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        block
            .state_lookup
            .get(values.as_slice())
            .copied()
            .ok_or_else(|| RegistryError::InvalidStateCombination(block_name.to_owned()))
    }

    pub fn with_property(
        &self,
        state_id: BlockStateId,
        property_name: &str,
        value: &str,
    ) -> Result<BlockStateId, RegistryError> {
        let state = self.state(state_id)?;
        let block = self.block(state.block)?;
        let Some(index) = block
            .properties
            .iter()
            .position(|property| property.name() == property_name)
        else {
            return Err(RegistryError::UnknownProperty {
                block: block.name.to_string(),
                property: property_name.to_owned(),
            });
        };
        let Some(value_index) = block.properties[index].value_index(value) else {
            return Err(RegistryError::InvalidPropertyValue {
                block: block.name.to_string(),
                property: property_name.to_owned(),
                value: value.to_owned(),
            });
        };
        let mut values = state.values.to_vec();
        values[index] = value_index;
        block
            .state_lookup
            .get(values.as_slice())
            .copied()
            .ok_or_else(|| RegistryError::InvalidStateCombination(block.name.to_string()))
    }

    pub fn state_properties(
        &self,
        state_id: BlockStateId,
    ) -> Result<Vec<(&str, String)>, RegistryError> {
        let state = self.state(state_id)?;
        let block = self.block(state.block)?;
        block
            .properties
            .iter()
            .zip(state.values.iter().copied())
            .map(|(property, value)| {
                let rendered = match property.kind() {
                    PropertyKind::Integer { min, max } => {
                        let value = min + i32::from(value);
                        if !(*min..=*max).contains(&value) {
                            return Err(RegistryError::InvalidGeneratedData(format!(
                                "invalid property index in state {}",
                                state_id.raw()
                            )));
                        }
                        value.to_string()
                    }
                    _ => property
                        .value_name(value)
                        .ok_or_else(|| {
                            RegistryError::InvalidGeneratedData(format!(
                                "invalid property index in state {}",
                                state_id.raw()
                            ))
                        })?
                        .to_owned(),
                };
                Ok((property.name(), rendered))
            })
            .collect()
    }
}

pub fn vanilla_registries() -> Result<&'static Registries, RegistryError> {
    VANILLA_REGISTRIES
        .get_or_init(load_registries)
        .as_ref()
        .map_err(|error| RegistryError::InvalidGeneratedData(error.clone()))
}

fn load_registries() -> Result<Registries, String> {
    let compressed = include_bytes!("../data/26.1.2/vanilla.json.gz");
    let mut json = Vec::new();
    GzDecoder::new(compressed.as_slice())
        .take(MAX_REGISTRY_BYTES + 1)
        .read_to_end(&mut json)
        .map_err(|error| format!("gzip decode failed: {error}"))?;
    if json.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(format!(
            "decoded registry is {} bytes; limit is {MAX_REGISTRY_BYTES}",
            json.len()
        ));
    }
    let generated: GeneratedRegistries =
        serde_json::from_slice(&json).map_err(|error| format!("JSON decode failed: {error}"))?;
    Registries::from_generated(generated).map_err(|error| error.to_string())
}

impl Registries {
    fn from_generated(generated: GeneratedRegistries) -> Result<Self, RegistryError> {
        if generated.minecraft_version != MINECRAFT_VERSION
            || generated.protocol_version != PROTOCOL_VERSION
        {
            return Err(invalid("registry version does not match this server build"));
        }
        if generated.blocks.len() > usize::from(u16::MAX)
            || generated.items.len() > usize::from(u16::MAX)
            || generated.biomes.len() > usize::from(u16::MAX)
        {
            return Err(invalid("registry contains too many entries"));
        }

        let mut blocks = Vec::with_capacity(generated.blocks.len());
        let mut blocks_by_name = HashMap::with_capacity(generated.blocks.len());
        let state_count = generated
            .blocks
            .iter()
            .map(|block| block.states.len())
            .sum::<usize>();
        let mut states = vec![None; state_count];
        for (expected_id, generated_block) in generated.blocks.into_iter().enumerate() {
            let raw_id = u16::try_from(expected_id).map_err(|_| invalid("block ID overflow"))?;
            if generated_block.id != raw_id {
                return Err(invalid(format!("missing or duplicate block ID {raw_id}")));
            }
            let id = BlockId::from_generated(raw_id);
            let name = NamespacedId::parse(&generated_block.name)?;
            if blocks_by_name.insert(name.as_str().into(), id).is_some() {
                return Err(invalid(format!("duplicate block name {name}")));
            }
            let properties = generated_block
                .properties
                .into_iter()
                .map(build_property)
                .collect::<Result<Vec<_>, _>>()?;
            let mut property_names = HashSet::with_capacity(properties.len());
            for property in &properties {
                if !property_names.insert(property.name.as_ref()) {
                    return Err(invalid(format!(
                        "duplicate property {} on block {name}",
                        property.name
                    )));
                }
            }
            let expected_combinations =
                properties.iter().try_fold(1_usize, |total, property| {
                    total
                        .checked_mul(property_value_count(property))
                        .ok_or_else(|| invalid(format!("state combination overflow for {name}")))
                })?;
            if generated_block.states.is_empty()
                || generated_block.states.len() != expected_combinations
            {
                return Err(invalid(format!(
                    "block {name} has incomplete state combinations"
                )));
            }
            let mut block_states = Vec::with_capacity(generated_block.states.len());
            let mut state_lookup = HashMap::with_capacity(generated_block.states.len());
            let mut default_found = false;
            for generated_state in generated_block.states {
                let state_id = usize::try_from(generated_state.id)
                    .map_err(|_| invalid("negative block-state ID"))?;
                let raw_state_id = u16::try_from(state_id)
                    .map_err(|_| invalid(format!("block-state ID {state_id} exceeds u16")))?;
                if state_id >= states.len() || states[state_id].is_some() {
                    return Err(invalid(format!(
                        "missing, duplicate, or out-of-range block-state ID {state_id}"
                    )));
                }
                if generated_state.values.len() != properties.len() {
                    return Err(invalid(format!(
                        "state {state_id} has the wrong property count"
                    )));
                }
                let values = generated_state
                    .values
                    .iter()
                    .zip(&properties)
                    .map(|(value, property)| {
                        property.value_index(value).ok_or_else(|| {
                            invalid(format!(
                                "state {state_id} has invalid value {value} for {}",
                                property.name
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice();
                let state_id = BlockStateId::from_generated(raw_state_id);
                if state_lookup.insert(values.clone(), state_id).is_some() {
                    return Err(invalid(format!("duplicate state combination for {name}")));
                }
                states[usize::from(raw_state_id)] = Some(BlockState {
                    id: state_id,
                    block: id,
                    values,
                });
                block_states.push(state_id);
                default_found |= generated_block.default_state == raw_state_id;
            }
            if !default_found {
                return Err(invalid(format!("invalid default state for {name}")));
            }
            blocks.push(BlockDefinition {
                id,
                name,
                default_state: BlockStateId::from_generated(generated_block.default_state),
                states: block_states.into_boxed_slice(),
                properties: properties.into_boxed_slice(),
                state_lookup,
                item: None,
                collision: generated_block.collision.into(),
                replaceable: generated_block.replaceable,
                hardness: None,
            });
        }
        let block_states = states
            .into_iter()
            .enumerate()
            .map(|(id, state)| state.ok_or_else(|| invalid(format!("missing block-state ID {id}"))))
            .collect::<Result<Vec<_>, _>>()?;

        let mut items = Vec::with_capacity(generated.items.len());
        let mut items_by_name = HashMap::with_capacity(generated.items.len());
        for (expected_id, generated_item) in generated.items.into_iter().enumerate() {
            let raw_id = u16::try_from(expected_id).map_err(|_| invalid("item ID overflow"))?;
            if generated_item.id != raw_id {
                return Err(invalid(format!("missing or duplicate item ID {raw_id}")));
            }
            let id = ItemId::from_generated(raw_id);
            let name = NamespacedId::parse(&generated_item.name)?;
            if items_by_name.insert(name.as_str().into(), id).is_some() {
                return Err(invalid(format!("duplicate item name {name}")));
            }
            let block = generated_item
                .block
                .map(|raw| {
                    let block = blocks
                        .get(usize::from(raw))
                        .ok_or_else(|| invalid(format!("item {name} references block ID {raw}")))?;
                    if block.item.is_some() {
                        return Err(invalid(format!("block {} has multiple items", block.name)));
                    }
                    Ok(BlockId::from_generated(raw))
                })
                .transpose()?;
            if let Some(block_id) = block {
                blocks[usize::from(block_id.raw())].item = Some(id);
            }
            items.push(ItemDefinition {
                id,
                name,
                max_stack_size: generated_item.max_stack_size,
                block,
            });
        }

        let mut biomes = Vec::with_capacity(generated.biomes.len());
        let mut biomes_by_name = HashMap::with_capacity(generated.biomes.len());
        for (expected_id, generated_biome) in generated.biomes.into_iter().enumerate() {
            let raw_id = u16::try_from(expected_id).map_err(|_| invalid("biome ID overflow"))?;
            if generated_biome.id != raw_id {
                return Err(invalid(format!("missing or duplicate biome ID {raw_id}")));
            }
            let id = BiomeId::from_generated(raw_id);
            let name = NamespacedId::parse(&generated_biome.name)?;
            if biomes_by_name.insert(name.as_str().into(), id).is_some() {
                return Err(invalid(format!("duplicate biome name {name}")));
            }
            biomes.push(BiomeDefinition { id, name });
        }

        Ok(Self {
            blocks,
            block_states,
            items,
            biomes,
            blocks_by_name,
            items_by_name,
            biomes_by_name,
        })
    }
}

fn build_property(generated: GeneratedProperty) -> Result<BlockProperty, RegistryError> {
    if generated.values.is_empty() {
        return Err(invalid(format!(
            "property {} has no values",
            generated.name
        )));
    }
    let mut unique = HashSet::with_capacity(generated.values.len());
    if generated.values.iter().any(|value| !unique.insert(value)) {
        return Err(invalid(format!(
            "property {} has duplicate values",
            generated.name
        )));
    }
    let kind = if generated.values == ["true", "false"] {
        PropertyKind::Boolean
    } else if let Some(range) = integer_range(&generated.values) {
        PropertyKind::Integer {
            min: range.0,
            max: range.1,
        }
    } else {
        PropertyKind::Enum {
            values: generated
                .values
                .into_iter()
                .map(String::into_boxed_str)
                .collect(),
        }
    };
    Ok(BlockProperty {
        name: generated.name.into_boxed_str(),
        kind,
    })
}

fn integer_range(values: &[String]) -> Option<(i32, i32)> {
    let parsed = values
        .iter()
        .map(|value| value.parse::<i32>())
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let min = *parsed.iter().min()?;
    let max = *parsed.iter().max()?;
    (parsed.len() == usize::try_from(max - min + 1).ok()?
        && (min..=max).all(|value| parsed.contains(&value)))
    .then_some((min, max))
}

fn property_value_count(property: &BlockProperty) -> usize {
    match &property.kind {
        PropertyKind::Boolean => 2,
        PropertyKind::Integer { min, max } => usize::try_from(max - min + 1).unwrap_or(0),
        PropertyKind::Enum { values } => values.len(),
    }
}

fn invalid(message: impl Into<String>) -> RegistryError {
    RegistryError::InvalidGeneratedData(message.into())
}

#[derive(Deserialize)]
struct GeneratedRegistries {
    minecraft_version: String,
    protocol_version: i32,
    blocks: Vec<GeneratedBlock>,
    items: Vec<GeneratedItem>,
    biomes: Vec<GeneratedBiome>,
}

#[derive(Deserialize)]
struct GeneratedBlock {
    id: u16,
    name: String,
    default_state: u16,
    collision: GeneratedCollision,
    replaceable: bool,
    properties: Vec<GeneratedProperty>,
    states: Vec<GeneratedState>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum GeneratedCollision {
    Empty,
    FullCube,
    Complex,
}

impl From<GeneratedCollision> for CollisionCategory {
    fn from(value: GeneratedCollision) -> Self {
        match value {
            GeneratedCollision::Empty => Self::Empty,
            GeneratedCollision::FullCube => Self::FullCube,
            GeneratedCollision::Complex => Self::Complex,
        }
    }
}

#[derive(Deserialize)]
struct GeneratedProperty {
    name: String,
    values: Vec<String>,
}

#[derive(Deserialize)]
struct GeneratedState {
    id: i32,
    values: Vec<String>,
}

#[derive(Deserialize)]
struct GeneratedItem {
    id: u16,
    name: String,
    max_stack_size: u8,
    block: Option<u16>,
}

#[derive(Deserialize)]
struct GeneratedBiome {
    id: u16,
    name: String,
}

#[cfg(test)]
mod tests {
    use crate::{BlockStateId, PropertyKind, RegistryError, vanilla_registries};

    #[test]
    fn bundled_registry_has_expected_shape_and_core_entries() {
        let registries = vanilla_registries().expect("bundled registry should load");
        assert_eq!(registries.counts().blocks, 1168);
        assert_eq!(registries.counts().block_states, 29_873);
        assert_eq!(registries.counts().items, 1506);
        assert_eq!(registries.counts().biomes, 65);
        assert_eq!(
            registries
                .block_by_name("minecraft:stone")
                .expect("stone")
                .default_state(),
            BlockStateId::STONE
        );
        assert_eq!(
            registries
                .biome_by_name("minecraft:plains")
                .expect("plains")
                .id()
                .raw(),
            40
        );
    }

    #[test]
    fn log_properties_validate_and_round_trip() {
        let registries = vanilla_registries().expect("bundled registry should load");
        let log = registries
            .block_by_name("minecraft:oak_log")
            .expect("oak log");
        assert!(matches!(
            log.properties()[0].kind(),
            PropertyKind::Enum { .. }
        ));
        for (axis, expected) in [
            ("x", BlockStateId::OAK_LOG_X),
            ("y", BlockStateId::OAK_LOG),
            ("z", BlockStateId::OAK_LOG_Z),
        ] {
            let state = registries
                .resolve_state("minecraft:oak_log", [("axis", axis)])
                .expect("valid axis");
            assert_eq!(state, expected);
            assert_eq!(registries.state(state).expect("state").block(), log.id());
        }
    }

    #[test]
    fn boolean_and_integer_properties_are_typed_and_validated() {
        let registries = vanilla_registries().expect("bundled registry should load");
        let leaves = registries
            .block_by_name("minecraft:oak_leaves")
            .expect("oak leaves");
        let persistent = leaves
            .properties()
            .iter()
            .find(|property| property.name() == "persistent")
            .expect("persistent property");
        assert_eq!(persistent.kind(), &PropertyKind::Boolean);

        let water = registries.block_by_name("minecraft:water").expect("water");
        assert!(matches!(
            water.properties()[0].kind(),
            PropertyKind::Integer { min: 0, max: 15 }
        ));
        let source_water = registries
            .resolve_state("minecraft:water", [("level", "0")])
            .expect("source water");
        let level_seven = registries
            .with_property(source_water, "level", "7")
            .expect("valid water level");
        assert_eq!(
            registries.state_properties(level_seven),
            Ok(vec![("level", "7".to_owned())])
        );
    }

    #[test]
    fn invalid_properties_are_typed_errors() {
        let registries = vanilla_registries().expect("bundled registry should load");
        assert!(matches!(
            registries.resolve_state("minecraft:oak_log", [("direction", "x")]),
            Err(RegistryError::UnknownProperty { .. })
        ));
        assert!(matches!(
            registries.resolve_state("minecraft:oak_log", [("axis", "sideways")]),
            Err(RegistryError::InvalidPropertyValue { .. })
        ));
    }

    #[test]
    fn every_state_and_associated_item_is_internally_consistent() {
        let registries = vanilla_registries().expect("bundled registry should load");
        for raw in 0..registries.counts().block_states {
            let id = BlockStateId::from_generated(raw as u16);
            let state = registries.state(id).expect("contiguous state ID");
            let block = registries.block(state.block()).expect("state block");
            assert!(block.states().contains(&id));
        }
        for block in registries.blocks() {
            if let Some(item_id) = block.item() {
                assert_eq!(
                    registries.item(item_id).expect("item").block(),
                    Some(block.id())
                );
            }
        }
    }
}
