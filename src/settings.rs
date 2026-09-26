//! Application settings — the `settings.toml` behind Settings' seven panes.
//!
//! A sibling of `favourites.toml`, written through the same atomic temp-and-rename
//! dance, and deliberately a *separate* file so a settings write can never rewrite the
//! favourites list (handoff decision 5). The schema is the handoff README's, verbatim.
//!
//! `[defaults] username` used to live in `favourites.toml`; on first load it migrates
//! here (see [`Settings::load_or_default`] + [`Settings::adopt_username`]) and the
//! favourites copy stops being read.
//!
//! **No secrets.** Settings carry preferences and paths, never credentials, and nothing
//! here may gain a password field.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How a session window opens when nothing more specific asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowMode {
    #[default]
    Fullscreen,
    Explicit,
}

/// Clipboard sharing direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardDirection {
    #[default]
    Both,
    ToRemote,
    FromRemote,
    Off,
}

/// Connect-stage logging level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageLogLevel {
    Off,
    #[default]
    Stages,
    Verbose,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DefaultsSettings {
    /// Account used when neither a flag nor a favourite names one. Migrated from
    /// `favourites.toml`'s `[defaults]` on first load.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub port: u16,
    pub window: WindowMode,
    pub width: u16,
    pub height: u16,
    pub keep_launcher_open: bool,
    pub reconnect_last: bool,
    /// Transport preference when the favourite does not say (and for bare hosts):
    /// probe for rhydra (`auto`), insist on it (`always`), or never probe (`never`).
    pub native: crate::favourites::NativeMode,
}

