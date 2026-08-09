//! Bounded, vanilla-shaped player data loading and crash-safe persistence.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;
use toucan_nbt::{NamedTag, NbtError, NbtLimits, Tag, from_gzip, to_gzip};
use uuid::Uuid;

const DATA_VERSION_26_1_2: i32 = 4790;
const MAX_PLAYER_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_HORIZONTAL_POSITION: f64 = 30_000_000.0;
static TEMPORARY_FILE_ID: AtomicU64 = AtomicU64::new(1);

/// Authoritative player state retained between connections.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerData {
    uuid: Uuid,
    position: [f64; 3],
    rotation: [f32; 2],
    game_mode: u8,
    selected_hotbar: u8,
    document: NamedTag,
}

impl PlayerData {
    /// Creates a new vanilla-shaped player document at the world spawn.
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
            document,
        };
        player.synchronize_document()?;
        Ok(player)
    }

    /// Returns the player's stable UUID.
    #[must_use]
    pub const fn uuid(&self) -> Uuid {
        self.uuid
    }

    /// Returns the authoritative position.
    #[must_use]
    pub const fn position(&self) -> [f64; 3] {
        self.position
    }

    /// Returns yaw and pitch in degrees.
    #[must_use]
    pub const fn rotation(&self) -> [f32; 2] {
        self.rotation
    }

    /// Returns the vanilla game-mode ordinal.
    #[must_use]
    pub const fn game_mode(&self) -> u8 {
        self.game_mode
    }

    /// Returns the selected hotbar index in the range 0 through 8.
    #[must_use]
    pub const fn selected_hotbar(&self) -> u8 {
        self.selected_hotbar
    }

    /// Replaces the persisted session fields after validating their bounds.
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
        Ok(())
    }
}

/// Player-folder service with bounded parsing and atomic replacement writes.
#[derive(Clone, Debug)]
pub struct PlayerStore {
    directory: PathBuf,
    limits: NbtLimits,
}

impl PlayerStore {
    /// Creates a player store rooted at a world's `playerdata` directory.
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

    /// Loads one UUID file while retaining fields Toucan does not interpret.
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

    /// Atomically writes one player file and synchronizes its parent directory.
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

/// Player file parsing or persistence failure.
#[derive(Debug, Error)]
pub enum PlayerDataError {
    /// File-system access failed for a concrete path.
    #[error("player data I/O failed at {path}: {source}")]
    Io {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying operating-system error.
        #[source]
        source: std::io::Error,
    },
    /// A compressed or decoded NBT document was invalid.
    #[error(transparent)]
    Nbt(#[from] NbtError),
    /// One required field had the wrong type or an unsafe value.
    #[error("invalid player data field {field}")]
    InvalidField {
        /// Vanilla field name.
        field: &'static str,
    },
    /// The compressed file exceeded the player-specific bound.
    #[error("player data file is {actual} bytes; limit is {limit}")]
    FileTooLarge {
        /// Observed compressed size.
        actual: u64,
        /// Configured hard limit.
        limit: u64,
    },
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
    use super::{PlayerData, PlayerStore};
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
        assert_eq!(
            reopened.document.value.get("ToucanUnknownTest"),
            Some(&toucan_nbt::Tag::Long(42))
        );
        fs::remove_dir_all(directory)?;
        Ok(())
    }
}
