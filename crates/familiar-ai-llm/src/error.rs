#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("backend not loaded")]
    NotLoaded,
    #[error("configuration error: {0}")]
    Config(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("backend error: {0}")]
    Backend(String),
    #[error("unhealthy: {0}")]
    Unhealthy(String),
    #[error("timeout")]
    Timeout,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_not_loaded() {
        assert_eq!(LlmError::NotLoaded.to_string(), "backend not loaded");
    }

    #[test]
    fn display_config() {
        assert_eq!(
            LlmError::Config("bad".into()).to_string(),
            "configuration error: bad"
        );
    }

    #[test]
    fn display_transport() {
        assert_eq!(
            LlmError::Transport("conn refused".into()).to_string(),
            "transport error: conn refused"
        );
    }

    #[test]
    fn display_backend() {
        assert_eq!(
            LlmError::Backend("oops".into()).to_string(),
            "backend error: oops"
        );
    }

    #[test]
    fn display_unhealthy() {
        assert_eq!(
            LlmError::Unhealthy("dead".into()).to_string(),
            "unhealthy: dead"
        );
    }

    #[test]
    fn display_timeout() {
        assert_eq!(LlmError::Timeout.to_string(), "timeout");
    }
}