impl Default for DefaultsSettings {
    fn default() -> Self {
        DefaultsSettings {
            username: None,
            port: crate::favourites::DEFAULT_PORT,
            window: WindowMode::Fullscreen,
            width: 1920,
            height: 1080,
            keep_launcher_open: true,
            reconnect_last: false,
            native: crate::favourites::NativeMode::Auto,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphicsSettings {
    pub clear_codec: bool,
    pub rfx_progressive: bool,
    pub allow_uncompressed: bool,
    pub dynamic_resolution: bool,
    /// On a monitor past the H.264 encoder ceiling (4096x2304), fullscreen drops to an
    /// integer division of native resolution for pixel-exact 2x presentation, instead
    /// of the fractional best fit. See `session::fullscreen_request`.
    pub integer_fullscreen_fit: bool,
}

impl Default for GraphicsSettings {
    fn default() -> Self {
        GraphicsSettings {
            clear_codec: true,
            rfx_progressive: true,
            allow_uncompressed: false,
            dynamic_resolution: true,
            integer_fullscreen_fit: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyboardSettings {
    /// Resolve printable keycaps through the local Mac layout and send Unicode input.
    /// Shortcuts, navigation and editing keys remain positional scancodes.
    pub mac_keyboard_mode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    pub playback: bool,
    /// Offer the default local input device after the RDP host sends OPEN.
    pub microphone: bool,
    pub device: String,
}

impl Default for AudioSettings {
    fn default() -> Self {
        AudioSettings {
            playback: true,
            microphone: false,
            device: "default".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipboardSettings {
    pub direction: ClipboardDirection,
    pub max_image_bytes: u64,
    pub timeout_secs: u64,
}

impl Default for ClipboardSettings {
    fn default() -> Self {
        ClipboardSettings {
            direction: ClipboardDirection::Both,
            max_image_bytes: 64 * 1024 * 1024,
            timeout_secs: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosticsSettings {
    pub overlay_on_connect: bool,
    pub metrics_dir: String,
    pub stage_log: StageLogLevel,
}

impl Default for DiagnosticsSettings {
    fn default() -> Self {
        DiagnosticsSettings {
            overlay_on_connect: false,
            metrics_dir: "~/mdrdp/runs".to_owned(),
            stage_log: StageLogLevel::Stages,
        }
    }
}

/// The whole settings file. Every table and field is defaulted, so a partial or absent
/// file always loads to something usable and an old build reads a newer file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub defaults: DefaultsSettings,
    pub graphics: GraphicsSettings,
    pub keyboard: KeyboardSettings,
    pub audio: AudioSettings,
    pub clipboard: ClipboardSettings,
    pub diagnostics: DiagnosticsSettings,
}

#[derive(Debug)]
pub enum SettingsError {
    /// The file exists but is not valid settings TOML. Left untouched on disk.
    Malformed {
        path: PathBuf,
        source: toml::de::Error,
    },
    Io(io::Error),
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::Malformed { path, source } => write!(
                f,
                "{} is not valid settings TOML ({source}) — refusing to overwrite it; \
                 fix or remove the file by hand",
                path.display()
            ),
            SettingsError::Io(e) => write!(f, "settings I/O error: {e}"),
        }
    }
}

impl std::error::Error for SettingsError {}

impl From<io::Error> for SettingsError {
    fn from(e: io::Error) -> Self {
        SettingsError::Io(e)
    }
}

impl Settings {
    /// `settings.toml` beside `favourites.toml`, whatever platform directory that is.
    pub fn default_path() -> Result<PathBuf, crate::favourites::FavouritesError> {
        Ok(crate::favourites::Favourites::default_path()?.with_file_name("settings.toml"))
    }

    /// Load from `path`; an absent file is the defaults, a malformed one is an error —
    /// settings are user data, and silently replacing them would lose real choices.
    pub fn load_from(path: &Path) -> Result<Settings, SettingsError> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
            Err(e) => return Err(e.into()),
        };
        toml::from_str(&text).map_err(|source| SettingsError::Malformed {
            path: path.to_owned(),
            source,
        })
    }

    /// Adopt a username migrated from `favourites.toml`'s legacy `[defaults]` table.
    ///
    /// Only fills a hole: a username already present in `settings.toml` wins over the
    /// legacy location. Returns whether anything changed (i.e. whether saving is due).
    pub fn adopt_username(&mut self, legacy: Option<&str>) -> bool {
        match (&self.defaults.username, legacy) {
            (None, Some(name)) if !name.is_empty() => {
                self.defaults.username = Some(name.to_owned());
                true
            }
            _ => false,
        }
    }

    /// Atomic save: temp file in the target directory, fsync, rename — the same dance
    /// as `Favourites::save_to`, for the same crash-safety reason.
    pub fn save_to(&self, path: &Path) -> Result<(), SettingsError> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir)?;
        let text = toml::to_string_pretty(self)
            .expect("Settings serializes without error: plain fields only");
        let tmp_path = dir.join(format!(
            ".settings.toml.tmp.{}.{}",
            std::process::id(),
            nonce()
        ));
        {
            let mut f = fs::File::create(&tmp_path)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        let renamed = fs::rename(&tmp_path, path);
        if renamed.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        renamed.map_err(Into::into)
    }
}

fn nonce() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mdrdp-settings-test-{}", nonce()));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn an_absent_file_loads_as_the_documented_defaults() {
        let s = Settings::load_from(Path::new("/definitely/not/a/real/settings.toml"))
            .expect("absent file is defaults");
        assert_eq!(s.defaults.port, 3389);
        assert_eq!(s.defaults.window, WindowMode::Fullscreen);
        assert_eq!(s.defaults.width, 1920);
        assert!(s.defaults.keep_launcher_open);
        assert!(!s.defaults.reconnect_last);
        assert!(s.graphics.clear_codec);
        assert!(s.graphics.rfx_progressive);
        assert!(!s.graphics.allow_uncompressed);
        assert!(s.graphics.dynamic_resolution);
        assert!(s.graphics.integer_fullscreen_fit);
        assert!(!s.keyboard.mac_keyboard_mode);
        assert!(s.audio.playback);
        assert!(!s.audio.microphone);
        assert_eq!(s.audio.device, "default");
        assert_eq!(s.clipboard.direction, ClipboardDirection::Both);
        assert_eq!(s.clipboard.max_image_bytes, 64 * 1024 * 1024);
        assert_eq!(s.clipboard.timeout_secs, 5);
        assert!(!s.diagnostics.overlay_on_connect);
        assert_eq!(s.diagnostics.stage_log, StageLogLevel::Stages);
    }

    #[test]
    fn the_readme_schema_parses_field_for_field() {
        // The handoff README's example, verbatim — the schema is a published contract.
        let text = r#"
[defaults]
username = "alice"
port = 3389
window = "fullscreen"
width = 1920
height = 1080
keep_launcher_open = true
reconnect_last = false

[graphics]
clear_codec = true
rfx_progressive = true
allow_uncompressed = false
dynamic_resolution = true

[keyboard]
mac_keyboard_mode = false

[audio]
playback = true
device = "default"
microphone = false

[clipboard]
direction = "both"
max_image_bytes = 1048576
timeout_secs = 5

[diagnostics]
overlay_on_connect = false
metrics_dir = "~/mdrdp/runs"
stage_log = "stages"
"#;
        let s: Settings = toml::from_str(text).expect("README schema parses");
        assert_eq!(s.defaults.username.as_deref(), Some("alice"));
        assert_eq!(s.clipboard.max_image_bytes, 1_048_576);
        assert_eq!(s.diagnostics.metrics_dir, "~/mdrdp/runs");
    }

    #[test]
    fn microphone_preference_round_trips_and_defaults_off() {
        let configured: Settings =
            toml::from_str("[audio]\nplayback = true\ndevice = \"default\"\nmicrophone = true\n")
                .expect("settings parse");
        let saved = toml::to_string(&configured).expect("settings serialize");
        assert!(saved.contains("microphone = true"));

        let defaults = toml::to_string(&Settings::default()).expect("defaults serialize");
        assert!(defaults.contains("microphone = false"));
    }

    #[test]
    fn a_round_trip_preserves_distinct_values_in_every_table() {
        let dir = tmpdir();
        let path = dir.join("settings.toml");
        let mut s = Settings::default();
        s.defaults.username = Some("bob".to_owned());
        s.defaults.port = 3391;
        s.defaults.window = WindowMode::Explicit;
        s.defaults.width = 2560;
        s.defaults.height = 1440;
        s.defaults.keep_launcher_open = false;
        s.defaults.reconnect_last = true;
        s.graphics.clear_codec = false;
        s.graphics.allow_uncompressed = true;
        s.graphics.integer_fullscreen_fit = false;
        s.keyboard.mac_keyboard_mode = true;
        s.audio.playback = false;
        s.audio.microphone = true;
        s.audio.device = "USB Audio".to_owned();
        s.clipboard.direction = ClipboardDirection::ToRemote;
        s.clipboard.max_image_bytes = 2_097_152;
        s.clipboard.timeout_secs = 9;
        s.diagnostics.overlay_on_connect = true;
        s.diagnostics.metrics_dir = "/tmp/runs".to_owned();
        s.diagnostics.stage_log = StageLogLevel::Verbose;

        s.save_to(&path).expect("save");
        let loaded = Settings::load_from(&path).expect("load");
        assert_eq!(loaded, s);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn a_malformed_file_is_an_error_not_silently_default() {
        let dir = tmpdir();
        let path = dir.join("settings.toml");
        fs::write(&path, "port = \"not a table\"\n[defaults\n").expect("write junk");
        assert!(matches!(
            Settings::load_from(&path),
            Err(SettingsError::Malformed { .. })
        ));
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn migration_fills_a_hole_but_never_overwrites() {
        let mut s = Settings::default();
        assert!(s.adopt_username(Some("legacy@example.com")));
        assert_eq!(s.defaults.username.as_deref(), Some("legacy@example.com"));

        // Already set: the settings copy wins and nothing is due for saving.
        assert!(!s.adopt_username(Some("other@example.com")));
        assert_eq!(s.defaults.username.as_deref(), Some("legacy@example.com"));

        let mut empty = Settings::default();
        assert!(!empty.adopt_username(None));
        assert!(!empty.adopt_username(Some("")));
        assert_eq!(empty.defaults.username, None);
    }

    #[test]
    fn a_partial_file_defaults_the_rest() {
        let s: Settings =
            toml::from_str("[clipboard]\ntimeout_secs = 30\n").expect("partial parses");
        assert_eq!(s.clipboard.timeout_secs, 30);
        assert_eq!(
            s.clipboard.max_image_bytes,
            64 * 1024 * 1024,
            "sibling defaulted"
        );
        assert_eq!(s.defaults.port, 3389, "other tables defaulted");
    }
}
