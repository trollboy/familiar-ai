#[derive(Debug, thiserror::Error)]
pub enum FamiliarError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("daemon is already running")]
    AlreadyRunning,

    #[error("shutdown error: {0}")]
    Shutdown(String),

    #[error("database error: {0}")]
    Database(String),

    #[error("watcher error: {0}")]
    Watcher(String),

    #[error("MCP error: {0}")]
    Mcp(String),

    #[error("LLM error: {0}")]
    Llm(String),
}

pub type Result<T> = std::result::Result<T, FamiliarError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_config_error() {
        let err = FamiliarError::Config("bad value".into());
        assert_eq!(err.to_string(), "configuration error: bad value");
    }

    #[test]
    fn display_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err = FamiliarError::Io(io_err);
        assert!(err.to_string().starts_with("I/O error:"));
    }

    #[test]
    fn display_already_running() {
        let err = FamiliarError::AlreadyRunning;
        assert_eq!(err.to_string(), "daemon is already running");
    }

    #[test]
    fn display_shutdown_error() {
        let err = FamiliarError::Shutdown("timeout".into());
        assert_eq!(err.to_string(), "shutdown error: timeout");
    }

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err: FamiliarError = io_err.into();
        assert!(matches!(err, FamiliarError::Io(_)));
    }

    #[test]
    fn display_database_error() {
        let err = FamiliarError::Database("connection failed".into());
        assert_eq!(err.to_string(), "database error: connection failed");
    }

    #[test]
    fn display_watcher_error() {
        let err = FamiliarError::Watcher("notify failed".into());
        assert_eq!(err.to_string(), "watcher error: notify failed");
    }

    #[test]
    fn display_mcp_error() {
        let err = FamiliarError::Mcp("invalid params".into());
        assert_eq!(err.to_string(), "MCP error: invalid params");
    }

    #[test]
    fn display_llm_error() {
        let err = FamiliarError::Llm("backend unhealthy".into());
        assert_eq!(err.to_string(), "LLM error: backend unhealthy");
    }

    #[test]
    fn debug_format_works() {
        let err = FamiliarError::AlreadyRunning;
        let debug = format!("{err:?}");
        assert!(debug.contains("AlreadyRunning"));
    }
}
