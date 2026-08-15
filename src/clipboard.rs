//! Clipboard bridge between the OS pasteboard and the CLIPRDR virtual channel.
//!
//! The headline requirement is that this bridge must never wedge: Microsoft's own client
//! is known to have the clipboard "stop working randomly after a bit" on long sessions, and
//! that is exactly the failure mode this module is built to rule out.
//!
//! # Why two structs
//!
//! [`ironrdp_cliprdr::backend::CliprdrBackend`] callbacks take `&mut self` and return `()` —
//! they have no access to the network transport, so they cannot themselves cause a PDU to be
//! sent. [`ClipboardBackend`] therefore does almost nothing: every callback just forwards the
//! raw event as a [`ClipboardAction`] over an `mpsc` channel and returns immediately. It must
//! never block, never do I/O that can hang, and never panic.
//!
//! [`ClipboardBridge`] is the session-side half that owns the channel's receiver, the OS
//! clipboard handle, and the entire clipboard state machine (which formats the remote last
//! offered, whether we're waiting on a paste response, whether our last advertise was
//! accepted). The session loop drains it with [`ClipboardBridge::pump`] (whenever there is
//! channel or timer activity) and drives local-clipboard detection with
//! [`ClipboardBridge::poll_local_change`] on a ~250ms timer, since the OS gives no
//! cross-platform clipboard-change notification.
//!
//! Putting all the state on the bridge side (rather than splitting it, or sharing it behind a
//! mutex) keeps the anti-wedge invariants easy to see in one place: every state has an
//! explicit path back to `Idle`, and nothing here can block on the network.
//!
//! Scope is text only: `CF_UNICODETEXT` is what we advertise and prefer; `CF_TEXT` is
//! accepted inbound as a fallback. File formats get safe no-op handling.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc;

use ironrdp_cliprdr::backend::CliprdrBackend;
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest,
    FileContentsResponse, FormatDataRequest, FormatDataResponse, LockDataId,
    OwnedFormatDataResponse,
};
use ironrdp_cliprdr::{Cliprdr, CliprdrSvcMessages, Role};
use ironrdp_svc::pdu::IntoOwned as _;
use ironrdp_svc::pdu::ironrdp_core::AsAny;
use sha2::{Digest, Sha256};
use tracing::{debug, trace, warn};

/// How long we'll wait for the remote to answer a paste request before giving up and
/// returning to `Idle`. A stuck "pending" flag with no way back is the classic wedge.
pub const DEFAULT_PASTE_TIMEOUT_MS: u64 = 5_000;

/// Bounded retries for a rejected format-list advertise (MS-RDPECLIP allows the remote to
/// reject transiently, e.g. its window wasn't focused). Small and bounded on purpose: see
/// the hazard documented at `CliprdrBackend::on_format_list_response`.
const MAX_ADVERTISE_ATTEMPTS: u8 = 3;

/// Cap on how many bytes of clipboard text [`content_fingerprint`] will hash.
///
/// Text at or under this size is fingerprinted exactly as before (every byte hashed) — no
/// behaviour change for realistic clipboard content: a URL, a paragraph, a code snippet,
/// even a sizeable spreadsheet selection. Above the cap, only the first
/// `HASH_PREFIX_CAP_BYTES` bytes are hashed and the *full* byte length is folded into the
/// same digest, so the cost of every poll is bounded by this constant no matter how large
/// the payload on the clipboard gets.
///
/// At SHA-256's ~1-2 GB/s (the figure that made the original bug real: a 100 MB payload
/// cost 50-100 ms per poll), 256 KiB costs roughly 0.1-0.3 ms — negligible next to the
/// 250 ms poll cadence and the session pump's 5 ms read slice (`session::READ_SLICE`).
const HASH_PREFIX_CAP_BYTES: usize = 256 * 1024;

/// Raw clipboard events forwarded from [`ClipboardBackend`] to [`ClipboardBridge`].
///
/// Each variant mirrors a [`CliprdrBackend`] callback (or, for [`Self::AdvertiseRequested`],
/// a local-clipboard-change detected by [`ClipboardBridge::poll_local_change`]). The bridge
/// interprets these with full access to the clipboard state machine and the live
/// [`Cliprdr`] instance; the backend that produces them does no interpretation at all.
#[derive(Debug)]
pub enum ClipboardAction {
    /// We should (re-)advertise our current text formats to the remote.
    AdvertiseRequested,
    /// The remote acknowledged or rejected our last format-list advertise.
    FormatListAcked(bool),
    /// The remote's clipboard changed; these are the formats it now offers.
    RemoteCopy(Vec<ClipboardFormat>),
    /// The remote wants our clipboard content in this format.
    LocalDataRequested(ClipboardFormatId),
    /// The remote sent us clipboard content (or an explicit error) for a paste we requested.
    RemoteDataReceived(OwnedFormatDataResponse),
}

/// Abstracts OS clipboard access so the state machine can be tested without touching the
/// real pasteboard.
pub trait OsClipboard: Send {
    /// Reads the current clipboard text. Errs if there is no text content or the OS
    /// clipboard could not be accessed.
    fn get_text(&mut self) -> Result<String, String>;
    /// Writes text to the clipboard, replacing its current content.
    fn set_text(&mut self, text: String) -> Result<(), String>;
}

