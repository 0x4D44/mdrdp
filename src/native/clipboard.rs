//! The Mac end of the auxiliary channel's clipboard: an adapter, and nothing
//! else.
//!
//! The bridge itself — the suppression policy, the size gate, the direction
//! gates, and the poll/apply decisions — lives in [`rhydra::clipboard`], because
//! the host runs exactly the same logic. Suppression is subtle enough that two
//! implementations would be two chances to get it wrong, and the counterexample
//! recorded there is proof that getting it wrong is easy.
//!
//! What is genuinely local is this file's whole content: turning mdrdp's
//! `OsClipboard` (which also speaks images, for the RDP path) into the
//! text-only [`TextClipboard`] the shared bridge asks for.

pub use rhydra::clipboard::{
    Bridge, Incoming, Outgoing, Policy, TextClipboard, apply_remote, poll_local,
};

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use crate::clipboard::{ClipboardContent, OsClipboard};

/// Presents an [`OsClipboard`] as the text-only clipboard the shared bridge
/// expects.
///
/// The one decision here is what an image means. It is **not** an error: an
/// error says "ask again shortly", and the bridge deliberately leaves its
/// suppression slot untouched on one. An image is a definite answer — there is
/// no text to send — so it maps to `Ok(None)`, which the bridge ignores without
/// disturbing the slot. Conflating the two would make the next text copy look
/// unchanged if it happened to match what was there before the image.
pub struct TextOnly<T: OsClipboard>(pub T);

impl<T: OsClipboard> TextClipboard for TextOnly<T> {
    fn read_text(&mut self) -> Result<Option<String>, String> {
        match self.0.get_content()? {
            ClipboardContent::Text(text) => Ok(Some(text)),
            ClipboardContent::Image { .. } => Ok(None),
        }
    }

    fn write_text(&mut self, text: &str) -> Result<(), String> {
        // macOS takes the wire's canonical LF unchanged; the CRLF expansion is
        // the Windows implementation's job, at the other end of the channel.
        self.0.set_content(ClipboardContent::Text(text.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Result<ClipboardContent, String>);

    impl OsClipboard for Fake {
        fn get_content(&mut self) -> Result<ClipboardContent, String> {
            self.0.clone()
        }
        fn set_content(&mut self, content: ClipboardContent) -> Result<(), String> {
            self.0 = Ok(content);
            Ok(())
        }
    }

    #[test]
    fn an_image_reads_as_no_text_rather_than_as_an_error() {
        // The distinction the bridge depends on: an error leaves its slot
        // alone so the copy can be picked up next lap, while "no text" is a
        // settled answer. Mapping an image to Err would strand a later copy of
        // whatever text was there before it.
        let mut os = TextOnly(Fake(Ok(ClipboardContent::Image {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        })));
        assert_eq!(os.read_text(), Ok(None));
    }

    #[test]
    fn text_passes_through_both_ways() {
        let mut os = TextOnly(Fake(Ok(ClipboardContent::Text("hello".to_owned()))));
        assert_eq!(os.read_text(), Ok(Some("hello".to_owned())));
        assert_eq!(os.write_text("goodbye\nworld"), Ok(()));
        assert_eq!(os.read_text(), Ok(Some("goodbye\nworld".to_owned())));
    }

    #[test]
    fn a_read_error_stays_an_error() {
        let mut os = TextOnly(Fake(Err("the pasteboard is busy".to_owned())));
        assert!(os.read_text().is_err());
    }
}

/// What the clipboard did this session.
///
/// A process **global**, and that is not a shortcut: the architecture is one OS
/// process per session (parent HLD), so there is exactly one of these and
/// threading it through four call sites would buy nothing.
///
/// It exists because several acceptance criteria are about **counts on the
/// wire** rather than about content — "exactly one message each way per copy,
/// and none after" cannot be asserted by looking at a clipboard. Counts also
/// carry no content, so this is the one clipboard telemetry that is safe to
/// print.
pub static COUNTERS: ClipboardCounters = ClipboardCounters::new();

#[derive(Debug)]
pub struct ClipboardCounters {
    /// Local copies handed to the writer for the host.
    sent: AtomicU64,
    /// Host payloads applied to the local pasteboard.
    applied: AtomicU64,
    /// Host payloads recognised as our own content coming back.
    ///
    /// Only counted when a message actually arrived — the poll's "nothing
    /// changed" verdict is the common case four times a second and says
    /// nothing. This one is the echo signal.
    echoes_suppressed: AtomicU64,
    /// Payloads refused, in either direction, by the size ceiling or a
    /// direction gate.
    refused: AtomicU64,
}

impl ClipboardCounters {
    const fn new() -> Self {
        Self {
            sent: AtomicU64::new(0),
            applied: AtomicU64::new(0),
            echoes_suppressed: AtomicU64::new(0),
            refused: AtomicU64::new(0),
        }
    }

    pub fn note_sent(&self) {
        self.sent.fetch_add(1, AtomicOrdering::Relaxed);
    }
    pub fn note_applied(&self) {
        self.applied.fetch_add(1, AtomicOrdering::Relaxed);
    }
    pub fn note_echo_suppressed(&self) {
        self.echoes_suppressed.fetch_add(1, AtomicOrdering::Relaxed);
    }
    pub fn note_refused(&self) {
        self.refused.fetch_add(1, AtomicOrdering::Relaxed);
    }

    /// `(sent, applied, echoes suppressed, refused)`.
    pub fn snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.sent.load(AtomicOrdering::Relaxed),
            self.applied.load(AtomicOrdering::Relaxed),
            self.echoes_suppressed.load(AtomicOrdering::Relaxed),
            self.refused.load(AtomicOrdering::Relaxed),
        )
    }
}
