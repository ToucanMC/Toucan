//! Bounded, crash-resistant access to vanilla Anvil region files.

use std::array;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::read::{GzDecoder, ZlibDecoder};
use flate2::write::ZlibEncoder;
use thiserror::Error;
use toucan_nbt::{NamedTag, NbtError, NbtLimits, from_bytes, to_bytes};

const SECTOR_BYTES: usize = 4096;
const HEADER_BYTES: usize = SECTOR_BYTES * 2;
const REGION_ENTRIES: usize = 1024;
const MAX_SECTORS_PER_CHUNK: usize = u8::MAX as usize;

/// Chunk coordinate used to select one entry in an Anvil region.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RegionChunkPosition {
    /// East/west chunk coordinate.
    pub x: i32,
    /// North/south chunk coordinate.
    pub z: i32,
}

impl RegionChunkPosition {
    fn region(self) -> (i32, i32) {
        (self.x.div_euclid(32), self.z.div_euclid(32))
    }

    fn index(self) -> usize {
        (self.z.rem_euclid(32) as usize) * 32 + self.x.rem_euclid(32) as usize
    }
}

/// Region directory service with strict document and file-size bounds.
#[derive(Clone, Debug)]
pub struct RegionStore {
    directory: PathBuf,
    limits: NbtLimits,
    max_region_bytes: usize,
}

impl RegionStore {
    /// Creates a store rooted at an existing or future `region` directory.
    #[must_use]
    pub fn new(directory: impl AsRef<Path>, limits: NbtLimits, max_region_bytes: usize) -> Self {
        Self {
            directory: directory.as_ref().to_owned(),
            limits,
            max_region_bytes: max_region_bytes.max(HEADER_BYTES),
        }
    }

    /// Reads and decompresses one chunk document, or returns `None` when absent.
    pub fn read_chunk(
        &self,
        position: RegionChunkPosition,
    ) -> Result<Option<NamedTag>, RegionError> {
        let path = self.region_path(position);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(RegionError::Read { path, source }),
        };
        self.validate_region_size(&path, bytes.len())?;
        let Some(record) = record_at(&bytes, position.index(), &path)? else {
            return Ok(None);
        };
        let decoded = decode_record(record, self.limits.max_bytes)?;
        Ok(Some(from_bytes(&decoded, self.limits)?))
    }

    /// Atomically rewrites one region while preserving every unrelated record.
    pub fn write_chunk(
        &self,
        position: RegionChunkPosition,
        document: &NamedTag,
    ) -> Result<(), RegionError> {
        self.write_chunks(&[(position, document)])
    }

    /// Atomically rewrites several chunks in one region with one file commit.
    ///
    /// Every position must belong to the same region and may occur only once.
    pub fn write_chunks(
        &self,
        chunks: &[(RegionChunkPosition, &NamedTag)],
    ) -> Result<(), RegionError> {
        let Some((first, _)) = chunks.first() else {
            return Ok(());
        };
        let region = first.region();
        let mut changed = [false; REGION_ENTRIES];
        for (position, _) in chunks {
            if position.region() != region {
                return Err(RegionError::MixedRegions);
            }
            let index = position.index();
            if std::mem::replace(&mut changed[index], true) {
                return Err(RegionError::DuplicateChunk { index });
            }
        }
        fs::create_dir_all(&self.directory).map_err(|source| RegionError::CreateDirectory {
            path: self.directory.clone(),
            source,
        })?;
        let path = self.region_path(*first);
        let existing = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(source) => {
                return Err(RegionError::Read {
                    path: path.clone(),
                    source,
                });
            }
        };
        if !existing.is_empty() {
            self.validate_region_size(&path, existing.len())?;
        }
        let mut records: [Option<Vec<u8>>; REGION_ENTRIES] = array::from_fn(|_| None);
        let mut timestamps = [0_u32; REGION_ENTRIES];
        if !existing.is_empty() {
            if existing.len() < HEADER_BYTES {
                return Err(RegionError::InvalidHeader {
                    path,
                    reason: "file is shorter than the 8192-byte header",
                });
            }
            for index in 0..REGION_ENTRIES {
                records[index] = record_at(&existing, index, &path)?.map(ToOwned::to_owned);
                let timestamp_offset = SECTOR_BYTES + index * 4;
                timestamps[index] = u32::from_be_bytes(
                    existing[timestamp_offset..timestamp_offset + 4]
                        .try_into()
                        .map_err(|_| RegionError::InvalidHeader {
                            path: path.clone(),
                            reason: "timestamp table is truncated",
                        })?,
                );
            }
        }

        let timestamp = unix_timestamp();
        for (position, document) in chunks {
            records[position.index()] = Some(encode_record(document, self.limits)?);
            timestamps[position.index()] = timestamp;
        }
        let output = rebuild_region(&records, &timestamps, &path, self.max_region_bytes)?;
        let temporary = temporary_path(&path);
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| RegionError::Write {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(&output)
            .and_then(|()| file.sync_all())
            .map_err(|source| RegionError::Write {
                path: temporary.clone(),
                source,
            })?;
        drop(file);
        fs::rename(&temporary, &path).map_err(|source| RegionError::Write {
            path: path.clone(),
            source,
        })?;
        sync_directory(&self.directory)?;
        Ok(())
    }

    fn region_path(&self, position: RegionChunkPosition) -> PathBuf {
        let (region_x, region_z) = position.region();
        self.directory.join(format!("r.{region_x}.{region_z}.mca"))
    }

    fn validate_region_size(&self, path: &Path, actual: usize) -> Result<(), RegionError> {
        if actual > self.max_region_bytes {
            return Err(RegionError::RegionLimit {
                path: path.to_owned(),
                actual,
                limit: self.max_region_bytes,
            });
        }
        Ok(())
    }
}

