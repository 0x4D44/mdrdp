//! The native transport's clipboard bridge: the decision layer, with no OS in it.
//!
//! Everything here is a pure function of state plus one observation, so the
//! whole echo-suppression policy is testable without a pasteboard, a socket or
//! a Windows box. The OS ends plug in above: the Mac polls `arboard` and calls
//! [`Bridge::on_local_change`]; the wire reader calls [`Bridge::on_remote_text`].
//!
//! # Why this is not the RDP bridge
//!
//! `crate::clipboard` is built around CLIPRDR's request/response: it asks the
//! peer for content and may never be answered, so it spends a 5 s timeout and a
//! state machine escaping that. The native channel *pushes*, so that whole state
//! is absent. What is reused is the part that is about the OS rather than the
//! protocol — the `arboard` handle, the direction gates, and the shape of the
//! bounded-cost fingerprint (HLD tranche 5 §3).
//!
//! # The suppression model, and the one that was rejected
//!
//! **One slot, holding the fingerprint of the last content seen from *either*
//! source.** A local observation updates it; applying wire content updates it.
//!
//! The design this replaced kept the fingerprint of the last content *applied
//! from the wire* and refused to send anything matching it. That silently and
//! permanently loses a legitimate copy:
//!
//! 1. We apply **X** from the wire.
//! 2. The user copies **Y**; we send it. The peer now holds Y. The slot still
//!    says X, because we originated Y and never applied it.
//! 3. The user copies **X** again — it matches, so it is suppressed. The peer
//!    still holds Y, and no further event will ever correct it.
//!
//! "I copied it and it didn't paste", permanently, from three ordinary actions.
//! The single-slot model sends X at step 3, because step 2 moved the slot to Y.
//!
//! **A last-*sent* slot was specified as a second belt and is deliberately not
//! implemented.** Consulted as a suppression key it reintroduces exactly the bug
//! above: after the sequence send-X, apply-Y, a re-copy of X matches the
//! last-sent slot and is refused. The loop it was meant to bound is closed
//! properly by [`Bridge::note_applied`] instead — see below.
//!
//! # Line endings are canonical on the wire, and so is the fingerprint
//!
//! LF on the wire; the Windows end converts to CRLF on set and back on read.
//! Without that a multi-line copy ping-pongs forever at poll cadence, and no
//! fingerprint can break the loop because the round trip is not byte-identical.
//!
//! The fingerprint is taken over the **canonical form**, which is the part that
//! makes suppression work at all across the two conventions: a Mac holding
//! `a\nb` and a Windows box holding `a\r\nb` hold the same clipboard, and any
//! scheme that hashed the local bytes would call them different forever.

use sha2::{Digest, Sha256};

use rhydra::aux_proto::{MAX_CLIPBOARD_BYTES, to_wire_newlines};

use crate::clipboard::{ClipboardContent, OsClipboard};

/// How much of a payload the fingerprint reads.
///
/// Equal to the wire ceiling on purpose, so every payload this bridge can carry
/// is hashed **whole**. The RDP bridge hashes a prefix plus the total length,
/// which cannot distinguish a same-length edit past the prefix; capping the wire
/// at the same figure removes that class here by construction rather than
/// accepting it (HLD §7).
const HASH_PREFIX_CAP_BYTES: usize = MAX_CLIPBOARD_BYTES;

/// What the session is allowed to do with the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Local copies may travel to the host.
    pub to_remote: bool,
    /// Host copies may be applied locally.
    pub from_remote: bool,
    /// Largest text payload, in bytes of the canonical wire form.
    pub max_text_bytes: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            to_remote: true,
            from_remote: true,
            max_text_bytes: MAX_CLIPBOARD_BYTES,
        }
    }
}