/// Real [`OsClipboard`] backed by `arboard`.
///
/// The `arboard::Clipboard` handle is created lazily and dropped (to be recreated on next
/// use) whenever an operation errors, so a poisoned handle can never disable the clipboard
/// for the rest of the session.
pub struct ArboardClipboard {
    inner: Option<arboard::Clipboard>,
}

impl ArboardClipboard {
    pub fn new() -> Self {
        Self { inner: None }
    }

    fn ensure(&mut self) -> Result<&mut arboard::Clipboard, String> {
        if self.inner.is_none() {
            let clipboard = arboard::Clipboard::new().map_err(|error| error.to_string())?;
            self.inner = Some(clipboard);
        }
        Ok(self.inner.as_mut().expect("just inserted above"))
    }
}

impl Default for ArboardClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl OsClipboard for ArboardClipboard {
    fn get_text(&mut self) -> Result<String, String> {
        let clipboard = self.ensure()?;
        match clipboard.get_text() {
            Ok(text) => Ok(text),
            Err(error) => {
                self.inner = None;
                Err(error.to_string())
            }
        }
    }

    fn set_text(&mut self, text: String) -> Result<(), String> {
        let clipboard = self.ensure()?;
        match clipboard.set_text(text) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.inner = None;
                Err(error.to_string())
            }
        }
    }
}

/// Abstracts the monotonic clock used for paste-request timeouts, so tests can advance time
/// deterministically instead of sleeping.
pub trait ClipboardClock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Real [`ClipboardClock`] backed by a process-local `Instant` epoch.
#[derive(Default)]
pub struct SystemClock;

impl ClipboardClock for SystemClock {
    fn now_ms(&self) -> u64 {
        use std::sync::OnceLock;
        use std::time::Instant;

        static EPOCH: OnceLock<Instant> = OnceLock::new();
        let epoch = EPOCH.get_or_init(Instant::now);
        u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// State of an outstanding paste request (us asking the remote for its clipboard content).
///
/// Every request we send must be able to time out: [`ClipboardBridge::check_timeouts`]
/// returns any stuck `Requested` state to `Idle` so the next remote copy still works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PasteState {
    Idle,
    Requested {
        format: ClipboardFormatId,
        requested_at_ms: u64,
    },
}

/// State of our outbound format-list advertise (us telling the remote what we have).
///
/// `on_format_list_response(false)` retries a bounded number of times from `Pending`, and a
/// stray/late rejection can never downgrade `Confirmed` — seeing an `Ok` always means we
/// stop re-advertising the same content, per the hazard documented at
/// `CliprdrBackend::on_format_list_response`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdvertiseState {
    Idle,
    Pending { attempt: u8 },
    Confirmed,
}

/// Implements [`CliprdrBackend`]. Deliberately thin: every callback forwards the raw event
/// over the channel and returns immediately, doing no I/O and holding no state of its own.
#[derive(Debug)]
pub struct ClipboardBackend {
    tx: mpsc::Sender<ClipboardAction>,
}

