//! Typed loading and validation for Toucan's TOML configuration.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

/// Complete server configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// User-visible and gameplay-adjacent server settings.
    pub server: ServerConfig,
    /// Runtime and future simulation limits.
    pub performance: PerformanceConfig,
    /// Connection resource limits.
    pub network: NetworkConfig,
    /// Structured logging settings.
    pub logging: LoggingConfig,
}

/// User-visible server settings.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Listener IP address.
    pub address: String,
    /// Listener TCP port. Zero requests an ephemeral port and is useful in tests.
    pub port: u16,
    /// Server-list message of the day.
    pub motd: String,
    /// Advertised player capacity.
    pub max_players: u32,
    /// Requested chunk view distance.
    pub view_distance: u8,
    /// Requested simulation distance.
    pub simulation_distance: u8,
    /// Whether Mojang session authentication will be required in Phase 2.
    pub online_mode: bool,
    /// Packet compression threshold for Phase 2, or -1 to disable compression.
    pub compression_threshold: i32,
    /// Vanilla-compatible world folder.
    pub world: PathBuf,
    /// Default world difficulty.
    pub difficulty: Difficulty,
    /// Default game mode for new players.
    pub default_gamemode: GameMode,
}

/// Runtime and simulation tuning settings.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceConfig {
    /// Tokio worker count, or zero to use Tokio's default.
    pub worker_threads: usize,
    /// Reserved chunk I/O worker count for Phase 3.
    pub chunk_io_threads: usize,
    /// Future per-tick inbound packet budget.
    pub max_packets_per_tick: usize,
    /// Future autosave interval.
    pub autosave_interval_seconds: u64,
}

/// Network safety limits.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    /// Largest accepted uncompressed packet body.
    pub max_packet_size: usize,
    /// Maximum number of concurrently serviced TCP connections.
    pub max_connections: usize,
    /// Time allowed to receive each packet.
    pub packet_timeout_seconds: u64,
    /// Time allowed for connection tasks to finish during shutdown.
    pub shutdown_timeout_seconds: u64,
}

/// Structured logging settings.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    /// Minimum emitted severity.
    pub level: LogLevel,
    /// Human-readable or machine-readable output.
    pub format: LogFormat,
}

/// Supported world difficulties.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Difficulty {
    /// Peaceful difficulty.
    Peaceful,
    /// Easy difficulty.
    Easy,
    /// Normal difficulty.
    Normal,
    /// Hard difficulty.
    Hard,
}

/// Supported default game modes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum GameMode {
    /// Survival mode.
    Survival,
    /// Creative mode.
    Creative,
    /// Adventure mode.
    Adventure,
    /// Spectator mode.
    Spectator,
}

/// Configurable logging severity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Error records only.
    Error,
    /// Warnings and errors.
    Warn,
    /// Normal operator information.
    Info,
    /// Diagnostic information.
    Debug,
    /// Very detailed tracing.
    Trace,
}

impl LogLevel {
    /// Returns the tracing filter directive for this level.
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

/// Supported log encodings.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Concise operator-facing text.
    Pretty,
    /// Newline-delimited JSON.
    Json,
}

/// A single invalid configuration value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationIssue {
    field: &'static str,
    message: String,
}

/// Collection of invalid configuration values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationErrors(Vec<ValidationIssue>);

impl ValidationErrors {
    /// Returns the number of invalid values.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns whether no invalid values were recorded.
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

/// Configuration loading failure.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The configuration file could not be read.
    #[error("failed to read configuration at {path}: {source}")]
    Read {
        /// Requested path.
        path: PathBuf,
        /// Filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// TOML syntax or shape was invalid.
    #[error("invalid TOML configuration: {0}")]
    Parse(#[from] toml::de::Error),
    /// One or more values were outside safe bounds.
    #[error("configuration validation failed: {0}")]
    Validation(ValidationErrors),
}

impl Config {
    /// Loads and validates a configuration file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let source = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&source)
    }

    /// Parses and validates TOML configuration text.
    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(source)?;
        config.validate()?;
        Ok(config)
    }

    /// Returns the validated listener address.
    pub fn bind_address(&self) -> Result<SocketAddr, ConfigError> {
        let ip = self.server.address.parse::<IpAddr>().map_err(|error| {
            ConfigError::Validation(ValidationErrors(vec![ValidationIssue::new(
                "server.address",
                error.to_string(),
            )]))
        })?;
        Ok(SocketAddr::new(ip, self.server.port))
    }

    /// Checks values that serde's type system cannot express.
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
        if self.performance.chunk_io_threads == 0 {
            issues.push(ValidationIssue::new(
                "performance.chunk_io_threads",
                "must be greater than zero",
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
    use super::{Config, ConfigError};

    const VALID: &str = include_str!("../../../config/toucan.toml");

    #[test]
    fn parses_checked_in_configuration() -> Result<(), ConfigError> {
        let config = Config::parse(VALID)?;
        assert_eq!(config.server.port, 25_565);
        assert_eq!(config.bind_address()?.to_string(), "0.0.0.0:25565");
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
}