/// What to do about a local clipboard change.
///
/// `Debug` is hand-written throughout this module: the payload variants carry
/// clipboard content, and a derived `Debug` would put a password or a one-time
/// code into any `debug!(?outcome)` written later. The server half of this path
/// writes to an unrotated log file on the host that is read and quoted verbatim
/// during diagnosis, so this is not a theoretical rule.
#[derive(Clone, PartialEq, Eq)]
pub enum Outgoing {
    /// Send this canonical-form text to the host.
    Send(String),
    /// Both ends already have it. Not an error, and by far the common case.
    Suppressed,
    /// `to_remote` is off.
    Disabled,
    /// Over the ceiling. Refused whole, never truncated: silently sending half
    /// a document is a data-integrity bug.
    TooLarge { bytes: usize, limit: usize },
}

/// What to do about text arriving from the host.
#[derive(Clone, PartialEq, Eq)]
pub enum Incoming {
    /// Put this on the local pasteboard, then report what the OS ends up
    /// holding via [`Bridge::note_applied`].
    Apply(String),
    /// Our own content coming back, or content we already hold.
    Suppressed,
    /// `from_remote` is off.
    Disabled,
    /// Over the ceiling.
    TooLarge { bytes: usize, limit: usize },
}

impl std::fmt::Debug for Outgoing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outgoing::Send(t) => f.debug_struct("Send").field("bytes", &t.len()).finish(),
            Outgoing::Suppressed => f.write_str("Suppressed"),
            Outgoing::Disabled => f.write_str("Disabled"),
            Outgoing::TooLarge { bytes, limit } => f
                .debug_struct("TooLarge")
                .field("bytes", bytes)
                .field("limit", limit)
                .finish(),
        }
    }
}

impl std::fmt::Debug for Incoming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Incoming::Apply(t) => f.debug_struct("Apply").field("bytes", &t.len()).finish(),
            Incoming::Suppressed => f.write_str("Suppressed"),
            Incoming::Disabled => f.write_str("Disabled"),
            Incoming::TooLarge { bytes, limit } => f
                .debug_struct("TooLarge")
                .field("bytes", bytes)
                .field("limit", limit)
                .finish(),
        }
    }
}

/// The clipboard decision state for one session.
pub struct Bridge {
    policy: Policy,
    /// Fingerprint of the last content seen from **either** source.
    last_observed: Option<[u8; 32]>,
    /// Per-session salt for correlation tags. Never logged, never sent.
    salt: [u8; 16],
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Not even the raw fingerprint: it is an unsalted hash of the content,
        // so for a PIN or a six-digit code it is an offline verifier rather
        // than an opaque token. The salt is never printed at all.
        f.debug_struct("Bridge")
            .field("policy", &self.policy)
            .field("has_observed", &self.last_observed.is_some())
            .finish()
    }
}