fn record_at<'a>(
    region: &'a [u8],
    index: usize,
    path: &Path,
) -> Result<Option<&'a [u8]>, RegionError> {
    if region.len() < HEADER_BYTES {
        return Err(RegionError::InvalidHeader {
            path: path.to_owned(),
            reason: "file is shorter than the 8192-byte header",
        });
    }
    let header = index * 4;
    let sector = (usize::from(region[header]) << 16)
        | (usize::from(region[header + 1]) << 8)
        | usize::from(region[header + 2]);
    let sectors = usize::from(region[header + 3]);
    if sector == 0 && sectors == 0 {
        return Ok(None);
    }
    if sector < 2 || sectors == 0 {
        return Err(RegionError::InvalidLocation {
            path: path.to_owned(),
            index,
        });
    }
    let start = sector
        .checked_mul(SECTOR_BYTES)
        .ok_or_else(|| RegionError::InvalidLocation {
            path: path.to_owned(),
            index,
        })?;
    let bytes = sectors
        .checked_mul(SECTOR_BYTES)
        .and_then(|length| start.checked_add(length))
        .ok_or_else(|| RegionError::InvalidLocation {
            path: path.to_owned(),
            index,
        })?;
    if bytes > region.len() {
        return Err(RegionError::InvalidLocation {
            path: path.to_owned(),
            index,
        });
    }
    let length = u32::from_be_bytes(region[start..start + 4].try_into().map_err(|_| {
        RegionError::InvalidLocation {
            path: path.to_owned(),
            index,
        }
    })?) as usize;
    if length < 1 || length > sectors * SECTOR_BYTES - 4 {
        return Err(RegionError::InvalidChunkLength {
            path: path.to_owned(),
            index,
            length,
        });
    }
    Ok(Some(&region[start..start + 4 + length]))
}

fn decode_record(record: &[u8], max_bytes: usize) -> Result<Vec<u8>, RegionError> {
    let compression = record[4];
    let payload = &record[5..];
    let cap = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    let mut output = Vec::new();
    match compression {
        1 => {
            GzDecoder::new(payload)
                .take(cap.saturating_add(1))
                .read_to_end(&mut output)?;
        }
        2 => {
            ZlibDecoder::new(payload)
                .take(cap.saturating_add(1))
                .read_to_end(&mut output)?;
        }
        3 => output.extend_from_slice(payload),
        other => return Err(RegionError::UnsupportedCompression(other)),
    }
    if output.len() > max_bytes {
        return Err(RegionError::ChunkLimit {
            actual: output.len(),
            limit: max_bytes,
        });
    }
    Ok(output)
}

