use std::ffi::OsStr;
use std::process::Command;

pub const PKG_CONFIG_ENV: &str = "FAMILIAR_AI_TRAY_PKG_CONFIG";

pub fn probe_linux_gtk(program: &OsStr) -> Result<(), String> {
    let status = Command::new(program)
        .args(["--exists", "--atleast-version=3.0", "gtk+-3.0"])
        .status()
        .map_err(|error| diagnostic(&format!("could not run pkg-config: {error}")))?;

    if status.success() {
        Ok(())
    } else {
        Err(diagnostic("pkg-config could not resolve gtk+-3.0 >= 3.0"))
    }
}

fn diagnostic(detail: &str) -> String {
    format!(
        "Familiar's Linux tray requires GTK 3 development files; install \
         `pkg-config libgtk-3-dev` on Debian/Ubuntu or `pkgconf-pkg-config \
         gtk3-devel` on Fedora ({detail})"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn removed_gtk_dependency_produces_an_early_named_diagnostic() {
        assert_eq!(PKG_CONFIG_ENV, "FAMILIAR_AI_TRAY_PKG_CONFIG");
        let error = probe_linux_gtk(OsStr::new("/usr/bin/false"))
            .expect_err("a deliberately absent GTK probe must fail");

        assert!(error.contains("Linux tray requires GTK 3"), "{error}");
        assert!(error.contains("libgtk-3-dev"), "{error}");
        assert!(error.contains("gtk3-devel"), "{error}");
        assert!(
            error.contains("pkg-config could not resolve gtk+-3.0"),
            "{error}"
        );
    }
}
