//! `mdrdp ssh-setup <host>` — get SSH working to a Windows host, both ends.
//!
//! SSH is the transport `mdrdp deploy` runs on, so a host with no SSH is a host
//! deploy cannot reach. Setting that up is a two-sided job with a bootstrap gap
//! in the middle: **you cannot SSH in to set up SSH**. So the split is
//!
//! 1. *this* machine — key and `~/.ssh/config` entry, done automatically here;
//! 2. *the host* — a script the user pastes into a PowerShell window over RDP or
//!    at the console (see [`crate::hostscripts::setup_ssh`]);
//! 3. *this* machine again — [`diagnose`] says whether it worked, and when it
//!    did not, which of the known failures it is.
//!
//! Step 3 exists because the host-side failures are actively misleading. A
//! firewall-dropped connection times out with sshd running and listening
//! perfectly, which reads as a network fault and is not one. [`Diagnosis`]
//! encodes the three that have actually bitten us; each maps to a distinct,
//! observable client-side symptom, so the tool can name the fix rather than
//! leaving the user to guess.
//!
//! **No secrets.** This module reads and writes public keys and config text
//! only. The private key is created by `ssh-keygen` and never opened, logged, or
//! copied by us — see [`ensure_key`].

use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The shared fleet key's basename. One key for every Windows host: it lands in
/// `administrators_authorized_keys`, which is machine-wide for *all* members of
/// Administrators, so a second admin account on the same box needs no extra
/// setup.
pub const KEY_NAME: &str = "id_ed25519_fleet";

/// The comment baked into the public key, so a host's `authorized_keys` says
/// where the key came from rather than showing an anonymous blob.
pub const KEY_COMMENT: &str = "mdrdp-fleet-key";

/// How long to wait for a TCP connect before calling it a timeout.
///
/// A dropped packet has no reply to wait for, so this is purely how long we are
/// willing to sit before declaring the drop. Two seconds is far above any real
/// LAN handshake and short enough that a diagnosis feels immediate.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The SSH port. Not configurable: the host script opens 22, and a mismatch
/// between the two halves is a failure mode with no upside.
pub const SSH_PORT: u16 = 22;

/// What went wrong between here and the host's sshd — or that nothing did.
///
/// Each variant is distinguishable from the client alone, which is what makes
/// this worth having: no host access is needed to tell them apart, and host
/// access is exactly what is missing when SSH is broken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnosis {
    /// Key authentication succeeded. The host is ready for `mdrdp deploy`.
    Working { user: String },
    /// The name does not resolve. Under DHCP this usually means the box is off
    /// or asleep rather than misconfigured — a powered-down host stops
    /// registering its name.
    NameNotResolved,
    /// TCP connect timed out: packets are being dropped in flight.
    ///
    /// Nearly always the firewall-profile trap. Windows' built-in OpenSSH rule
    /// is **Private**-profile only, and these hosts sit on a network Windows has
    /// categorised **Public**, so the rule never applies. sshd is running and
    /// listening the whole time, which is why this looks like a network fault.
    Dropped,
    /// TCP connect was refused: the host answered, but nothing holds port 22.
    ///
    /// The firewall is fine; sshd is not installed or not running. Distinct from
    /// [`Diagnosis::Dropped`] precisely because the fixes are opposite ends of
    /// the script.
    NothingListening,
    /// TCP is fine and sshd answered, but it would not take the key.
    ///
    /// The host-side key step did not land: either the paste was truncated, or
    /// the public key went to `~/.ssh/authorized_keys` instead of
    /// `administrators_authorized_keys` (which is the file Windows OpenSSH
    /// consults for administrators, and only that file).
    KeyRejected { detail: String },
}

impl Diagnosis {
    /// Whether SSH is usable. The one question callers usually have.
    pub fn is_working(&self) -> bool {
        matches!(self, Diagnosis::Working { .. })
    }

