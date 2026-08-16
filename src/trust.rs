//! Trust-on-first-use certificate pinning.
//!
//! RDP hosts present self-signed certificates, so a PKI chain check is not available.
//! The alternative most clients take is to accept every certificate — which is what
//! `ironrdp-tls` does, and it means never detecting a man-in-the-middle. On a LAN, where
//! ARP spoofing is easy, that is a real exposure rather than a theoretical one.
//!
//! So: pin on first sight, require the same certificate afterwards, and fail hard on a
//! change. Weaker than PKI, far stronger than nothing, and the right trade for a fixed
//! set of personal machines.
//!
//! Note what is *not* weakened. Only the chain-of-trust check is replaced by the pin;
//! TLS signature verification still runs against the crypto provider's real algorithms.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// SHA-256 of a certificate's DER encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    pub fn of_der(der: &[u8]) -> Self {
        let digest = Sha256::digest(der);
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        Fingerprint(out)
    }

    pub fn to_hex(self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(s: &str) -> Result<Self, TrustError> {
        if s.len() != 64 {
            return Err(TrustError::MalformedFingerprint(s.to_owned()));
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .map_err(|_| TrustError::MalformedFingerprint(s.to_owned()))?;
        }
        Ok(Fingerprint(out))
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.to_hex())
    }
}

#[derive(Debug)]
pub enum TrustError {
    /// The store could not be parsed. Deliberately fatal — see `KnownHosts::load`.
    MalformedEntry {
        line_number: usize,
        line: String,
    },
    MalformedFingerprint(String),
    NoConfigDirectory,
    Io(std::io::Error),
}

impl fmt::Display for TrustError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrustError::MalformedEntry { line_number, line } => write!(
                f,
                "known_hosts line {line_number} is malformed: {line:?} — \
                 refusing to continue rather than silently dropping a pin"
            ),
            TrustError::MalformedFingerprint(s) => {
                write!(f, "not a sha256 hex fingerprint: {s:?}")
            }
            TrustError::NoConfigDirectory => {
                write!(f, "could not determine a configuration directory")
            }
            TrustError::Io(e) => write!(f, "known_hosts I/O error: {e}"),
        }
    }
}

impl std::error::Error for TrustError {}

impl From<std::io::Error> for TrustError {
    fn from(e: std::io::Error) -> Self {
        TrustError::Io(e)
    }
}

/// What happened when a certificate was checked against the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustOutcome {
    /// Matched a fingerprint already stored.
    Pinned,
    /// Not seen before; recorded now.
    PinnedOnFirstSight,
    /// Not seen before; the user accepted this session only — nothing was stored,
    /// so the next connection is a first sight again.
    AcceptedOnce,
}

/// A first-sight certificate, presented for a decision.
#[derive(Debug, Clone)]
pub struct FirstSight {
    /// `host:port`, as keyed in the store.
    pub host: String,
    pub fingerprint: Fingerprint,
    /// Where a pin would be recorded — the dialog names the durable file it touches.
    pub store_path: PathBuf,
}

/// What the user decided about a first-sight certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    PinAndConnect,
    ConnectOnce,
    Reject,
}

/// Blocks the connecting thread until someone decides about a first sight.
///
/// The handoff's decision 6: the GUI fronts this with the TOFU dialog. With no prompt
/// installed the verifier keeps the original CLI behaviour — pin on first sight —
/// because a scripted `mdrdp <host>` has nobody to ask. A CHANGED certificate is
/// refused outright either way; there is deliberately no prompt for that case.
pub type TrustPrompt = Arc<dyn Fn(&FirstSight) -> TrustDecision + Send + Sync>;

/// The `host:port → fingerprint` store, in ssh's format because it is well understood
/// and readable in a text editor.
#[derive(Debug, Default)]
pub struct KnownHosts {
    entries: BTreeMap<String, Fingerprint>,
}

