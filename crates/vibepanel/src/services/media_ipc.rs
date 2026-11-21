//! State file for CLI ↔ MediaService communication.
//!
//! The panel writes the currently active player to a state file so that
//! CLI commands (`vibepanel media play-pause`, etc.) can control the same
//! player that's shown in the UI.
//!
//! State file: `$XDG_RUNTIME_DIR/vibepanel-media-state` contains:
//! - Line 1: active player bus name (or empty if none)

use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::debug;

/// Get the state file path.
fn state_file_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir).join("vibepanel-media-state")
    } else {
        PathBuf::from("/tmp/vibepanel-media-state")
    }
}

/// Write current media state to the state file.
///
/// Called by MediaService when the active player changes.
pub fn write_state(active_bus_name: Option<&str>) {
    let path = state_file_path();
    write_state_at(&path, active_bus_name.unwrap_or(""));
}

/// Write `content` by creating a fresh temp file and renaming it into place.
///
/// `create_new` refuses an existing (including symlinked) temp name, and rename
/// replaces the destination entry itself rather than following a symlink there.
fn write_state_at(path: &Path, content: &str) {
    let Some(parent) = path.parent() else {
        debug!("Media state: state path has no parent directory");
        return;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(
        ".vibepanel-media-state.tmp.{}.{nanos}",
        std::process::id()
    ));

    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(content.as_bytes())?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        debug!("Media state: failed to write state file: {}", e);
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Read current active player from the state file.
///
/// Returns `None` if file doesn't exist or is empty.
pub fn read_state() -> Option<String> {
    let path = state_file_path();
    std::fs::read_to_string(&path).ok().and_then(|s| {
        let s = s.trim();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("vp-media-ipc-test-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_replaces_existing_state() {
        let dir = unique_dir();
        let path = dir.join("vibepanel-media-state");
        std::fs::write(&path, "old-player").unwrap();

        write_state_at(&path, "new-player");

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new-player");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_does_not_follow_symlink_at_state_path() {
        let dir = unique_dir();
        let victim = dir.join("victim");
        std::fs::write(&victim, "do-not-touch").unwrap();
        let path = dir.join("vibepanel-media-state");
        std::os::unix::fs::symlink(&victim, &path).unwrap();

        write_state_at(&path, "new-player");

        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "do-not-touch");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new-player");
        assert!(!path.is_symlink());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