fn encode_record(document: &NamedTag, limits: NbtLimits) -> Result<Vec<u8>, RegionError> {
    let nbt = to_bytes(document, limits)?;
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&nbt)?;
    let compressed = encoder.finish()?;
    let length = compressed
        .len()
        .checked_add(1)
        .ok_or(RegionError::ChunkTooLarge)?;
    let mut record = Vec::with_capacity(length + 4);
    record.extend_from_slice(
        &u32::try_from(length)
            .map_err(|_| RegionError::ChunkTooLarge)?
            .to_be_bytes(),
    );
    record.push(2);
    record.extend_from_slice(&compressed);
    if record.len().div_ceil(SECTOR_BYTES) > MAX_SECTORS_PER_CHUNK {
        return Err(RegionError::ChunkTooLarge);
    }
    Ok(record)
}

fn rebuild_region(
    records: &[Option<Vec<u8>>; REGION_ENTRIES],
    timestamps: &[u32; REGION_ENTRIES],
    path: &Path,
    max_region_bytes: usize,
) -> Result<Vec<u8>, RegionError> {
    let mut output = vec![0_u8; HEADER_BYTES];
    let mut next_sector = 2_usize;
    for (index, record) in records.iter().enumerate() {
        let Some(record) = record else { continue };
        let sectors = record.len().div_ceil(SECTOR_BYTES);
        if sectors == 0 || sectors > MAX_SECTORS_PER_CHUNK || next_sector > 0x00ff_ffff {
            return Err(RegionError::ChunkTooLarge);
        }
        let header = index * 4;
        output[header] = ((next_sector >> 16) & 0xff) as u8;
        output[header + 1] = ((next_sector >> 8) & 0xff) as u8;
        output[header + 2] = (next_sector & 0xff) as u8;
        output[header + 3] = sectors as u8;
        output[SECTOR_BYTES + header..SECTOR_BYTES + header + 4]
            .copy_from_slice(&timestamps[index].to_be_bytes());
        let padded = sectors * SECTOR_BYTES;
        let new_length = output
            .len()
            .checked_add(padded)
            .ok_or(RegionError::ChunkTooLarge)?;
        if new_length > max_region_bytes {
            return Err(RegionError::RegionLimit {
                path: path.to_owned(),
                actual: new_length,
                limit: max_region_bytes,
            });
        }
        let start = output.len();
        output.resize(new_length, 0);
        output[start..start + record.len()].copy_from_slice(record);
        next_sector += sectors;
    }
    Ok(output)
}

fn temporary_path(path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    path.with_extension(format!("mca.toucan-{}-{nonce}.tmp", std::process::id()))
}

fn unix_timestamp() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_secs().min(u64::from(u32::MAX)) as u32
        })
}

fn sync_directory(path: &Path) -> Result<(), RegionError> {
    let directory =
        OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|source| RegionError::Write {
                path: path.to_owned(),
                source,
            })?;
    directory.sync_all().map_err(|source| RegionError::Write {
        path: path.to_owned(),
        source,
    })
}