impl KnownHosts {
    /// `$XDG_CONFIG_HOME/mdrdp/known_hosts`, `~/.config/mdrdp/known_hosts`, or
    /// `%APPDATA%\mdrdp\known_hosts`.
    pub fn default_path() -> Result<PathBuf, TrustError> {
        let base = if cfg!(windows) {
            std::env::var_os("APPDATA").map(PathBuf::from)
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        };
        base.map(|b| b.join("mdrdp").join("known_hosts"))
            .ok_or(TrustError::NoConfigDirectory)
    }

    /// Load the store. A missing file is an empty store; a **malformed** file is an
    /// error.
    ///
    /// Failing closed matters here. Skipping an unparseable line would drop a pin, and a
    /// dropped pin turns the next connection into a "first sight" that silently accepts
    /// whatever certificate arrives — which is exactly the attack the pin exists to stop.
    pub fn load(path: &Path) -> Result<Self, TrustError> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };

        let mut entries = BTreeMap::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (host, fp) =
                line.split_once(char::is_whitespace)
                    .ok_or_else(|| TrustError::MalformedEntry {
                        line_number: i + 1,
                        line: line.to_owned(),
                    })?;
            let hex =
                fp.trim()
                    .strip_prefix("sha256:")
                    .ok_or_else(|| TrustError::MalformedEntry {
                        line_number: i + 1,
                        line: line.to_owned(),
                    })?;
            let fingerprint =
                Fingerprint::from_hex(hex).map_err(|_| TrustError::MalformedEntry {
                    line_number: i + 1,
                    line: line.to_owned(),
                })?;
            entries.insert(host.to_owned(), fingerprint);
        }
        Ok(KnownHosts { entries })
    }

    pub fn get(&self, host: &str) -> Option<Fingerprint> {
        self.entries.get(host).copied()
    }

    pub fn insert(&mut self, host: &str, fp: Fingerprint) {
        self.entries.insert(host.to_owned(), fp);
    }

    /// Every pin, in host order — what Settings ▸ Certificate trust lists.
    pub fn pins(&self) -> impl Iterator<Item = (&str, Fingerprint)> {
        self.entries.iter().map(|(host, fp)| (host.as_str(), *fp))
    }

    /// Drop a pin, returning whether one was there. The caller still has to
    /// [`save`](Self::save) for the file itself to change.
    pub fn forget(&mut self, host: &str) -> bool {
        self.entries.remove(host).is_some()
    }

    pub fn save(&self, path: &Path) -> Result<(), TrustError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut out =
            String::from("# mdrdp known hosts — one 'host:port sha256:<hex>' per line.\n");
        for (host, fp) in &self.entries {
            out.push_str(&format!("{host} {fp}\n"));
        }
        fs::write(path, out)?;
        Ok(())
    }
}

/// A rustls verifier that pins instead of chaining.
pub struct TofuVerifier {
    host: String,
    store_path: PathBuf,
    store: Mutex<KnownHosts>,
    outcome: Mutex<Option<TrustOutcome>>,
    provider: Arc<CryptoProvider>,
    prompt: Option<TrustPrompt>,
}

impl std::fmt::Debug for TofuVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TofuVerifier")
            .field("host", &self.host)
            .field("store_path", &self.store_path)
            .finish_non_exhaustive()
    }
}

impl TofuVerifier {
    pub fn new(
        host: &str,
        store_path: PathBuf,
        store: KnownHosts,
        provider: Arc<CryptoProvider>,
    ) -> Self {
        TofuVerifier {
            host: host.to_owned(),
            store_path,
            store: Mutex::new(store),
            outcome: Mutex::new(None),
            provider,
            prompt: None,
        }
    }

    /// Consult `prompt` on first sight instead of auto-pinning.
    pub fn with_prompt(mut self, prompt: TrustPrompt) -> Self {
        self.prompt = Some(prompt);
        self
    }

    /// What the handshake decided, once it has run.
    pub fn outcome(&self) -> Option<TrustOutcome> {
        *self.outcome.lock().expect("trust outcome mutex")
    }
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let presented = Fingerprint::of_der(end_entity.as_ref());
        let mut store = self.store.lock().expect("known hosts mutex");

