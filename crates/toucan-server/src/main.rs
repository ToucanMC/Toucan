//! Toucan process coordinator.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use thiserror::Error;
use tokio::runtime::Builder;
use toucan_config::{Config, ConfigError};
use toucan_network::{ServerError, ToucanServer};
use toucan_observability::ObservabilityError;
use toucan_protocol::{MINECRAFT_VERSION, PROTOCOL_VERSION};
use tracing::{info, warn};

#[derive(Debug, Error)]
enum ApplicationError {
    #[error(transparent)]
    Arguments(#[from] ArgumentError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Observability(#[from] ObservabilityError),
    #[error("failed to create async runtime: {0}")]
    Runtime(#[source] std::io::Error),
    #[error(transparent)]
    Server(#[from] ServerError),
}

#[derive(Debug, Error)]
enum ArgumentError {
    #[error("--config requires a path")]
    MissingConfigPath,
    #[error("unknown argument `{0}`; use --help for usage")]
    Unknown(String),
    #[error("argument is not valid UTF-8")]
    InvalidUtf8,
}

enum StartupAction {
    Run(PathBuf),
    Help,
}

fn main() -> ExitCode {
    match start(std::env::args_os().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("toucan: {error}");
            ExitCode::FAILURE
        }
    }
}

fn start(arguments: impl Iterator<Item = OsString>) -> Result<(), ApplicationError> {
    let action = parse_arguments(arguments)?;
    let StartupAction::Run(config_path) = action else {
        print_help();
        return Ok(());
    };

    let config = Arc::new(Config::load(&config_path)?);
    toucan_observability::init(&config.logging)?;
    let mut runtime = Builder::new_multi_thread();
    runtime.enable_all();
    if config.performance.worker_threads > 0 {
        runtime.worker_threads(config.performance.worker_threads);
    }
    let runtime = runtime.build().map_err(ApplicationError::Runtime)?;

    runtime.block_on(async move {
        let server = ToucanServer::bind(Arc::clone(&config)).await?;
        let address = server.local_addr().map_err(ServerError::Io)?;
        info!(
            %address,
            minecraft_version = MINECRAFT_VERSION,
            protocol = PROTOCOL_VERSION,
            online_mode = config.server.online_mode,
            "Toucan server listening"
        );
        server
            .serve_until(async {
                if let Err(error) = shutdown_signal().await {
                    warn!(%error, "shutdown signal handler failed; stopping server");
                }
            })
            .await
            .map_err(ApplicationError::Server)
    })
}

fn parse_arguments(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<StartupAction, ArgumentError> {
    let mut config_path = PathBuf::from("config/toucan.toml");
    while let Some(argument) = arguments.next() {
        let argument = argument
            .into_string()
            .map_err(|_| ArgumentError::InvalidUtf8)?;
        match argument.as_str() {
            "--config" => {
                config_path =
                    PathBuf::from(arguments.next().ok_or(ArgumentError::MissingConfigPath)?);
            }
            "-h" | "--help" => return Ok(StartupAction::Help),
            unknown => return Err(ArgumentError::Unknown(unknown.to_owned())),
        }
    }
    Ok(StartupAction::Run(config_path))
}

fn print_help() {
    println!(
        "Toucan Minecraft server\n\nUsage: toucan-server [--config PATH]\n\nOptions:\n  --config PATH  TOML configuration (default: config/toucan.toml)\n  -h, --help     Show this help"
    );
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<(), std::io::Error> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<(), std::io::Error> {
    tokio::signal::ctrl_c().await
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;

    use super::{ArgumentError, StartupAction, parse_arguments};

    #[test]
    fn uses_default_config_path() {
        let action = parse_arguments(std::iter::empty());
        assert!(matches!(
            action,
            Ok(StartupAction::Run(path)) if path == Path::new("config/toucan.toml")
        ));
    }

    #[test]
    fn accepts_explicit_config_path() {
        let arguments = [OsString::from("--config"), OsString::from("custom.toml")];
        let action = parse_arguments(arguments.into_iter());
        assert!(matches!(
            action,
            Ok(StartupAction::Run(path)) if path == Path::new("custom.toml")
        ));
    }

    #[test]
    fn rejects_unknown_arguments() {
        let action = parse_arguments([OsString::from("--mystery")].into_iter());
        assert!(matches!(action, Err(ArgumentError::Unknown(_))));
    }
}
