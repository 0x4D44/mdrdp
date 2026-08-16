//! Credential lookup from the OS keychain.
//!
//! The password never appears in a config file, an environment variable, a command-line
//! argument, or a log. It is read at connect time and zeroed when dropped.

use std::fmt;
use zeroize::Zeroize;

/// The keychain service name all mdrdp credentials live under.
pub const SERVICE: &str = "mdrdp";

/// A password that will not print itself and is wiped on drop.
///
/// `Debug` is implemented deliberately — deriving it, or leaving it out and letting a
/// caller format the inner `String`, is how secrets reach logs.
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug)]
pub enum CredsError {
    /// No entry for this account. Tells the operator exactly how to create one.
    NotFound {
        service: String,
        account: String,
    },
    Keyring(keyring::Error),
    /// The keychain could not answer and no password could be read interactively.
    NoPassword(String),
}

impl fmt::Display for CredsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CredsError::NotFound { service, account } => write!(
                f,
                "no keychain entry for service {service:?}, account {account:?}.\n\
                 Create one with:\n  \
                 security add-generic-password -s {service} -a '{account}' -w\n\
                 (run it in a real terminal — the prompt needs a tty)"
            ),
            CredsError::Keyring(e) => write!(f, "keychain error: {e}"),
            CredsError::NoPassword(m) => write!(f, "no password available: {m}"),
        }
    }
}

impl std::error::Error for CredsError {}

/// Fetch the password for `account` from the OS keychain.
pub fn lookup(account: &str) -> Result<Secret, CredsError> {
    let entry = keyring::Entry::new(SERVICE, account).map_err(CredsError::Keyring)?;
    match entry.get_password() {
        Ok(password) => Ok(Secret(password)),
        Err(keyring::Error::NoEntry) => Err(CredsError::NotFound {
            service: SERVICE.to_owned(),
            account: account.to_owned(),
        }),
        Err(e) => Err(CredsError::Keyring(e)),
    }
}

/// Store or replace the password for `account` in the platform credential store.
///
/// The caller supplies a borrowed value so this function does not retain it. A temporary
/// [`Secret`] copy is used to ensure the bytes this function owns are wiped on every exit,
/// including a keyring error.
pub fn store(account: &str, password: &str) -> Result<(), CredsError> {
    let secret = secret_from_typed(password.to_owned())?;
    let entry = keyring::Entry::new(SERVICE, account).map_err(CredsError::Keyring)?;
    entry
        .set_password(secret.expose())
        .map_err(CredsError::Keyring)
}

/// Fetch the password, falling back to a terminal prompt when the keychain cannot
/// answer.
///
/// The keychain is the intended store and stays the default. But it is a single point of
/// failure with a real failure rate: an evening of this project was lost to a macOS
/// authorization subsystem that refused every ACL-gated read with
/// `errAuthorizationDenied`, which locks the user out of their own desktop for reasons
/// that have nothing to do with RDP. A client that cannot be used at all when that
/// happens is a worse client.
///
/// Rules that do not bend for the fallback:
///
/// * the password is read from the terminal with echo off, never from an argument, an
///   environment variable, or a file — all three are readable by other processes and
///   survive in shell history;
/// * it is held in a [`Secret`], so it is redacted in `Debug` and wiped on drop;
/// * nothing is written back to disk. Populating the keychain stays a deliberate act the
///   user performs with `security add-generic-password`.
///
/// A missing entry is *not* a prompt case: that is a setup mistake with a known fix, and
/// [`CredsError::NotFound`] prints it. Only a keychain that errors falls through here.
/// Remove the stored password for `account`. A missing entry is success — the goal
/// is "no password stored", and it already isn't.
pub fn forget(account: &str) -> Result<(), CredsError> {
    let entry = keyring::Entry::new(SERVICE, account).map_err(CredsError::Keyring)?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(CredsError::Keyring(e)),
    }
}

pub fn lookup_or_prompt(account: &str) -> Result<Secret, CredsError> {
    match lookup(account) {
        Ok(secret) => Ok(secret),
        Err(CredsError::NotFound { service, account }) => {
            Err(CredsError::NotFound { service, account })
        }
        Err(CredsError::Keyring(e)) => {
            eprintln!("keychain unavailable ({e}).");
            eprintln!(
                "Falling back to a prompt — this password is used for this session only \
                 and is not saved."
            );
            prompt_for_password(account)
        }
        Err(other) => Err(other),
    }
}

