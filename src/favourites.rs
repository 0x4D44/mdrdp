//! Saved connections ("favourites") — data model and persistence.
//!
//! This module is deliberately free of any UI/windowing types so it stays trivially
//! testable and reusable by both the favourites launcher UI and the CLI's `resolve`
//! path. It owns *what* a favourite is and *where* it lives on disk; the list widget
//! that double-clicks one to connect lives elsewhere.
//!
//! **No passwords here.** A favourite may carry an `account` key used to look the
//! credential up in the OS keychain (see `crate::creds`), but the secret itself never
//! touches this file. `favourites.toml` is plain text on disk — storing a password in
//! it would defeat the entire point of the keychain.

use std::fmt;
use std::fs;
use std::io;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Default RDP port, used when a favourite does not specify one.
pub const DEFAULT_PORT: u16 = 3389;

/// The desired window size for a session launched from a favourite.
///
/// Mutually exclusive with an explicit size by construction — you cannot express both a
/// resolution and "fullscreen" at once, which matches how a launcher actually offers the
/// choice to a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum WindowSize {
    /// An explicit session resolution.
    Explicit { width: u16, height: u16 },
    /// Open full-screen on whichever display the session window lands on.
    #[default]
    Fullscreen,
}

/// One saved connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favourite {
    /// Display label. Unique (case-insensitive) within a `Favourites` collection,
    /// non-empty.
    pub name: String,
    /// Hostname or IP address. Non-empty.
    pub host: String,
    /// TCP port. Defaults to 3389; zero is rejected on add/rename.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Username to log on with, if known ahead of time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// NTLM/Kerberos domain, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// Desired window size for sessions launched from this favourite.
    #[serde(default)]
    pub window_size: WindowSize,
    /// Keychain account key (see `crate::creds::lookup`) used to fetch the password at
    /// connect time. `None` means "prompt" — this is never a password itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keychain_account: Option<String>,
    /// When this favourite last launched a session, as Unix seconds. Display-only —
    /// the launcher list sorts and captions with it; nothing else reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<u64>,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

impl Favourite {
    /// Construct a favourite with the given name/host and every other field at its
    /// default (port 3389, fullscreen, no username/domain/keychain account).
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Self {
        Favourite {
            name: name.into(),
            host: host.into(),
            port: DEFAULT_PORT,
            username: None,
            domain: None,
            window_size: WindowSize::default(),
            keychain_account: None,
            last_used: None,
        }
    }
}

/// Why a `Favourites` mutation or load/save was rejected.
#[derive(Debug)]
pub enum FavouritesError {
    /// `name` was empty or all whitespace.
    EmptyName,
    /// `host` was empty or all whitespace.
    EmptyHost,
    /// `port` was zero.
    ZeroPort,
    /// A favourite with this name (case-insensitive) already exists.
    DuplicateName(String),
    /// No favourite with this name exists.
    NotFound(String),
    /// Could not determine a platform configuration directory.
    NoConfigDirectory,
    /// The file exists but could not be parsed. The file is left untouched — see
    /// `Favourites::load`.
    Malformed {
        path: PathBuf,
        source: toml::de::Error,
    },
    Io(io::Error),
}

impl fmt::Display for FavouritesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FavouritesError::EmptyName => write!(f, "favourite name must not be empty"),
            FavouritesError::EmptyHost => write!(f, "favourite host must not be empty"),
            FavouritesError::ZeroPort => write!(f, "favourite port must not be zero"),
            FavouritesError::DuplicateName(name) => {
                write!(f, "a favourite named {name:?} already exists")
            }
            FavouritesError::NotFound(name) => write!(f, "no favourite named {name:?}"),
            FavouritesError::NoConfigDirectory => {
                write!(f, "could not determine a configuration directory")
            }
            FavouritesError::Malformed { path, source } => write!(
                f,
                "{} is not valid favourites TOML ({source}) — refusing to overwrite it; \
                 fix or remove the file by hand",
                path.display()
            ),
            FavouritesError::Io(e) => write!(f, "favourites I/O error: {e}"),
        }
    }
}

impl std::error::Error for FavouritesError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FavouritesError::Malformed { source, .. } => Some(source),
            FavouritesError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for FavouritesError {
    fn from(e: io::Error) -> Self {
        FavouritesError::Io(e)
    }
}