impl AsAny for ClipboardBackend {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl ClipboardBackend {
    fn send(&self, action: ClipboardAction) {
        if self.tx.send(action).is_err() {
            warn!("clipboard bridge is gone; dropping clipboard event");
        }
    }
}

impl CliprdrBackend for ClipboardBackend {
    fn temporary_directory(&self) -> &str {
        // File transfer is out of scope; we never advertise FileGroupDescriptorW.
        ""
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {
        debug!("cliprdr channel ready");
    }

    fn on_request_format_list(&mut self) {
        self.send(ClipboardAction::AdvertiseRequested);
    }

    fn on_format_list_response(&mut self, ok: bool) {
        self.send(ClipboardAction::FormatListAcked(ok));
    }

    fn on_process_negotiated_capabilities(
        &mut self,
        _capabilities: ClipboardGeneralCapabilityFlags,
    ) {
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        self.send(ClipboardAction::RemoteCopy(available_formats.to_vec()));
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        self.send(ClipboardAction::LocalDataRequested(request.format));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        self.send(ClipboardAction::RemoteDataReceived(response.into_owned()));
    }

    fn on_file_contents_request(&mut self, _request: FileContentsRequest) {
        // File transfer is out of scope. Safe no-op: we never advertise a file list, so a
        // spec-compliant remote will not send this.
    }

    fn on_file_contents_response(&mut self, _response: FileContentsResponse<'_>) {}

    fn on_lock(&mut self, _data_id: LockDataId) {}

    fn on_unlock(&mut self, _data_id: LockDataId) {}
}

/// The session-side half of the clipboard bridge: owns the OS clipboard handle, the channel
/// receiver, and the whole clipboard state machine.
pub struct ClipboardBridge {
    rx: mpsc::Receiver<ClipboardAction>,
    /// Actions generated locally by [`Self::poll_local_change`], which has no access to a
    /// live [`Cliprdr`] to act on them immediately. Drained by [`Self::pump`] alongside `rx`.
    local_pending: VecDeque<ClipboardAction>,
    os: Box<dyn OsClipboard>,
    clock: Arc<dyn ClipboardClock>,
    remote_formats: Vec<ClipboardFormat>,
    paste_state: PasteState,
    advertise_state: AdvertiseState,
    /// Fingerprint ([`content_fingerprint`]) of the last clipboard text we either wrote (from
    /// the remote) or advertised (from local content). Lets `poll_local_change` detect
    /// genuinely new local content, and stops remote-origin text from being advertised
    /// straight back — the advertise-loop hazard.
    last_seen_fingerprint: Option<[u8; 32]>,
    paste_timeout_ms: u64,
}

/// Creates a matched [`ClipboardBackend`]/[`ClipboardBridge`] pair sharing one channel, using
/// the real system clock.
pub fn clipboard_channel(os: Box<dyn OsClipboard>) -> (ClipboardBackend, ClipboardBridge) {
    clipboard_channel_with_clock(os, Arc::new(SystemClock))
}

/// Same as [`clipboard_channel`], but with an injectable clock (for deterministic timeout
/// tests).
pub fn clipboard_channel_with_clock(
    os: Box<dyn OsClipboard>,
    clock: Arc<dyn ClipboardClock>,
) -> (ClipboardBackend, ClipboardBridge) {
    let (tx, rx) = mpsc::channel();
    let backend = ClipboardBackend { tx };
    let bridge = ClipboardBridge {
        rx,
        local_pending: VecDeque::new(),
        os,
        clock,
        remote_formats: Vec::new(),
        paste_state: PasteState::Idle,
        advertise_state: AdvertiseState::Idle,
        last_seen_fingerprint: None,
        paste_timeout_ms: DEFAULT_PASTE_TIMEOUT_MS,
    };
    (backend, bridge)
}

impl ClipboardBridge {
    /// Formats most recently offered by the remote's clipboard (from the last
    /// `on_remote_copy`).
    pub fn remote_formats(&self) -> &[ClipboardFormat] {
        &self.remote_formats
    }

    /// Drains all pending clipboard actions (from the channel and from
    /// [`Self::poll_local_change`]), turning each into wire messages via `cliprdr`. Errors
    /// from one action are logged and skipped — never abort the pump.
    pub fn pump<R: Role>(&mut self, cliprdr: &mut Cliprdr<R>) -> Vec<CliprdrSvcMessages<R>> {
        self.check_timeouts();

        let mut out = Vec::new();
        while let Some(action) = self.next_action() {
            self.handle_action(action, cliprdr, &mut out);
        }
        out
    }

    /// Throw away queued actions when there is no clipboard channel to send them on.
    ///
    /// A server that never joins CLIPRDR still leaves us polling the local clipboard, and
    /// every change queues an advertise that [`Self::pump`] will never be called to drain.
    /// Over a long session that is an unbounded queue for a channel that does not exist.
    /// State returns to idle: nothing can be in flight when there is nowhere to send it.
    pub fn discard_pending(&mut self) {
        while self.next_action().is_some() {}
        self.paste_state = PasteState::Idle;
        self.advertise_state = AdvertiseState::Idle;
    }

    /// Resets any paste request that has been pending too long back to `Idle`, so the next
    /// remote copy is not blocked by one that never got an answer. Safe to call often; also
    /// called at the top of every [`Self::pump`].
    pub fn check_timeouts(&mut self) {
        if let PasteState::Requested {
            requested_at_ms, ..
        } = self.paste_state
        {
            let elapsed = self.clock.now_ms().saturating_sub(requested_at_ms);
            if elapsed >= self.paste_timeout_ms {
                warn!(
                    elapsed_ms = elapsed,
                    "clipboard paste request timed out; resetting to idle"
                );
                self.paste_state = PasteState::Idle;
            }
        }
    }

    /// Checks whether the OS clipboard's text content has changed since we last saw it, and
    /// if so, queues an advertise. Call on a timer (~250ms) — the OS gives no cross-platform
    /// clipboard-change notification.
    ///
    /// Text we just *wrote* here from the remote is not re-advertised: [`Self::pump`] updates
    /// `last_seen_fingerprint` whenever it writes remote data locally, so this sees "no
    /// change" for exactly that content.
    ///
    /// Change detection is a [`content_fingerprint`], not a full hash: cost is bounded by
    /// [`HASH_PREFIX_CAP_BYTES`] regardless of how large the clipboard payload is, so a
    /// multi-hundred-MB clipboard costs the same per poll as a one-line copy. The `get_text`
    /// call itself (an OS IPC round trip plus allocating the whole payload) still happens
    /// every poll — `arboard` has no cheaper "did it change" signal to check first (no
    /// `NSPasteboard.changeCount` equivalent is exposed) — but that cost no longer compounds
    /// with an O(size) hash on top of it.
    pub fn poll_local_change(&mut self) {
        let text = match self.os.get_text() {
            Ok(text) => text,
            Err(error) => {
                trace!(%error, "poll_local_change: OS clipboard read failed");
                return;
            }
        };

        let fingerprint = content_fingerprint(&text);
        if self.last_seen_fingerprint == Some(fingerprint) {
            return;
        }
        self.last_seen_fingerprint = Some(fingerprint);

        if text.is_empty() {
            return;
        }
        self.local_pending
            .push_back(ClipboardAction::AdvertiseRequested);
    }

    fn next_action(&mut self) -> Option<ClipboardAction> {
        if let Some(action) = self.local_pending.pop_front() {
            return Some(action);
        }
        self.rx.try_recv().ok()
    }

    fn handle_action<R: Role>(
        &mut self,
        action: ClipboardAction,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        match action {
            ClipboardAction::AdvertiseRequested => self.advertise(cliprdr, out),
            ClipboardAction::FormatListAcked(ok) => self.handle_format_list_acked(ok, cliprdr, out),
            ClipboardAction::RemoteCopy(formats) => self.handle_remote_copy(formats, cliprdr, out),
            ClipboardAction::LocalDataRequested(format) => {
                self.handle_local_data_requested(format, cliprdr, out)
            }
            ClipboardAction::RemoteDataReceived(response) => {
                self.handle_remote_data_received(response)
            }
        }
    }

    fn advertise<R: Role>(
        &mut self,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        self.advertise_state = AdvertiseState::Pending { attempt: 0 };
        self.send_advertise(cliprdr, out);
    }

    fn send_advertise<R: Role>(
        &mut self,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        match cliprdr.initiate_copy(&text_formats()) {
            Ok(messages) => out.push(messages),
            Err(error) => {
                warn!(%error, "failed to encode clipboard format-list advertise; giving up for now");
                self.advertise_state = AdvertiseState::Idle;
            }
        }
    }

    fn handle_format_list_acked<R: Role>(
        &mut self,
        ok: bool,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        match self.advertise_state {
            AdvertiseState::Pending { .. } if ok => {
                self.advertise_state = AdvertiseState::Confirmed;
            }
            AdvertiseState::Pending { attempt } => {
                if attempt + 1 < MAX_ADVERTISE_ATTEMPTS {
                    self.advertise_state = AdvertiseState::Pending {
                        attempt: attempt + 1,
                    };
                    debug!(
                        attempt = attempt + 1,
                        "remote rejected clipboard format list, retrying"
                    );
                    self.send_advertise(cliprdr, out);
                } else {
                    warn!(
                        attempts = MAX_ADVERTISE_ATTEMPTS,
                        "remote rejected clipboard format list; giving up until next local change"
                    );
                    self.advertise_state = AdvertiseState::Idle;
                }
            }
            AdvertiseState::Idle | AdvertiseState::Confirmed => {
                if ok {
                    self.advertise_state = AdvertiseState::Confirmed;
                } else {
                    // Never let a stale/late rejection clear state a later success already
                    // established.
                    trace!("ignoring stale clipboard format-list rejection");
                }
            }
        }
    }

    fn handle_remote_copy<R: Role>(
        &mut self,
        formats: Vec<ClipboardFormat>,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        self.remote_formats = formats;

        let Some(format) = best_text_format(&self.remote_formats) else {
            debug!("remote copy offered no supported text format");
            self.paste_state = PasteState::Idle;
            return;
        };

        self.paste_state = PasteState::Requested {
            format,
            requested_at_ms: self.clock.now_ms(),
        };
        match cliprdr.initiate_paste(format) {
            Ok(messages) => out.push(messages),
            Err(error) => {
                warn!(%error, "failed to send clipboard paste request");
                self.paste_state = PasteState::Idle;
            }
        }
    }

    fn handle_local_data_requested<R: Role>(
        &mut self,
        format: ClipboardFormatId,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        // A dropped response is what wedges the remote's paste: every branch below submits
        // something, even on failure.
        let response = match self.os.get_text() {
            Ok(text) if format == ClipboardFormatId::CF_UNICODETEXT => {
                OwnedFormatDataResponse::new_unicode_string(&text)
            }
            Ok(text) if format == ClipboardFormatId::CF_TEXT => {
                OwnedFormatDataResponse::new_string(&text)
            }
            Ok(_) => {
                debug!(?format, "remote requested an unsupported clipboard format");
                OwnedFormatDataResponse::new_error()
            }
            Err(error) => {
                warn!(%error, "failed to read OS clipboard for remote's paste request; sending error response");
                OwnedFormatDataResponse::new_error()
            }
        };

        match cliprdr.submit_format_data(response) {
            Ok(messages) => out.push(messages),
            Err(error) => warn!(%error, "failed to encode clipboard format-data response"),
        }
    }

    fn handle_remote_data_received(&mut self, response: OwnedFormatDataResponse) {
        let format = match std::mem::replace(&mut self.paste_state, PasteState::Idle) {
            PasteState::Requested { format, .. } => format,
            PasteState::Idle => {
                debug!("received clipboard data with no pending paste request; ignoring");
                return;
            }
        };

        if response.is_error() {
            warn!("remote reported an error providing clipboard data");
            return;
        }

        let text = if format == ClipboardFormatId::CF_UNICODETEXT {
            decode_utf16le_text(response.data())
        } else {
            decode_ansi_text(response.data())
        };

        match self.os.set_text(text.clone()) {
            Ok(()) => {
                // Remember what we just wrote so poll_local_change doesn't loop it straight
                // back to the remote as if the user had copied it locally.
                self.last_seen_fingerprint = Some(content_fingerprint(&text));
            }
            Err(error) => {
                warn!(%error, "failed to write remote clipboard data to the OS clipboard")
            }
        }
    }
}

fn text_formats() -> Vec<ClipboardFormat> {
    vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]
}

fn best_text_format(formats: &[ClipboardFormat]) -> Option<ClipboardFormatId> {
    if formats
        .iter()
        .any(|format| format.id() == ClipboardFormatId::CF_UNICODETEXT)
    {
        Some(ClipboardFormatId::CF_UNICODETEXT)
    } else if formats
        .iter()
        .any(|format| format.id() == ClipboardFormatId::CF_TEXT)
    {
        Some(ClipboardFormatId::CF_TEXT)
    } else {
        None
    }
}

/// Size-bounded fingerprint of clipboard text: a SHA-256 over at most the first
/// [`HASH_PREFIX_CAP_BYTES`] bytes of `text`, with the *full* byte length folded into the
/// same digest.
///
/// For text at or under the cap this is exactly equivalent to hashing the whole payload —
/// no behaviour change from the old full-hash for realistic clipboard content. Above the
/// cap, two different payloads of the same total length that share the same first
/// `HASH_PREFIX_CAP_BYTES` bytes are indistinguishable to this fingerprint: a local edit
/// that only changes content past the prefix, without changing the total length, will not
/// be detected as a new copy.
///
/// This is an accepted trade, not an oversight: it only bites clipboard content larger than
/// the cap that shares a huge common prefix and an unchanged length — a narrow case — and
/// the alternative is hashing every byte on every ~250ms poll for as long as that payload
/// sits on the clipboard, which is the latency-degrading defect this fingerprint exists to
/// fix. See the tests `single_poll_of_a_huge_payload_only_hashes_the_bounded_prefix`,
/// `repeated_polls_of_unchanged_huge_payload_cost_a_constant_capped_amount_each_time`, and
/// `a_tail_only_change_past_the_prefix_cap_with_unchanged_length_is_not_detected`.
fn content_fingerprint(text: &str) -> [u8; 32] {
    let bytes = text.as_bytes();
    let prefix_len = bytes.len().min(HASH_PREFIX_CAP_BYTES);

    let mut hasher = Sha256::new();
    hasher.update(&bytes[..prefix_len]);
    hasher.update((bytes.len() as u64).to_le_bytes());

    #[cfg(test)]
    tests::HASHED_BYTES.with(|cell| cell.set(cell.get() + prefix_len));

    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Decodes CLIPRDR `CF_UNICODETEXT` wire bytes (UTF-16LE, NUL-terminated) into a `String`.
/// Never panics: an odd trailing byte is dropped, all trailing NUL code units are trimmed,
/// and invalid sequences (including lone surrogates) are replaced with U+FFFD.
fn decode_utf16le_text(bytes: &[u8]) -> String {
    let mut units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    while units.last() == Some(&0) {
        units.pop();
    }
    String::from_utf16_lossy(&units)
}

/// Decodes CLIPRDR `CF_TEXT` wire bytes (single-byte, NUL-terminated) into a `String`.
/// Treated as Latin-1, which is a reasonable minimal decode for the inbound-only fallback
/// format; never panics.
fn decode_ansi_text(bytes: &[u8]) -> String {
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == 0 {
        end -= 1;
    }
    bytes[..end].iter().map(|&byte| byte as char).collect()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    use ironrdp_cliprdr::pdu::{ClipboardPdu, FormatListResponse};
    use ironrdp_cliprdr::{Client, CliprdrClient};
    use ironrdp_svc::pdu::{Decode as _, ReadCursor};
    use ironrdp_svc::{SvcMessage, SvcProcessor};

    use super::*;

    thread_local! {
        /// Total bytes fed to the hasher inside [`content_fingerprint`], across every call
        /// made on the current test thread. `cargo test` runs each `#[test]` fn on its own
        /// thread by default, so this is effectively per-test despite being `thread_local`
        /// rather than per-instance — see [`reset_hashed_bytes`] / [`hashed_bytes`].
        pub(crate) static HASHED_BYTES: Cell<usize> = const { Cell::new(0) };
    }

    fn reset_hashed_bytes() {
        HASHED_BYTES.with(|cell| cell.set(0));
    }

    fn hashed_bytes() -> usize {
        HASHED_BYTES.with(|cell| cell.get())
    }

    #[derive(Default)]
    struct FakeClipboardState {
        text: Option<String>,
        fail_next_get: bool,
        fail_next_set: bool,
        /// How many times [`OsClipboard::get_text`] was called — the observable proxy for
        /// the "IPC round trip + full allocation" cost `poll_local_change` pays every poll.
        get_text_calls: usize,
    }

    struct FakeOsClipboard(Arc<Mutex<FakeClipboardState>>);

    impl OsClipboard for FakeOsClipboard {
        fn get_text(&mut self) -> Result<String, String> {
            let mut state = self.0.lock().unwrap();
            state.get_text_calls += 1;
            if state.fail_next_get {
                state.fail_next_get = false;
                return Err("fake read failure".to_string());
            }
            state
                .text
                .clone()
                .ok_or_else(|| "fake clipboard is empty".to_string())
        }

        fn set_text(&mut self, text: String) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            if state.fail_next_set {
                state.fail_next_set = false;
                return Err("fake write failure".to_string());
            }
            state.text = Some(text);
            Ok(())
        }
    }

    fn fake_clipboard() -> (Arc<Mutex<FakeClipboardState>>, Box<dyn OsClipboard>) {
        let state = Arc::new(Mutex::new(FakeClipboardState::default()));
        (state.clone(), Box::new(FakeOsClipboard(state)))
    }

    struct FakeClock(AtomicU64);

    impl FakeClock {
        fn new() -> Arc<Self> {
            Arc::new(Self(AtomicU64::new(0)))
        }

        fn advance(&self, ms: u64) {
            self.0.fetch_add(ms, Ordering::SeqCst);
        }
    }

    impl ClipboardClock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// Builds a `CliprdrClient` wrapping `backend` and drives it straight to `Ready` state by
    /// feeding it one real, wire-encoded `FormatListResponse::Ok` PDU — no socket involved,
    /// just the same encode/decode path the real client would use.
    fn ready_client(backend: ClipboardBackend) -> CliprdrClient {
        let mut cliprdr = CliprdrClient::new(Box::new(backend));
        let bytes =
            ironrdp_svc::pdu::encode_vec(&ClipboardPdu::FormatListResponse(FormatListResponse::Ok))
                .expect("encode FormatListResponse::Ok");
        SvcProcessor::process(&mut cliprdr, &bytes).expect("process FormatListResponse::Ok");
        cliprdr
    }

    #[test]
    fn discarding_pending_actions_empties_the_queue_and_returns_to_idle() {
        // A server that never joins CLIPRDR still leaves the local poll queueing
        // advertises. If they are not drained, the channel grows for the life of the
        // session for a channel that does not exist.
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel(os);
        let mut cliprdr = ready_client(backend);

        // Queue work from both sources the bridge drains: the backend's channel...
        backend_mut(&mut cliprdr).on_request_format_list();
        // ...and the local-change poll.
        state.lock().unwrap().text = Some("something copied locally".to_string());
        bridge.poll_local_change();

        bridge.discard_pending();

        let out = bridge.pump(&mut cliprdr);
        assert!(
            out.is_empty(),
            "discarded actions must not still be waiting to be sent"
        );
    }

    #[test]
    fn a_discard_does_not_stop_the_clipboard_working_later() {
        // Discarding is not a kill switch: if the channel appears later, or the poll runs
        // again, the next change must still be advertised.
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel(os);
        let mut cliprdr = ready_client(backend);

        state.lock().unwrap().text = Some("first".to_string());
        bridge.poll_local_change();
        bridge.discard_pending();

        state.lock().unwrap().text = Some("second".to_string());
        bridge.poll_local_change();
        assert!(
            !bridge.pump(&mut cliprdr).is_empty(),
            "a later change must still be advertised"
        );
    }

    fn backend_mut(cliprdr: &mut CliprdrClient) -> &mut ClipboardBackend {
        cliprdr
            .downcast_backend_mut::<ClipboardBackend>()
            .expect("backend type")
    }

    fn unicode_format() -> ClipboardFormat {
        ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)
    }