impl Bridge {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            last_observed: None,
            salt: fresh_salt(),
        }
    }

    pub fn policy(&self) -> Policy {
        self.policy
    }

    /// Seed the slot from whatever the local clipboard already holds.
    ///
    /// Without this the first poll of an unchanged clipboard reads as a change,
    /// and one end clobbers the other's clipboard with no user action — with
    /// which end wins decided by a race.
    pub fn seed(&mut self, current_local: Option<&str>) {
        self.last_observed = current_local.map(|t| fingerprint(&to_wire_newlines(t)));
    }

    /// Whether inbound clipboard messages may be processed at all.
    ///
    /// Checked by the reader **before** decoding, so a payload the policy
    /// forbids is dropped without ever being turned into a `String`. Handing
    /// content to the allocator and then discarding it is not the same as
    /// refusing it.
    pub fn accepts_incoming(&self) -> bool {
        self.policy.from_remote
    }

    /// The local clipboard now holds `text`.
    pub fn on_local_change(&mut self, text: &str) -> Outgoing {
        let wire = to_wire_newlines(text);
        // Suppression is checked BEFORE the size gate, and the order is
        // load-bearing: the local end is polled roughly four times a second, so
        // a size check that ran first would re-report the same oversize
        // clipboard on every lap for as long as it sat there. Fingerprinting an
        // oversize payload is affordable precisely because the hash is capped
        // at the wire ceiling.
        let fp = fingerprint(&wire);
        if self.last_observed == Some(fp) {
            return Outgoing::Suppressed;
        }
        self.last_observed = Some(fp);
        if wire.len() > self.limit() {
            return Outgoing::TooLarge {
                bytes: wire.len(),
                limit: self.limit(),
            };
        }
        if !self.policy.to_remote {
            return Outgoing::Disabled;
        }
        Outgoing::Send(wire)
    }

    /// `text` arrived from the host.
    pub fn on_remote_text(&mut self, text: &str) -> Incoming {
        if !self.policy.from_remote {
            return Incoming::Disabled;
        }
        let wire = to_wire_newlines(text);
        if wire.len() > self.limit() {
            return Incoming::TooLarge {
                bytes: wire.len(),
                limit: self.limit(),
            };
        }
        let fp = fingerprint(&wire);
        if self.last_observed == Some(fp) {
            return Incoming::Suppressed;
        }
        self.last_observed = Some(fp);
        Incoming::Apply(wire)
    }

    /// Record what the OS actually holds after an [`Incoming::Apply`].
    ///
    /// **This, not a last-sent slot, is what closes the normalisation loop.**
    /// The pasteboard may not hand back the bytes it was given; if the slot held
    /// what we *asked* for rather than what is *there*, the very next poll would
    /// read a change and send it, and the peer would answer, forever. Seeding
    /// from the read-back makes the next poll agree by construction.
    pub fn note_applied(&mut self, read_back: &str) {
        self.last_observed = Some(fingerprint(&to_wire_newlines(read_back)));
    }

    /// A short, **salted** tag for correlating one payload across log lines.
    ///
    /// The raw fingerprint must never be logged: it is an unsalted SHA-256, so
    /// for a six-digit code, a PIN or a short password it is an offline verifier
    /// — anyone holding the log can confirm a guess. The salt comes from the OS
    /// at session start and is never logged, so a tag correlates lines within
    /// one session and is useless outside it. The length is deliberately not
    /// reported alongside it, because a length narrows a brute-force search.
    pub fn log_tag(&self, text: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.salt);
        hasher.update(fingerprint(&to_wire_newlines(text)));
        let digest = hasher.finalize();
        digest[..4].iter().map(|b| format!("{b:02x}")).collect()
    }

    fn limit(&self) -> usize {
        // The wire ceiling wins even if a policy asks for more: above it the
        // framing layer's own error is terminal and would drop the session.
        self.policy.max_text_bytes.min(MAX_CLIPBOARD_BYTES)
    }
}