/// Settings that apply when neither a flag nor a favourite supplies a value.
///
/// Lives in `favourites.toml` as a `[defaults]` table so there is exactly one
/// configuration file to know about:
///
/// ```toml
/// [defaults]
/// username = "someone@example.com"
/// ```
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Defaults {
    /// Account to log on with when `mdrdp <host>` names a host that has no saved
    /// favourite and no `--user` flag was given. Never a password — see the module note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    username: Option<String>,
}

/// On-disk shape. A bare `Vec<Favourite>` at the TOML top level does not round-trip
/// through the `toml` crate the way a named array-of-tables does, so this wraps it.
#[derive(Debug, Default, Serialize, Deserialize)]
struct FavouritesFile {
    #[serde(default)]
    defaults: Defaults,
    #[serde(default)]
    favourite: Vec<Favourite>,
}

/// An ordered collection of saved connections, backed by a TOML file on disk.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Favourites {
    entries: Vec<Favourite>,
    defaults: Defaults,
}

impl Favourites {
    /// `$HOME/Library/Application Support/mdrdp/favourites.toml` on macOS,
    /// `%APPDATA%\mdrdp\favourites.toml` on Windows, otherwise
    /// `$XDG_CONFIG_HOME/mdrdp/favourites.toml` falling back to
    /// `$HOME/.config/mdrdp/favourites.toml`.
    pub fn default_path() -> Result<PathBuf, FavouritesError> {
        let base = if cfg!(target_os = "macos") {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
        } else if cfg!(windows) {
            std::env::var_os("APPDATA").map(PathBuf::from)
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        };
        base.map(|b| b.join("mdrdp").join("favourites.toml"))
            .ok_or(FavouritesError::NoConfigDirectory)
    }

    /// Load from the default platform path. See `load_from` for the missing/malformed
    /// file behaviour.
    pub fn load() -> Result<Self, FavouritesError> {
        Self::load_from(&Self::default_path()?)
    }

    /// Save to the default platform path.
    pub fn save(&self) -> Result<(), FavouritesError> {
        self.save_to(&Self::default_path()?)
    }