    fn only_pdu(msgs: Vec<CliprdrSvcMessages<Client>>) -> ClipboardPdu<'static> {
        let mut svc_messages: Vec<SvcMessage> = Vec::new();
        for group in msgs {
            svc_messages.extend(Vec::<SvcMessage>::from(group));
        }
        assert_eq!(svc_messages.len(), 1, "expected exactly one wire PDU");
        let bytes = svc_messages[0]
            .encode_unframed_pdu()
            .expect("encode wire PDU");
        let mut cursor = ReadCursor::new(&bytes);
        match ClipboardPdu::decode(&mut cursor).expect("decode wire PDU") {
            ClipboardPdu::FormatDataResponse(response) => {
                ClipboardPdu::FormatDataResponse(response.into_owned())
            }
            other => panic!("unexpected pdu variant: {other:?}"),
        }
    }

    #[test]
    fn remote_copy_triggers_paste_request_then_writes_local_clipboard_on_response() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr); // drain the bootstrap FormatListAcked(true)

        backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
        let msgs = bridge.pump(&mut cliprdr);
        assert_eq!(msgs.len(), 1, "expected exactly one paste request");
        assert_eq!(
            bridge.paste_state,
            PasteState::Requested {
                format: ClipboardFormatId::CF_UNICODETEXT,
                requested_at_ms: 0,
            }
        );

        backend_mut(&mut cliprdr)
            .on_format_data_response(FormatDataResponse::new_unicode_string("hello from remote"));
        bridge.pump(&mut cliprdr);

        assert_eq!(bridge.paste_state, PasteState::Idle);
        assert_eq!(
            state.lock().unwrap().text.as_deref(),
            Some("hello from remote")
        );
    }

    /// The regression test for "stops working after a bit": two independent copy/paste
    /// cycles back to back, proving the state machine actually returns to Idle in between.
    #[test]
    fn two_consecutive_copy_paste_cycles_both_succeed() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        for (cycle, text) in ["first copy", "second copy"].into_iter().enumerate() {
            backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
            let msgs = bridge.pump(&mut cliprdr);
            assert_eq!(
                msgs.len(),
                1,
                "cycle {cycle}: expected a paste request to be sent"
            );
            assert!(
                matches!(bridge.paste_state, PasteState::Requested { .. }),
                "cycle {cycle}: expected Requested state"
            );

            backend_mut(&mut cliprdr)
                .on_format_data_response(FormatDataResponse::new_unicode_string(text));
            bridge.pump(&mut cliprdr);

            assert_eq!(
                bridge.paste_state,
                PasteState::Idle,
                "cycle {cycle}: must return to Idle so the next copy works"
            );
            assert_eq!(
                state.lock().unwrap().text.as_deref(),
                Some(text),
                "cycle {cycle}: OS clipboard should hold the new text"
            );
        }
    }

    #[test]
    fn format_list_rejection_retries_are_bounded() {
        let (_state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        backend_mut(&mut cliprdr).on_request_format_list();
        bridge.pump(&mut cliprdr);
        assert_eq!(
            bridge.advertise_state,
            AdvertiseState::Pending { attempt: 0 }
        );

        backend_mut(&mut cliprdr).on_format_list_response(false);
        let msgs = bridge.pump(&mut cliprdr);
        assert_eq!(msgs.len(), 1, "first rejection should retry");
        assert_eq!(
            bridge.advertise_state,
            AdvertiseState::Pending { attempt: 1 }
        );

        backend_mut(&mut cliprdr).on_format_list_response(false);
        let msgs = bridge.pump(&mut cliprdr);
        assert_eq!(msgs.len(), 1, "second rejection should retry");
        assert_eq!(
            bridge.advertise_state,
            AdvertiseState::Pending { attempt: 2 }
        );

        backend_mut(&mut cliprdr).on_format_list_response(false);
        let msgs = bridge.pump(&mut cliprdr);
        assert!(msgs.is_empty(), "must give up rather than retry forever");
        assert_eq!(bridge.advertise_state, AdvertiseState::Idle);
    }

    #[test]
    fn format_list_rejection_never_clears_a_prior_confirmation() {
        let (_state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        backend_mut(&mut cliprdr).on_request_format_list();
        bridge.pump(&mut cliprdr);
        backend_mut(&mut cliprdr).on_format_list_response(true);
        bridge.pump(&mut cliprdr);
        assert_eq!(bridge.advertise_state, AdvertiseState::Confirmed);

        // A stray/late rejection must not undo the confirmed state.
        backend_mut(&mut cliprdr).on_format_list_response(false);
        bridge.pump(&mut cliprdr);
        assert_eq!(bridge.advertise_state, AdvertiseState::Confirmed);
    }

    #[test]
    fn paste_request_timeout_resets_to_idle_and_next_copy_still_works() {
        let (state, os) = fake_clipboard();
        let clock = FakeClock::new();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, clock.clone());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
        bridge.pump(&mut cliprdr);
        assert!(matches!(bridge.paste_state, PasteState::Requested { .. }));

        // Remote never answers. Advance well past the timeout.
        clock.advance(DEFAULT_PASTE_TIMEOUT_MS + 1_000);
        bridge.check_timeouts();
        assert_eq!(
            bridge.paste_state,
            PasteState::Idle,
            "a stuck pending flag is the classic clipboard wedge"
        );

        // A subsequent copy must still work.
        backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
        bridge.pump(&mut cliprdr);
        backend_mut(&mut cliprdr)
            .on_format_data_response(FormatDataResponse::new_unicode_string("still works"));
        bridge.pump(&mut cliprdr);

        assert_eq!(bridge.paste_state, PasteState::Idle);
        assert_eq!(state.lock().unwrap().text.as_deref(), Some("still works"));
    }

    #[test]
    fn read_failure_on_remote_request_sends_error_response_not_silence() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        state.lock().unwrap().fail_next_get = true;

        backend_mut(&mut cliprdr).on_format_data_request(FormatDataRequest {
            format: ClipboardFormatId::CF_UNICODETEXT,
        });
        let msgs = bridge.pump(&mut cliprdr);
        assert_eq!(msgs.len(), 1, "must respond, never stay silent");

        match only_pdu(msgs) {
            ClipboardPdu::FormatDataResponse(response) => {
                assert!(
                    response.is_error(),
                    "a failed local read must produce an explicit error response"
                );
            }
            other => panic!("unexpected pdu: {other:?}"),
        }
    }

    #[test]
    fn local_change_is_advertised_once_via_poll_local_change() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        state.lock().unwrap().text = Some("user copied this".to_string());
        bridge.poll_local_change();
        let msgs = bridge.pump(&mut cliprdr);
        assert_eq!(
            msgs.len(),
            1,
            "a genuinely new local copy must be advertised"
        );
        assert_eq!(
            bridge.advertise_state,
            AdvertiseState::Pending { attempt: 0 }
        );

        // Polling again with unchanged content must not re-advertise.
        bridge.poll_local_change();
        let msgs = bridge.pump(&mut cliprdr);
        assert!(
            msgs.is_empty(),
            "unchanged content must not be re-advertised on every poll"
        );
    }

    #[test]
    fn remote_written_text_is_not_re_advertised_by_poll_local_change() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
        bridge.pump(&mut cliprdr);
        backend_mut(&mut cliprdr)
            .on_format_data_response(FormatDataResponse::new_unicode_string("from remote"));
        bridge.pump(&mut cliprdr);
        assert_eq!(state.lock().unwrap().text.as_deref(), Some("from remote"));

        // The OS clipboard now holds exactly what we just wrote from the remote; polling
        // must see "no change" and must not loop it back as a fresh local advertise.
        bridge.poll_local_change();
        let msgs = bridge.pump(&mut cliprdr);
        assert!(
            msgs.is_empty(),
            "must not loop the remote's own content back to it"
        );
    }

    /// Representative of the defect report's "100 MB log tail" / "big spreadsheet region" —
    /// picked smaller only so the test suite stays fast; it is still ~80x the hash cap, so
    /// nothing about the assertions below depends on the exact multiple.
    const LARGE_PAYLOAD_BYTES: usize = 20 * 1024 * 1024;

    #[test]
    fn single_poll_of_a_huge_payload_only_hashes_the_bounded_prefix() {
        // This is the regression test for the reported defect: before the fix,
        // `content_hash` ran SHA-256 over the *entire* payload every poll — 50-100 ms for
        // 100 MB. One poll of a huge payload must now hash exactly the bounded prefix, not
        // a byte more, regardless of how large the payload actually is.
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        state.lock().unwrap().text = Some("x".repeat(LARGE_PAYLOAD_BYTES));
        reset_hashed_bytes();

        bridge.poll_local_change();

        assert_eq!(
            hashed_bytes(),
            HASH_PREFIX_CAP_BYTES,
            "a single poll must hash exactly the bounded prefix cap, not the full \
             {LARGE_PAYLOAD_BYTES}-byte payload"
        );
        // And the change is still correctly detected and advertised.
        let msgs = bridge.pump(&mut cliprdr);
        assert_eq!(
            msgs.len(),
            1,
            "a genuine large local copy must still be advertised"
        );
    }

    #[test]
    fn repeated_polls_of_unchanged_huge_payload_cost_a_constant_capped_amount_each_time() {
        // The actual defect scenario: the same huge payload sits on the clipboard across
        // many poll ticks (a copy that just stays there while the session keeps running).
        // Before the fix, every one of those ticks re-hashed the whole payload — 50-100 ms
        // apiece, compounding for as long as the content sat on the clipboard. Each
        // "nothing changed" poll after the first must now cost exactly the capped amount,
        // never the full payload size, and never zero (detection must stay live).
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        state.lock().unwrap().text = Some("y".repeat(LARGE_PAYLOAD_BYTES));
        bridge.poll_local_change(); // baseline poll: establishes last_seen_fingerprint
        bridge.pump(&mut cliprdr); // drain the resulting advertise

        reset_hashed_bytes();
        let get_text_calls_before = state.lock().unwrap().get_text_calls;

        const REPEAT_POLLS: usize = 10;
        for _ in 0..REPEAT_POLLS {
            bridge.poll_local_change();
        }

        let msgs = bridge.pump(&mut cliprdr);
        assert!(
            msgs.is_empty(),
            "content never changed; none of the {REPEAT_POLLS} repeat polls may advertise"
        );
        assert_eq!(
            hashed_bytes(),
            REPEAT_POLLS * HASH_PREFIX_CAP_BYTES,
            "each unchanged poll must cost exactly the capped amount — not the \
             {LARGE_PAYLOAD_BYTES}-byte payload, and not zero (detection must stay live)"
        );
        let get_text_calls_after = state.lock().unwrap().get_text_calls;
        assert_eq!(
            get_text_calls_after - get_text_calls_before,
            REPEAT_POLLS,
            "get_text must still be called on every poll — bounding the hash must not \
             disable change detection"
        );
    }

    #[test]
    fn a_tail_only_change_past_the_prefix_cap_with_unchanged_length_is_not_detected() {
        // Documents the accepted trade-off of a bounded-prefix fingerprint: two payloads of
        // the same length that share the same first HASH_PREFIX_CAP_BYTES bytes are
        // indistinguishable to it. This only bites content larger than the cap that shares
        // a huge common prefix and keeps the same total length — accepted because the
        // alternative is the unbounded full-payload hash this fix exists to remove.
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        let shared_prefix = "p".repeat(HASH_PREFIX_CAP_BYTES);
        let payload_a = format!("{shared_prefix}AAAA");
        let payload_b = format!("{shared_prefix}BBBB"); // same length, differs only past the cap
        assert_eq!(payload_a.len(), payload_b.len());

        state.lock().unwrap().text = Some(payload_a);
        bridge.poll_local_change();
        bridge.pump(&mut cliprdr); // drain the first advertise

        state.lock().unwrap().text = Some(payload_b);
        bridge.poll_local_change();
        let msgs = bridge.pump(&mut cliprdr);
        assert!(
            msgs.is_empty(),
            "documented limitation: a same-length change entirely past the prefix cap is \
             not detected by this fingerprint"
        );

        // Sanity check on the other side of that trade-off: a change that alters the
        // *length* is still always detected, even past the cap, because the length is
        // folded into the fingerprint alongside the prefix.
        state.lock().unwrap().text = Some(format!("{shared_prefix}BBBBB"));
        bridge.poll_local_change();
        let msgs = bridge.pump(&mut cliprdr);
        assert_eq!(
            msgs.len(),
            1,
            "a length change past the cap must still be detected"
        );
    }

    #[test]
    fn utf16_decode_strips_trailing_nul() {
        let mut bytes = Vec::new();
        for unit in "hi".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&0u16.to_le_bytes());
        assert_eq!(decode_utf16le_text(&bytes), "hi");
    }

    #[test]
    fn utf16_decode_of_empty_text_is_empty_string() {
        assert_eq!(decode_utf16le_text(&0u16.to_le_bytes()), "");
        assert_eq!(decode_utf16le_text(&[]), "");
    }

    #[test]
    fn utf16_decode_replaces_lone_surrogate_without_panicking() {
        let lone_high_surrogate: u16 = 0xD800;
        let bytes = lone_high_surrogate.to_le_bytes();
        assert_eq!(decode_utf16le_text(&bytes), "\u{FFFD}");
    }
}