        match store.get(&self.host) {
            Some(expected) if expected == presented => {
                *self.outcome.lock().expect("trust outcome mutex") = Some(TrustOutcome::Pinned);
                Ok(ServerCertVerified::assertion())
            }
            Some(expected) => Err(TlsError::General(format!(
                "certificate for {} has CHANGED.\n  pinned:    {}\n  presented: {}\n\
                 Refusing to connect. If this change is legitimate, remove the entry from {} \
                 and reconnect to pin the new certificate.",
                self.host,
                expected,
                presented,
                self.store_path.display()
            ))),
            None => {
                let decision = match &self.prompt {
                    Some(prompt) => prompt(&FirstSight {
                        host: self.host.clone(),
                        fingerprint: presented,
                        store_path: self.store_path.clone(),
                    }),
                    // No prompt installed: the original CLI behaviour, pin on sight.
                    None => TrustDecision::PinAndConnect,
                };
                match decision {
                    TrustDecision::PinAndConnect => {
                        store.insert(&self.host, presented);
                        store.save(&self.store_path).map_err(|e| {
                            // A pin we cannot persist would silently re-trust every run.
                            TlsError::General(format!(
                                "could not record certificate pin for {}: {e}",
                                self.host
                            ))
                        })?;
                        *self.outcome.lock().expect("trust outcome mutex") =
                            Some(TrustOutcome::PinnedOnFirstSight);
                        Ok(ServerCertVerified::assertion())
                    }
                    TrustDecision::ConnectOnce => {
                        *self.outcome.lock().expect("trust outcome mutex") =
                            Some(TrustOutcome::AcceptedOnce);
                        Ok(ServerCertVerified::assertion())
                    }
                    TrustDecision::Reject => Err(TlsError::General(format!(
                        "first connection to {} rejected by the user",
                        self.host
                    ))),
                }
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "mdrdp-trust-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&base).expect("create temp dir");
        base
    }

    const FP_A: &str = "2c7d5aaba40f744c5fb9105eea59346b2aa9105a5e1317e8f086ee69813bebf4";
    const FP_B: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    #[test]
    fn fingerprint_is_sha256_of_the_der() {
        // Independently checkable: sha256 of the three bytes "abc".
        let fp = Fingerprint::of_der(b"abc");
        assert_eq!(
            fp.to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn fingerprint_hex_round_trips() {
        let fp = Fingerprint::from_hex(FP_A).expect("valid hex");
        assert_eq!(fp.to_hex(), FP_A);
        assert_eq!(fp.to_string(), format!("sha256:{FP_A}"));
    }

    #[test]
    fn rejects_short_and_non_hex_fingerprints() {
        assert!(Fingerprint::from_hex("abcd").is_err());
        assert!(Fingerprint::from_hex(&"z".repeat(64)).is_err());
    }

    #[test]
    fn missing_store_is_an_empty_store_not_an_error() {
        let path = tmpdir().join("does-not-exist");
        let store = KnownHosts::load(&path).expect("missing file is fine");
        assert!(store.get("temper:3389").is_none());
    }

    #[test]
    fn store_round_trips_through_disk() {
        let path = tmpdir().join("known_hosts_roundtrip");
        let mut store = KnownHosts::default();
        store.insert("temper:3389", Fingerprint::from_hex(FP_A).unwrap());
        store.insert("other:3389", Fingerprint::from_hex(FP_B).unwrap());
        store.save(&path).expect("save");

        let reloaded = KnownHosts::load(&path).expect("load");
        assert_eq!(reloaded.get("temper:3389").unwrap().to_hex(), FP_A);
        assert_eq!(reloaded.get("other:3389").unwrap().to_hex(), FP_B);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let path = tmpdir().join("known_hosts_comments");
        fs::write(
            &path,
            format!("# a comment\n\n  \ntemper:3389 sha256:{FP_A}\n"),
        )
        .unwrap();
        let store = KnownHosts::load(&path).expect("load");
        assert_eq!(store.get("temper:3389").unwrap().to_hex(), FP_A);
    }

    #[test]
    fn a_malformed_line_fails_closed_rather_than_dropping_a_pin() {
        // Silently skipping this line would drop the pin, and a dropped pin turns the
        // next connection into a "first sight" that accepts any certificate offered.
        let path = tmpdir().join("known_hosts_malformed");
        fs::write(
            &path,
            format!("temper:3389 sha256:{FP_A}\nbroken-line-no-fingerprint\n"),
        )
        .unwrap();
        assert!(matches!(
            KnownHosts::load(&path),
            Err(TrustError::MalformedEntry { line_number: 2, .. })
        ));
    }

    #[test]
    fn a_truncated_fingerprint_fails_closed() {
        let path = tmpdir().join("known_hosts_truncated");
        fs::write(&path, "temper:3389 sha256:deadbeef\n").unwrap();
        assert!(matches!(
            KnownHosts::load(&path),
            Err(TrustError::MalformedEntry { line_number: 1, .. })
        ));
    }

    /// Drive the verifier the way rustls does. `verify_server_cert` only hashes the DER,
    /// so arbitrary bytes stand in for a certificate.
    fn verify(verifier: &TofuVerifier, der: &[u8]) -> Result<ServerCertVerified, TlsError> {
        verifier.verify_server_cert(
            &CertificateDer::from(der.to_vec()),
            &[],
            &ServerName::try_from("temper").unwrap(),
            &[],
            UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_700_000_000)),
        )
    }

    fn verifier_at(path: &Path) -> TofuVerifier {
        let store = KnownHosts::load(path).expect("load");
        TofuVerifier::new(
            "temper:3389",
            path.to_path_buf(),
            store,
            Arc::new(rustls::crypto::ring::default_provider()),
        )
    }

    #[test]
    fn first_sight_pins_and_persists() {
        let path = tmpdir().join("known_hosts_firstsight");
        let _ = fs::remove_file(&path);

        let verifier = verifier_at(&path);
        verify(&verifier, b"server-cert-one").expect("first sight is accepted");
        assert_eq!(verifier.outcome(), Some(TrustOutcome::PinnedOnFirstSight));

        // The pin must survive on disk, or every run silently re-trusts.
        let reloaded = KnownHosts::load(&path).expect("reload");
        assert_eq!(
            reloaded.get("temper:3389"),
            Some(Fingerprint::of_der(b"server-cert-one"))
        );
    }

    #[test]
    fn the_same_certificate_is_accepted_as_pinned() {
        let path = tmpdir().join("known_hosts_same");
        let _ = fs::remove_file(&path);

        verify(&verifier_at(&path), b"server-cert-one").expect("first sight");

        let second = verifier_at(&path);
        verify(&second, b"server-cert-one").expect("same cert is accepted");
        assert_eq!(second.outcome(), Some(TrustOutcome::Pinned));
    }

    #[test]
    fn a_changed_certificate_is_refused() {
        // The whole point of the module. If this ever passes, we are accepting a
        // man-in-the-middle.
        let path = tmpdir().join("known_hosts_changed");
        let _ = fs::remove_file(&path);

        verify(&verifier_at(&path), b"server-cert-one").expect("first sight");

        let second = verifier_at(&path);
        let err = verify(&second, b"attacker-cert").expect_err("a changed cert must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("CHANGED"),
            "error should name the change: {msg}"
        );
        assert!(
            msg.contains(&path.display().to_string()),
            "error should say where the pin lives: {msg}"
        );
        assert_eq!(second.outcome(), None, "a refusal is not an outcome");

        // The stored pin must be untouched — a rejected connection must never re-pin.
        let reloaded = KnownHosts::load(&path).expect("reload");
        assert_eq!(
            reloaded.get("temper:3389"),
            Some(Fingerprint::of_der(b"server-cert-one"))
        );
    }

    #[test]
    fn a_different_host_is_pinned_separately() {
        let path = tmpdir().join("known_hosts_hosts");
        let _ = fs::remove_file(&path);

        verify(&verifier_at(&path), b"temper-cert").expect("first sight");

        let other = TofuVerifier::new(
            "other:3389",
            path.clone(),
            KnownHosts::load(&path).expect("load"),
            Arc::new(rustls::crypto::ring::default_provider()),
        );
        other
            .verify_server_cert(
                &CertificateDer::from(b"other-cert".to_vec()),
                &[],
                &ServerName::try_from("other").unwrap(),
                &[],
                UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_700_000_000)),
            )
            .expect("a different host pins independently");