/// Size-bounded fingerprint of canonical-form clipboard text.
///
/// The caller owes the canonicalisation; hashing the local bytes instead would
/// make a Mac's `a\nb` and a Windows box's `a\r\nb` permanently different.
fn fingerprint(canonical: &str) -> [u8; 32] {
    let bytes = canonical.as_bytes();
    let prefix = bytes.len().min(HASH_PREFIX_CAP_BYTES);
    let mut hasher = Sha256::new();
    hasher.update(b"text");
    hasher.update(&bytes[..prefix]);
    hasher.update((bytes.len() as u64).to_le_bytes());
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Per-session salt, from the OS.
///
/// `RandomState`'s keys are drawn from the operating system's RNG when the
/// thread starts, which is the property that matters here: an attacker holding
/// a log cannot reproduce the salt. It is not a cryptographic KDF and is not
/// claimed to be one — the job is to stop a short secret being confirmed
/// offline from a shared log file, not to resist an attacker on this machine.
fn fresh_salt() -> [u8; 16] {
    use std::hash::{BuildHasher, Hasher};
    let state = std::collections::hash_map::RandomState::new();
    let mut out = [0u8; 16];
    for (i, chunk) in out.chunks_mut(8).enumerate() {
        let mut h = state.build_hasher();
        h.write_usize(i);
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}

/// One poll of the local clipboard: what should go to the host, if anything.
///
/// Everything the poll thread decides lives here, so the thread itself is a
/// timer and nothing more.
///
/// **A read failure leaves the slot untouched on purpose.** `ArboardClipboard`
/// drops and rebuilds its handle on any error, so failures are transient by
/// design; treating one as "no content" would move the suppression slot to a
/// value the pasteboard never held, and the copy that follows would be
/// suppressed as unchanged. Doing nothing costs one poll interval.
pub fn poll_local(
    os: &mut dyn OsClipboard,
    bridge: &mut Bridge,
    report: &mut impl FnMut(&str),
) -> Option<String> {
    let content = match os.get_content() {
        Ok(content) => content,
        Err(e) => {
            // Third-party error text, so it may not be quoted near content.
            report(&format!("clipboard read failed: {e}"));
            return None;
        }
    };
    let text = match content {
        ClipboardContent::Text(text) => text,
        // Images are out of scope this tranche. Ignored rather than recorded:
        // moving the slot for content we cannot send would make the next text
        // copy look unchanged if it happened to match.
        ClipboardContent::Image { .. } => return None,
    };
    match bridge.on_local_change(&text) {
        Outgoing::Send(wire) => Some(wire),
        Outgoing::Suppressed | Outgoing::Disabled => None,
        Outgoing::TooLarge { bytes, limit } => {
            report(&format!(
                "clipboard not shared: {bytes} bytes is over the {limit}-byte limit"
            ));
            None
        }
    }
}

/// Apply one payload that arrived from the host to the local clipboard.
///
/// The read-back after the set is what closes the normalisation loop — see
/// [`Bridge::note_applied`]. It matters only for mangling that canonicalisation
/// does not already undo: a pasteboard that stores CRLF needs no help here,
/// because the fingerprint is taken over the canonical LF form either way.
///
/// A failed read-back needs no fallback — deciding to apply already recorded
/// the text.
pub fn apply_remote(
    os: &mut dyn OsClipboard,
    bridge: &mut Bridge,
    text: &str,
    report: &mut impl FnMut(&str),
) {
    match bridge.on_remote_text(text) {
        Incoming::Apply(wire) => {
            if let Err(e) = os.set_content(ClipboardContent::Text(wire.clone())) {
                report(&format!("clipboard write failed: {e}"));
                return;
            }
            // A failed read-back needs no `else`: deciding to apply already
            // recorded the text, which is the best guess available. What would
            // be actively wrong is *clearing* the slot on failure — the next
            // poll would read the content we just wrote, find no record of it,
            // and send it straight back to the host.
            if let Ok(ClipboardContent::Text(read_back)) = os.get_content() {
                bridge.note_applied(&read_back);
            }
        }
        Incoming::Suppressed | Incoming::Disabled => {}
        Incoming::TooLarge { bytes, limit } => {
            report(&format!(
                "clipboard from the host not applied: {bytes} bytes is over the {limit}-byte limit"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge() -> Bridge {
        Bridge::new(Policy::default())
    }

    /// A pasteboard that records what it was asked to do and can be told to
    /// fail or to mangle what it stores — all three are real `arboard`
    /// behaviours this code has to survive.
    struct FakeClipboard {
        content: Option<ClipboardContent>,
        read_fails: bool,
        /// Fail this many reads, then behave. Lets a read-BACK fail while the
        /// poll that follows succeeds — `read_fails` alone cannot express that,
        /// and a test using it would be measuring the wrong read.
        fail_next_reads: usize,
        write_fails: bool,
        /// Applied to anything stored, standing in for OS normalisation.
        mangle: Option<fn(&str) -> String>,
        sets: usize,
    }

    impl FakeClipboard {
        fn holding(text: &str) -> Self {
            Self {
                content: Some(ClipboardContent::Text(text.to_owned())),
                read_fails: false,
                fail_next_reads: 0,
                write_fails: false,
                mangle: None,
                sets: 0,
            }
        }
        fn text(&self) -> Option<&str> {
            match &self.content {
                Some(ClipboardContent::Text(t)) => Some(t),
                _ => None,
            }
        }
    }

    impl OsClipboard for FakeClipboard {
        fn get_content(&mut self) -> Result<ClipboardContent, String> {
            if self.fail_next_reads > 0 {
                self.fail_next_reads -= 1;
                return Err("the pasteboard is busy".to_owned());
            }
            if self.read_fails {
                return Err("the pasteboard is busy".to_owned());
            }
            self.content.clone().ok_or_else(|| "empty".to_owned())
        }
        fn set_content(&mut self, content: ClipboardContent) -> Result<(), String> {
            if self.write_fails {
                return Err("the pasteboard refused".to_owned());
            }
            self.sets += 1;
            self.content = Some(match (content, self.mangle) {
                (ClipboardContent::Text(t), Some(f)) => ClipboardContent::Text(f(&t)),
                (other, _) => other,
            });
            Ok(())
        }
    }

    #[test]
    fn a_local_change_is_offered_to_the_host_and_an_unchanged_poll_is_not() {
        let mut os = FakeClipboard::holding("copied");
        let mut b = bridge();
        let mut reports = Vec::new();
        let mut r = |m: &str| reports.push(m.to_owned());
        assert_eq!(
            poll_local(&mut os, &mut b, &mut r),
            Some("copied".to_owned())
        );
        // The clipboard has not changed, so the next lap must be silent — this
        // runs four times a second for the life of the session.
        assert_eq!(poll_local(&mut os, &mut b, &mut r), None);
        assert!(reports.is_empty());
    }

    #[test]
    fn a_read_failure_leaves_the_suppression_slot_alone() {
        // arboard drops and rebuilds its handle on error, so failures are
        // transient. Recording one as "no content" would move the slot to a
        // value the pasteboard never held, and the NEXT copy of that same text
        // would then be suppressed as unchanged — a silently lost copy.
        let mut os = FakeClipboard::holding("copied");
        os.read_fails = true;
        let mut b = bridge();
        // A RefCell rather than a plain Vec: this test has to read the log
        // BETWEEN two uses of the reporting closure.
        let reports = std::cell::RefCell::new(Vec::new());
        let mut r = |m: &str| reports.borrow_mut().push(m.to_owned());
        assert_eq!(poll_local(&mut os, &mut b, &mut r), None);
        assert_eq!(reports.borrow().len(), 1);

        os.read_fails = false;
        assert_eq!(
            poll_local(&mut os, &mut b, &mut r),
            Some("copied".to_owned()),
            "the copy that was unreadable during the failure must still be sent"
        );
    }

    #[test]
    fn an_image_on_the_clipboard_is_ignored_without_disturbing_suppression() {
        let mut os = FakeClipboard::holding("text first");
        let mut b = bridge();
        let mut r = |_: &str| {};
        assert_eq!(
            poll_local(&mut os, &mut b, &mut r),
            Some("text first".to_owned())
        );

        os.content = Some(ClipboardContent::Image {
            width: 2,
            height: 2,
            rgba: vec![0; 16],
        });
        assert_eq!(poll_local(&mut os, &mut b, &mut r), None);

        // Back to the same text: still unchanged, so still silent. If the image
        // had moved the slot this would send a copy the user never made.
        os.content = Some(ClipboardContent::Text("text first".to_owned()));
        assert_eq!(poll_local(&mut os, &mut b, &mut r), None);
    }

    #[test]
    fn an_oversize_local_copy_is_reported_and_not_offered() {
        let mut os = FakeClipboard::holding(&"x".repeat(MAX_CLIPBOARD_BYTES + 1));
        let mut b = bridge();
        let mut reports = Vec::new();
        let mut r = |m: &str| reports.push(m.to_owned());
        assert_eq!(poll_local(&mut os, &mut b, &mut r), None);
        assert_eq!(reports.len(), 1);
        assert!(reports[0].contains("over the"));
        assert!(
            !reports[0].contains("xxx"),
            "a report names sizes, never content"
        );
    }

    #[test]
    fn applying_a_host_payload_writes_it_and_records_what_the_os_kept() {
        // The mangle here is deliberately NOT a line-ending change. A CRLF
        // mangle cannot test this: the fingerprint is taken over the canonical
        // LF form, so the request and the read-back hash identically and the
        // assertion passes whether or not the read-back is used at all — this
        // test was written that way first, and a mutation proved it vacuous. It
        // has to be a mangle canonicalisation does not undo; here, a pasteboard
        // that strips trailing whitespace.
        let mut os = FakeClipboard::holding("something else");
        os.mangle = Some(|t| t.trim_end().to_owned());
        let mut b = bridge();
        let mut r = |_: &str| {};
        apply_remote(&mut os, &mut b, "host text\n", &mut r);
        assert_eq!(os.text(), Some("host text"));
        assert_eq!(os.sets, 1);

        // The next poll reads what the OS actually kept. It must be silent: if
        // the slot held what we ASKED for, this would look like a new local copy
        // and bounce straight back to the host, and round it would go.
        assert_eq!(poll_local(&mut os, &mut b, &mut r), None);
    }

    #[test]
    fn our_own_content_arriving_back_never_touches_the_pasteboard() {
        let mut os = FakeClipboard::holding("mine");
        let mut b = bridge();
        let mut r = |_: &str| {};
        assert_eq!(poll_local(&mut os, &mut b, &mut r), Some("mine".to_owned()));
        apply_remote(&mut os, &mut b, "mine", &mut r);
        assert_eq!(
            os.sets, 0,
            "an echo must not be written back to the pasteboard"
        );
    }

    #[test]
    fn from_remote_off_leaves_the_pasteboard_byte_identical() {
        // AC7: the payload is refused, and the local clipboard is untouched.
        let mut os = FakeClipboard::holding("local content");
        let mut b = Bridge::new(Policy {
            from_remote: false,
            ..Policy::default()
        });
        let mut r = |_: &str| {};
        apply_remote(&mut os, &mut b, "from the host", &mut r);
        assert_eq!(os.text(), Some("local content"));
        assert_eq!(os.sets, 0);
    }

    #[test]
    fn a_write_failure_is_reported_and_does_not_claim_the_content_was_applied() {
        let mut os = FakeClipboard::holding("untouched");
        os.write_fails = true;
        let mut b = bridge();
        let mut reports = Vec::new();
        let mut r = |m: &str| reports.push(m.to_owned());
        apply_remote(&mut os, &mut b, "from the host", &mut r);
        assert_eq!(os.text(), Some("untouched"));
        assert_eq!(reports.len(), 1);
        assert!(reports[0].contains("write failed"));
        assert!(
            !reports[0].contains("from the host"),
            "an error must not quote the payload: {}",
            reports[0]
        );
    }

    #[test]
    fn a_failed_read_back_still_leaves_the_bridge_able_to_suppress_the_echo() {
        // The set works and the read-back does not. Exactly one read fails, so
        // the poll that follows succeeds — otherwise this would be measuring a
        // failed poll rather than a failed read-back, which is how it was
        // written first, and a mutation proved it vacuous.
        //
        // What this pins is that the failure needs no repair: the record made
        // when the payload was accepted still holds. The wrong implementation
        // it guards against is clearing the slot because "we don't know what is
        // there" — which would bounce the content straight back to the host.
        let mut os = FakeClipboard::holding("before");
        os.fail_next_reads = 1;
        let mut b = bridge();
        let mut r = |_: &str| {};
        apply_remote(&mut os, &mut b, "from the host", &mut r);
        assert_eq!(os.text(), Some("from the host"));
        assert_eq!(
            os.fail_next_reads, 0,
            "the read-back must actually have been attempted"
        );
        // Falling back to the applied text is the best guess available, and it
        // must still stop the next poll bouncing the content back to the host.
        assert_eq!(poll_local(&mut os, &mut b, &mut r), None);
    }

    #[test]
    fn the_counterexample_that_sank_the_first_design_now_sends() {
        // Apply X, copy Y, copy X again. The rejected model suppressed that
        // last step permanently. This is the whole reason the model changed, so
        // it is asserted as a sequence rather than as three separate states.
        let mut b = bridge();
        assert_eq!(b.on_remote_text("X"), Incoming::Apply("X".to_owned()));
        b.note_applied("X");
        assert_eq!(b.on_local_change("Y"), Outgoing::Send("Y".to_owned()));
        assert_eq!(
            b.on_local_change("X"),
            Outgoing::Send("X".to_owned()),
            "re-copying earlier content must reach the peer, which now holds Y"
        );
    }

    #[test]
    fn our_own_content_coming_back_from_the_host_is_suppressed() {
        let mut b = bridge();
        assert_eq!(
            b.on_local_change("hello"),
            Outgoing::Send("hello".to_owned())
        );
        assert_eq!(b.on_remote_text("hello"), Incoming::Suppressed);
    }

    #[test]
    fn content_applied_from_the_wire_is_not_immediately_sent_back() {
        let mut b = bridge();
        assert_eq!(
            b.on_remote_text("from host"),
            Incoming::Apply("from host".to_owned())
        );
        b.note_applied("from host");
        assert_eq!(
            b.on_local_change("from host"),
            Outgoing::Suppressed,
            "the poll that follows an apply must not echo it"
        );
    }

    #[test]
    fn a_windows_crlf_round_trip_is_suppressed_rather_than_looping() {
        // The crux. The peer stores CRLF and reports CRLF back. Hashing local
        // bytes would call that a different clipboard forever, and a multi-line
        // copy would ping-pong at poll cadence with no way to stop it.
        let mut b = bridge();
        assert_eq!(
            b.on_local_change("a\nb\nc"),
            Outgoing::Send("a\nb\nc".to_owned())
        );
        assert_eq!(
            b.on_remote_text("a\r\nb\r\nc"),
            Incoming::Suppressed,
            "CRLF and LF forms of the same text must fingerprint identically"
        );
    }

    #[test]
    fn seeding_at_connect_stops_the_first_poll_clobbering_the_peer() {
        let mut b = bridge();
        b.seed(Some("already here"));
        assert_eq!(b.on_local_change("already here"), Outgoing::Suppressed);
    }

    #[test]
    fn an_unseeded_bridge_does_send_its_first_observation() {
        // The negative control for the test above: if a fresh bridge suppressed
        // everything, that test would pass without seeding doing anything.
        let mut b = bridge();
        assert_eq!(
            b.on_local_change("already here"),
            Outgoing::Send("already here".to_owned())
        );
    }

    #[test]
    fn note_applied_records_what_the_os_holds_not_what_we_asked_for() {
        // The pasteboard may hand back something other than what it was given.
        // If the slot held the applied text, the next poll would read a change
        // and send it, and the peer would answer — forever.
        let mut b = bridge();
        assert_eq!(
            b.on_remote_text("asked"),
            Incoming::Apply("asked".to_owned())
        );
        b.note_applied("what the OS actually kept");
        assert_eq!(
            b.on_local_change("what the OS actually kept"),
            Outgoing::Suppressed,
            "the loop closes on the read-back, not on the request"
        );
    }

    #[test]
    fn oversize_local_content_is_refused_and_reported_once_not_every_poll() {
        let mut b = bridge();
        let huge = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
        assert_eq!(
            b.on_local_change(&huge),
            Outgoing::TooLarge {
                bytes: MAX_CLIPBOARD_BYTES + 1,
                limit: MAX_CLIPBOARD_BYTES,
            }
        );
        // The same content still sitting there on the next poll is not a new
        // event, or a big copy would shout on a 250 ms cadence until replaced.
        assert_eq!(b.on_local_change(&huge), Outgoing::Suppressed);
    }

    #[test]
    fn a_policy_ceiling_below_the_wire_ceiling_is_honoured() {
        let mut b = Bridge::new(Policy {
            max_text_bytes: 8,
            ..Policy::default()
        });
        assert_eq!(
            b.on_local_change("123456789"),
            Outgoing::TooLarge { bytes: 9, limit: 8 }
        );
        assert_eq!(
            b.on_local_change("12345678"),
            Outgoing::Send("12345678".to_owned())
        );
    }

    #[test]
    fn a_policy_ceiling_above_the_wire_ceiling_cannot_raise_it() {
        // A policy may tighten the limit, never loosen it: above the wire
        // ceiling the framing error is terminal and drops the whole session.
        let mut b = Bridge::new(Policy {
            max_text_bytes: usize::MAX,
            ..Policy::default()
        });
        let huge = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
        assert!(matches!(
            b.on_local_change(&huge),
            Outgoing::TooLarge {
                limit: MAX_CLIPBOARD_BYTES,
                ..
            }
        ));
    }

    #[test]
    fn oversize_content_from_the_wire_is_refused_too() {
        // A peer may ignore the rule; the receiver must not depend on it.
        let mut b = bridge();
        let huge = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
        assert!(matches!(b.on_remote_text(&huge), Incoming::TooLarge { .. }));
    }

    #[test]
    fn to_remote_off_never_yields_something_to_send() {
        let mut b = Bridge::new(Policy {
            to_remote: false,
            ..Policy::default()
        });
        assert_eq!(b.on_local_change("secret"), Outgoing::Disabled);
        // …and a second, different copy is still refused rather than the gate
        // being consumed by the first one.
        assert_eq!(b.on_local_change("another"), Outgoing::Disabled);
    }

    #[test]
    fn from_remote_off_is_refusable_before_the_payload_is_decoded() {
        // AC7 requires the discard to happen before the bytes become a String,
        // so the reader needs a decision it can take from the policy alone.
        let b = Bridge::new(Policy {
            from_remote: false,
            ..Policy::default()
        });
        assert!(!b.accepts_incoming());
        assert!(bridge().accepts_incoming());
    }

    #[test]
    fn from_remote_off_also_refuses_at_the_text_entry_point() {
        // Belt to accepts_incoming's braces: a caller that forgets the pre-check
        // must still not apply anything.
        let mut b = Bridge::new(Policy {
            from_remote: false,
            ..Policy::default()
        });
        assert_eq!(b.on_remote_text("from host"), Incoming::Disabled);
    }

    #[test]
    fn simultaneous_copies_disagree_but_do_not_loop() {
        // The stated, unsolved case (HLD §5): both ends copy within one poll
        // interval. Each suppresses the other's arrival. That is a disagreement
        // and it is bounded — asserted here so a future change that turns it
        // into a loop is caught.
        let mut b = bridge();
        assert_eq!(b.on_local_change("mine"), Outgoing::Send("mine".to_owned()));
        assert_eq!(
            b.on_remote_text("theirs"),
            Incoming::Apply("theirs".to_owned())
        );
        b.note_applied("theirs");
        // No further traffic without a new user action.
        assert_eq!(b.on_local_change("theirs"), Outgoing::Suppressed);
        assert_eq!(b.on_remote_text("theirs"), Incoming::Suppressed);
    }

    #[test]
    fn the_log_tag_is_salted_per_session_and_never_holds_the_content() {
        let a = bridge();
        let b = bridge();
        let secret = "123456";
        let ta = a.log_tag(secret);
        let tb = b.log_tag(secret);
        assert_ne!(
            ta, tb,
            "two sessions must not produce the same tag for the same content, \
             or the tag is an offline verifier for a short secret"
        );
        // Stable within one session, or it correlates nothing.
        assert_eq!(ta, a.log_tag(secret));
        assert!(!ta.contains("123456"));
        assert_eq!(ta.len(), 8);
    }

    #[test]
    fn debug_never_prints_clipboard_content() {
        let secret = "hunter2-the-actual-secret";
        for rendered in [
            format!("{:?}", Outgoing::Send(secret.to_owned())),
            format!("{:?}", Incoming::Apply(secret.to_owned())),
            format!("{:?}", bridge()),
        ] {
            assert!(
                !rendered.contains("hunter2"),
                "Debug leaked clipboard content: {rendered}"
            );
        }
    }
}
