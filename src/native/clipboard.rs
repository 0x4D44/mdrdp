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