    /// A one-line summary of the state.
    pub fn headline(&self) -> String {
        match self {
            Diagnosis::Working { user } => format!("SSH is working (authenticated as {user})"),
            Diagnosis::NameNotResolved => "the host name does not resolve".to_owned(),
            Diagnosis::Dropped => "connection timed out — packets are being dropped".to_owned(),
            Diagnosis::NothingListening => {
                "connection refused — nothing is listening on port 22".to_owned()
            }
            Diagnosis::KeyRejected { .. } => "reachable, but the key was rejected".to_owned(),
        }
    }

    /// What to do about it, naming the specific cause rather than a checklist.
    ///
    /// This is the payload of the whole module: the symptom a user sees points
    /// away from the cause in two of these four cases, so a generic "check your
    /// firewall" would be worse than useless.
    pub fn remedy(&self) -> &'static str {
        match self {
            Diagnosis::Working { .. } => "Nothing to do.",
            Diagnosis::NameNotResolved => {
                "The box is probably off, asleep, or has not re-registered its name \
                 under DHCP. Check it is powered on, then try again."
            }
            Diagnosis::Dropped => {
                "Almost certainly the firewall rule's profile. Windows' built-in \
                 OpenSSH rule is Private-profile only and this network is categorised \
                 Public, so the rule never applies — sshd is listening the whole time. \
                 Re-run the host script: it adds a separate rule named 'mdrdp-sshd-in' \
                 with Profile Any, which is also why it survives a later OpenSSH \
                 install (the capability overwrites any rule sharing its own name)."
            }
            Diagnosis::NothingListening => {
                "sshd is not installed or not running. Re-run the host script and watch \
                 the OpenSSH install step — under PowerShell 7 the DISM cmdlets can fail \
                 with 'Class not registered', which the script avoids by self-elevating \
                 into Windows PowerShell 5.1."
            }
            Diagnosis::KeyRejected { .. } => {
                "The key did not land on the host. Windows OpenSSH ignores \
                 ~\\.ssh\\authorized_keys for administrators and reads only \
                 C:\\ProgramData\\ssh\\administrators_authorized_keys — re-run the host \
                 script, which writes that file and fixes its ACLs."
            }
        }
    }
}

/// Where OpenSSH keeps this user's client configuration.
///
/// `~/.ssh` on Unix, `%USERPROFILE%\.ssh` on Windows. Deliberately *not* the
/// app's own config directory ([`crate::favourites::Favourites::default_path`]):
/// this is OpenSSH's location, and OpenSSH is the program that has to find it.
pub fn ssh_dir() -> Option<PathBuf> {
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE")
    } else {
        std::env::var_os("HOME")
    };
    home.map(|h| PathBuf::from(h).join(".ssh"))
}

/// The private key path for [`KEY_NAME`] under [`ssh_dir`].
pub fn key_path() -> Option<PathBuf> {
    ssh_dir().map(|d| d.join(KEY_NAME))
}

/// The `~/.ssh/config` path.
pub fn config_path() -> Option<PathBuf> {
    ssh_dir().map(|d| d.join("config"))
}

/// Render the `Host` block for one host.
///
/// `IdentitiesOnly yes` matters: without it ssh offers every key the agent holds
/// before ours, and a host that rejects enough offers closes the connection
/// before the right key is tried.
pub fn config_block(host: &str, user: &str, key: &Path) -> String {
    let short = host.split('.').next().unwrap_or(host);
    // A bare short name and the FQDN both reach the same block, so `ssh kiln`
    // and `ssh kiln.lan.example` behave identically.
    let aliases = if short == host {
        host.to_owned()
    } else {
        format!("{short} {host}")
    };
    format!(
        "Host {aliases}\n    \
         HostName {host}\n    \
         User {user}\n    \
         IdentityFile {}\n    \
         IdentitiesOnly yes\n",
        key.display()
    )
}

