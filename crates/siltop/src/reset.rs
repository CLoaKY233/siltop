//! `siltop reset` deletes this Mac's config, state and history, and
//! unloads and removes the background agent's `LaunchAgent`.
//! It asks for the word `reset` unless `--yes` was given.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::cli::EXIT_ERROR;

const CONFIRM: &str = "reset";

/// The directories and files a reset removes, when they exist.
#[must_use]
pub fn targets() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) =
        crate::config_io::default_path().and_then(|path| path.parent().map(Path::to_path_buf))
    {
        out.push(dir);
    }
    if let Some(dir) =
        crate::store_disk::default_root().and_then(|path| path.parent().map(Path::to_path_buf))
    {
        out.push(dir);
    }
    out
}

/// Delete `targets`. Missing paths are fine.
pub fn wipe(paths: &[PathBuf]) -> io::Result<()> {
    for path in paths {
        remove(path)?;
    }
    Ok(())
}

fn remove(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// What the confirmation says is about to go.
pub const WARNING: &str = "siltop: this deletes config, state and history, and removes the background agent (LaunchAgent).";

/// `agent uninstall`: `launchctl bootout` and delete the plist. Its
/// "agent removed" line is kept only when there was an agent to remove.
fn remove_agent(out: &mut dyn Write) {
    let mut said = Vec::new();
    let mut quiet = Vec::new();
    crate::agent::manage(
        crate::cli::AgentAction::Uninstall,
        None,
        &mut said,
        &mut quiet,
    );
    let said = String::from_utf8_lossy(&said);
    if said.trim() == "agent removed" {
        let _ = writeln!(out, "siltop: background agent removed");
    }
}

pub fn command(yes: bool, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let paths = targets();
    if !yes {
        let _ = writeln!(err, "{WARNING}");
        let _ = writeln!(err, "siltop: type {CONFIRM} to continue");
        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() || line.trim() != CONFIRM {
            let _ = writeln!(err, "siltop: reset cancelled");
            return EXIT_ERROR;
        }
    }
    // First, so a running agent does not write its state back after the wipe.
    remove_agent(out);
    match wipe(&paths) {
        Ok(()) => {
            let _ = writeln!(out, "siltop: local data deleted");
            0
        }
        Err(error) => {
            let _ = writeln!(err, "siltop: {error}");
            EXIT_ERROR
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_confirmation_names_the_launch_agent() {
        assert!(WARNING.contains("LaunchAgent"));
        assert!(WARNING.contains("history"));
    }

    #[test]
    fn wipe_removes_a_config_and_a_state_dir_and_ignores_a_missing_one() {
        let root = std::env::temp_dir().join(format!("siltop-reset-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let config = root.join("config");
        let state = root.join("state");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(state.join("history")).unwrap();
        fs::write(config.join("config.toml"), "zen = true\n").unwrap();
        fs::write(state.join("updates.toml"), "skipped=\n").unwrap();
        let missing = root.join("nope");
        wipe(&[config.clone(), state.clone(), missing]).unwrap();
        assert!(!config.exists());
        assert!(!state.exists());
        let _ = fs::remove_dir_all(&root);
    }
}
