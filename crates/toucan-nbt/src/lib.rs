//! Bounded, lossless Named Binary Tag parsing for vanilla world storage.

use std::collections::BTreeMap;
use std::io::Read;

use flate2::read::GzDecoder;
use thiserror::Error;

/// Defensive limits applied before allocating NBT values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NbtLimits {
    /// Maximum complete uncompressed document size.
    pub max_bytes: usize,
    /// Maximum nested list/compound depth.
    pub max_depth: usize,
    /// Maximum elements in any list or array.
    pub max_collection_len: usize,
    /// Maximum modified-UTF-8 bytes in one string.
    pub max_string_bytes: usize,
}

impl Default for NbtLimits {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024 * 1024,
            max_depth: 512,
            max_collection_len: 1024 * 1024,
            max_string_bytes: u16::MAX as usize,
        }
    }
}

/// One named root NBT document.
#[derive(Clone, Debug, PartialEq)]
pub struct NamedTag {
    /// Root name, conventionally empty in modern files.
    pub name: String,
    /// Root value.
    pub value: Tag,
}

/// Lossless NBT value tree.
#[derive(Clone, Debug, PartialEq)]
pub enum Tag {
    /// Signed byte.
    Byte(i8),
    /// Signed short.
    Short(i16),
    /// Signed integer.
    Int(i32),
    /// Signed long.
    Long(i64),
    /// IEEE-754 single precision.
    Float(f32),
    /// IEEE-754 double precision.
    Double(f64),
    /// Raw signed-byte array, retained as identical bytes.
    ByteArray(Vec<u8>),
    /// Modified-UTF-8 string.
    String(String),
    /// Homogeneous list. Empty lists retain their declared element type.
    List {
        /// Declared homogeneous wire type.
        element_type: u8,
        /// Ordered list values.
        values: Vec<Tag>,
    },
    /// Named children in deterministic key order.
    Compound(BTreeMap<String, Tag>),
    /// Signed integer array.
    IntArray(Vec<i32>),
    /// Signed long array.
    LongArray(Vec<i64>),
}

impl Tag {
    /// Returns this tag's wire type ID.
    #[must_use]
    pub const fn id(&self) -> u8 {
        match self {
            Self::Byte(_) => 1,
            Self::Short(_) => 2,
            Self::Int(_) => 3,
            Self::Long(_) => 4,
            Self::Float(_) => 5,
            Self::Double(_) => 6,
            Self::ByteArray(_) => 7,
            Self::String(_) => 8,
            Self::List { .. } => 9,
            Self::Compound(_) => 10,
            Self::IntArray(_) => 11,
            Self::LongArray(_) => 12,
        }
    }

    /// Returns a compound child when this value is a compound.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Compound(values) => values.get(key),
            _ => None,
        }
    }

    /// Returns an integer value without numeric coercion.
    #[must_use]
    pub const fn as_i32(&self) -> Option<i32> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns a string value.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }
}