/// Whether `config` already has a `Host` block naming `host`.
///
/// Matches on the `Host` line's aliases, which is what ssh itself keys on. Kept
/// deliberately conservative: a false "present" would silently skip the write
/// and leave the user with no config, so only an exact alias match counts.
pub fn block_present(config: &str, host: &str) -> bool {
    let short = host.split('.').next().unwrap_or(host);
    config
        .lines()
        .filter_map(|l| l.trim().strip_prefix("Host "))
        .any(|rest| {
            rest.split_whitespace()
                .any(|alias| alias == host || alias == short)
        })
}

/// Append `block` to `config`, guaranteeing the separation ssh needs.
///
/// A `Host` block that runs onto the previous block's last line silently becomes
/// part of *that* block, so the blank-line separation is load-bearing rather
/// than cosmetic. An empty config is handled without a leading blank line.
pub fn with_block(config: &str, block: &str) -> String {
    if config.trim().is_empty() {
        return block.to_owned();
    }
    let mut out = config.trim_end().to_owned();
    out.push_str("\n\n");
    out.push_str(block);
    out
}

/// Errors from the client-side half.
#[derive(Debug)]
pub enum SetupError {
    NoHome,
    Io(std::io::Error),
    KeygenFailed(String),
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetupError::NoHome => write!(f, "could not locate the home directory"),
            SetupError::Io(e) => write!(f, "{e}"),
            SetupError::KeygenFailed(e) => write!(f, "ssh-keygen failed: {e}"),
        }
    }
}

impl std::error::Error for SetupError {}

impl From<std::io::Error> for SetupError {
    fn from(e: std::io::Error) -> Self {
        SetupError::Io(e)
    }
}

