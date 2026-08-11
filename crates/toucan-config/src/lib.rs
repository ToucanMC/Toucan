use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

pub const DEFAULT_CONFIG: &str = include_str!("../../../config/toucan.toml");

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub performance: PerformanceConfig,
    pub network: NetworkConfig,
    pub logging: LoggingConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub address: String,
    pub port: u16,
    pub motd: String,
    pub max_players: u32,
    pub view_distance: u8,
    pub simulation_distance: u8,
    pub online_mode: bool,
    #[serde(default = "default_true")]
    pub fetch_profile_textures: bool,
    pub compression_threshold: i32,
    pub world: PathBuf,
    pub world_generator: WorldGenerator,
    pub world_seed: i64,
    pub difficulty: Difficulty,
    pub default_gamemode: GameMode,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceConfig {
    pub worker_threads: usize,
    pub chunk_io_threads: usize,
    pub max_loaded_chunks: usize,
    pub max_packets_per_tick: usize,
    pub autosave_interval_seconds: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub max_packet_size: usize,
    pub max_connections: usize,
    pub packet_timeout_seconds: u64,
    pub shutdown_timeout_seconds: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    pub level: LogLevel,
    pub format: LogFormat,
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Difficulty {
    Peaceful,
    Easy,
    Normal,
    Hard,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum GameMode {
    Survival,
    Creative,
    Adventure,
    Spectator,
}

impl GameMode {
    #[must_use]
    pub const fn protocol_id(self) -> u8 {
        match self {
            Self::Survival => 0,
            Self::Creative => 1,
            Self::Adventure => 2,
            Self::Spectator => 3,
        }
    }

    #[must_use]
    pub const fn from_protocol_id(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Survival),
            1 => Some(Self::Creative),
            2 => Some(Self::Adventure),
            3 => Some(Self::Spectator),
            _ => None,
        }
    }

    #[must_use]
    pub const fn can_modify_blocks(self) -> bool {
        matches!(self, Self::Survival | Self::Creative)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum WorldGenerator {
    Terrain,
    Flat,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Pretty,
    Json,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationIssue {
    field: &'static str,
    message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationErrors(Vec<ValidationIssue>);

impl ValidationErrors {
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered = self
            .0
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        formatter.write_str(&rendered)
    }
}

impl std::error::Error for ValidationErrors {}

impl ValidationIssue {
    fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read configuration at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to create configuration directory at {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write default configuration at {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid TOML configuration: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("configuration validation failed: {0}")]
    Validation(ValidationErrors),
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let source = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&source)
    }

    pub fn load_or_create(path: impl AsRef<Path>) -> Result<(Self, bool), ConfigError> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(source) => return Ok((Self::parse(&source)?, false)),
            Err(source) if source.kind() != std::io::ErrorKind::NotFound => {
                return Err(ConfigError::Read {
                    path: path.to_owned(),
                    source,
                });
            }
            Err(_) => {}
        }

        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|source| ConfigError::CreateDirectory {
                path: parent.to_owned(),
                source,
            })?;
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(|source| ConfigError::Write {
                path: path.to_owned(),
                source,
            })?;
        file.write_all(DEFAULT_CONFIG.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|source| ConfigError::Write {
                path: path.to_owned(),
                source,
            })?;
        Ok((Self::parse(DEFAULT_CONFIG)?, true))
    }

    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(source)?;
        config.validate()?;
        Ok(config)
    }

    pub fn bind_address(&self) -> Result<SocketAddr, ConfigError> {
        let ip = self.server.address.parse::<IpAddr>().map_err(|error| {
            ConfigError::Validation(ValidationErrors(vec![ValidationIssue::new(
                "server.address",
                error.to_string(),
            )]))
        })?;
        Ok(SocketAddr::new(ip, self.server.port))
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut issues = Vec::new();

        if self.server.address.parse::<IpAddr>().is_err() {
            issues.push(ValidationIssue::new(
                "server.address",
                "must be an IPv4 or IPv6 address",
            ));
        }
        if self.server.motd.chars().count() > 512 {
            issues.push(ValidationIssue::new(
                "server.motd",
                "must contain at most 512 Unicode scalar values",
            ));
        }
        if self.server.max_players == 0 {
            issues.push(ValidationIssue::new(
                "server.max_players",
                "must be greater than zero",
            ));
        }
        if !(2..=32).contains(&self.server.view_distance) {
            issues.push(ValidationIssue::new(
                "server.view_distance",
                "must be between 2 and 32",
            ));
        }
        if !(2..=32).contains(&self.server.simulation_distance) {
            issues.push(ValidationIssue::new(
                "server.simulation_distance",
                "must be between 2 and 32",
            ));
        }
        if self.server.compression_threshold < -1 {
            issues.push(ValidationIssue::new(
                "server.compression_threshold",
                "must be -1 or greater",
            ));
        } else if usize::try_from(self.server.compression_threshold)
            .is_ok_and(|threshold| threshold > self.network.max_packet_size)
        {
            issues.push(ValidationIssue::new(
                "server.compression_threshold",
                "must not exceed network.max_packet_size",
            ));
        }
        if self.server.world.as_os_str().is_empty() {
            issues.push(ValidationIssue::new("server.world", "must not be empty"));
        }
        if self.performance.worker_threads > 256 {
            issues.push(ValidationIssue::new(
                "performance.worker_threads",
                "must be zero or at most 256",
            ));
        }
        if self.performance.chunk_io_threads == 0 {
            issues.push(ValidationIssue::new(
                "performance.chunk_io_threads",
                "must be greater than zero",
            ));
        }
        if self.performance.chunk_io_threads > 256 {
            issues.push(ValidationIssue::new(
                "performance.chunk_io_threads",
                "must be at most 256",
            ));
        }
        if !(25..=1_048_576).contains(&self.performance.max_loaded_chunks) {
            issues.push(ValidationIssue::new(
                "performance.max_loaded_chunks",
                "must be between 25 and 1048576",
            ));
        }
        let diameter = usize::from(self.server.view_distance) * 2 + 1;
        let initial_chunks = diameter * diameter;
        if self.performance.max_loaded_chunks < initial_chunks {
            issues.push(ValidationIssue::new(
                "performance.max_loaded_chunks",
                format!("must be at least {initial_chunks} for server.view_distance"),
            ));
        }
        if self.performance.max_packets_per_tick == 0 {
            issues.push(ValidationIssue::new(
                "performance.max_packets_per_tick",
                "must be greater than zero",
            ));
        }
        if self.performance.autosave_interval_seconds == 0 {
            issues.push(ValidationIssue::new(
                "performance.autosave_interval_seconds",
                "must be greater than zero",
            ));
        }
        if !(1..=16_777_216).contains(&self.network.max_packet_size) {
            issues.push(ValidationIssue::new(
                "network.max_packet_size",
                "must be between 1 and 16777216 bytes",
            ));
        }
        if self.network.max_connections == 0 {
            issues.push(ValidationIssue::new(
                "network.max_connections",
                "must be greater than zero",
            ));
        }
        if self.network.packet_timeout_seconds == 0 {
            issues.push(ValidationIssue::new(
                "network.packet_timeout_seconds",
                "must be greater than zero",
            ));
        }
        if self.network.shutdown_timeout_seconds == 0 {
            issues.push(ValidationIssue::new(
                "network.shutdown_timeout_seconds",
                "must be greater than zero",
            ));
        }

        if issues.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Validation(ValidationErrors(issues)))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{Config, ConfigError, DEFAULT_CONFIG};

    const VALID: &str = DEFAULT_CONFIG;

    #[test]
    fn parses_checked_in_configuration() -> Result<(), ConfigError> {
        let config = Config::parse(VALID)?;
        assert_eq!(config.server.port, 25_565);
        assert_eq!(config.bind_address()?.to_string(), "0.0.0.0:25565");
        Ok(())
    }

    #[test]
    fn all_game_modes_parse_and_map_to_protocol_ordinals() -> Result<(), ConfigError> {
        for (name, expected) in [
            ("survival", 0),
            ("creative", 1),
            ("adventure", 2),
            ("spectator", 3),
        ] {
            let source = VALID
                .lines()
                .map(|line| {
                    if line.starts_with("default_gamemode = ") {
                        format!("default_gamemode = \"{name}\"")
                    } else {
                        line.to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(
                Config::parse(&source)?
                    .server
                    .default_gamemode
                    .protocol_id(),
                expected
            );
        }
        Ok(())
    }

    #[test]
    fn reports_all_invalid_resource_limits() {
        let invalid = VALID
            .replace("max_players = 20", "max_players = 0")
            .replace("max_packet_size = 2097152", "max_packet_size = 0")
            .replace("packet_timeout_seconds = 10", "packet_timeout_seconds = 0");

        let error = Config::parse(&invalid).err();
        let Some(ConfigError::Validation(issues)) = error else {
            panic!("expected validation error");
        };
        assert_eq!(issues.len(), 4);
    }

    #[test]
    fn rejects_unknown_keys() {
        let invalid = VALID.replace("motd =", "mystery = true\nmotd =");
        assert!(matches!(
            Config::parse(&invalid),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn rejects_compression_threshold_above_packet_limit() {
        let invalid = VALID
            .replace(
                "compression_threshold = 256",
                "compression_threshold = 4096",
            )
            .replace("max_packet_size = 2097152", "max_packet_size = 1024");
        assert!(matches!(
            Config::parse(&invalid),
            Err(ConfigError::Validation(_))
        ));
    }

    #[test]
    fn missing_configuration_is_created_and_reloaded() -> Result<(), ConfigError> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let directory =
            std::env::temp_dir().join(format!("toucan-config-{}-{nonce}", std::process::id()));
        let path = directory.join("nested/toucan.toml");
        let (created, was_created) = Config::load_or_create(&path)?;
        assert!(was_created);
        assert_eq!(created.server.port, 25_565);
        assert_eq!(std::fs::read_to_string(&path).ok().as_deref(), Some(VALID));
        let (_, was_created_again) = Config::load_or_create(&path)?;
        assert!(!was_created_again);
        let _ = std::fs::remove_dir_all(directory);
        Ok(())
    }
}
