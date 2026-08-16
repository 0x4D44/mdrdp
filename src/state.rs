//! Remembered per-target window state — currently just "was it fullscreen?".
//!
//! This is *derived UI state*, not user data, which drives two decisions that differ
//! from `crate::favourites`:
//!
//! - A malformed file is discarded with a warning rather than refused: nothing here is
//!   irreplaceable, and refusing to start over a corrupt cache would be absurd.
//! - Nothing here is ever worth prompting about. Saving is best-effort; a failed save
//!   costs the user one fullscreen toggle on the next launch.
//!
//! Keyed by `host:port` rather than favourite name, so `mdrdp temper` and a favourite
//! pointing at `temper` share one memory, and renaming a favourite does not forget it.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One target's remembered state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetState {
    /// Whether the session window was fullscreen when it closed.
    #[serde(default)]
    pub fullscreen: bool,
}

/// On-disk shape: a table of targets keyed by `host:port`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    target: BTreeMap<String, TargetState>,
}

/// Remembered window state for every target, backed by `state.toml` in the config dir.
#[derive(Debug, Default)]
pub struct SessionState {
    targets: BTreeMap<String, TargetState>,
}

/// The canonical key for a target: `host:port`, host lowercased so `Temper` and
/// `temper` share a memory.
pub fn target_key(host: &str, port: u16) -> String {
    format!("{}:{}", host.to_lowercase(), port)
}

impl SessionState {
    /// `state.toml` next to `favourites.toml`.
    pub fn default_path() -> Option<PathBuf> {
        crate::favourites::Favourites::default_path()
            .ok()
            .map(|p| p.with_file_name("state.toml"))
    }

    /// Load from `path`. Missing means empty; malformed means empty **with a warning**,
    /// because this file is a cache the next save will simply rewrite.
    pub fn load_from(path: &Path) -> Self {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                eprintln!(
                    "warning: could not read {} ({e}); starting fresh",
                    path.display()
                );
                return Self::default();
            }
        };
        match toml::from_str::<StateFile>(&text) {
            Ok(file) => SessionState {
                targets: file.target,
            },
            Err(e) => {
                eprintln!(
                    "warning: {} is not valid state TOML ({e}); starting fresh",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Whether this target's window was fullscreen when it last closed. `None` means the
    /// target has never been seen.
    pub fn fullscreen_for(&self, key: &str) -> Option<bool> {
        self.targets.get(key).map(|t| t.fullscreen)
    }

    pub fn set_fullscreen(&mut self, key: &str, fullscreen: bool) {
        self.targets.entry(key.to_owned()).or_default().fullscreen = fullscreen;
    }

    /// Save to `path`, atomically (same temp-and-rename dance as the favourites file, so
    /// a crash mid-write cannot leave a truncated file).
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        use std::io::Write as _;
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir)?;

        let file = StateFile {
            target: self.targets.clone(),
        };
        let text = toml::to_string_pretty(&file)
            .expect("TargetState serializes without error: plain bools under string keys");

        let tmp_path = dir.join(format!(".state.toml.tmp.{}", std::process::id()));
        {
            let mut f = fs::File::create(&tmp_path)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        let renamed = fs::rename(&tmp_path, path);
        if renamed.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        renamed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "mdrdp-state-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&base).expect("create temp dir");
        base
    }

    #[test]
    fn fullscreen_state_round_trips_per_target() {
        let dir = tmpdir();
        let path = dir.join("state.toml");

        let mut state = SessionState::default();
        state.set_fullscreen(&target_key("temper", 3389), true);
        state.set_fullscreen(&target_key("other", 3390), false);
        state.save_to(&path).expect("save");

        let loaded = SessionState::load_from(&path);
        assert_eq!(loaded.fullscreen_for("temper:3389"), Some(true));
        assert_eq!(loaded.fullscreen_for("other:3390"), Some(false));
        assert_eq!(
            loaded.fullscreen_for("never-seen:3389"),
            None,
            "an unseen target has no remembered state"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_key_ignores_host_case_but_not_port() {
        assert_eq!(target_key("Temper", 3389), target_key("temper", 3389));
        assert_ne!(target_key("temper", 3389), target_key("temper", 3390));
    }

    #[test]
    fn a_missing_file_is_empty_and_a_malformed_file_starts_fresh() {
        let dir = tmpdir();
        let missing = SessionState::load_from(&dir.join("nope.toml"));
        assert_eq!(missing.fullscreen_for("x:1"), None);

        let bad = dir.join("bad.toml");
        fs::write(&bad, "not [ valid { toml").unwrap();
        let fresh = SessionState::load_from(&bad);
        assert_eq!(
            fresh.fullscreen_for("x:1"),
            None,
            "state is a cache; malformed must not be fatal"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn updating_one_target_leaves_the_others_alone() {
        let mut state = SessionState::default();
        state.set_fullscreen("a:1", true);
        state.set_fullscreen("b:1", true);
        state.set_fullscreen("a:1", false);
        assert_eq!(state.fullscreen_for("a:1"), Some(false));
        assert_eq!(state.fullscreen_for("b:1"), Some(true));
    }
}
