//! Vanilla world-folder metadata loading and future chunk-service boundary.

use std::path::{Path, PathBuf};

use thiserror::Error;
use toucan_nbt::{NamedTag, NbtError, NbtLimits, Tag, from_gzip};

/// Integer block coordinates used by world-domain state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BlockPosition {
    /// East/west coordinate.
    pub x: i32,
    /// Vertical coordinate.
    pub y: i32,
    /// North/south coordinate.
    pub z: i32,
}

/// Metadata read from a vanilla world's gzip-compressed `level.dat`.
#[derive(Clone, Debug, PartialEq)]
pub struct LevelMetadata {
    /// Vanilla data-version number recorded by the generating version.
    pub data_version: i32,
    /// User-visible level name.
    pub level_name: String,
    /// Default overworld spawn.
    pub spawn: BlockPosition,
    /// Complete original document, including fields Toucan does not interpret.
    pub document: NamedTag,
}

impl LevelMetadata {
    /// Loads `<world>/level.dat` with bounded gzip and NBT decoding.
    pub fn load(world: impl AsRef<Path>, limits: NbtLimits) -> Result<Self, WorldError> {
        let path = world.as_ref().join("level.dat");
        let bytes = std::fs::read(&path).map_err(|source| WorldError::Read {
            path: path.clone(),
            source,
        })?;
        Self::from_gzip(&bytes, limits)
    }

    /// Decodes already-read `level.dat` bytes.
    pub fn from_gzip(bytes: &[u8], limits: NbtLimits) -> Result<Self, WorldError> {
        let document = from_gzip(bytes, limits)?;
        Self::from_document(document)
    }

    fn from_document(document: NamedTag) -> Result<Self, WorldError> {
        let data = compound_field(&document.value, "Data")?;
        let data_version = int_field(data, "DataVersion")?;
        let level_name = string_field(data, "LevelName")?.to_owned();
        let spawn = BlockPosition {
            x: int_field(data, "SpawnX")?,
            y: int_field(data, "SpawnY")?,
            z: int_field(data, "SpawnZ")?,
        };
        Ok(Self {
            data_version,
            level_name,
            spawn,
            document,
        })
    }
}

/// World-folder validation failure.
#[derive(Debug, Error)]
pub enum WorldError {
    /// `level.dat` could not be read.
    #[error("failed to read world metadata at {path}: {source}")]
    Read {
        /// Attempted file path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The compressed NBT document was malformed or exceeded a limit.
    #[error(transparent)]
    Nbt(#[from] NbtError),
    /// Required vanilla metadata was absent.
    #[error("level.dat is missing required field Data.{0}")]
    MissingField(&'static str),
    /// Required metadata used another NBT type.
    #[error("level.dat field Data.{0} has the wrong NBT type")]
    InvalidFieldType(&'static str),
}

fn compound_field<'a>(
    tag: &'a Tag,
    field: &'static str,
) -> Result<&'a std::collections::BTreeMap<String, Tag>, WorldError> {
    let value = tag.get(field).ok_or(WorldError::MissingField(field))?;
    match value {
        Tag::Compound(value) => Ok(value),
        _ => Err(WorldError::InvalidFieldType(field)),
    }
}

fn int_field(
    data: &std::collections::BTreeMap<String, Tag>,
    field: &'static str,
) -> Result<i32, WorldError> {
    data.get(field)
        .ok_or(WorldError::MissingField(field))?
        .as_i32()
        .ok_or(WorldError::InvalidFieldType(field))
}

fn string_field<'a>(
    data: &'a std::collections::BTreeMap<String, Tag>,
    field: &'static str,
) -> Result<&'a str, WorldError> {
    data.get(field)
        .ok_or(WorldError::MissingField(field))?
        .as_str()
        .ok_or(WorldError::InvalidFieldType(field))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use toucan_nbt::{NamedTag, NbtLimits, Tag, to_bytes};

    use super::{BlockPosition, LevelMetadata};

    #[test]
    fn loads_required_metadata_and_retains_unknown_fields() {
        let mut data = BTreeMap::new();
        data.insert("DataVersion".into(), Tag::Int(4790));
        data.insert("LevelName".into(), Tag::String("Toucan Test".into()));
        data.insert("SpawnX".into(), Tag::Int(10));
        data.insert("SpawnY".into(), Tag::Int(64));
        data.insert("SpawnZ".into(), Tag::Int(-5));
        data.insert("UnknownFutureField".into(), Tag::Long(42));
        let mut root = BTreeMap::new();
        root.insert("Data".into(), Tag::Compound(data));
        let document = NamedTag {
            name: String::new(),
            value: Tag::Compound(root),
        };
        let bytes = to_bytes(&document, NbtLimits::default()).unwrap_or_default();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        assert!(encoder.write_all(&bytes).is_ok());
        let compressed = encoder.finish().unwrap_or_default();

        let metadata = LevelMetadata::from_gzip(&compressed, NbtLimits::default())
            .unwrap_or_else(|error| panic!("level.dat should load: {error}"));
        assert_eq!(metadata.data_version, 4790);
        assert_eq!(metadata.level_name, "Toucan Test");
        assert_eq!(
            metadata.spawn,
            BlockPosition {
                x: 10,
                y: 64,
                z: -5,
            }
        );
        assert_eq!(
            metadata
                .document
                .value
                .get("Data")
                .and_then(|data| data.get("UnknownFutureField")),
            Some(&Tag::Long(42))
        );
    }
}
