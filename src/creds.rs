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