    /// Load the list from `path`.
    ///
    /// A **missing** file means an empty list — a clean machine has no favourites yet,
    /// and that must not be an error. A **malformed** file is an error, and the file is
    /// left exactly as it was: silently treating unparseable TOML as an empty list would
    /// mean the next save overwrites — and destroys — the user's real saved list.
    pub fn load_from(path: &Path) -> Result<Self, FavouritesError> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let file: FavouritesFile =
            toml::from_str(&text).map_err(|source| FavouritesError::Malformed {
                path: path.to_path_buf(),
                source,
            })?;
        let loaded = Favourites {
            entries: file.favourite,
            defaults: file.defaults,
        };
        // `add`/`rename` enforce unique names, but a hand-edited file bypasses both, and
        // everything downstream identifies a favourite BY NAME — the launcher returns a
        // name, the CLI resolves a name. A duplicate therefore does not merely shadow an
        // entry: it silently connects to a different host, as a different account, than
        // the row the user clicked. Refusing to load is the safe answer, and it matches
        // how a malformed file is treated: report it, change nothing, let the user fix it.
        if let Some(dup) = loaded.first_duplicate_name() {
            return Err(FavouritesError::DuplicateName(dup));
        }
        Ok(loaded)
    }

    /// The first name that appears more than once, compared case-insensitively.
    fn first_duplicate_name(&self) -> Option<String> {
        let mut seen: Vec<String> = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            // ASCII lowering, to match the uniqueness rule `add` enforces — Unicode
            // lowering here would refuse to reload a file `add` legitimately created.
            let key = entry.name.to_ascii_lowercase();
            if seen.contains(&key) {
                return Some(entry.name.clone());
            }
            seen.push(key);
        }
        None
    }

    /// Save the list to `path`, atomically.
    ///
    /// Writes to a temp file in the same directory as `path` and renames it over the
    /// target, so a crash or power loss mid-write can never leave a truncated or empty
    /// favourites file — the rename either lands the complete new content or does not
    /// happen at all. Parent directories are created as needed.
    pub fn save_to(&self, path: &Path) -> Result<(), FavouritesError> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir)?;

        let file = FavouritesFile {
            defaults: self.defaults.clone(),
            favourite: self.entries.clone(),
        };
        let text = toml::to_string_pretty(&file)
            .expect("Favourite serializes without error: no maps with non-string keys, no NaN/inf");

        let tmp_path = dir.join(format!(
            ".favourites.toml.tmp.{}.{}",
            std::process::id(),
            tmp_nonce()
        ));
        // Write, then flush to the device before renaming. Without the sync the rename
        // can become durable while the bytes it points at are not, which on an unclean
        // shutdown leaves an empty favourites file — the exact data loss the
        // temp-and-rename dance exists to prevent.
        {
            let mut f = fs::File::create(&tmp_path)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        let rename_result = fs::rename(&tmp_path, path);
        if rename_result.is_err() {
            // Best-effort cleanup; the rename error is what gets reported.
            let _ = fs::remove_file(&tmp_path);
        }
        rename_result?;
        Ok(())
    }

    /// Number of saved favourites.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if there are no saved favourites.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate favourites in display order.
    pub fn iter(&self) -> impl Iterator<Item = &Favourite> {
        self.entries.iter()
    }

    /// Look up a favourite by name, case-insensitively — names are unique
    /// case-insensitively (see `add`), so this is never ambiguous.
    pub fn find(&self, name: &str) -> Option<&Favourite> {
        self.position_ci(name).map(|i| &self.entries[i])
    }

    /// Record that `name` just launched a session, as Unix seconds now.
    ///
    /// Display-only bookkeeping for the launcher list; a missing name is a no-op
    /// rather than an error because the launch itself already succeeded.
    pub fn touch(&mut self, name: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Some(idx) = self.position_ci(name) {
            self.entries[idx].last_used = Some(now);
        }
    }

    fn position_ci(&self, name: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|f| f.name.eq_ignore_ascii_case(name))
    }

    /// Validate a candidate name/host/port triple shared by `add` and `rename`.
    fn validate(name: &str, host: &str, port: u16) -> Result<(), FavouritesError> {
        if name.trim().is_empty() {
            return Err(FavouritesError::EmptyName);
        }
        if host.trim().is_empty() {
            return Err(FavouritesError::EmptyHost);
        }
        if port == 0 {
            return Err(FavouritesError::ZeroPort);
        }
        Ok(())
    }

    /// Add a new favourite. Rejects an empty name, empty host, zero port, or a name
    /// that already exists (case-insensitive).
    pub fn add(&mut self, favourite: Favourite) -> Result<(), FavouritesError> {
        Self::validate(&favourite.name, &favourite.host, favourite.port)?;
        if self.position_ci(&favourite.name).is_some() {
            return Err(FavouritesError::DuplicateName(favourite.name));
        }
        self.entries.push(favourite);
        Ok(())
    }

    /// Remove the favourite named `name` (case-insensitive). Returns it, or an error if
    /// no such favourite exists.
    pub fn remove(&mut self, name: &str) -> Result<Favourite, FavouritesError> {
        let idx = self
            .position_ci(name)
            .ok_or_else(|| FavouritesError::NotFound(name.to_owned()))?;
        Ok(self.entries.remove(idx))
    }

    /// Rename the favourite currently called `old_name` to `new_name`. Rejects an empty
    /// new name or a collision (case-insensitive) with a *different* existing favourite;
    /// renaming a favourite to a case variant of its own current name is allowed.
    /// Replace the favourite called `name` with `updated`, keeping its position.
    ///
    /// The replacement's name may differ (a rename-in-edit); the usual name rules and
    /// duplicate check apply against every *other* entry.
    pub fn update(&mut self, name: &str, updated: Favourite) -> Result<(), FavouritesError> {
        let index = self
            .position_ci(name)
            .ok_or_else(|| FavouritesError::NotFound(name.to_owned()))?;
        Self::validate(&updated.name, &updated.host, updated.port)?;
        let clashes = self
            .entries
            .iter()
            .enumerate()
            .any(|(i, f)| i != index && f.name.eq_ignore_ascii_case(&updated.name));
        if clashes {
            return Err(FavouritesError::DuplicateName(updated.name));
        }
        self.entries[index] = updated;
        Ok(())
    }

    pub fn rename(&mut self, old_name: &str, new_name: &str) -> Result<(), FavouritesError> {
        let idx = self
            .position_ci(old_name)
            .ok_or_else(|| FavouritesError::NotFound(old_name.to_owned()))?;

        if new_name.trim().is_empty() {
            return Err(FavouritesError::EmptyName);
        }
        if let Some(collision) = self.position_ci(new_name)
            && collision != idx
        {
            return Err(FavouritesError::DuplicateName(new_name.to_owned()));
        }

        self.entries[idx].name = new_name.to_owned();
        Ok(())
    }

    /// Move the favourite named `name` one position earlier (towards index 0). A no-op
    /// if it is already first.
    pub fn move_up(&mut self, name: &str) -> Result<(), FavouritesError> {
        let idx = self
            .position_ci(name)
            .ok_or_else(|| FavouritesError::NotFound(name.to_owned()))?;
        if idx > 0 {
            self.entries.swap(idx, idx - 1);
        }
        Ok(())
    }

    /// Move the favourite named `name` one position later (towards the end). A no-op if
    /// it is already last.
    pub fn move_down(&mut self, name: &str) -> Result<(), FavouritesError> {
        let idx = self
            .position_ci(name)
            .ok_or_else(|| FavouritesError::NotFound(name.to_owned()))?;
        if idx + 1 < self.entries.len() {
            self.entries.swap(idx, idx + 1);
        }
        Ok(())
    }

    /// Resolve a CLI argument to a favourite: a name match first, falling back to
    /// treating `s` as a bare hostname so `mdrdp <host>` keeps working for a host that
    /// was never saved. Both comparisons are case-insensitive — names are unique that
    /// way already, and DNS hostnames are case-insensitive by definition.
    pub fn resolve(&self, s: &str) -> Option<&Favourite> {
        self.find(s)
            .or_else(|| self.entries.iter().find(|f| f.host.eq_ignore_ascii_case(s)))
    }

    /// The `[defaults]` username, used when neither a flag nor a favourite names one.
    pub fn default_username(&self) -> Option<&str> {
        self.defaults.username.as_deref()
    }
}