/// NBT validation or I/O failure.
#[derive(Debug, Error)]
pub enum NbtError {
    /// Input ended inside a value.
    #[error("unexpected end of NBT data")]
    UnexpectedEof,
    /// A tag type was unknown or invalid in context.
    #[error("invalid NBT tag type {0}")]
    InvalidTagType(u8),
    /// A signed collection length was negative.
    #[error("negative NBT collection length {0}")]
    NegativeLength(i32),
    /// A configured defensive limit was exceeded.
    #[error("NBT {kind} {actual} exceeds limit {limit}")]
    Limit {
        /// Limited resource.
        kind: &'static str,
        /// Observed value.
        actual: usize,
        /// Configured limit.
        limit: usize,
    },
    /// A string was not valid Java modified UTF-8.
    #[error("invalid modified UTF-8 string")]
    InvalidModifiedUtf8,
    /// Bytes remained after the root value.
    #[error("NBT document has {0} trailing bytes")]
    TrailingData(usize),
    /// A list contained a value unlike its declared type.
    #[error("NBT list declares type {declared} but contains type {actual}")]
    HeterogeneousList {
        /// Type declared by the list header.
        declared: u8,
        /// Type found in one value.
        actual: u8,
    },
    /// Compression or stream I/O failed.
    #[error("NBT I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Parses one uncompressed named NBT document.
pub fn from_bytes(bytes: &[u8], limits: NbtLimits) -> Result<NamedTag, NbtError> {
    if bytes.len() > limits.max_bytes {
        return Err(limit("document bytes", bytes.len(), limits.max_bytes));
    }
    let mut parser = Parser {
        remaining: bytes,
        limits,
    };
    let tag_type = parser.u8()?;
    if !(1..=12).contains(&tag_type) {
        return Err(NbtError::InvalidTagType(tag_type));
    }
    let name = parser.string()?;
    let value = parser.payload(tag_type, 0)?;
    if !parser.remaining.is_empty() {
        return Err(NbtError::TrailingData(parser.remaining.len()));
    }
    Ok(NamedTag { name, value })
}

/// Decompresses and parses a gzip-compressed NBT document such as `level.dat`.
pub fn from_gzip(bytes: &[u8], limits: NbtLimits) -> Result<NamedTag, NbtError> {
    let cap = u64::try_from(limits.max_bytes).unwrap_or(u64::MAX);
    let mut decoded = Vec::new();
    GzDecoder::new(bytes)
        .take(cap.saturating_add(1))
        .read_to_end(&mut decoded)?;
    if decoded.len() > limits.max_bytes {
        return Err(limit("document bytes", decoded.len(), limits.max_bytes));
    }
    from_bytes(&decoded, limits)
}

/// Serializes one named NBT document, preserving all modeled fields.
pub fn to_bytes(document: &NamedTag, limits: NbtLimits) -> Result<Vec<u8>, NbtError> {
    let mut output = Vec::new();
    output.push(document.value.id());
    write_string(&mut output, &document.name, limits)?;
    write_payload(&mut output, &document.value, limits, 0)?;
    if output.len() > limits.max_bytes {
        return Err(limit("document bytes", output.len(), limits.max_bytes));
    }
    Ok(output)
}

struct Parser<'a> {
    remaining: &'a [u8],
    limits: NbtLimits,
}

impl Parser<'_> {
    fn payload(&mut self, tag_type: u8, depth: usize) -> Result<Tag, NbtError> {
        if depth > self.limits.max_depth {
            return Err(limit("depth", depth, self.limits.max_depth));
        }
        Ok(match tag_type {
            1 => Tag::Byte(self.u8()? as i8),
            2 => Tag::Short(i16::from_be_bytes(self.array()?)),
            3 => Tag::Int(i32::from_be_bytes(self.array()?)),
            4 => Tag::Long(i64::from_be_bytes(self.array()?)),
            5 => Tag::Float(f32::from_bits(u32::from_be_bytes(self.array()?))),
            6 => Tag::Double(f64::from_bits(u64::from_be_bytes(self.array()?))),
            7 => {
                let length = self.length()?;
                Tag::ByteArray(self.take(length)?.to_vec())
            }
            8 => Tag::String(self.string()?),
            9 => {
                let element_type = self.u8()?;
                let length = self.length()?;
                if element_type == 0 && length != 0 || element_type > 12 {
                    return Err(NbtError::InvalidTagType(element_type));
                }
                let mut values = Vec::with_capacity(length.min(1024));
                for _ in 0..length {
                    values.push(self.payload(element_type, depth + 1)?);
                }
                Tag::List {
                    element_type,
                    values,
                }
            }
            10 => {
                let mut values = BTreeMap::new();
                loop {
                    let child_type = self.u8()?;
                    if child_type == 0 {
                        break;
                    }
                    if child_type > 12 {
                        return Err(NbtError::InvalidTagType(child_type));
                    }
                    let name = self.string()?;
                    if values.len() >= self.limits.max_collection_len {
                        return Err(limit(
                            "compound entries",
                            values.len() + 1,
                            self.limits.max_collection_len,
                        ));
                    }
                    values.insert(name, self.payload(child_type, depth + 1)?);
                }
                Tag::Compound(values)
            }
            11 => {
                let length = self.length()?;
                self.require_array_bytes(length, 4)?;
                let mut values = Vec::with_capacity(length);
                for _ in 0..length {
                    values.push(i32::from_be_bytes(self.array()?));
                }
                Tag::IntArray(values)
            }
            12 => {
                let length = self.length()?;
                self.require_array_bytes(length, 8)?;
                let mut values = Vec::with_capacity(length);
                for _ in 0..length {
                    values.push(i64::from_be_bytes(self.array()?));
                }
                Tag::LongArray(values)
            }
            value => return Err(NbtError::InvalidTagType(value)),
        })
    }

    fn length(&mut self) -> Result<usize, NbtError> {
        let signed = i32::from_be_bytes(self.array()?);
        let length = usize::try_from(signed).map_err(|_| NbtError::NegativeLength(signed))?;
        if length > self.limits.max_collection_len {
            return Err(limit(
                "collection length",
                length,
                self.limits.max_collection_len,
            ));
        }
        Ok(length)
    }

    fn string(&mut self) -> Result<String, NbtError> {
        let length = usize::from(u16::from_be_bytes(self.array()?));
        if length > self.limits.max_string_bytes {
            return Err(limit("string bytes", length, self.limits.max_string_bytes));
        }
        decode_modified_utf8(self.take(length)?)
    }

    fn require_array_bytes(&self, length: usize, element_bytes: usize) -> Result<(), NbtError> {
        let required = length
            .checked_mul(element_bytes)
            .ok_or_else(|| limit("array bytes", usize::MAX, self.limits.max_bytes))?;
        if required > self.remaining.len() {
            return Err(NbtError::UnexpectedEof);
        }
        Ok(())
    }

    fn u8(&mut self) -> Result<u8, NbtError> {
        Ok(self.take(1)?[0])
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], NbtError> {
        let mut output = [0; N];
        output.copy_from_slice(self.take(N)?);
        Ok(output)
    }

    fn take(&mut self, length: usize) -> Result<&[u8], NbtError> {
        if self.remaining.len() < length {
            return Err(NbtError::UnexpectedEof);
        }
        let (value, rest) = self.remaining.split_at(length);
        self.remaining = rest;
        Ok(value)
    }
}

