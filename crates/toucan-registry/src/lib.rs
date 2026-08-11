use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use flate2::read::GzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fmt::Write;
use std::io::Read;
use std::sync::OnceLock;
use thiserror::Error;

const FIXTURE_JSON_SHA256: &str =
    "fdd36af0e682d702577e8ac183c86ddf6970edd4648e861d4306bddeff206824";
const MAX_FIXTURE_BYTES: u64 = 2 * 1024 * 1024;
const EXPECTED_PACKET_COUNT: usize = 29;
const EXPECTED_REGISTRY_COUNT: usize = 28;

static CONFIGURATION_PACKETS: OnceLock<Result<Vec<ConfigurationPacket>, String>> = OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigurationPacket {
    pub id: i32,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RegistryError {
    #[error("invalid embedded 26.1.2 registry fixture: {0}")]
    InvalidFixture(String),
}

pub fn configuration_packets() -> Result<&'static [ConfigurationPacket], RegistryError> {
    CONFIGURATION_PACKETS
        .get_or_init(load_configuration_packets)
        .as_deref()
        .map_err(|error| RegistryError::InvalidFixture(error.clone()))
}

fn load_configuration_packets() -> Result<Vec<ConfigurationPacket>, String> {
    let compressed = STANDARD
        .decode(
            include_str!("configuration_26_1_2.json.gz.b64")
                .split_whitespace()
                .collect::<String>(),
        )
        .map_err(|error| format!("base64 decode failed: {error}"))?;
    let mut json = Vec::new();
    GzDecoder::new(compressed.as_slice())
        .take(MAX_FIXTURE_BYTES + 1)
        .read_to_end(&mut json)
        .map_err(|error| format!("gzip decode failed: {error}"))?;
    if json.len() as u64 > MAX_FIXTURE_BYTES {
        return Err(format!(
            "decoded fixture is {} bytes; limit is {MAX_FIXTURE_BYTES}",
            json.len()
        ));
    }

    let digest = Sha256::digest(&json);
    let mut checksum = String::with_capacity(digest.len() * 2);

    for byte in digest.iter() {
        write!(&mut checksum, "{byte:02x}").expect("writing to a String cannot fail");
    }

    if checksum != FIXTURE_JSON_SHA256 {
        return Err(format!(
            "checksum {checksum} does not match {FIXTURE_JSON_SHA256}"
        ));
    }

    let encoded: Vec<EncodedPacket> =
        serde_json::from_slice(&json).map_err(|error| format!("JSON decode failed: {error}"))?;
    if encoded.len() != EXPECTED_PACKET_COUNT {
        return Err(format!(
            "contains {} packets; expected {EXPECTED_PACKET_COUNT}",
            encoded.len()
        ));
    }

    let mut packets = Vec::with_capacity(encoded.len());
    for packet in encoded {
        packets.push(ConfigurationPacket {
            id: packet.id,
            payload: STANDARD
                .decode(packet.payload)
                .map_err(|error| format!("packet payload decode failed: {error}"))?,
        });
    }
    let registry_count = packets.iter().filter(|packet| packet.id == 0x07).count();
    if registry_count != EXPECTED_REGISTRY_COUNT
        || packets.last().map(|packet| packet.id) != Some(0x0d)
    {
        return Err(format!(
            "expected {EXPECTED_REGISTRY_COUNT} registry packets followed by tags"
        ));
    }
    Ok(packets)
}

#[derive(Deserialize)]
struct EncodedPacket {
    id: i32,
    payload: String,
}

#[cfg(test)]
mod tests {
    use super::configuration_packets;

    #[test]
    fn fixture_checksum_and_packet_shape_are_valid() {
        let packets = configuration_packets();
        assert!(packets.is_ok());
        let packets = packets.unwrap_or_default();
        assert_eq!(packets.len(), 29);
        assert_eq!(packets.iter().filter(|packet| packet.id == 7).count(), 28);
        assert_eq!(packets.last().map(|packet| packet.id), Some(13));
    }
}