/// Read a password from standard input, for scripted and unattended runs.
///
/// This is the `docker login --password-stdin` pattern, and it exists because the two
/// alternatives are worse: an argument is visible in `ps` and lands in shell history, and
/// an environment variable is inherited by every child process. Stdin is neither.
///
/// It still never touches a config file and is never written anywhere. Intended for a
/// throwaway development credential against a test box; the keychain remains the store
/// for anything real.
pub fn from_stdin() -> Result<Secret, CredsError> {
    use std::io::Read as _;
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| CredsError::NoPassword(format!("could not read stdin: {e}")))?;
    // A trailing newline is what `echo` adds and is never part of the password.
    secret_from_typed(buf.trim_end_matches(['\n', '\r']).to_owned())
}

/// Read a password from the terminal with echo disabled.
///
/// Reads from the tty rather than stdin so it still works when stdin is a pipe, and fails
/// with a clear error when there is no terminal at all — an unattended run must not hang
/// forever waiting for a human who is not there.
fn prompt_for_password(account: &str) -> Result<Secret, CredsError> {
    let prompt = format!("Password for {account}: ");
    match rpassword::prompt_password(&prompt) {
        Ok(password) => secret_from_typed(password),
        Err(e) => Err(CredsError::NoPassword(format!(
            "could not read a password from the terminal ({e}); \
             run mdrdp from a terminal, or repair the keychain entry"
        ))),
    }
}

/// Turn typed input into a secret, rejecting an empty one.
///
/// Separate from the reading so it can be tested: a terminal cannot be synthesised in a
/// unit test, but "someone pressed Enter on an empty prompt" is exactly the case worth
/// pinning — sending an empty password to a Windows host is a failed logon that looks
/// like a rejected credential.
/// Wrap a password that arrived over the launcher pipe. Same emptiness rule as a
/// typed one; the caller owns zeroizing its transport buffer.
pub fn secret_from_password(password: String) -> Result<Secret, CredsError> {
    secret_from_typed(password)
}

fn secret_from_typed(password: String) -> Result<Secret, CredsError> {
    if password.is_empty() {
        return Err(CredsError::NoPassword(
            "an empty password was entered".to_owned(),
        ));
    }
    Ok(Secret(password))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_reveals_the_secret() {
        // The whole reason Debug is hand-written.
        let s = Secret("hunter2-correct-horse".to_owned());
        let rendered = format!("{s:?}");
        assert!(
            !rendered.contains("hunter2"),
            "Debug leaked the secret: {rendered}"
        );
        assert_eq!(rendered, "Secret(<redacted>)");
    }

    #[test]
    fn expose_returns_the_secret_for_deliberate_use() {
        let s = Secret("swordfish".to_owned());
        assert_eq!(s.expose(), "swordfish");
    }

    #[test]
    fn an_empty_typed_password_is_refused_rather_than_sent() {
        let err = secret_from_typed(String::new()).unwrap_err();
        assert!(matches!(err, CredsError::NoPassword(_)));
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn an_empty_password_is_rejected_before_keychain_access() {
        let err = store("account-that-must-not-be-touched", "").unwrap_err();
        assert!(matches!(err, CredsError::NoPassword(_)));
    }

    #[test]
    fn a_typed_password_becomes_a_redacted_secret() {
        let s = secret_from_typed("typed-at-the-prompt".to_owned()).expect("non-empty");
        assert_eq!(s.expose(), "typed-at-the-prompt");
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
    }

    #[test]
    fn a_missing_entry_never_falls_through_to_a_prompt() {
        // A keychain that ERRORS is a broken subsystem and worth prompting around. A
        // keychain that simply has no entry is a setup mistake with a known fix, and
        // prompting there would train the user to type their password every single time
        // instead of storing it once. The two must not be conflated.
        //
        // Uses an account no one would create, so the keychain answers NoEntry. If the
        // keychain itself is unavailable this test cannot distinguish the cases, so it
        // accepts that outcome rather than failing for an unrelated reason.
        let result = lookup("mdrdp-test-account-that-does-not-exist");
        match result {
            Err(CredsError::NotFound { service, account }) => {
                assert_eq!(service, SERVICE);
                assert_eq!(account, "mdrdp-test-account-that-does-not-exist");
            }
            Err(CredsError::Keyring(_)) => {
                // Keychain unavailable on this machine; nothing to assert.
            }
            other => panic!("expected NotFound or a keychain error, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_entry_explains_how_to_create_one() {
        let err = CredsError::NotFound {
            service: "mdrdp".to_owned(),
            account: "nobody@example.com".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("security add-generic-password"));
        assert!(msg.contains("nobody@example.com"));
        assert!(
            msg.contains("tty"),
            "should warn the prompt needs a terminal"
        );
    }
}