fn write_payload(
    output: &mut Vec<u8>,
    tag: &Tag,
    limits: NbtLimits,
    depth: usize,
) -> Result<(), NbtError> {
    if depth > limits.max_depth {
        return Err(limit("depth", depth, limits.max_depth));
    }
    match tag {
        Tag::Byte(value) => output.push(*value as u8),
        Tag::Short(value) => output.extend_from_slice(&value.to_be_bytes()),
        Tag::Int(value) => output.extend_from_slice(&value.to_be_bytes()),
        Tag::Long(value) => output.extend_from_slice(&value.to_be_bytes()),
        Tag::Float(value) => output.extend_from_slice(&value.to_bits().to_be_bytes()),
        Tag::Double(value) => output.extend_from_slice(&value.to_bits().to_be_bytes()),
        Tag::ByteArray(values) => {
            write_length(output, values.len(), limits)?;
            output.extend_from_slice(values);
        }
        Tag::String(value) => write_string(output, value, limits)?,
        Tag::List {
            element_type,
            values,
        } => {
            if *element_type > 12 || *element_type == 0 && !values.is_empty() {
                return Err(NbtError::InvalidTagType(*element_type));
            }
            output.push(*element_type);
            write_length(output, values.len(), limits)?;
            for value in values {
                if value.id() != *element_type {
                    return Err(NbtError::HeterogeneousList {
                        declared: *element_type,
                        actual: value.id(),
                    });
                }
                write_payload(output, value, limits, depth + 1)?;
            }
        }
        Tag::Compound(values) => {
            if values.len() > limits.max_collection_len {
                return Err(limit(
                    "compound entries",
                    values.len(),
                    limits.max_collection_len,
                ));
            }
            for (name, value) in values {
                output.push(value.id());
                write_string(output, name, limits)?;
                write_payload(output, value, limits, depth + 1)?;
            }
            output.push(0);
        }
        Tag::IntArray(values) => {
            write_length(output, values.len(), limits)?;
            for value in values {
                output.extend_from_slice(&value.to_be_bytes());
            }
        }
        Tag::LongArray(values) => {
            write_length(output, values.len(), limits)?;
            for value in values {
                output.extend_from_slice(&value.to_be_bytes());
            }
        }
    }
    Ok(())
}

fn write_length(output: &mut Vec<u8>, length: usize, limits: NbtLimits) -> Result<(), NbtError> {
    if length > limits.max_collection_len || length > i32::MAX as usize {
        return Err(limit(
            "collection length",
            length,
            limits.max_collection_len.min(i32::MAX as usize),
        ));
    }
    output.extend_from_slice(&(length as i32).to_be_bytes());
    Ok(())
}

fn write_string(output: &mut Vec<u8>, value: &str, limits: NbtLimits) -> Result<(), NbtError> {
    let encoded = encode_modified_utf8(value);
    let limit = limits.max_string_bytes.min(u16::MAX as usize);
    if encoded.len() > limit {
        return Err(limit_error("string bytes", encoded.len(), limit));
    }
    output.extend_from_slice(&(encoded.len() as u16).to_be_bytes());
    output.extend_from_slice(&encoded);
    Ok(())
}

