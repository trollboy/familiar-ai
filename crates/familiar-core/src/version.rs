use std::fmt;

#[derive(Debug, Clone)]
pub struct VersionInfo {
    pub version: &'static str,
    pub git_sha: Option<&'static str>,
    pub build_date: Option<&'static str>,
    pub rust_version: Option<&'static str>,
}

impl VersionInfo {
    pub fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            git_sha: option_env!("FAMILIAR_GIT_SHA"),
            build_date: option_env!("FAMILIAR_BUILD_DATE"),
            rust_version: option_env!("FAMILIAR_RUST_VERSION"),
        }
    }
}

impl fmt::Display for VersionInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "familiar {}", self.version)?;
        if let Some(sha) = self.git_sha {
            write!(f, " ({sha})")?;
        }
        if let Some(date) = self.build_date {
            write!(f, " built {date}")?;
        }
        if let Some(rustc) = self.rust_version {
            write!(f, " [rustc {rustc}]")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_has_version() {
        let info = VersionInfo::current();
        assert!(!info.version.is_empty());
    }

    #[test]
    fn display_includes_version() {
        let info = VersionInfo::current();
        let display = format!("{info}");
        assert!(display.starts_with("familiar "));
        assert!(display.contains(info.version));
    }

    #[test]
    fn display_with_all_fields() {
        let info = VersionInfo {
            version: "0.1.0",
            git_sha: Some("abc1234"),
            build_date: Some("2026-04-06"),
            rust_version: Some("1.78.0"),
        };
        let display = format!("{info}");
        assert_eq!(
            display,
            "familiar 0.1.0 (abc1234) built 2026-04-06 [rustc 1.78.0]"
        );
    }
}
