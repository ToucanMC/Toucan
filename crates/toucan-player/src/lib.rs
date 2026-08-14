use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;
use toucan_nbt::{NamedTag, NbtError, NbtLimits, Tag, from_gzip, to_gzip};
use toucan_registry::{ItemId, RegistryError, vanilla_registries};
use uuid::Uuid;

const DATA_VERSION_26_1_2: i32 = 4790;
const MAX_PLAYER_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_HORIZONTAL_POSITION: f64 = 30_000_000.0;
static TEMPORARY_FILE_ID: AtomicU64 = AtomicU64::new(1);

pub const INVENTORY_SLOT_COUNT: usize = 36;
pub const HOTBAR_SLOT_COUNT: usize = 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ItemStack {
    item: ItemId,
    count: u8,
}

impl ItemStack {
    pub fn new(item: ItemId, count: u8) -> Result<Self, InventoryError> {
        let definition = vanilla_registries()?.item(item)?;
        if count == 0 || count > definition.max_stack_size() {
            return Err(InventoryError::InvalidStackCount {
                count,
                maximum: definition.max_stack_size(),
            });
        }
        if definition.name().as_str() == "minecraft:air" {
            return Err(InventoryError::AirItem);
        }
        Ok(Self { item, count })
    }

    #[must_use]
    pub const fn item(self) -> ItemId {
        self.item
    }

    #[must_use]
    pub const fn count(self) -> u8 {
        self.count
    }