        let reloaded = KnownHosts::load(&path).expect("reload");
        assert_eq!(
            reloaded.get("temper:3389"),
            Some(Fingerprint::of_der(b"temper-cert"))
        );
        assert_eq!(
            reloaded.get("other:3389"),
            Some(Fingerprint::of_der(b"other-cert"))
        );
    }

    fn verifier_with_prompt(path: &Path, decision: TrustDecision) -> TofuVerifier {
        verifier_at(path).with_prompt(Arc::new(move |_sight: &FirstSight| decision))
    }

    #[test]
    fn connect_once_accepts_without_writing_a_pin() {
        let path = tmpdir().join("known_hosts_once");
        let _ = fs::remove_file(&path);
        let verifier = verifier_with_prompt(&path, TrustDecision::ConnectOnce);
        verify(&verifier, b"once-cert").expect("accepted for this session");
        assert_eq!(verifier.outcome(), Some(TrustOutcome::AcceptedOnce));
        let reloaded = KnownHosts::load(&path).expect("reload");
        assert_eq!(reloaded.get("temper:3389"), None, "nothing may be stored");
    }

    #[test]
    fn a_rejected_first_sight_fails_and_stores_nothing() {
        let path = tmpdir().join("known_hosts_reject");
        let _ = fs::remove_file(&path);
        let verifier = verifier_with_prompt(&path, TrustDecision::Reject);
        let err = verify(&verifier, b"reject-cert").expect_err("rejected");
        assert!(err.to_string().contains("rejected by the user"), "{err}");
        assert_eq!(verifier.outcome(), None);
        assert_eq!(
            KnownHosts::load(&path).expect("reload").get("temper:3389"),
            None
        );
    }

    #[test]
    fn a_prompted_pin_persists_and_reports_the_sight() {
        let path = tmpdir().join("known_hosts_prompted_pin");
        let _ = fs::remove_file(&path);
        let seen: Arc<Mutex<Option<FirstSight>>> = Arc::new(Mutex::new(None));
        let seen_in = Arc::clone(&seen);
        let verifier = verifier_at(&path).with_prompt(Arc::new(move |sight: &FirstSight| {
            *seen_in.lock().unwrap() = Some(sight.clone());
            TrustDecision::PinAndConnect
        }));
        verify(&verifier, b"prompted-cert").expect("pinned");
        assert_eq!(verifier.outcome(), Some(TrustOutcome::PinnedOnFirstSight));
        let sight = seen.lock().unwrap().clone().expect("prompt was consulted");
        assert_eq!(sight.host, "temper:3389");
        assert_eq!(sight.fingerprint, Fingerprint::of_der(b"prompted-cert"));
        assert_eq!(sight.store_path, path);
        assert_eq!(
            KnownHosts::load(&path).expect("reload").get("temper:3389"),
            Some(Fingerprint::of_der(b"prompted-cert"))
        );
    }

    #[test]
    fn a_changed_certificate_never_consults_the_prompt() {
        // No accept path for CHANGED, with or without a prompt installed.
        let path = tmpdir().join("known_hosts_changed_prompt");
        let _ = fs::remove_file(&path);
        verify(&verifier_at(&path), b"original-cert").expect("first sight");
        let called = Arc::new(Mutex::new(false));
        let called_in = Arc::clone(&called);
        let verifier = verifier_at(&path).with_prompt(Arc::new(move |_s: &FirstSight| {
            *called_in.lock().unwrap() = true;
            TrustDecision::PinAndConnect
        }));
        verify(&verifier, b"changed-cert").expect_err("CHANGED is refused");
        assert!(!*called.lock().unwrap(), "the prompt must not be consulted");
    }

    #[test]
    fn a_fingerprint_without_the_sha256_prefix_fails_closed() {
        let path = tmpdir().join("known_hosts_noprefix");
        fs::write(&path, format!("temper:3389 {FP_A}\n")).unwrap();
        assert!(matches!(
            KnownHosts::load(&path),
            Err(TrustError::MalformedEntry { line_number: 1, .. })
        ));
    }
}
