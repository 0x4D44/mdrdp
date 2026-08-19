//! The local deploy record — which hosts `native = auto` may probe.
//!
//! `mdrdp deploy` writes a host in here on success; the Auto connect path consults
//! it and goes straight to RDP for any host it does not name. That carries deploy's
//! opt-in to the client: Auto never sprays SSH logon attempts (Security-log noise,
//! domain lockout risk) at boxes that never opted in. `Always`/`--native` ignore
//! the record and probe regardless — a host deployed from a different Mac is
//! reachable that way, an accepted v1 cost.
//!
//! Stored as `native-hosts.toml` beside `favourites.toml`. Like window state, it is
//! local run-record, not configuration: a missing file just means "nothing deployed
//! from this machine yet".

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The record file: a list of hosts deploy has succeeded against.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeHosts {
    #[serde(default, rename = "hosts")]
    pub hosts: Vec<DeployedHost>,
}

/// One deployed host: enough to gate Auto and to say what deploy last put there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployedHost {
    pub host: String,
    /// The rhydra version the last successful deploy shipped.
    pub version: String,
    /// When that deploy finished (Unix seconds).
    pub deployed_unix: u64,
}

/// `native-hosts.toml` beside `favourites.toml`; `None` when there is no config
/// directory at all — then Auto simply never finds a recorded host.
pub fn default_path() -> Option<PathBuf> {
    crate::favourites::Favourites::default_path()
        .ok()
        .map(|p| p.with_file_name("native-hosts.toml"))
}

impl NativeHosts {
    /// Read the record; a missing file is an empty record. A malformed file is
    /// reported on stderr and treated as empty rather than blocking every connect —
    /// the worst outcome is Auto probing nothing until the next deploy rewrites it.
    pub fn load_from(path: &Path) -> NativeHosts {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return NativeHosts::default(),
            Err(e) => {
                eprintln!("warning: could not read {}: {e}", path.display());
                return NativeHosts::default();
            }
        };
        match toml::from_str(&text) {
            Ok(record) => record,
            Err(e) => {
                eprintln!("warning: ignoring malformed {}: {e}", path.display());
                NativeHosts::default()
            }
        }
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    /// Whether Auto may probe `host`. Case-insensitive, but not alias-aware:
    /// `quench` and `quench.lan.example` are different entries, so deploy and connect
    /// must agree on the name — favourites make that the normal case.
    pub fn is_recorded(&self, host: &str) -> bool {
        self.hosts.iter().any(|h| h.host.eq_ignore_ascii_case(host))
    }

    /// Upsert `host` after a successful deploy (matching case-insensitively, so a
    /// re-deploy under different capitalisation updates rather than duplicates).
    pub fn record(&mut self, host: &str, version: &str, now_unix: u64) {
        let entry = DeployedHost {
            host: host.to_owned(),
            version: version.to_owned(),
            deployed_unix: now_unix,
        };
        match self
            .hosts
            .iter_mut()
            .find(|h| h.host.eq_ignore_ascii_case(host))
        {
            Some(existing) => *existing = entry,
            None => self.hosts.push(entry),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpfile(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mdrdp-native-hosts-{tag}-{}-{:?}.toml",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    #[test]
    fn a_missing_record_means_no_host_is_recorded() {
        let record = NativeHosts::load_from(Path::new("/nonexistent/native-hosts.toml"));
        assert!(record.hosts.is_empty());
        assert!(!record.is_recorded("quench.lan.example"));
    }

    #[test]
    fn record_round_trips_and_gates_case_insensitively() {
        let path = tmpfile("roundtrip");
        let mut record = NativeHosts::default();
        record.record("quench.lan.example", "0.3.0", 1_755_500_000);
        record.save_to(&path).expect("save record");

        let loaded = NativeHosts::load_from(&path);
        assert_eq!(loaded, record);
        assert!(loaded.is_recorded("QUENCH.lan.example"));
        assert!(!loaded.is_recorded("temper.lan.example"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_redeploy_updates_the_entry_rather_than_duplicating_it() {
        let mut record = NativeHosts::default();
        record.record("quench.lan.example", "0.2.0", 1_755_400_000);
        record.record("Quench.lan.example", "0.3.0", 1_755_500_000);
        assert_eq!(record.hosts.len(), 1);
        assert_eq!(record.hosts[0].version, "0.3.0");
        assert_eq!(record.hosts[0].deployed_unix, 1_755_500_000);
    }

    #[test]
    fn a_malformed_record_degrades_to_empty_not_a_blocked_connect() {
        let path = tmpfile("malformed");
        std::fs::write(&path, "hosts = \"not a list").expect("write junk");
        let record = NativeHosts::load_from(&path);
        assert!(record.hosts.is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