fn decode_modified_utf8(bytes: &[u8]) -> Result<String, NbtError> {
    let mut units = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let first = bytes[index];
        if first != 0 && first & 0x80 == 0 {
            units.push(u16::from(first));
            index += 1;
        } else if first & 0xe0 == 0xc0 && index + 1 < bytes.len() {
            let second = bytes[index + 1];
            if second & 0xc0 != 0x80 {
                return Err(NbtError::InvalidModifiedUtf8);
            }
            let unit = (u16::from(first & 0x1f) << 6) | u16::from(second & 0x3f);
            if unit != 0 && unit < 0x80 {
                return Err(NbtError::InvalidModifiedUtf8);
            }
            units.push(unit);
            index += 2;
        } else if first & 0xf0 == 0xe0 && index + 2 < bytes.len() {
            let second = bytes[index + 1];
            let third = bytes[index + 2];
            if second & 0xc0 != 0x80 || third & 0xc0 != 0x80 {
                return Err(NbtError::InvalidModifiedUtf8);
            }
            let unit = (u16::from(first & 0x0f) << 12)
                | (u16::from(second & 0x3f) << 6)
                | u16::from(third & 0x3f);
            if unit < 0x800 {
                return Err(NbtError::InvalidModifiedUtf8);
            }
            units.push(unit);
            index += 3;
        } else {
            return Err(NbtError::InvalidModifiedUtf8);
        }
    }
    String::from_utf16(&units).map_err(|_| NbtError::InvalidModifiedUtf8)
}

fn encode_modified_utf8(value: &str) -> Vec<u8> {
    let mut output = Vec::with_capacity(value.len());
    for unit in value.encode_utf16() {
        match unit {
            0x0001..=0x007f => output.push(unit as u8),
            0x0000..=0x07ff => {
                output.push((0xc0 | (unit >> 6)) as u8);
                output.push((0x80 | (unit & 0x3f)) as u8);
            }
            _ => {
                output.push((0xe0 | (unit >> 12)) as u8);
                output.push((0x80 | ((unit >> 6) & 0x3f)) as u8);
                output.push((0x80 | (unit & 0x3f)) as u8);
            }
        }
    }
    output
}

fn limit(kind: &'static str, actual: usize, limit: usize) -> NbtError {
    limit_error(kind, actual, limit)
}

fn limit_error(kind: &'static str, actual: usize, limit: usize) -> NbtError {
    NbtError::Limit {
        kind,
        actual,
        limit,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{NamedTag, NbtError, NbtLimits, Tag, from_bytes, to_bytes};

    #[test]
    fn fixed_compound_fixture_decodes() {
        let fixture = [10, 0, 0, 3, 0, 1, b'x', 0, 0, 0, 42, 0];
        let document = from_bytes(&fixture, NbtLimits::default())
            .unwrap_or_else(|error| panic!("fixture should decode: {error}"));
        assert_eq!(document.name, "");
        assert_eq!(document.value.get("x").and_then(Tag::as_i32), Some(42));
    }

    #[test]
    fn all_value_types_round_trip_losslessly() {
        let mut root = BTreeMap::new();
        root.insert("byte".into(), Tag::Byte(-1));
        root.insert("short".into(), Tag::Short(-2));
        root.insert("int".into(), Tag::Int(3));
        root.insert("long".into(), Tag::Long(4));
        root.insert("float".into(), Tag::Float(1.5));
        root.insert("double".into(), Tag::Double(-2.5));
        root.insert("bytes".into(), Tag::ByteArray(vec![0, 128, 255]));
        root.insert("text".into(), Tag::String("null\0 bird 😀".into()));
        root.insert(
            "list".into(),
            Tag::List {
                element_type: 3,
                values: vec![Tag::Int(1), Tag::Int(2)],
            },
        );
        root.insert("ints".into(), Tag::IntArray(vec![-1, 2]));
        root.insert("longs".into(), Tag::LongArray(vec![-3, 4]));
        let document = NamedTag {
            name: "root".into(),
            value: Tag::Compound(root),
        };
        let encoded = to_bytes(&document, NbtLimits::default()).unwrap_or_default();
        let decoded = from_bytes(&encoded, NbtLimits::default())
            .unwrap_or_else(|error| panic!("round trip should decode: {error}"));
        assert_eq!(decoded, document);
    }

    #[test]
    fn collection_limit_precedes_allocation() {
        let fixture = [9, 0, 0, 1, 0, 0, 0, 2];
        let limits = NbtLimits {
            max_collection_len: 1,
            ..NbtLimits::default()
        };
        assert!(matches!(
            from_bytes(&fixture, limits),
            Err(NbtError::Limit {
                kind: "collection length",
                ..
            })
        ));
    }
}
