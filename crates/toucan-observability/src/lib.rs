use thiserror::Error;
use toucan_config::{LogFormat, LoggingConfig};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::FmtSpan;

#[derive(Debug, Error)]
pub enum ObservabilityError {
    #[error("invalid logging filter: {0}")]
    Filter(#[from] tracing_subscriber::filter::ParseError),
    #[error("a global tracing subscriber is already installed")]
    AlreadyInitialized,
}

pub fn init(config: &LoggingConfig) -> Result<(), ObservabilityError> {
    let filter = EnvFilter::try_new(config.level.as_str())?;
    let result = match config.format {
        LogFormat::Pretty => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_thread_ids(false)
            .with_span_events(FmtSpan::NONE)
            .compact()
            .try_init(),
        LogFormat::Json => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_span_events(FmtSpan::NONE)
            .json()
            .try_init(),
    };
    result.map_err(|_| ObservabilityError::AlreadyInitialized)
}