/// Anvil region parsing, compression, or crash-safe write failure.
#[derive(Debug, Error)]
pub enum RegionError {
    /// Region directory creation failed.
    #[error("failed to create region directory at {path}: {source}")]
    CreateDirectory {
        /// Directory that could not be created.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// Region file reading failed.
    #[error("failed to read region file at {path}: {source}")]
    Read {
        /// Region path that could not be read.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// Region file writing or synchronization failed.
    #[error("failed to write region file at {path}: {source}")]
    Write {
        /// Region or temporary path that could not be written.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// Region header was absent or truncated.
    #[error("invalid region header at {path}: {reason}")]
    InvalidHeader {
        /// Corrupt region path.
        path: PathBuf,
        /// Header invariant that failed.
        reason: &'static str,
    },
    /// One location-table entry pointed outside the file.
    #[error("invalid region location entry {index} at {path}")]
    InvalidLocation {
        /// Corrupt region path.
        path: PathBuf,
        /// Location-table index.
        index: usize,
    },
    /// One record length did not fit its allocated sectors.
    #[error("invalid chunk length {length} for entry {index} at {path}")]
    InvalidChunkLength {
        /// Corrupt region path.
        path: PathBuf,
        /// Location-table index.
        index: usize,
        /// Declared record length.
        length: usize,
    },
    /// Compression byte is not supported by the target alpha.
    #[error("unsupported Anvil chunk compression type {0}")]
    UnsupportedCompression(u8),
    /// A decompressed chunk exceeded its defensive bound.
    #[error("decompressed chunk bytes {actual} exceed limit {limit}")]
    ChunkLimit {
        /// Decompressed byte count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// A complete region exceeded its defensive bound.
    #[error("region file at {path} is {actual} bytes; limit is {limit}")]
    RegionLimit {
        /// Oversized region path.
        path: PathBuf,
        /// Observed or proposed byte count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// A chunk could not fit Anvil's one-byte sector count.
    #[error("chunk is too large for one Anvil location entry")]
    ChunkTooLarge,
    /// One batch attempted to span more than one region file.
    #[error("one region write batch cannot span multiple region files")]
    MixedRegions,
    /// One batch provided the same local chunk entry more than once.
    #[error("duplicate chunk entry {index} in one region write batch")]
    DuplicateChunk {
        /// Repeated location-table index.
        index: usize,
    },
    /// NBT validation failed.
    #[error(transparent)]
    Nbt(#[from] NbtError),
    /// Compression stream I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use toucan_nbt::{NamedTag, NbtLimits, Tag};

    use super::{HEADER_BYTES, RegionChunkPosition, RegionError, RegionStore, SECTOR_BYTES};

    fn document(value: i32) -> NamedTag {
        let mut root = BTreeMap::new();
        root.insert("value".into(), Tag::Int(value));
        NamedTag {
            name: String::new(),
            value: Tag::Compound(root),
        }
    }

    fn temporary_directory(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "toucan-region-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn writes_reads_and_preserves_unrelated_chunks() -> Result<(), Box<dyn std::error::Error>> {
        let directory = temporary_directory("round-trip");
        let store = RegionStore::new(&directory, NbtLimits::default(), 16 * 1024 * 1024);
        let first = RegionChunkPosition { x: -1, z: 0 };
        let second = RegionChunkPosition { x: -2, z: 0 };
        store.write_chunk(first, &document(1))?;
        store.write_chunks(&[(second, &document(2)), (first, &document(3))])?;
        assert_eq!(store.read_chunk(first)?, Some(document(3)));
        assert_eq!(store.read_chunk(second)?, Some(document(2)));
        assert_eq!(store.read_chunk(RegionChunkPosition { x: 0, z: 0 })?, None);
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn batch_rejects_mixed_regions_and_duplicate_chunks() {
        let directory = temporary_directory("invalid-batch");
        let store = RegionStore::new(&directory, NbtLimits::default(), 16 * 1024 * 1024);
        let first = RegionChunkPosition { x: 0, z: 0 };
        assert!(matches!(
            store.write_chunks(&[(first, &document(1)), (first, &document(2))]),
            Err(RegionError::DuplicateChunk { index: 0 })
        ));
        assert!(matches!(
            store.write_chunks(&[
                (first, &document(1)),
                (RegionChunkPosition { x: 32, z: 0 }, &document(2)),
            ]),
            Err(RegionError::MixedRegions)
        ));
        assert!(!directory.exists());
    }

    #[test]
    fn rejects_corrupt_locations_and_unknown_compression() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = temporary_directory("corrupt");
        fs::create_dir_all(&directory)?;
        let path = directory.join("r.0.0.mca");
        fs::write(&path, vec![0_u8; 10])?;
        let store = RegionStore::new(&directory, NbtLimits::default(), 16 * 1024 * 1024);
        assert!(matches!(
            store.read_chunk(RegionChunkPosition { x: 0, z: 0 }),
            Err(RegionError::InvalidHeader { .. })
        ));

        let mut region = vec![0_u8; HEADER_BYTES + SECTOR_BYTES];
        region[2] = 2;
        region[3] = 1;
        region[HEADER_BYTES..HEADER_BYTES + 4].copy_from_slice(&2_u32.to_be_bytes());
        region[HEADER_BYTES + 4] = 99;
        region[HEADER_BYTES + 5] = 0;
        fs::write(&path, region)?;
        assert!(matches!(
            store.read_chunk(RegionChunkPosition { x: 0, z: 0 }),
            Err(RegionError::UnsupportedCompression(99))
        ));
        fs::remove_dir_all(directory)?;
        Ok(())
    }
}