/// A small per-call nonce so two saves racing in the same millisecond (unlikely, but
/// exactly the kind of thing a flaky test finds) don't collide on a temp path.
fn tmp_nonce() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "mdrdp-favourites-test-{}-{:?}-{}",
            std::process::id(),
            std::thread::current().id(),
            tmp_nonce()
        ));
        fs::create_dir_all(&base).expect("create temp dir");
        base
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    /// A Windows account name contains a backslash, which TOML basic strings treat as an
    /// escape. A literal (single-quoted) string is the correct way to write one, and this
    /// pins that the documented form actually survives the parser — getting it wrong
    /// yields either a parse error or, worse, a silently mangled account name that fails
    /// the logon for no visible reason.
    #[test]
    fn a_domain_qualified_account_with_a_backslash_round_trips() {
        let dir = tmpdir();
        let path = dir.join("favourites.toml");
        std::fs::write(
            &path,
            "[[favourite]]\nname = \"Temper\"\nhost = \"temper\"\n\
             username = 'MicrosoftAccount\\user@example.com'\n",
        )
        .unwrap();

        let loaded = Favourites::load_from(&path).expect("literal string must parse");
        assert_eq!(
            loaded.iter().next().unwrap().username.as_deref(),
            Some("MicrosoftAccount\\user@example.com"),
            "the backslash must survive verbatim"
        );
        cleanup(&dir);
    }

    /// The exact snippet published in README.md must parse.
    ///
    /// Documentation that does not round-trip through the real parser is how a user's
    /// first five minutes get spent debugging our example instead of their connection.
    #[test]
    fn the_readme_example_parses() {
        let readme = include_str!("../README.md");
        let example = readme
            .split("```toml")
            .nth(1)
            .and_then(|rest| rest.split("```").next())
            .expect("README should contain a toml example block");

        let dir = tmpdir();
        let path = dir.join("favourites.toml");
        std::fs::write(&path, example).unwrap();

        let loaded = Favourites::load_from(&path).expect("the README example must parse");
        assert_eq!(
            loaded.len(),
            1,
            "example should define exactly one favourite"
        );
        let f = loaded.iter().next().unwrap();
        assert_eq!(f.name, "Temper");
        assert_eq!(f.host, "temper");
        assert_eq!(f.port, 3389);
        assert_eq!(
            f.window_size,
            WindowSize::Explicit {
                width: 1920,
                height: 1080
            }
        );
        cleanup(&dir);
    }

    #[test]
    fn a_defaults_username_parses_and_survives_a_round_trip() {
        let dir = tmpdir();
        let path = dir.join("favourites.toml");
        std::fs::write(
            &path,
            "[defaults]\nusername = \"everywhere@example.com\"\n\n\
             [[favourite]]\nname = \"Temper\"\nhost = \"temper\"\n",
        )
        .unwrap();

        let loaded = Favourites::load_from(&path).expect("defaults table must parse");
        assert_eq!(loaded.default_username(), Some("everywhere@example.com"));

        // A save driven by a favourites edit must not drop the defaults table.
        loaded.save_to(&path).expect("save");
        let reloaded = Favourites::load_from(&path).expect("reload");
        assert_eq!(reloaded.default_username(), Some("everywhere@example.com"));
        assert_eq!(reloaded.len(), 1);
        cleanup(&dir);
    }

    #[test]
    fn a_file_without_a_defaults_table_still_loads_with_no_default() {
        let dir = tmpdir();
        let path = dir.join("favourites.toml");
        std::fs::write(&path, "[[favourite]]\nname = \"A\"\nhost = \"a\"\n").unwrap();
        let loaded = Favourites::load_from(&path).expect("load");
        assert_eq!(loaded.default_username(), None);
        cleanup(&dir);
    }

    #[test]
    fn a_file_with_duplicate_names_is_refused_rather_than_silently_ambiguous() {
        // Everything downstream identifies a favourite by name, so a duplicate would
        // connect to a different host than the row the user clicked — the worst kind of
        // failure, because it looks like it worked.
        let dir = tmpdir();
        let path = dir.join("favourites.toml");
        std::fs::write(
            &path,
            "[[favourite]]\nname = \"Work\"\nhost = \"first.example\"\n\
             [[favourite]]\nname = \"work\"\nhost = \"second.example\"\n",
        )
        .unwrap();

        let err = Favourites::load_from(&path).unwrap_err();
        assert!(
            matches!(err, FavouritesError::DuplicateName(ref n) if n.eq_ignore_ascii_case("work")),
            "got {err:?}"
        );
        cleanup(&dir);
    }

    #[test]
    fn distinct_names_still_load_normally() {
        let dir = tmpdir();
        let path = dir.join("favourites.toml");
        std::fs::write(
            &path,
            "[[favourite]]\nname = \"Work\"\nhost = \"first.example\"\n\
             [[favourite]]\nname = \"Home\"\nhost = \"second.example\"\n",
        )
        .unwrap();
        assert_eq!(Favourites::load_from(&path).unwrap().len(), 2);
        cleanup(&dir);
    }

    #[test]
    fn missing_file_yields_empty_list_not_error() {
        let dir = tmpdir();
        let path = dir.join("does-not-exist.toml");
        let loaded = Favourites::load_from(&path).expect("missing file is fine");
        assert!(loaded.is_empty());
        assert_eq!(loaded.len(), 0);
        cleanup(&dir);
    }

    #[test]
    fn round_trip_preserves_order_and_every_field() {
        let dir = tmpdir();
        let path = dir.join("favourites.toml");

        // Every field gets a distinct, recognisable value so a swapped field or wrong
        // order is detectable — not "a", "a", "a" everywhere.
        let full = Favourite {
            name: "Office Desktop".to_owned(),
            host: "office-pc.lan".to_owned(),
            port: 3390,
            username: Some("mgd".to_owned()),
            domain: Some("CORP".to_owned()),
            window_size: WindowSize::Explicit {
                width: 2560,
                height: 1440,
            },
            keychain_account: Some("office-account".to_owned()),
            last_used: Some(1_755_300_000),
        };
        let sparse = Favourite {
            name: "Home Lab".to_owned(),
            host: "192.0.2.171".to_owned(),
            port: DEFAULT_PORT,
            username: None,
            domain: None,
            window_size: WindowSize::Fullscreen,
            keychain_account: None,
            last_used: None,
        };

        let mut favs = Favourites::default();
        favs.add(full.clone()).expect("add full");
        favs.add(sparse.clone()).expect("add sparse");
        favs.save_to(&path).expect("save");

        let reloaded = Favourites::load_from(&path).expect("load");
        let entries: Vec<&Favourite> = reloaded.iter().collect();
        assert_eq!(entries.len(), 2);
        // Order preserved: full first, sparse second.
        assert_eq!(entries[0], &full);
        assert_eq!(entries[1], &sparse);
        cleanup(&dir);
    }

    #[test]
    fn malformed_toml_returns_error_and_leaves_file_intact() {
        let dir = tmpdir();
        let path = dir.join("favourites.toml");
        let garbage = b"this is not valid = [ toml {{{ at all\n";
        fs::write(&path, garbage).expect("write garbage");

        let before = fs::read(&path).expect("read before");
        let err = Favourites::load_from(&path).expect_err("garbage must not parse");
        assert!(matches!(err, FavouritesError::Malformed { .. }));
        let after = fs::read(&path).expect("read after");
        assert_eq!(
            before, after,
            "malformed file must be left byte-for-byte intact"
        );
        assert_eq!(
            after, garbage,
            "file contents must be exactly what was written"
        );
        cleanup(&dir);
    }

    #[test]
    fn duplicate_name_rejected_case_insensitively() {
        let mut favs = Favourites::default();
        favs.add(Favourite::new("Temper", "temper.lan.example"))
            .expect("first add");
        let err = favs
            .add(Favourite::new("temper", "other-host"))
            .expect_err("case-insensitive duplicate must be rejected");
        assert!(matches!(err, FavouritesError::DuplicateName(n) if n == "temper"));
        assert_eq!(favs.len(), 1, "the rejected add must not have been applied");
    }

    #[test]
    fn empty_name_is_rejected() {
        let mut favs = Favourites::default();
        let err = favs
            .add(Favourite::new("   ", "somehost"))
            .expect_err("blank name must be rejected");
        assert!(matches!(err, FavouritesError::EmptyName));
    }

    #[test]
    fn empty_host_is_rejected() {
        let mut favs = Favourites::default();
        let err = favs
            .add(Favourite::new("Some Name", "  "))
            .expect_err("blank host must be rejected");
        assert!(matches!(err, FavouritesError::EmptyHost));
    }

    #[test]
    fn zero_port_is_rejected() {
        let mut favs = Favourites::default();
        let mut fav = Favourite::new("Some Name", "somehost");
        fav.port = 0;
        let err = favs.add(fav).expect_err("zero port must be rejected");
        assert!(matches!(err, FavouritesError::ZeroPort));
    }

    #[test]
    fn reorder_moves_the_right_element_and_is_noop_at_ends() {
        let mut favs = Favourites::default();
        favs.add(Favourite::new("Alpha", "alpha-host")).unwrap();
        favs.add(Favourite::new("Bravo", "bravo-host")).unwrap();
        favs.add(Favourite::new("Charlie", "charlie-host")).unwrap();

        // Move Bravo (middle) up: Bravo, Alpha, Charlie.
        favs.move_up("Bravo").expect("move up");
        let names: Vec<&str> = favs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Bravo", "Alpha", "Charlie"]);

        // Move Bravo (now first) up again: no-op.
        favs.move_up("Bravo").expect("move up at start is a no-op");
        let names: Vec<&str> = favs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Bravo", "Alpha", "Charlie"]);

        // Move Charlie (last) down: no-op.
        favs.move_down("Charlie")
            .expect("move down at end is a no-op");
        let names: Vec<&str> = favs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Bravo", "Alpha", "Charlie"]);

        // Move Alpha (middle) down: Bravo, Charlie, Alpha.
        favs.move_down("Alpha").expect("move down");
        let names: Vec<&str> = favs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Bravo", "Charlie", "Alpha"]);
    }

    #[test]
    fn resolve_prefers_exact_name_match_over_hostname_match() {
        let mut favs = Favourites::default();
        // A trap for an implementation that checks hostname before name: one
        // favourite's *name* equals another favourite's *host*.
        favs.add(Favourite::new("192.0.2.171", "decoy-host"))
            .expect("add name-that-looks-like-an-ip");
        favs.add(Favourite::new("Home Lab", "192.0.2.171"))
            .expect("add the real host owner");

        let resolved = favs
            .resolve("192.0.2.171")
            .expect("should resolve by exact name first");
        assert_eq!(resolved.name, "192.0.2.171");
        assert_eq!(resolved.host, "decoy-host");
    }

    /// Names are hostnames in practice, and hostnames are case-insensitive — so every
    /// name-keyed operation must match regardless of case. Each favourite's fields are
    /// distinct so a wrong match is detectable.
    #[test]
    fn name_lookups_are_case_insensitive() {
        let mut favs = Favourites::default();
        favs.add(Favourite::new("Temper", "temper.lan.example"))
            .unwrap();
        favs.add(Favourite::new("Quench", "quench.lan.example"))
            .unwrap();

        assert_eq!(favs.find("temper").unwrap().host, "temper.lan.example");
        assert_eq!(favs.find("TEMPER").unwrap().host, "temper.lan.example");
        assert_eq!(favs.resolve("tEmPeR").unwrap().host, "temper.lan.example");

        favs.touch("temper");
        assert!(favs.find("Temper").unwrap().last_used.is_some());

        favs.move_down("TEMPER").expect("move_down by case variant");
        let names: Vec<&str> = favs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Quench", "Temper"]);
        favs.move_up("temper").expect("move_up by case variant");

        favs.rename("QUENCH", "Quench Lab")
            .expect("rename by case variant");
        assert_eq!(favs.find("quench lab").unwrap().host, "quench.lan.example");

        let removed = favs.remove("temper").expect("remove by case variant");
        assert_eq!(removed.host, "temper.lan.example");
        assert_eq!(favs.len(), 1);
    }

    /// DNS hostnames are case-insensitive by definition, so the bare-hostname fallback
    /// in `resolve` must match them that way too.
    #[test]
    fn resolve_matches_hostname_case_insensitively() {
        let mut favs = Favourites::default();
        favs.add(Favourite::new("Home Lab", "temper.lan.example"))
            .unwrap();
        assert_eq!(favs.resolve("TEMPER.lan.example").unwrap().name, "Home Lab");
    }

    #[test]
    fn resolve_falls_back_to_bare_hostname() {
        let mut favs = Favourites::default();
        favs.add(Favourite::new("Home Lab", "192.0.2.171"))
            .expect("add");

        let resolved = favs
            .resolve("192.0.2.171")
            .expect("should fall back to hostname match");
        assert_eq!(resolved.name, "Home Lab");

        assert!(favs.resolve("nonexistent.example").is_none());
    }

    #[test]
    fn rename_updates_name_and_still_rejects_collisions() {
        let mut favs = Favourites::default();
        favs.add(Favourite::new("Old Name", "somehost")).unwrap();
        favs.add(Favourite::new("Other", "otherhost")).unwrap();

        favs.rename("Old Name", "New Name").expect("rename");
        assert!(favs.find("Old Name").is_none());
        assert_eq!(favs.find("New Name").unwrap().host, "somehost");

        let err = favs
            .rename("New Name", "other")
            .expect_err("case-insensitive collision with a different entry");
        assert!(matches!(err, FavouritesError::DuplicateName(_)));
    }

    #[test]
    fn remove_deletes_and_returns_the_favourite() {
        let mut favs = Favourites::default();
        favs.add(Favourite::new("Alpha", "alpha-host")).unwrap();
        favs.add(Favourite::new("Bravo", "bravo-host")).unwrap();

        let removed = favs.remove("Alpha").expect("remove");
        assert_eq!(removed.host, "alpha-host");
        assert_eq!(favs.len(), 1);
        assert!(favs.find("Alpha").is_none());
        assert!(favs.find("Bravo").is_some());
    }

    #[test]
    fn save_creates_parent_directories() {
        let dir = tmpdir();
        let path = dir.join("nested").join("deeper").join("favourites.toml");
        let mut favs = Favourites::default();
        favs.add(Favourite::new("Alpha", "alpha-host")).unwrap();
        favs.save_to(&path).expect("save should create parents");

        let reloaded = Favourites::load_from(&path).expect("load");
        assert_eq!(reloaded.len(), 1);
        cleanup(&dir);
    }
}