    pub fn set_count(&mut self, count: u8) -> Result<(), InventoryError> {
        *self = Self::new(self.item, count)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerInventory {
    slots: [Option<ItemStack>; INVENTORY_SLOT_COUNT],
}

impl Default for PlayerInventory {
    fn default() -> Self {
        Self {
            slots: [None; INVENTORY_SLOT_COUNT],
        }
    }
}

impl PlayerInventory {
    #[must_use]
    pub const fn slots(&self) -> &[Option<ItemStack>; INVENTORY_SLOT_COUNT] {
        &self.slots
    }

    pub fn slot(&self, index: usize) -> Result<Option<ItemStack>, InventoryError> {
        self.slots
            .get(index)
            .copied()
            .ok_or(InventoryError::InvalidSlot(index))
    }

    pub fn set_slot(
        &mut self,
        index: usize,
        stack: Option<ItemStack>,
    ) -> Result<(), InventoryError> {
        let slot = self
            .slots
            .get_mut(index)
            .ok_or(InventoryError::InvalidSlot(index))?;
        *slot = stack;
        Ok(())
    }

    pub fn add(&mut self, item: ItemId, mut count: u8) -> Result<u8, InventoryError> {
        let maximum = vanilla_registries()?.item(item)?.max_stack_size();
        for stack in self
            .slots
            .iter_mut()
            .flatten()
            .filter(|stack| stack.item == item)
        {
            let available = maximum.saturating_sub(stack.count);
            let added = available.min(count);
            stack.count += added;
            count -= added;
            if count == 0 {
                return Ok(0);
            }
        }
        for slot in self.slots.iter_mut().filter(|slot| slot.is_none()) {
            let added = maximum.min(count);
            *slot = Some(ItemStack::new(item, added)?);
            count -= added;
            if count == 0 {
                return Ok(0);
            }
        }
        Ok(count)
    }

    pub fn consume_one(&mut self, index: usize) -> Result<(), InventoryError> {
        let slot = self
            .slots
            .get_mut(index)
            .ok_or(InventoryError::InvalidSlot(index))?;
        let Some(stack) = slot else {
            return Ok(());
        };
        if stack.count == 1 {
            *slot = None;
        } else {
            stack.count -= 1;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum InventoryError {
    #[error("invalid player inventory slot {0}")]
    InvalidSlot(usize),
    #[error("item stack count {count} is outside 1..={maximum}")]
    InvalidStackCount { count: u8, maximum: u8 },
    #[error("minecraft:air cannot be stored as an item stack")]
    AirItem,
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerData {
    uuid: Uuid,
    position: [f64; 3],
    rotation: [f32; 2],
    game_mode: u8,
    selected_hotbar: u8,
    inventory: PlayerInventory,
    document: NamedTag,
}

impl PlayerData {
    pub fn new(
        uuid: Uuid,
        position: [f64; 3],
        rotation: [f32; 2],
        game_mode: u8,
        selected_hotbar: u8,
    ) -> Result<Self, PlayerDataError> {
        validate_state(position, rotation, game_mode, selected_hotbar)?;
        let mut abilities = BTreeMap::new();
        abilities.insert("flying".into(), Tag::Byte(0));
        abilities.insert("instabuild".into(), Tag::Byte(i8::from(game_mode == 1)));
        abilities.insert("invulnerable".into(), Tag::Byte(i8::from(game_mode == 1)));
        abilities.insert("mayBuild".into(), Tag::Byte(i8::from(game_mode <= 1)));
        abilities.insert("mayfly".into(), Tag::Byte(i8::from(game_mode == 1)));
        abilities.insert("flySpeed".into(), Tag::Float(0.05));
        abilities.insert("walkSpeed".into(), Tag::Float(0.1));

        let mut root = BTreeMap::new();
        root.insert("DataVersion".into(), Tag::Int(DATA_VERSION_26_1_2));
        root.insert("UUID".into(), Tag::IntArray(uuid_int_array(uuid).to_vec()));
        root.insert(
            "Dimension".into(),
            Tag::String("minecraft:overworld".into()),
        );
        root.insert("OnGround".into(), Tag::Byte(0));
        root.insert("Health".into(), Tag::Float(20.0));
        root.insert("foodLevel".into(), Tag::Int(20));
        root.insert("Air".into(), Tag::Short(300));
        root.insert("abilities".into(), Tag::Compound(abilities));
        root.insert(
            "Inventory".into(),
            Tag::List {
                element_type: 10,
                values: Vec::new(),
            },
        );
        root.insert(
            "EnderItems".into(),
            Tag::List {
                element_type: 10,
                values: Vec::new(),
            },
        );
        let document = NamedTag {
            name: String::new(),
            value: Tag::Compound(root),
        };
        let mut player = Self {
            uuid,
            position,
            rotation,
            game_mode,
            selected_hotbar,
            inventory: PlayerInventory::default(),
            document,
        };
        player.synchronize_document()?;
        Ok(player)
    }

    #[must_use]
    pub const fn uuid(&self) -> Uuid {
        self.uuid
    }

    #[must_use]
    pub const fn position(&self) -> [f64; 3] {
        self.position
    }

    #[must_use]
    pub const fn rotation(&self) -> [f32; 2] {
        self.rotation
    }

    #[must_use]
    pub const fn game_mode(&self) -> u8 {
        self.game_mode
    }

    #[must_use]
    pub const fn selected_hotbar(&self) -> u8 {
        self.selected_hotbar
    }

    #[must_use]
    pub const fn inventory(&self) -> &PlayerInventory {
        &self.inventory
    }

    pub fn set_inventory(&mut self, inventory: PlayerInventory) {
        self.inventory = inventory;
    }

    pub fn update_session(
        &mut self,
        position: [f64; 3],
        rotation: [f32; 2],
        game_mode: u8,
        selected_hotbar: u8,
    ) -> Result<(), PlayerDataError> {
        validate_state(position, rotation, game_mode, selected_hotbar)?;
        self.position = position;
        self.rotation = rotation;
        self.game_mode = game_mode;
        self.selected_hotbar = selected_hotbar;
        Ok(())
    }

    fn from_document(uuid: Uuid, document: NamedTag) -> Result<Self, PlayerDataError> {
        let root = compound(&document.value, "player root")?;
        let position = list_f64(root.get("Pos"), "Pos", 3)?;
        let rotation = list_f32(root.get("Rotation"), "Rotation", 2)?;
        let game_mode = integer(root.get("playerGameType"), "playerGameType")?;
        let selected_hotbar = integer(root.get("SelectedItemSlot"), "SelectedItemSlot")?;
        let inventory = decode_inventory(root.get("Inventory"))?;
        let game_mode = u8::try_from(game_mode).map_err(|_| PlayerDataError::InvalidField {
            field: "playerGameType",
        })?;
        let selected_hotbar =
            u8::try_from(selected_hotbar).map_err(|_| PlayerDataError::InvalidField {
                field: "SelectedItemSlot",
            })?;
        validate_state(position, rotation, game_mode, selected_hotbar)?;
        Ok(Self {
            uuid,
            position,
            rotation,
            game_mode,
            selected_hotbar,
            inventory,
            document,
        })
    }

    fn synchronize_document(&mut self) -> Result<(), PlayerDataError> {
        let Tag::Compound(root) = &mut self.document.value else {
            return Err(PlayerDataError::InvalidField {
                field: "player root",
            });
        };
        root.insert("DataVersion".into(), Tag::Int(DATA_VERSION_26_1_2));
        root.insert(
            "UUID".into(),
            Tag::IntArray(uuid_int_array(self.uuid).to_vec()),
        );
        root.insert(
            "Pos".into(),
            Tag::List {
                element_type: 6,
                values: self.position.into_iter().map(Tag::Double).collect(),
            },
        );
        root.insert(
            "Rotation".into(),
            Tag::List {
                element_type: 5,
                values: self.rotation.into_iter().map(Tag::Float).collect(),
            },
        );
        root.insert("playerGameType".into(), Tag::Int(i32::from(self.game_mode)));
        root.insert(
            "SelectedItemSlot".into(),
            Tag::Int(i32::from(self.selected_hotbar)),
        );
        root.insert("Inventory".into(), encode_inventory(&self.inventory)?);
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct PlayerStore {
    directory: PathBuf,
    limits: NbtLimits,
}

impl PlayerStore {
    #[must_use]
    pub fn new(directory: impl AsRef<Path>) -> Self {
        Self {
            directory: directory.as_ref().to_owned(),
            limits: NbtLimits {
                max_bytes: MAX_PLAYER_FILE_BYTES as usize,
                ..NbtLimits::default()
            },
        }
    }

    pub fn load(&self, uuid: Uuid) -> Result<Option<PlayerData>, PlayerDataError> {
        let path = self.path(uuid);
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(PlayerDataError::Io { path, source }),
        };
        if metadata.len() > MAX_PLAYER_FILE_BYTES {
            return Err(PlayerDataError::FileTooLarge {
                actual: metadata.len(),
                limit: MAX_PLAYER_FILE_BYTES,
            });
        }
        let bytes = fs::read(&path).map_err(|source| PlayerDataError::Io {
            path: path.clone(),
            source,
        })?;
        let document = from_gzip(&bytes, self.limits)?;
        PlayerData::from_document(uuid, document).map(Some)
    }

    pub fn save(&self, player: &PlayerData) -> Result<(), PlayerDataError> {
        fs::create_dir_all(&self.directory).map_err(|source| PlayerDataError::Io {
            path: self.directory.clone(),
            source,
        })?;
        let mut player = player.clone();
        player.synchronize_document()?;
        let bytes = to_gzip(&player.document, self.limits)?;
        let target = self.path(player.uuid);
        let temporary = self.directory.join(format!(
            ".{}.tmp.{}.{}",
            player.uuid,
            std::process::id(),
            TEMPORARY_FILE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| PlayerDataError::Io {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| PlayerDataError::Io {
                path: temporary.clone(),
                source,
            })?;
        drop(file);
        if let Err(source) = fs::rename(&temporary, &target) {
            let _ = fs::remove_file(&temporary);
            return Err(PlayerDataError::Io {
                path: target,
                source,
            });
        }
        let directory = fs::File::open(&self.directory).map_err(|source| PlayerDataError::Io {
            path: self.directory.clone(),
            source,
        })?;
        directory.sync_all().map_err(|source| PlayerDataError::Io {
            path: self.directory.clone(),
            source,
        })?;
        Ok(())
    }

    fn path(&self, uuid: Uuid) -> PathBuf {
        self.directory.join(format!("{uuid}.dat"))
    }
}

#[derive(Debug, Error)]
pub enum PlayerDataError {
    #[error("player data I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Nbt(#[from] NbtError),
    #[error(transparent)]
    Inventory(#[from] InventoryError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error("invalid player data field {field}")]
    InvalidField { field: &'static str },
    #[error("player data file is {actual} bytes; limit is {limit}")]
    FileTooLarge { actual: u64, limit: u64 },
}

fn decode_inventory(value: Option<&Tag>) -> Result<PlayerInventory, PlayerDataError> {
    let Some(Tag::List {
        element_type: 10,
        values,
    }) = value
    else {
        return Err(PlayerDataError::InvalidField { field: "Inventory" });
    };
    let registries = vanilla_registries()?;
    let mut inventory = PlayerInventory::default();
    for value in values {
        let entry = compound(value, "Inventory entry")?;
        let slot = match entry.get("Slot") {
            Some(Tag::Byte(slot)) if *slot >= 0 => *slot as usize,
            _ => {
                return Err(PlayerDataError::InvalidField {
                    field: "Inventory Slot",
                });
            }
        };
        if slot >= INVENTORY_SLOT_COUNT || inventory.slots[slot].is_some() {
            return Err(PlayerDataError::InvalidField {
                field: "Inventory Slot",
            });
        }
        let item_name = match entry.get("id") {
            Some(Tag::String(value)) => value,
            _ => {
                return Err(PlayerDataError::InvalidField {
                    field: "Inventory id",
                });
            }
        };
        let count = match entry.get("count") {
            Some(Tag::Int(value)) => u8::try_from(*value),
            Some(Tag::Byte(value)) => u8::try_from(*value),
            _ => {
                return Err(PlayerDataError::InvalidField {
                    field: "Inventory count",
                });
            }
        }
        .map_err(|_| PlayerDataError::InvalidField {
            field: "Inventory count",
        })?;
        if entry.get("components").is_some_and(
            |components| !matches!(components, Tag::Compound(values) if values.is_empty()),
        ) {
            return Err(PlayerDataError::InvalidField {
                field: "Inventory components",
            });
        }
        let item = registries.item_by_name(item_name)?.id();
        inventory.slots[slot] = Some(ItemStack::new(item, count)?);
    }
    Ok(inventory)
}

fn encode_inventory(inventory: &PlayerInventory) -> Result<Tag, PlayerDataError> {
    let registries = vanilla_registries()?;
    let mut values = Vec::new();
    for (slot, stack) in inventory.slots.iter().enumerate() {
        let Some(stack) = stack else {
            continue;
        };
        let mut entry = BTreeMap::new();
        entry.insert("Slot".into(), Tag::Byte(slot as i8));
        entry.insert(
            "id".into(),
            Tag::String(registries.item(stack.item)?.name().to_string()),
        );
        entry.insert("count".into(), Tag::Int(i32::from(stack.count)));
        values.push(Tag::Compound(entry));
    }
    Ok(Tag::List {
        element_type: 10,
        values,
    })
}

fn validate_state(
    position: [f64; 3],
    rotation: [f32; 2],
    game_mode: u8,
    selected_hotbar: u8,
) -> Result<(), PlayerDataError> {
    if !position.into_iter().all(f64::is_finite)
        || position[0].abs() > MAX_HORIZONTAL_POSITION
        || position[2].abs() > MAX_HORIZONTAL_POSITION
        || !(-20_000_000.0..=20_000_000.0).contains(&position[1])
    {
        return Err(PlayerDataError::InvalidField { field: "Pos" });
    }
    if !rotation.into_iter().all(f32::is_finite) {
        return Err(PlayerDataError::InvalidField { field: "Rotation" });
    }
    if game_mode > 3 {
        return Err(PlayerDataError::InvalidField {
            field: "playerGameType",
        });
    }
    if selected_hotbar > 8 {
        return Err(PlayerDataError::InvalidField {
            field: "SelectedItemSlot",
        });
    }
    Ok(())
}

fn compound<'a>(
    value: &'a Tag,
    field: &'static str,
) -> Result<&'a BTreeMap<String, Tag>, PlayerDataError> {
    match value {
        Tag::Compound(values) => Ok(values),
        _ => Err(PlayerDataError::InvalidField { field }),
    }
}

fn integer(value: Option<&Tag>, field: &'static str) -> Result<i32, PlayerDataError> {
    match value {
        Some(Tag::Int(value)) => Ok(*value),
        _ => Err(PlayerDataError::InvalidField { field }),
    }
}

fn list_f64(
    value: Option<&Tag>,
    field: &'static str,
    length: usize,
) -> Result<[f64; 3], PlayerDataError> {
    let Some(Tag::List {
        element_type: 6,
        values,
    }) = value
    else {
        return Err(PlayerDataError::InvalidField { field });
    };
    if values.len() != length {
        return Err(PlayerDataError::InvalidField { field });
    }
    let values = values
        .iter()
        .map(|value| match value {
            Tag::Double(value) => Ok(*value),
            _ => Err(PlayerDataError::InvalidField { field }),
        })
        .collect::<Result<Vec<_>, _>>()?;
    values
        .try_into()
        .map_err(|_| PlayerDataError::InvalidField { field })
}

fn list_f32(
    value: Option<&Tag>,
    field: &'static str,
    length: usize,
) -> Result<[f32; 2], PlayerDataError> {
    let Some(Tag::List {
        element_type: 5,
        values,
    }) = value
    else {
        return Err(PlayerDataError::InvalidField { field });
    };
    if values.len() != length {
        return Err(PlayerDataError::InvalidField { field });
    }
    let values = values
        .iter()
        .map(|value| match value {
            Tag::Float(value) => Ok(*value),
            _ => Err(PlayerDataError::InvalidField { field }),
        })
        .collect::<Result<Vec<_>, _>>()?;
    values
        .try_into()
        .map_err(|_| PlayerDataError::InvalidField { field })
}

fn uuid_int_array(uuid: Uuid) -> [i32; 4] {
    let bytes = uuid.as_bytes();
    [
        i32::from_be_bytes(bytes[0..4].try_into().expect("fixed UUID slice")),
        i32::from_be_bytes(bytes[4..8].try_into().expect("fixed UUID slice")),
        i32::from_be_bytes(bytes[8..12].try_into().expect("fixed UUID slice")),
        i32::from_be_bytes(bytes[12..16].try_into().expect("fixed UUID slice")),
    ]
}

#[cfg(test)]
mod tests {
    use super::{ItemStack, PlayerData, PlayerInventory, PlayerStore};
    use std::error::Error;
    use std::fs;
    use uuid::Uuid;

    #[test]
    fn round_trip_updates_fields_and_preserves_unknown_data() -> Result<(), Box<dyn Error>> {
        let directory =
            std::env::temp_dir().join(format!("toucan-player-test-{}", std::process::id()));
        if directory.exists() {
            fs::remove_dir_all(&directory)?;
        }
        let store = PlayerStore::new(&directory);
        let uuid = Uuid::parse_str("8cfe44ed-15ac-4f0f-ae3b-e050d3c74a68")?;
        let mut player = PlayerData::new(uuid, [0.5, 64.0, 0.5], [0.0, 0.0], 0, 0)?;
        let registries = toucan_registry::vanilla_registries()?;
        let stone = registries.item_by_name("minecraft:stone")?.id();
        let oak_planks = registries.item_by_name("minecraft:oak_planks")?.id();
        let mut inventory = PlayerInventory::default();
        inventory.set_slot(0, Some(ItemStack::new(stone, 17)?))?;
        inventory.set_slot(35, Some(ItemStack::new(oak_planks, 64)?))?;
        player.set_inventory(inventory.clone());
        if let toucan_nbt::Tag::Compound(root) = &mut player.document.value {
            root.insert("ToucanUnknownTest".into(), toucan_nbt::Tag::Long(42));
        }
        store.save(&player)?;

        let mut loaded = store.load(uuid)?.ok_or("missing saved player")?;
        loaded.update_session([48.5, 70.0, -2.25], [90.0, 12.5], 1, 7)?;
        store.save(&loaded)?;
        let reopened = store.load(uuid)?.ok_or("missing rewritten player")?;
        assert_eq!(reopened.position(), [48.5, 70.0, -2.25]);
        assert_eq!(reopened.rotation(), [90.0, 12.5]);
        assert_eq!(reopened.game_mode(), 1);
        assert_eq!(reopened.selected_hotbar(), 7);
        assert_eq!(reopened.inventory(), &inventory);
        assert_eq!(
            reopened.document.value.get("ToucanUnknownTest"),
            Some(&toucan_nbt::Tag::Long(42))
        );
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn inventory_stacks_merge_then_fill_empty_slots() -> Result<(), Box<dyn Error>> {
        let stone = toucan_registry::vanilla_registries()?
            .item_by_name("minecraft:stone")?
            .id();
        let mut inventory = PlayerInventory::default();
        inventory.set_slot(0, Some(ItemStack::new(stone, 63)?))?;
        assert_eq!(inventory.add(stone, 4)?, 0);
        assert_eq!(inventory.slot(0)?.map(ItemStack::count), Some(64));
        assert_eq!(inventory.slot(1)?.map(ItemStack::count), Some(3));
        inventory.consume_one(0)?;
        assert_eq!(inventory.slot(0)?.map(ItemStack::count), Some(63));
        Ok(())
    }
}
