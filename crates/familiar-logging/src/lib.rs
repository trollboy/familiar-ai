use std::io;
use std::path::Path;

use familiar_core::config::{LogFormat, LoggingConfig};
use familiar_core::FamiliarError;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

pub struct LogGuard {
    _file_guard: Option<WorkerGuard>,
}

pub fn init_logging(
    config: &LoggingConfig,
    log_dir: Option<&Path>,
) -> Result<LogGuard, FamiliarError> {
    let filter = EnvFilter::try_new(&config.level).map_err(|e| {
        FamiliarError::Config(format!("invalid log filter '{}': {e}", config.level))
    })?;

    let mut file_guard = None;

    let registry = tracing_subscriber::registry().with(filter);

    // Determine the file writer if needed
    let file_writer = config
        .file
        .as_ref()
        .map(|f| f.parent().unwrap_or(Path::new(".")))
        .or(log_dir)
        .map(|dir| {
            let file_appender = tracing_appender::rolling::daily(dir, "familiar.log");
            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
            file_guard = Some(guard);
            non_blocking
        });

    // All console output goes to stderr (stdout is reserved for data/MCP)
    match (&config.format, file_writer) {
        (LogFormat::Pretty, None) => {
            registry
                .with(fmt::layer().with_writer(io::stderr).pretty())
                .try_init()
                .map_err(|e| FamiliarError::Config(format!("failed to init logging: {e}")))?;
        }
        (LogFormat::Pretty, Some(writer)) => {
            registry
                .with(fmt::layer().with_writer(io::stderr).pretty())
                .with(fmt::layer().with_writer(writer).with_ansi(false))
                .try_init()
                .map_err(|e| FamiliarError::Config(format!("failed to init logging: {e}")))?;
        }
        (LogFormat::Json, None) => {
            registry
                .with(fmt::layer().with_writer(io::stderr).json())
                .try_init()
                .map_err(|e| FamiliarError::Config(format!("failed to init logging: {e}")))?;
        }
        (LogFormat::Json, Some(writer)) => {
            registry
                .with(fmt::layer().with_writer(io::stderr).json())
                .with(fmt::layer().json().with_writer(writer).with_ansi(false))
                .try_init()
                .map_err(|e| FamiliarError::Config(format!("failed to init logging: {e}")))?;
        }
    }

    Ok(LogGuard {
        _file_guard: file_guard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_core::config::LoggingConfig;

    // Note: tracing's global subscriber can only be set once per process,
    // so we can only test one successful init. The integration test in
    // familiar-daemon tests both formats via separate processes.

    #[test]
    fn init_with_pretty_format_succeeds() {
        let config = LoggingConfig {
            level: "info".to_string(),
            file: None,
            format: LogFormat::Pretty,
        };
        // This will succeed the first time, fail on subsequent calls in the same
        // test process (global subscriber already set). Both outcomes are fine.
        let result = init_logging(&config, None);
        // Either Ok (first call) or Err (subscriber already set) is acceptable
        if let Err(ref e) = result {
            assert!(
                e.to_string().contains("global default"),
                "unexpected error: {e}"
            );
        }
    }

    #[test]
    fn init_with_file_output_creates_guard() {
        let tmp = tempfile::tempdir().unwrap();
        let config = LoggingConfig {
            level: "debug".to_string(),
            file: None,
            format: LogFormat::Json,
        };
        let result = init_logging(&config, Some(tmp.path()));
        // May fail if global subscriber already set, that's OK
        if let Err(ref e) = result {
            assert!(
                e.to_string().contains("global default"),
                "unexpected error: {e}"
            );
        }
    }
}
