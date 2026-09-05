//! Configure-time detection of the tray's native Linux dependency on
//! `libxdo` (pulled in transitively by `muda` for global menu accelerators).
//!
//! `libxdo-sys`'s own build script unconditionally emits
//! `cargo:rustc-link-lib=xdo` with no existence check, so a host missing the
//! library previously only found out at the very end of the build, when the
//! final `familiar-ai-daemon` binary failed to link with a bare
//! "cannot find -lxdo" from the linker. `build.rs` calls [`find_missing`]
//! before that point so the failure is a named, early diagnostic instead.

use std::path::{Path, PathBuf};

/// A native shared library this crate could not locate on the host.
pub struct MissingDependency {
    /// The library's linker name (as passed to `-l`), e.g. `"xdo"`.
    pub lib_name: &'static str,
    /// The Debian/Ubuntu package that provides it.
    pub debian_package: &'static str,
}

impl MissingDependency {
    /// The message `build.rs` panics with — names the library and the
    /// package that provides it rather than leaving the operator to decode
    /// a linker error.
    pub fn diagnostic(&self) -> String {
        format!(
            "familiar-ai-tray: the `tray` feature requires the system library \
             `lib{name}` (linker name `-l{name}`), which was not found.\n\
             Install it and re-run the build — on Debian/Ubuntu:\n\
             \n    sudo apt-get install {package}\n\n\
             This check runs at configure time specifically so a missing tray \
             dependency is a named diagnostic here rather than a bare \
             \"cannot find -l{name}\" from the linker at the end of the build.",
            name = self.lib_name,
            package = self.debian_package,
        )
    }
}

/// Environment variable that, when set, replaces the fixed system search
/// dirs entirely (colon-separated paths, `std::env::split_paths`). Exists so
/// an out-of-process integration test can force a deterministic "missing"
/// or "present" outcome regardless of what the host actually has installed
/// — see `familiar-ai-daemon/tests/tray_feature_build.rs`. Not read anywhere
/// except here; production hosts never set it.
pub const SEARCH_DIRS_OVERRIDE_ENV: &str = "FAMILIAR_AI_TRAY_XDO_SEARCH_DIRS_OVERRIDE";

/// Directories on the linker's default search path, in the order a
/// distribution linker would consult them. Kept small and explicit rather
/// than shelling out to `ld --verbose`, since the only question that matters
/// here is "does *a* copy of `libxdo.so` exist anywhere plausible."
pub fn default_search_dirs() -> Vec<PathBuf> {
    if let Some(value) = std::env::var_os(SEARCH_DIRS_OVERRIDE_ENV) {
        return std::env::split_paths(&value).collect();
    }

    let mut dirs = vec![
        PathBuf::from("/usr/lib"),
        PathBuf::from("/usr/lib64"),
        PathBuf::from("/usr/local/lib"),
        PathBuf::from("/usr/lib/x86_64-linux-gnu"),
        PathBuf::from("/usr/lib/aarch64-linux-gnu"),
    ];
    for var in ["LIBRARY_PATH", "LD_LIBRARY_PATH"] {
        if let Some(value) = std::env::var_os(var) {
            dirs.extend(std::env::split_paths(&value));
        }
    }
    dirs
}

/// True if any directory in `search_dirs` contains a copy of `libxdo` that
/// `-lxdo` can actually resolve against: the unversioned development
/// symlink `libxdo.so`, or a static archive `libxdo.a`. A runtime-only
/// versioned file such as `libxdo.so.3` does *not* count — the linker looks
/// for the unversioned name, so a host with only the versioned `.so`
/// (e.g. `libxdo3` installed without `libxdo-dev`) would pass a check that
/// accepted it and then still fail at the real link step.
fn xdo_present_in(search_dirs: &[PathBuf]) -> bool {
    search_dirs.iter().any(|dir| has_libxdo(dir))
}

fn has_libxdo(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| name == "libxdo.so" || name == "libxdo.a")
    })
}

/// Checks the given search directories for `libxdo`, returning the named
/// diagnostic if it is missing. Takes an explicit directory list (rather
/// than always calling [`default_search_dirs`]) so tests can simulate a host
/// with the dependency removed without touching the real filesystem.
pub fn find_missing(search_dirs: &[PathBuf]) -> Option<MissingDependency> {
    if xdo_present_in(search_dirs) {
        None
    } else {
        Some(MissingDependency {
            lib_name: "xdo",
            debian_package: "libxdo-dev",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_libxdo_missing_when_no_search_dir_has_it() {
        let empty = tempfile::tempdir().unwrap();
        let missing = find_missing(&[empty.path().to_path_buf()]);
        let missing = missing.expect("libxdo must be reported missing");
        assert_eq!(missing.lib_name, "xdo");
        assert!(missing.diagnostic().contains("libxdo-dev"));
        assert!(missing.diagnostic().contains("-lxdo"));
    }

    #[test]
    fn finds_bare_libxdo_so() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("libxdo.so"), []).unwrap();
        assert!(find_missing(&[dir.path().to_path_buf()]).is_none());
    }

    #[test]
    fn finds_bare_libxdo_a() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("libxdo.a"), []).unwrap();
        assert!(find_missing(&[dir.path().to_path_buf()]).is_none());
    }

    #[test]
    fn versioned_only_libxdo_so_is_reported_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("libxdo.so.3"), []).unwrap();
        let missing = find_missing(&[dir.path().to_path_buf()]);
        let missing = missing.expect("a versioned-only libxdo.so.3 must not satisfy -lxdo");
        assert!(missing.diagnostic().contains("libxdo-dev"));
    }

    #[test]
    fn search_dirs_override_env_replaces_the_fixed_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("libxdo.so"), []).unwrap();
        std::env::set_var(SEARCH_DIRS_OVERRIDE_ENV, dir.path());
        let dirs = default_search_dirs();
        std::env::remove_var(SEARCH_DIRS_OVERRIDE_ENV);
        assert_eq!(dirs, vec![dir.path().to_path_buf()]);
    }

    #[test]
    fn ignores_a_search_dir_that_does_not_exist() {
        let missing = find_missing(&[PathBuf::from("/nonexistent/familiar-ai-test-dir")]);
        assert!(missing.is_some());
    }
}
