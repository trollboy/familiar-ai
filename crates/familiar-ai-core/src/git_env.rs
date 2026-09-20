//! Running `git` about the directory you meant.
//!
//! `-C <path>` and `Command::current_dir` decide which repository a `git`
//! invocation is about only when none of the location variables below are set
//! in the environment. Git exports them to every process a hook runs, so a
//! Familiar binary invoked from a hook — this repository's own pre-push gate,
//! for one — resolves against whatever repository git was busy with rather
//! than the path it was handed.
//!
//! This is the single definition. Every `git` invocation in the workspace goes
//! through [`git_command`], production and test alike: the leak reached the
//! tests too, where fixture repositories running `git add` and `git commit`
//! wrote to the real repository instead.

use std::path::Path;
use std::process::{Command, Stdio};

/// The variables that override the directory a `git` invocation was given.
pub const GIT_LOCATION_ENV: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
];

/// A `git` command about `cwd`, with the ambient location variables cleared.
///
/// Sets `current_dir` rather than passing `-C`, so the caller's own arguments
/// stay theirs, and closes stdin: git must never block a daemon on a prompt.
pub fn git_command(cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.args(args).current_dir(cwd).stdin(Stdio::null());
    clear_location_env(&mut command);
    command
}

/// A `git` command with the ambient location variables cleared and nothing
/// else decided, for callers that pass `-C` themselves.
pub fn git_command_bare() -> Command {
    let mut command = Command::new("git");
    clear_location_env(&mut command);
    command
}

fn clear_location_env(command: &mut Command) {
    for name in GIT_LOCATION_ENV {
        command.env_remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    /// Every location variable is removed from the child's environment.
    ///
    /// Asserted against the command's configured environment rather than by
    /// setting `GIT_DIR` and running git: the process environment is global,
    /// and a test that mutates it races every other test in the binary — the
    /// exact class of cross-talk this module exists to stop.
    fn removed(command: &Command) -> Vec<&OsStr> {
        command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key)
            .collect()
    }

    #[test]
    fn git_command_clears_every_location_variable() {
        let command = git_command(Path::new("/tmp"), &["status"]);
        let removed = removed(&command);
        for name in GIT_LOCATION_ENV {
            assert!(
                removed.contains(&OsStr::new(name)),
                "{name} is still inherited"
            );
        }
    }

    #[test]
    fn the_bare_command_clears_them_too() {
        let command = git_command_bare();
        let removed = removed(&command);
        for name in GIT_LOCATION_ENV {
            assert!(
                removed.contains(&OsStr::new(name)),
                "{name} is still inherited"
            );
        }
    }

    /// The directory is the one that was asked for, and stdin is closed so a
    /// daemon can never be blocked on a git prompt.
    #[test]
    fn git_command_is_about_the_directory_it_was_given() {
        let command = git_command(Path::new("/tmp"), &["rev-parse", "--show-toplevel"]);
        assert_eq!(command.get_current_dir(), Some(Path::new("/tmp")));
        let args: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(args, vec!["rev-parse", "--show-toplevel"]);
    }

    /// The whole set resolves the same repository a plain invocation would
    /// when nothing ambient is set — clearing the variables must not change
    /// the ordinary answer.
    #[test]
    fn clearing_them_does_not_change_the_ordinary_answer() {
        let repo = tempfile::tempdir().unwrap();
        assert!(git_command(repo.path(), &["init", "-q"])
            .status()
            .unwrap()
            .success());
        let output = git_command(repo.path(), &["rev-parse", "--show-toplevel"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let resolved = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            std::fs::canonicalize(resolved.trim()).unwrap(),
            std::fs::canonicalize(repo.path()).unwrap(),
        );
    }
}
