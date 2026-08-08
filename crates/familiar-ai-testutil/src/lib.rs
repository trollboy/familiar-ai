use std::collections::HashMap;
use std::ffi::OsString;

pub use tempfile::TempDir;

pub fn temp_dir() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

/// Guard that restores environment variables when dropped.
pub struct EnvGuard {
    originals: HashMap<String, Option<OsString>>,
}

impl EnvGuard {
    pub fn new(vars: &[(&str, &str)]) -> Self {
        let mut originals = HashMap::new();
        for (key, value) in vars {
            originals.insert(key.to_string(), std::env::var_os(key));
            std::env::set_var(key, value);
        }
        Self { originals }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, original) in &self.originals {
            match original {
                Some(val) => std::env::set_var(key, val),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_guard_sets_and_restores() {
        let key = "FAMILIAR_AI_TEST_GUARD_VAR";
        std::env::remove_var(key);

        {
            let _guard = EnvGuard::new(&[(key, "test_value")]);
            assert_eq!(std::env::var(key).unwrap(), "test_value");
        }

        assert!(std::env::var(key).is_err());
    }
}
