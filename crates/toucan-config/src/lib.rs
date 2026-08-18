use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

pub const DEFAULT_CONFIG: &str = include_str!("../../../config/toucan.toml");
pub const DEFAULT_ADVANCED_CONFIG: &str = include_str!("../../../config/advanced.toml");

#[derive(Clone, Debug)]
pub struct Config {
    pub server: ServerConfig,
    pub gameplay: GameplayConfig,
    pub world: WorldConfig,
    pub logging: LoggingConfig,
    pub advanced: AdvancedConfig,
    migrated_legacy_config: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserConfig {
    server: ServerConfig,
    gameplay: GameplayConfig,
    world: WorldConfig,
    logging: LoggingConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub bind_address: String,
    pub port: u16,
    pub motd: String,
    pub max_players: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameplayConfig {
    pub online_mode: bool,
    #[serde(default = "default_true")]
    pub fetch_profile_textures: bool,
    pub difficulty: Difficulty,
    pub default_game_mode: GameMode,
    pub view_distance_chunks: u8,
    pub simulation_distance_chunks: u8,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldConfig {
    pub path: PathBuf,
    pub seed: i64,
    pub generator: WorldGenerator,
    pub autosave_interval_seconds: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvancedConfig {
    pub runtime: RuntimeConfig,
    pub network: NetworkConfig,
    pub world: AdvancedWorldConfig,
    pub updates: UpdateConfig,
    pub broadcast: BroadcastConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub worker_threads: usize,
    pub chunk_io_max_blocking_threads: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub max_uncompressed_packet_bytes: usize,
    pub max_connections: usize,
    pub network_read_timeout_seconds: u64,
    pub shutdown_timeout_seconds: u64,
    pub compression_threshold_bytes: i32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvancedWorldConfig {
    pub chunk_cache_max_chunks: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateConfig {
    pub scheduled_updates_per_tick: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BroadcastConfig {
    pub world_event_capacity: usize,
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
        formatter.write_str(
            &self
                .0
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
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
    #[error("invalid TOML configuration in {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("configuration validation failed: {0}")]
    Validation(ValidationErrors),
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let source = read(path)?;
        if looks_like_legacy(&source) {
            return Self::parse_legacy(&source, path);
        }
        let advanced_path = advanced_path(path);
        let advanced = read(&advanced_path)?;
        Self::parse_files_named(&source, path, &advanced, &advanced_path)
    }

    pub fn load_or_create(path: impl AsRef<Path>) -> Result<(Self, bool), ConfigError> {
        let path = path.as_ref();
        let (source, user_created) = read_or_create(path, DEFAULT_CONFIG)?;
        if looks_like_legacy(&source) {
            return Ok((Self::parse_legacy(&source, path)?, false));
        }
        let advanced_path = advanced_path(path);
        let (advanced, advanced_created) = read_or_create(&advanced_path, DEFAULT_ADVANCED_CONFIG)?;
        Ok((
            Self::parse_files_named(&source, path, &advanced, &advanced_path)?,
            user_created || advanced_created,
        ))
    }

    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        Self::parse_files(source, DEFAULT_ADVANCED_CONFIG)
    }

    pub fn parse_files(user: &str, advanced: &str) -> Result<Self, ConfigError> {
        Self::parse_files_named(
            user,
            Path::new("toucan.toml"),
            advanced,
            Path::new("advanced.toml"),
        )
    }

    fn parse_files_named(
        user: &str,
        user_path: &Path,
        advanced: &str,
        advanced_path: &Path,
    ) -> Result<Self, ConfigError> {
        let user: UserConfig = toml::from_str(user).map_err(|source| ConfigError::Parse {
            path: user_path.to_owned(),
            source,
        })?;
        let advanced = toml::from_str(advanced).map_err(|source| ConfigError::Parse {
            path: advanced_path.to_owned(),
            source,
        })?;
        let config = Self {
            server: user.server,
            gameplay: user.gameplay,
            world: user.world,
            logging: user.logging,
            advanced,
            migrated_legacy_config: false,
        };
        config.validate()?;
        Ok(config)
    }

    #[must_use]
    pub const fn migrated_legacy_config(&self) -> bool {
        self.migrated_legacy_config
    }

    pub fn bind_address(&self) -> Result<SocketAddr, ConfigError> {
        let ip = self
            .server
            .bind_address
            .parse::<IpAddr>()
            .map_err(|error| {
                ConfigError::Validation(ValidationErrors(vec![ValidationIssue::new(
                    "server.bind_address",
                    error.to_string(),
                )]))
            })?;
        Ok(SocketAddr::new(ip, self.server.port))
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut issues = Vec::new();
        if self.server.bind_address.parse::<IpAddr>().is_err() {
            issues.push(ValidationIssue::new(
                "server.bind_address",
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
        if !(2..=32).contains(&self.gameplay.view_distance_chunks) {
            issues.push(ValidationIssue::new(
                "gameplay.view_distance_chunks",
                "must be between 2 and 32 chunks",
            ));
        }
        if !(2..=32).contains(&self.gameplay.simulation_distance_chunks) {
            issues.push(ValidationIssue::new(
                "gameplay.simulation_distance_chunks",
                "must be between 2 and 32 chunks",
            ));
        }
        if self.world.path.as_os_str().is_empty() {
            issues.push(ValidationIssue::new("world.path", "must not be empty"));
        }
        if self.world.autosave_interval_seconds == 0 {
            issues.push(ValidationIssue::new(
                "world.autosave_interval_seconds",
                "must be greater than zero seconds",
            ));
        }
        if self.advanced.runtime.worker_threads > 256 {
            issues.push(ValidationIssue::new(
                "runtime.worker_threads",
                "must be zero or at most 256",
            ));
        }
        if !(1..=256).contains(&self.advanced.runtime.chunk_io_max_blocking_threads) {
            issues.push(ValidationIssue::new(
                "runtime.chunk_io_max_blocking_threads",
                "must be between 1 and 256",
            ));
        }
        let diameter = usize::from(self.gameplay.view_distance_chunks) * 2 + 1;
        let required_chunks = diameter * diameter;
        if !(25..=1_048_576).contains(&self.advanced.world.chunk_cache_max_chunks) {
            issues.push(ValidationIssue::new(
                "world.chunk_cache_max_chunks",
                "must be between 25 and 1048576 chunks",
            ));
        } else if self.advanced.world.chunk_cache_max_chunks < required_chunks {
            issues.push(ValidationIssue::new(
                "world.chunk_cache_max_chunks",
                format!("must be at least {required_chunks} for gameplay.view_distance_chunks"),
            ));
        }
        if self.advanced.updates.scheduled_updates_per_tick == 0 {
            issues.push(ValidationIssue::new(
                "updates.scheduled_updates_per_tick",
                "must be greater than zero",
            ));
        }
        if !(1..=16_777_216).contains(&self.advanced.network.max_uncompressed_packet_bytes) {
            issues.push(ValidationIssue::new(
                "network.max_uncompressed_packet_bytes",
                "must be between 1 and 16777216 bytes",
            ));
        }
        if self.advanced.network.max_connections == 0 {
            issues.push(ValidationIssue::new(
                "network.max_connections",
                "must be greater than zero",
            ));
        }
        if self.advanced.network.network_read_timeout_seconds == 0 {
            issues.push(ValidationIssue::new(
                "network.network_read_timeout_seconds",
                "must be greater than zero seconds",
            ));
        }
        if self.advanced.network.shutdown_timeout_seconds == 0 {
            issues.push(ValidationIssue::new(
                "network.shutdown_timeout_seconds",
                "must be greater than zero seconds",
            ));
        }
        let threshold = self.advanced.network.compression_threshold_bytes;
        if threshold < -1 {
            issues.push(ValidationIssue::new(
                "network.compression_threshold_bytes",
                "must be -1 (disabled) or zero or greater",
            ));
        } else if usize::try_from(threshold)
            .is_ok_and(|threshold| threshold > self.advanced.network.max_uncompressed_packet_bytes)
        {
            issues.push(ValidationIssue::new(
                "network.compression_threshold_bytes",
                "must not exceed network.max_uncompressed_packet_bytes",
            ));
        }
        if self.advanced.broadcast.world_event_capacity == 0 {
            issues.push(ValidationIssue::new(
                "broadcast.world_event_capacity",
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

fn advanced_path(user_path: &Path) -> PathBuf {
    user_path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join("advanced.toml")
}

fn read(path: &Path) -> Result<String, ConfigError> {
    std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_owned(),
        source,
    })
}

fn read_or_create(path: &Path, template: &str) -> Result<(String, bool), ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(source) => return Ok((source, false)),
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
    file.write_all(template.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|source| ConfigError::Write {
            path: path.to_owned(),
            source,
        })?;
    Ok((template.to_owned(), true))
}

fn looks_like_legacy(source: &str) -> bool {
    source.lines().any(|line| line.trim() == "[performance]")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyConfig {
    server: LegacyServer,
    performance: LegacyPerformance,
    network: LegacyNetwork,
    logging: LoggingConfig,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyServer {
    address: String,
    port: u16,
    motd: String,
    max_players: u32,
    view_distance: u8,
    simulation_distance: u8,
    online_mode: bool,
    #[serde(default = "default_true")]
    fetch_profile_textures: bool,
    compression_threshold: i32,
    world: PathBuf,
    world_generator: WorldGenerator,
    world_seed: i64,
    difficulty: Difficulty,
    default_gamemode: GameMode,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyPerformance {
    worker_threads: usize,
    chunk_io_threads: usize,
    max_loaded_chunks: usize,
    max_packets_per_tick: usize,
    autosave_interval_seconds: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyNetwork {
    max_packet_size: usize,
    max_connections: usize,
    packet_timeout_seconds: u64,
    shutdown_timeout_seconds: u64,
}

impl Config {
    fn parse_legacy(source: &str, path: &Path) -> Result<Self, ConfigError> {
        let old: LegacyConfig = toml::from_str(source).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })?;
        let config = Self {
            server: ServerConfig {
                bind_address: old.server.address,
                port: old.server.port,
                motd: old.server.motd,
                max_players: old.server.max_players,
            },
            gameplay: GameplayConfig {
                online_mode: old.server.online_mode,
                fetch_profile_textures: old.server.fetch_profile_textures,
                difficulty: old.server.difficulty,
                default_game_mode: old.server.default_gamemode,
                view_distance_chunks: old.server.view_distance,
                simulation_distance_chunks: old.server.simulation_distance,
            },
            world: WorldConfig {
                path: old.server.world,
                seed: old.server.world_seed,
                generator: old.server.world_generator,
                autosave_interval_seconds: old.performance.autosave_interval_seconds,
            },
            logging: old.logging,
            advanced: AdvancedConfig {
                runtime: RuntimeConfig {
                    worker_threads: old.performance.worker_threads,
                    chunk_io_max_blocking_threads: old.performance.chunk_io_threads,
                },
                network: NetworkConfig {
                    max_uncompressed_packet_bytes: old.network.max_packet_size,
                    max_connections: old.network.max_connections,
                    network_read_timeout_seconds: old.network.packet_timeout_seconds,
                    shutdown_timeout_seconds: old.network.shutdown_timeout_seconds,
                    compression_threshold_bytes: old.server.compression_threshold,
                },
                world: AdvancedWorldConfig {
                    chunk_cache_max_chunks: old.performance.max_loaded_chunks,
                },
                updates: UpdateConfig {
                    scheduled_updates_per_tick: old.performance.max_packets_per_tick,
                },
                broadcast: BroadcastConfig {
                    world_event_capacity: old.performance.max_packets_per_tick.clamp(64, 65_536),
                },
            },
            migrated_legacy_config: true,
        };
        config.validate()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{Config, ConfigError, DEFAULT_ADVANCED_CONFIG, DEFAULT_CONFIG};

    #[test]
    fn checked_in_templates_parse_and_match_defaults() -> Result<(), ConfigError> {
        let config = Config::parse_files(DEFAULT_CONFIG, DEFAULT_ADVANCED_CONFIG)?;
        assert_eq!(config.server.port, 25_565);
        assert_eq!(config.bind_address()?.to_string(), "0.0.0.0:25565");
        assert_eq!(config.gameplay.view_distance_chunks, 10);
        assert_eq!(config.advanced.world.chunk_cache_max_chunks, 4096);
        assert!(DEFAULT_CONFIG.contains("# Maximum number of players"));
        assert!(DEFAULT_ADVANCED_CONFIG.contains("Normal users usually do not need"));
        Ok(())
    }

    #[test]
    fn rejects_unknown_and_invalid_values_in_both_files() {
        let unknown = DEFAULT_CONFIG.replace("motd =", "mystery = true\nmotd =");
        assert!(matches!(
            Config::parse_files(&unknown, DEFAULT_ADVANCED_CONFIG),
            Err(ConfigError::Parse { .. })
        ));
        let invalid_enum =
            DEFAULT_CONFIG.replace("difficulty = \"normal\"", "difficulty = \"nightmare\"");
        assert!(matches!(
            Config::parse_files(&invalid_enum, DEFAULT_ADVANCED_CONFIG),
            Err(ConfigError::Parse { .. })
        ));
        let invalid_advanced = DEFAULT_ADVANCED_CONFIG.replace(
            "scheduled_updates_per_tick = 1000",
            "scheduled_updates_per_tick = 0",
        );
        assert!(matches!(
            Config::parse_files(DEFAULT_CONFIG, &invalid_advanced),
            Err(ConfigError::Validation(_))
        ));
    }

    #[test]
    fn validates_cache_against_view_distance() {
        let user = DEFAULT_CONFIG.replace("view_distance_chunks = 10", "view_distance_chunks = 20");
        let advanced = DEFAULT_ADVANCED_CONFIG.replace(
            "chunk_cache_max_chunks = 4096",
            "chunk_cache_max_chunks = 1000",
        );
        assert!(matches!(
            Config::parse_files(&user, &advanced),
            Err(ConfigError::Validation(_))
        ));
    }

    #[test]
    fn creates_both_commented_files_without_machine_specific_paths() -> Result<(), ConfigError> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let directory =
            std::env::temp_dir().join(format!("toucan-config-{}-{nonce}", std::process::id()));
        let path = directory.join("nested/toucan.toml");
        let (_, created) = Config::load_or_create(&path)?;
        assert!(created);
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some(DEFAULT_CONFIG)
        );
        assert_eq!(
            std::fs::read_to_string(path.with_file_name("advanced.toml"))
                .ok()
                .as_deref(),
            Some(DEFAULT_ADVANCED_CONFIG)
        );
        let (_, created_again) = Config::load_or_create(&path)?;
        assert!(!created_again);
        let _ = std::fs::remove_dir_all(directory);
        Ok(())
    }

    #[test]
    fn maps_legacy_single_file_without_discarding_values() -> Result<(), ConfigError> {
        let legacy = r#"
[server]
address = "127.0.0.1"
port = 25570
motd = "Legacy"
max_players = 12
view_distance = 6
simulation_distance = 5
online_mode = false
fetch_profile_textures = false
compression_threshold = 128
world = "legacy-world"
world_generator = "flat"
world_seed = 99
difficulty = "hard"
default_gamemode = "creative"
[performance]
worker_threads = 2
chunk_io_threads = 3
max_loaded_chunks = 1024
max_packets_per_tick = 77
autosave_interval_seconds = 120
[network]
max_packet_size = 1048576
max_connections = 100
packet_timeout_seconds = 8
shutdown_timeout_seconds = 20
[logging]
level = "debug"
format = "pretty"
"#;
        let config = Config::parse_legacy(legacy, std::path::Path::new("legacy.toml"))?;
        assert!(config.migrated_legacy_config());
        assert_eq!(config.server.port, 25_570);
        assert_eq!(config.world.seed, 99);
        assert_eq!(config.advanced.updates.scheduled_updates_per_tick, 77);
        Ok(())
    }
}