/// Ensure the fleet key exists, returning its public half and whether we made it.
///
/// Idempotent: an existing key is reused, never regenerated — regenerating would
/// silently orphan every host that already trusts the old key. The private key
/// is written by `ssh-keygen` and never read by us.
pub fn ensure_key() -> Result<(String, bool), SetupError> {
    let dir = ssh_dir().ok_or(SetupError::NoHome)?;
    std::fs::create_dir_all(&dir)?;
    let key = dir.join(KEY_NAME);
    let pubkey = dir.join(format!("{KEY_NAME}.pub"));

    if key.exists() && pubkey.exists() {
        return Ok((std::fs::read_to_string(&pubkey)?.trim().to_owned(), false));
    }

    let output = std::process::Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-C", KEY_COMMENT, "-f"])
        .arg(&key)
        .output()
        .map_err(|e| SetupError::KeygenFailed(e.to_string()))?;
    if !output.status.success() {
        return Err(SetupError::KeygenFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok((std::fs::read_to_string(&pubkey)?.trim().to_owned(), true))
}

/// Ensure `~/.ssh/config` has a block for `host`, returning whether one was added.
///
/// Idempotent, and never rewrites an existing block: if the user has hand-tuned
/// theirs, ours would be the wrong answer. The previous file is copied to
/// `config.mdrdp-bak` before any write — this is a file people edit by hand, and
/// a lost `~/.ssh/config` is a bad afternoon.
pub fn ensure_config(host: &str, user: &str) -> Result<bool, SetupError> {
    let dir = ssh_dir().ok_or(SetupError::NoHome)?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("config");
    let key = dir.join(KEY_NAME);

    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(SetupError::Io(e)),
    };
    if block_present(&existing, host) {
        return Ok(false);
    }
    if !existing.is_empty() {
        std::fs::copy(&path, dir.join("config.mdrdp-bak"))?;
    }
    let updated = with_block(&existing, &config_block(host, user, &key));
    std::fs::write(&path, updated)?;
    restrict(&path)?;
    Ok(true)
}

/// Lock a file to its owner. ssh refuses a group- or world-readable config.
#[cfg(unix)]
fn restrict(path: &Path) -> Result<(), SetupError> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Windows has no mode bits; OpenSSH there judges the file by its ACL, which it
/// inherits correctly from the user's own profile directory.
#[cfg(not(unix))]
fn restrict(_path: &Path) -> Result<(), SetupError> {
    Ok(())
}

/// Classify a TCP probe plus an ssh attempt into a [`Diagnosis`].
///
/// Split out from the IO so the mapping — the part that encodes what we learned
/// the hard way — is testable without a host. `resolved` is whether the name
/// resolved at all; `tcp` is the connect result; `ssh_ok`/`ssh_err` the
/// key-authentication attempt.
pub fn classify(
    resolved: bool,
    tcp: Option<Result<(), std::io::ErrorKind>>,
    ssh: Option<Result<String, String>>,
) -> Diagnosis {
    if !resolved {
        return Diagnosis::NameNotResolved;
    }
    match tcp {
        Some(Err(std::io::ErrorKind::ConnectionRefused)) => return Diagnosis::NothingListening,
        // Anything that is not an explicit refusal, having resolved, is a drop.
        // TimedOut is the common one; WouldBlock appears on some platforms when a
        // non-blocking connect never completes.
        Some(Err(_)) | None => return Diagnosis::Dropped,
        Some(Ok(())) => {}
    }
    match ssh {
        Some(Ok(user)) => Diagnosis::Working { user },
        Some(Err(detail)) => Diagnosis::KeyRejected { detail },
        None => Diagnosis::KeyRejected {
            detail: "no ssh attempt was made".to_owned(),
        },
    }
}

/// Probe `host` from this machine and say what state SSH is in.
///
/// Read-only: resolves, opens a TCP connection, and runs `ssh ... whoami` in
/// batch mode so a missing key fails fast instead of prompting.
pub fn diagnose(host: &str) -> Diagnosis {
    let target = format!("{host}:{SSH_PORT}");
    let addr = match target.to_socket_addrs() {
        Ok(mut it) => it.next(),
        Err(_) => None,
    };
    let Some(addr) = addr else {
        return classify(false, None, None);
    };

    let tcp = match TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) {
        Ok(_) => Some(Ok(())),
        Err(e) => Some(Err(e.kind())),
    };
    if !matches!(tcp, Some(Ok(()))) {
        return classify(true, tcp, None);
    }

    let out = std::process::Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            "-o",
            "StrictHostKeyChecking=accept-new",
            host,
            "whoami",
        ])
        .output();
    let ssh = match out {
        Ok(o) if o.status.success() => {
            Some(Ok(String::from_utf8_lossy(&o.stdout).trim().to_owned()))
        }
        Ok(o) => Some(Err(String::from_utf8_lossy(&o.stderr).trim().to_owned())),
        Err(e) => Some(Err(e.to_string())),
    };
    classify(true, tcp, ssh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_block_carries_the_four_lines_ssh_needs() {
        let b = config_block(
            "kiln.lan.example",
            "marti",
            Path::new("/h/.ssh/id_ed25519_fleet"),
        );
        assert!(b.starts_with("Host kiln kiln.lan.example\n"), "{b}");
        assert!(b.contains("    HostName kiln.lan.example\n"));
        assert!(b.contains("    User marti\n"));
        assert!(b.contains("    IdentityFile /h/.ssh/id_ed25519_fleet\n"));
        // Without this ssh offers agent keys first and a host that rejects
        // enough offers hangs up before reaching ours.
        assert!(b.contains("    IdentitiesOnly yes\n"));
    }

    #[test]
    fn a_single_label_host_is_not_aliased_twice() {
        let b = config_block("kiln", "marti", Path::new("/k"));
        assert!(b.starts_with("Host kiln\n"), "{b}");
    }

    #[test]
    fn a_block_is_detected_by_either_alias() {
        let c = "Host kiln kiln.lan.example\n    User marti\n";
        assert!(block_present(c, "kiln"));
        assert!(block_present(c, "kiln.lan.example"));
    }

    #[test]
    fn an_unrelated_host_is_not_mistaken_for_a_match() {
        let c = "Host quench quench.lan.example\n    User ano\n";
        assert!(!block_present(c, "kiln"));
        // A substring of an existing alias must not match: "quen" is not "quench".
        assert!(!block_present(c, "quen"));
    }

    #[test]
    fn a_hostname_in_a_value_line_is_not_a_block() {
        // `HostName kiln.lan.example` is a value, not a Host declaration. Matching
        // it would skip writing the block the user actually needs.
        let c = "Host quench\n    HostName kiln.lan.example\n";
        assert!(!block_present(c, "kiln"));
    }

    #[test]
    fn appending_separates_blocks_with_a_blank_line() {
        // Run-on would fold the new block into the previous one, silently
        // applying the wrong User and IdentityFile.
        let existing = "Host quench\n    User ano\n";
        let out = with_block(existing, "Host kiln\n    User marti\n");
        assert!(out.contains("    User ano\n\nHost kiln\n"), "{out}");
    }

    #[test]
    fn appending_to_an_empty_config_adds_no_leading_blank_line() {
        let out = with_block("", "Host kiln\n");
        assert_eq!(out, "Host kiln\n");
    }

    #[test]
    fn appending_normalises_a_config_with_no_trailing_newline() {
        let out = with_block("Host quench\n    User ano", "Host kiln\n");
        assert!(out.contains("User ano\n\nHost kiln\n"), "{out}");
    }

    #[test]
    fn an_unresolvable_name_is_reported_as_such() {
        assert_eq!(classify(false, None, None), Diagnosis::NameNotResolved);
    }

    #[test]
    fn a_refusal_means_nothing_is_listening() {
        let d = classify(true, Some(Err(std::io::ErrorKind::ConnectionRefused)), None);
        assert_eq!(d, Diagnosis::NothingListening);
        assert!(d.remedy().contains("not installed or not running"));
    }

    #[test]
    fn a_timeout_means_a_firewall_drop_not_a_dead_service() {
        // The trap this whole module exists for: sshd is listening perfectly and
        // the connection still times out, because the rule is Private-profile on
        // a Public network. Refused and timed-out must never collapse together.
        let d = classify(true, Some(Err(std::io::ErrorKind::TimedOut)), None);
        assert_eq!(d, Diagnosis::Dropped);
        assert!(d.remedy().contains("Profile Any"));
        assert_ne!(
            d,
            classify(true, Some(Err(std::io::ErrorKind::ConnectionRefused)), None)
        );
    }

    #[test]
    fn a_good_tcp_connect_with_a_rejected_key_points_at_the_admin_keys_file() {
        let d = classify(
            true,
            Some(Ok(())),
            Some(Err("Permission denied".to_owned())),
        );
        assert!(matches!(d, Diagnosis::KeyRejected { .. }));
        assert!(d.remedy().contains("administrators_authorized_keys"));
        assert!(!d.is_working());
    }

    #[test]
    fn a_successful_login_reports_the_user_it_authenticated_as() {
        let d = classify(true, Some(Ok(())), Some(Ok("kiln\\marti".to_owned())));
        assert!(d.is_working());
        assert!(d.headline().contains("kiln\\marti"));
    }

    #[test]
    fn every_diagnosis_offers_a_distinct_remedy() {
        // A remedy shared between two causes would be a remedy that does not
        // discriminate — the failure this module is meant to prevent.
        let all = [
            Diagnosis::NameNotResolved,
            Diagnosis::Dropped,
            Diagnosis::NothingListening,
            Diagnosis::KeyRejected {
                detail: String::new(),
            },
        ];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a.remedy(), b.remedy(), "{a:?} and {b:?} share a remedy");
                assert_ne!(
                    a.headline(),
                    b.headline(),
                    "{a:?} and {b:?} share a headline"
                );
            }
        }
    }
}
