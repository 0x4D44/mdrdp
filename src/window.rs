//! The session window: presents the surface store, captures input, knows no RDP.
//!
//! Three decisions worth stating up front, because each is load-bearing:
//!
//! - **Only the user resizes the session.** `CLAUDE.md` is explicit: a
//!   display-configuration change must never reflow the remote desktop. Every `Resized`
//!   event first changes only the [`Viewport`] — the letterboxed rectangle the session
//!   image is scaled into — and is then read by the [`WindowPolicy`], the one component
//!   able to tell the user dragging from the system rearranging. A resize the policy
//!   judges to be the user's own act renegotiates the session resolution to match, after
//!   a settle delay so a drag costs one renegotiation, not a stream of them. A system
//!   imposition is argued with (the geometry is requested back) and never reaches the
//!   protocol. Fullscreen toggles renegotiate directly — they are user acts by
//!   construction.
//! - **Redraw is driven by change, not by a clock.** [`SurfaceStore::generation`] exists
//!   precisely so the presenter can tell "changed" from "unchanged". A producer nudges
//!   the loop with [`Waker::damaged`]; if the generation has not moved we do not even ask
//!   for a redraw. The event loop otherwise sits in `ControlFlow::Wait` and burns nothing.
//! - **Input leaves through a channel.** The window builds `crate::input::InputEvent`
//!   values and sends them. It never sees a PDU, a socket, or a credential.
//!
//! The session side learns the window has closed by its `Receiver` erroring: the app
//! drops its `Sender` on exit. That is the cue to send a graceful shutdown — abandoning
//! the connection wedges the host (see `CLAUDE.md`).
//!
//! **Security:** the buffer this module touches is session pixels. Nothing here formats,
//! logs, or serialises pixel data, and nothing should be added that does.

use crate::wake::WakingSender;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CustomCursor, Fullscreen, Window, WindowAttributes, WindowId};

use crate::input::{self, InputEvent, PointerMap};
use crate::session::SessionCommand;
use crate::stats::{SessionStats, StatsHandle};
use crate::surface::{PresentationCopy, PresentationSnapshot, SurfaceStore};
use crate::ui::font;
use crate::window_policy::{Geometry, ResizeVerdict, WindowPolicy};

/// What the caller must decide before a window exists.
#[derive(Debug, Clone)]
pub struct WindowConfig {
    pub title: String,
    /// Fixed session pixels. A windowed window opens at this size; fullscreen keeps these
    /// dimensions for the remote desktop while the window fills its monitor.
    pub session_width: u16,
    pub session_height: u16,
    /// Open the window borderless fullscreen on its current monitor.
    pub fullscreen: bool,
    /// When opening fullscreen, ask the session for the monitor's native resolution.
    ///
    /// Off when an explicit `--size` was given: flags always win, so a scripted
    /// `--size WxH --fullscreen` keeps its stated resolution. A later interactive
    /// fullscreen toggle still renegotiates — that is a fresh user action.
    pub negotiate_native_on_start: bool,
    /// Show the stats overlay from the first frame (Settings ▸ Diagnostics).
    pub overlay_on_start: bool,
    /// Allow fullscreen transitions to renegotiate the session resolution
    /// (Settings ▸ Graphics ▸ Dynamic resolution). Off = letterbox only, ever.
    pub dynamic_resolution: bool,
    /// On a monitor past the H.264 encoder ceiling, fullscreen asks for an integer
    /// division of native resolution (5K → 2560x1440 at 2x) instead of the fractional
    /// best fit (Settings ▸ Graphics). See [`crate::session::fullscreen_request`].
    pub integer_fullscreen_fit: bool,
    /// After a *user* resize of a windowed window settles, ask the session to match the
    /// new size, so the desktop renders 1:1 instead of scaling. Off when an explicit
    /// `--size` was given — a stated resolution is pinned, and dragging the window then
    /// only changes how much letterbox surrounds it. Requires `dynamic_resolution`.
    pub follow_window_resize: bool,
    /// The name this session shows on its macOS Dock tile — the favourite's name, or
    /// the host as typed. `None` leaves the platform default (see [`crate::dock`]).
    pub dock_label: Option<String>,
}

impl WindowConfig {
    pub fn new(title: impl Into<String>, session_width: u16, session_height: u16) -> Self {
        WindowConfig {
            title: title.into(),
            session_width,
            session_height,
            fullscreen: false,
            negotiate_native_on_start: true,
            overlay_on_start: false,
            dynamic_resolution: true,
            integer_fullscreen_fit: true,
            follow_window_resize: true,
            dock_label: None,
        }
    }

    /// Name this session on the Dock tile. Cosmetic, and macOS-only in effect.
    pub fn with_dock_label(mut self, label: impl Into<String>) -> Self {
        self.dock_label = Some(label.into());
        self
    }

    /// Show the stats overlay from the first frame.
    pub fn with_overlay_on_start(mut self, on: bool) -> Self {
        self.overlay_on_start = on;
        self
    }

    /// Permit or forbid resolution renegotiation on fullscreen transitions.
    pub fn with_dynamic_resolution(mut self, on: bool) -> Self {
        self.dynamic_resolution = on;
        self
    }

    /// Integer-divide (rather than fractionally clamp) fullscreen requests on a
    /// monitor past the H.264 encoder ceiling.
    pub fn with_integer_fullscreen_fit(mut self, on: bool) -> Self {
        self.integer_fullscreen_fit = on;
        self
    }

    /// Set whether the window should open borderless fullscreen.
    pub fn with_fullscreen(mut self, fullscreen: bool) -> Self {
        self.fullscreen = fullscreen;
        self
    }

    /// Keep the configured session resolution: don't renegotiate when opening
    /// fullscreen, and don't follow window drags. An explicit `--size` means this
    /// resolution, letterboxed where the window disagrees. (An interactive fullscreen
    /// *toggle* still renegotiates — that is a fresh, unambiguous user action.)
    pub fn keeping_stated_resolution(mut self) -> Self {
        self.negotiate_native_on_start = false;
        self.follow_window_resize = false;
        self
    }
}

/// How many consecutive present failures before the window gives up.
///
/// One is a hiccup; a sustained run means the surface is genuinely unusable and holding
/// the session open serves nobody.
const MAX_CONSECUTIVE_PRESENT_FAILURES: u32 = 30;

/// How long after the last user resize before the session is asked to match the window.
///
/// A drag delivers a stream of `Resized` events; each renegotiation costs the server a
/// Deactivate All round, an encoder re-init, and (EGFX state dying with the surface) a
/// decoder re-init here — so exactly one is sent, once the size has stopped moving.
const RESIZE_FOLLOW_SETTLE: Duration = Duration::from_millis(500);

/// How often the title-bar diagnostics refresh.
///
/// Once a second reads comfortably and keeps `set_title` — a real platform call — out of
/// the per-frame path. The refresh is driven by damage, so an idle session's title
/// simply stops updating, which is correct: none of its numbers are moving either.
const TITLE_REFRESH: Duration = Duration::from_secs(1);

/// Messages a producer thread can push into the event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// The surface store may have changed. The loop checks the generation and only then
    /// asks for a redraw.
    Damaged,
    /// The remote changed its pointer shape; mirror it on the local window.
    Cursor(CursorUpdate),
    /// The session ended; close the window.
    Close,
    /// A native (muda) menu item was activated, by id.
    Menu(String),
}

/// The session process's diagnostics windows, by purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagKind {
    Cache,
    Latency,
    Channels,
}

impl DiagKind {
    fn title(self) -> &'static str {
        match self {
            DiagKind::Cache => "Bitmap cache",
            DiagKind::Latency => "Latency and drift",
            DiagKind::Channels => "Channels and codecs",
        }
    }
}

/// Every extra window the session process can open beside the desktop: the three
/// diagnostics screens and the two Help windows.
///
/// One enum for all of them because they share everything that matters — one window
/// per kind, the same Esc/close key handling, the same removal path. What differs is
/// only which body is drawn, and that is one `match` in the redraw arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxKind {
    Diag(DiagKind),
    About,
    Shortcuts,
}

impl AuxKind {
    fn title(self) -> &'static str {
        match self {
            AuxKind::Diag(kind) => kind.title(),
            AuxKind::About => "About mdrdp",
            AuxKind::Shortcuts => "Keyboard shortcuts",
        }
    }

    /// The window size this kind opens at. Aux windows are not resizable, so these
    /// have to fit their content; the Help sizes live beside the bodies they have to
    /// hold, where a layout test can lay out a real frame at exactly them.
    fn size(self) -> [f32; 2] {
        match self {
            AuxKind::Diag(_) => [900.0, 700.0],
            AuxKind::About => crate::ui::help::ABOUT_WINDOW,
            AuxKind::Shortcuts => crate::ui::help::SHORTCUTS_WINDOW,
        }
    }
}

/// The UI bodies for the three diagnostics windows, supplied by whoever holds the
/// stats handles (`main.rs`). The window module stays ignorant of what they draw;
/// each returns `true` when its Close control was used and the window should go.
pub struct DiagnosticsUis {
    pub cache: Box<dyn FnMut(&mut egui::Ui) -> bool>,
    pub latency: Box<dyn FnMut(&mut egui::Ui) -> bool>,
    pub channels: Box<dyn FnMut(&mut egui::Ui) -> bool>,
}

impl DiagnosticsUis {
    fn ui_for(&mut self, kind: DiagKind) -> &mut Box<dyn FnMut(&mut egui::Ui) -> bool> {
        match kind {
            DiagKind::Cache => &mut self.cache,
            DiagKind::Latency => &mut self.latency,
            DiagKind::Channels => &mut self.channels,
        }
    }
}

/// One transient notification card (handoff §8): warn or danger, over the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    /// `true` = warn accent, `false` = danger accent.
    pub warn: bool,
    pub title: String,
    pub body: String,
}

/// What the transients poll returned: freshly fired toasts, plus the warn line the
/// Ctrl+Alt+S overlay should carry while a condition persists (`None` when clear).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TransientReport {
    pub toasts: Vec<Toast>,
    pub warn_line: Option<String>,
}

/// How long a toast stays up (handoff §8: 6 s, stacked).
const TOAST_TTL: Duration = Duration::from_secs(6);

/// How often open diagnostics windows refresh (the handoff's `refresh 1s`).
const DIAG_REFRESH: Duration = Duration::from_secs(1);

/// How long a reveal's repaint may take before the session is called stalled.
///
/// Armed only by a reveal, never by idleness: a still desktop legitimately sends
/// nothing for hours, so "no frames" alone says nothing. A reveal is different — it
/// carries an explicit Refresh Rect for the whole desktop (`session::visibility_pdus`),
/// which the server owes at least one frame in reply. Silence after *that* is a fault,
/// and it is the exact state a session sat in, unreported, while showing a black window
/// on a connection healthy by every other measure (MDR-BUG-FLUX-00008: a reveal produced
/// zero bytes for 14 s). Generous next to the observed timings — a repaint burst arrives
/// inside a second on a healthy host, even at 400 ms RTT.
const REPAINT_GRACE: Duration = Duration::from_secs(5);

/// Title of the card raised while a reveal's repaint is outstanding. Matched to find and
/// refresh the standing card, so the condition is identified by this string, not by
/// position in the stack.
const STALL_TOAST_TITLE: &str = "No picture from the server";

/// Sample every Nth pixel when asking whether a stalled frame is black.
///
/// Sampled, never exhaustive. This runs only while a repaint is already overdue — a rare
/// fault state — and even at this stride a 2560x1440 window still reads over 14,000
/// pixels, which is ample to tell a black screen from a desktop. Scanning whole frames on
/// the present path is precisely what MDR-BUG-FLUX-00005 was raised to stop.
const BLACK_SAMPLE_STRIDE: usize = 256;

/// A remote pointer change, already decoded to pixels. Session-layer types stay out of
/// this module, so the session thread translates IronRDP's pointer outputs into this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorUpdate {
    /// Show the platform's default arrow.
    Default,
    /// The remote is hiding the pointer (games, video players do this).
    Hidden,
    /// A concrete shape, straight-alpha RGBA in session pixels.
    Bitmap {
        width: u16,
        height: u16,
        hotspot_x: u16,
        hotspot_y: u16,
        rgba: Vec<u8>,
    },
}

/// Large canvases spend enough time in scalar scaling that change-driven redraws can
/// outrun the event loop. Keep one present at most every 33 ms (about 30 fps), while
/// continuing to decode and retain the newest complete canvas.
pub(crate) const DAMAGE_PRESENT_INTERVAL: Duration = Duration::from_millis(33);

/// A full IOSurface pool is transient compositor backpressure. Retry quickly enough
/// to stay below one 240 Hz refresh interval without spinning the event loop.
const PRESENT_BACKPRESSURE_RETRY: Duration = Duration::from_millis(2);

fn large_presentation(width: u16, height: u16) -> bool {
    width > 2560 || height > 1440
}

/// Return the next legal present time for a large surface, or `None` when it may run now.
fn presentation_deadline(
    last_presented: Option<Instant>,
    now: Instant,
    width: u16,
    height: u16,
) -> Option<Instant> {
    if !large_presentation(width, height) {
        return None;
    }
    let deadline = last_presented?.checked_add(DAMAGE_PRESENT_INTERVAL)?;
    (now < deadline).then_some(deadline)
}

fn copy_presentation_when_due(
    store: &SurfaceStore,
    snapshot: &mut PresentationSnapshot,
    presented: Option<u64>,
    last_presented: Option<Instant>,
    now: Instant,
    fallback_dimensions: (u16, u16),
) -> Result<PresentationCopy, Instant> {
    let generation = store.generation();
    let (width, height) = store
        .presentation_surface()
        .map_or(fallback_dimensions, |surface| {
            (surface.width, surface.height)
        });
    if presented != Some(generation)
        && let Some(deadline) = presentation_deadline(last_presented, now, width, height)
    {
        return Err(deadline);
    }
    Ok(store.copy_presentation_state(snapshot))
}

/// The one pending `Damaged` notification shared by the producer and event-loop threads.
///
/// A notification means only "the store may have changed"; once one is queued, another
/// carries no additional information. The bit is cleared at handler entry so damage that
/// races with handling gets a fresh event.
#[derive(Debug, Clone, Default)]
struct DamageGate(Arc<AtomicBool>);

impl DamageGate {
    fn claim(&self) -> bool {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn clear(&self) {
        self.0.store(false, Ordering::Release);
    }

    fn send_failed(&self) {
        self.clear();
    }
}

/// A `Send` handle onto a running window, for the thread that owns the RDP session.
#[derive(Debug, Clone)]
pub struct Waker {
    proxy: EventLoopProxy<SessionEvent>,
    damage: DamageGate,
}

impl Waker {
    /// Tell the window the store may have changed. `false` means the window is gone.
    pub fn damaged(&self) -> bool {
        if !self.damage.claim() {
            return true;
        }
        if self.proxy.send_event(SessionEvent::Damaged).is_ok() {
            true
        } else {
            self.damage.send_failed();
            false
        }
    }

    /// Ask the window to close. `false` means it already has.
    pub fn close(&self) -> bool {
        self.proxy.send_event(SessionEvent::Close).is_ok()
    }

    /// Mirror a remote pointer change. `false` means the window is gone.
    pub fn cursor(&self, update: CursorUpdate) -> bool {
        self.proxy.send_event(SessionEvent::Cursor(update)).is_ok()
    }
}

#[derive(Debug)]
pub enum WindowError {
    EventLoop(String),
    Os(String),
    Present(String),
}

impl std::fmt::Display for WindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WindowError::EventLoop(m) => write!(f, "event loop failed: {m}"),
            WindowError::Os(m) => write!(f, "window creation failed: {m}"),
            WindowError::Present(m) => write!(f, "presenting the frame failed: {m}"),
        }
    }
}

impl std::error::Error for WindowError {}

/// Where the session image lands inside the window, in window pixels.
///
/// Aspect ratio is preserved, so one axis usually has bars. This is also the inverse
/// mapping for the pointer — the same numbers, used backwards — which is why it lives in
/// one type instead of two that can drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub dest_x: u32,
    pub dest_y: u32,
    pub dest_width: u32,
    pub dest_height: u32,
    pub session_width: u16,
    pub session_height: u16,
}

impl Viewport {
    /// Fit `session_width` x `session_height` inside the window, centred, aspect preserved.
    pub fn letterbox(
        window_width: u32,
        window_height: u32,
        session_width: u16,
        session_height: u16,
    ) -> Viewport {
        let empty = Viewport {
            dest_x: 0,
            dest_y: 0,
            dest_width: 0,
            dest_height: 0,
            session_width,
            session_height,
        };
        if window_width == 0 || window_height == 0 || session_width == 0 || session_height == 0 {
            return empty;
        }

        let scale = (f64::from(window_width) / f64::from(session_width))
            .min(f64::from(window_height) / f64::from(session_height));
        // `max(1)` keeps a heavily shrunk window showing a sliver rather than nothing.
        let dest_width = ((f64::from(session_width) * scale).round() as u32).clamp(1, window_width);
        let dest_height =
            ((f64::from(session_height) * scale).round() as u32).clamp(1, window_height);

        Viewport {
            dest_x: (window_width - dest_width) / 2,
            dest_y: (window_height - dest_height) / 2,
            dest_width,
            dest_height,
            session_width,
            session_height,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dest_width == 0 || self.dest_height == 0
    }
}

impl PointerMap for Viewport {
    /// Undo the letterbox: window pixel in, session pixel out.
    ///
    /// Positions in the bars clamp to the nearest edge pixel rather than vanishing, so a
    /// drag that wanders off the image keeps tracking.
    fn to_session(&self, x: f64, y: f64) -> Option<(u16, u16)> {
        if self.is_empty() {
            return None;
        }
        let sx = (x - f64::from(self.dest_x)) * f64::from(self.session_width)
            / f64::from(self.dest_width);
        let sy = (y - f64::from(self.dest_y)) * f64::from(self.session_height)
            / f64::from(self.dest_height);
        input::clamp_to_session(sx, sy, self.session_width, self.session_height)
    }
}

/// Scale one RGBA session frame into a 0RGB `u32` window buffer, letterboxed.
///
/// Pure, so the scaling and the channel order are testable without a window.
///
/// - `src` is RGBA8, tightly packed, `viewport.session_width` pixels per row.
/// - `dst` is softbuffer's format: `0x00RRGGBB`, one `u32` per window pixel.
///
/// Sampling is nearest-neighbour. At 1:1 — the common case, since the window opens at
/// the session size — that is an exact copy, and the row-index table means a scaled frame
/// costs no divisions per pixel.
pub fn present_into(
    dst: &mut [u32],
    window_width: u32,
    window_height: u32,
    viewport: &Viewport,
    src: &[u8],
) {
    let needed = usize::from(viewport.session_width) * usize::from(viewport.session_height) * 4;
    let fits = dst.len() >= (window_width as usize) * (window_height as usize);
    // `letterbox` never places the origin outside the window, but this is a public
    // function taking a caller-supplied viewport: an out-of-window origin would underflow
    // the column arithmetic below. Blank the frame instead.
    let origin_inside = viewport.dest_x < window_width && viewport.dest_y < window_height;
    // A zero-dimension session underflows the column table below (`session_width - 1`)
    // and indexes an empty source. `letterbox` never produces one, but this is a public
    // function taking a caller-supplied viewport, and its contract says it blanks rather
    // than panics.
    let session_real = viewport.session_width > 0 && viewport.session_height > 0;
    if !fits || !origin_inside || !session_real || viewport.is_empty() || src.len() < needed {
        dst.fill(0);
        return;
    }

    // Saturating, not plain `+`: the guards above check the *origin* is inside the window
    // but say nothing about the extent, and this is a public function taking a
    // caller-supplied viewport. A dest_width near u32::MAX overflows — a panic in debug,
    // a wrap to a tiny value in release, which silently paints a sliver instead of
    // blanking as this function's contract promises.
    let dest_right = viewport
        .dest_x
        .saturating_add(viewport.dest_width)
        .min(window_width);
    let dest_bottom = viewport
        .dest_y
        .saturating_add(viewport.dest_height)
        .min(window_height);

    // Column table: one division per column instead of one per pixel.
    let mut col: Vec<u32> = Vec::with_capacity((dest_right - viewport.dest_x) as usize);
    for x in viewport.dest_x..dest_right {
        let sx = u64::from(x - viewport.dest_x) * u64::from(viewport.session_width)
            / u64::from(viewport.dest_width);
        col.push((sx as u32).min(u32::from(viewport.session_width) - 1));
    }

    for y in 0..window_height {
        let row = &mut dst
            [(y as usize) * (window_width as usize)..(y as usize + 1) * (window_width as usize)];

        if y < viewport.dest_y || y >= dest_bottom {
            row.fill(0);
            continue;
        }

        let sy = u64::from(y - viewport.dest_y) * u64::from(viewport.session_height)
            / u64::from(viewport.dest_height);
        let sy = (sy as u32).min(u32::from(viewport.session_height) - 1);
        let src_row = (sy as usize) * usize::from(viewport.session_width) * 4;

        row[..viewport.dest_x as usize].fill(0);
        row[dest_right as usize..].fill(0);

        for (i, out) in row[viewport.dest_x as usize..dest_right as usize]
            .iter_mut()
            .enumerate()
        {
            let off = src_row + (col[i] as usize) * 4;
            let r = u32::from(src[off]);
            let g = u32::from(src[off + 1]);
            let b = u32::from(src[off + 2]);
            *out = (r << 16) | (g << 8) | b;
        }
    }
}

/// Draw the stats panel over the session image.
///
/// Pure so the geometry is testable without a window. The panel is drawn on a dimmed
/// backing rather than straight onto the desktop: white text over an arbitrary remote
/// screen is unreadable about half the time, and the numbers exist to be read.
pub fn draw_overlay(dst: &mut [u32], window_width: u32, window_height: u32, lines: &[String]) {
    if lines.is_empty() || window_width == 0 || window_height == 0 {
        return;
    }
    const MARGIN: i32 = 12;
    const PAD: i32 = 8;

    let widest = lines.iter().map(|l| font::text_width(l)).max().unwrap_or(0);
    let panel_w = widest + PAD * 2;
    let panel_h = font::CHAR_H * lines.len() as i32 + PAD * 2;

    // Clamp so a narrow window gets a clipped panel rather than none at all.
    let x0 = MARGIN.min(window_width as i32);
    let y0 = MARGIN.min(window_height as i32);
    let x1 = (x0 + panel_w).min(window_width as i32);
    let y1 = (y0 + panel_h).min(window_height as i32);

    for y in y0..y1 {
        let row = y as usize * window_width as usize;
        for x in x0..x1 {
            let Some(px) = dst.get_mut(row + x as usize) else {
                continue;
            };
            // Halve each channel: darkens whatever is behind without hiding it, and
            // needs no alpha channel that softbuffer's 0RGB format does not have.
            *px = (*px >> 1) & 0x007f_7f7f;
        }
    }

    for (i, line) in lines.iter().enumerate() {
        font::draw_text(
            dst,
            window_width as usize,
            window_height as usize,
            x0 + PAD,
            y0 + PAD + font::CHAR_H * i as i32,
            line,
            0x00ff_ffff,
        );
    }
}

/// What the blank-window notice says.
///
/// Pure so the wording is testable — it is the whole point of the notice, and the last
/// line is load-bearing: the failure this exists for looks exactly like a dropped
/// connection to the user, and it is not one.
/// The Davidson tartan sett, as (colour, thread count) from one selvedge to the pivot.
///
/// Approximate, and deliberately labelled so: this is a recognisable Davidson — dark blue
/// and green grounds, black guards, a red overstripe and a white line — not an
/// authoritative reproduction of a registered thread count. It exists to be
/// unmistakable, not accurate.
const TARTAN_SETT: &[(u32, u16)] = &[
    (0x0000_0000, 8),  // black
    (0x0010_2A4A, 34), // navy
    (0x0000_0000, 30),
    (0x000E_3B26, 34), // forest green
    (0x0000_0000, 6),
    (0x008E_1B1B, 8), // red overstripe
    (0x0000_0000, 6),
    (0x000E_3B26, 34),
    (0x0000_0000, 30),
    (0x0010_2A4A, 34),
    (0x0000_0000, 8),
    (0x00D8_D8D8, 4), // white line at the pivot
];

/// Expand [`TARTAN_SETT`] into one full repeat, reflected about the pivot.
///
/// A tartan sett is symmetric: it runs to a pivot and mirrors back. Building the mirrored
/// half here rather than spelling it out keeps the constant honest — the two halves cannot
/// drift apart.
fn tartan_threads() -> Vec<u32> {
    let mut threads = Vec::new();
    for &(colour, count) in TARTAN_SETT {
        threads.extend(std::iter::repeat_n(colour, usize::from(count)));
    }
    // Mirror everything except the pivot thread itself, so the reflection has no seam.
    let reflected: Vec<u32> = threads.iter().rev().skip(1).copied().collect();
    threads.extend(reflected);
    threads
}

/// Fill `dst` with tartan.
///
/// Pure so the weave is testable without a window. Warp is indexed by column and weft by
/// row from the same sett — that is what makes a tartan a tartan — and the 2/2 twill picks
/// between them, which produces the diagonal texture the eye reads as woven rather than as
/// a checkerboard.
///
/// Drawn only where the window would otherwise be black with nothing behind it. Black is
/// ambiguous: a locked desktop, a screensaver and a dead session all look identical, and
/// that ambiguity is what made MDR-BUG-FLUX-00008 take a day to pin down. Nothing about a
/// tartan screen looks like a working remote desktop.
///
/// Note this never touches surface memory. ClearCodec seeds its decode from whatever the
/// surface already holds (`gfx::apply_wire_to_surface1`), so writing a pattern into a
/// surface would corrupt live decodes — FreeRDP memsets surfaces on ResetGraphics, and we
/// deliberately do not.
pub fn fill_tartan(dst: &mut [u32], window_width: u32, window_height: u32) {
    if window_width == 0 || window_height == 0 {
        return;
    }
    let threads = tartan_threads();
    let period = threads.len();
    if period == 0 {
        return;
    }
    let width = window_width as usize;
    for (i, px) in dst.iter_mut().enumerate() {
        let (x, y) = (i % width, i / width);
        // 2/2 twill: two-pixel diagonal bands decide warp or weft.
        *px = if ((x + y) / 2) % 2 == 0 {
            threads[x % period]
        } else {
            threads[y % period]
        };
    }
}

pub fn blank_notice_lines(waiting: Option<Duration>) -> Vec<String> {
    let detail = match waiting {
        Some(d) => format!(
            "Repaint requested {}s ago - nothing has arrived yet.",
            d.as_secs()
        ),
        None => "No desktop surface has been received yet.".to_owned(),
    };
    vec![
        "Waiting for the server to send a picture.".to_owned(),
        detail,
        "The connection is up - this is not a dropped session.".to_owned(),
    ]
}

/// Whether the composited frame is, on a sampled basis, entirely black.
///
/// Pure so the threshold and the sampling are testable. Used only to decide whether the
/// blank-window notice may be drawn over a frame we already know is stalled — never on
/// its own. A black *desktop* is legitimate (a lock screen, a full-screen terminal, a
/// screensaver), so blackness alone must never trigger anything; blackness plus a repaint
/// we asked for and never got is not legitimate, and that pairing is what makes it safe
/// to write over the frame.
fn frame_looks_black(buffer: &[u32], stride: usize) -> bool {
    const DARK: u32 = 0x10;
    buffer
        .iter()
        .step_by(stride.max(1))
        .all(|&px| (px & 0xff) <= DARK && ((px >> 8) & 0xff) <= DARK && ((px >> 16) & 0xff) <= DARK)
}

/// Whether a reveal's outstanding repaint has become a stall.
///
/// Pure so the rule is testable without a window. Two ways to be fine: a frame arrived
/// (the count moved), or the grace has not run out yet. Note what is deliberately NOT
/// part of this — how long the desktop has been still. A static desktop legitimately
/// sends nothing for hours; only a repaint we explicitly asked for and never got is a
/// fault, which is why this is armed by a reveal and never by idleness.
fn repaint_stalled(elapsed: Duration, frames_when_asked: u64, frames_now: u64) -> bool {
    frames_now == frames_when_asked && elapsed >= REPAINT_GRACE
}

/// Explain a window that has no session pixels at all, centred on the blank frame.
///
/// Pure so the wording and geometry are testable without a window. Drawn ONLY when no
/// surface is mapped to output — that is the one case where black means "we have
/// nothing" rather than "the desktop is dark", so there is no content to obscure. A
/// black-*pixel* test would be the wrong trigger: a locked Windows desktop, a full-screen
/// terminal or a screensaver are all legitimately black, and painting over them would
/// fire on healthy sessions and hide real content.
///
/// `waiting` is how long a reveal's repaint has been outstanding, when one is.
pub fn draw_blank_notice(
    dst: &mut [u32],
    window_width: u32,
    window_height: u32,
    waiting: Option<Duration>,
) {
    if window_width == 0 || window_height == 0 {
        return;
    }
    let lines = blank_notice_lines(waiting);

    let block_h = font::CHAR_H * lines.len() as i32;
    let top = (window_height as i32 - block_h) / 2;
    let widest = lines.iter().map(|l| font::text_width(l)).max().unwrap_or(0);

    // Darken a panel behind the text. The notice now sits on tartan, and plain glyphs over
    // a woven pattern are unreadable — the same reason `draw_overlay` dims rather than
    // writing straight onto the desktop.
    const PAD: i32 = 12;
    let x0 = ((window_width as i32 - widest) / 2 - PAD).max(0);
    let y0 = (top - PAD).max(0);
    let x1 = (x0 + widest + PAD * 2).min(window_width as i32);
    let y1 = (y0 + block_h + PAD * 2).min(window_height as i32);
    for y in y0..y1 {
        let row = y as usize * window_width as usize;
        for x in x0..x1 {
            let Some(px) = dst.get_mut(row + x as usize) else {
                continue;
            };
            *px = (*px >> 2) & 0x003f_3f3f;
        }
    }

    for (i, line) in lines.iter().enumerate() {
        let x = (window_width as i32 - font::text_width(line)) / 2;
        font::draw_text(
            dst,
            window_width as usize,
            window_height as usize,
            x.max(0),
            top + font::CHAR_H * i as i32,
            line,
            0x00ff_ffff,
        );
    }
}

/// Draw transient toast cards, stacked bottom-right (handoff §8).
///
/// Pure so the geometry is testable. Softbuffer gives no vector text, so the cards
/// use the overlay's bitmap font — colours and geometry per the handoff, typography
/// approximate (recorded as a deviation in the build journal).
pub fn draw_toasts(dst: &mut [u32], window_width: u32, window_height: u32, toasts: &[Toast]) {
    if toasts.is_empty() || window_width == 0 || window_height == 0 {
        return;
    }
    const WIDTH: i32 = 400;
    const PAD: i32 = 14;
    const MARGIN: i32 = 16;
    const GAP: i32 = 10;
    const BG: u32 = 0x001E2126; // bg.raised
    const BORDER: u32 = 0x00414A54; // line.strong
    const WARN: u32 = 0x00FFC93C;
    const DANGER: u32 = 0x00FF5D4D;
    const TITLE: u32 = 0x00EAEEF3;
    const BODY: u32 = 0x00A6AEB9;

    let card_h = PAD * 2 + font::CHAR_H * 2 + 4;
    let mut bottom = window_height as i32 - MARGIN;
    for (toast, _) in toasts.iter().rev().map(|t| (t, ())) {
        let top = bottom - card_h;
        if top < 0 {
            break; // A stack taller than the window keeps its newest cards.
        }
        let left = (window_width as i32 - MARGIN - WIDTH).max(0);
        let right = (left + WIDTH).min(window_width as i32);
        for y in top..bottom {
            let row = y as usize * window_width as usize;
            for x in left..right {
                let Some(px) = dst.get_mut(row + x as usize) else {
                    continue;
                };
                let on_border = y == top || y == bottom - 1 || x == right - 1;
                let accent = x < left + 2;
                *px = if accent {
                    if toast.warn { WARN } else { DANGER }
                } else if on_border {
                    BORDER
                } else {
                    BG
                };
            }
        }
        font::draw_text(
            dst,
            window_width as usize,
            window_height as usize,
            left + PAD,
            top + PAD,
            &toast.title,
            TITLE,
        );
        font::draw_text(
            dst,
            window_width as usize,
            window_height as usize,
            left + PAD,
            top + PAD + font::CHAR_H + 4,
            &toast.body,
            BODY,
        );
        bottom = top - GAP;
    }
}

/// The title-bar text: the base title plus live diagnostics.
///
/// Pure so the wording is testable without a window. The title is the one always-visible
/// surface the client has, so it carries the numbers someone glances at to answer "why
/// does this feel slow?" — resolution, latency (with drift), frame rate, inbound
/// bitrate, cache hit rate. The full breakdown stays on the Ctrl+Alt+S overlay.
pub fn title_line(
    base: &str,
    session_width: u16,
    session_height: u16,
    stats: &SessionStats,
    fps: f64,
    mbps: f64,
    codec: Option<&str>,
) -> String {
    let mut title = format!("{base} — {session_width}x{session_height}");
    if let Some(codec) = codec {
        title.push_str(&format!(" · {codec}"));
    }
    if let Some(p) = stats.latency.recent() {
        let p50_ms = f64::from(p.p50) / 1000.0;
        match stats.latency.drift_us() {
            Some(d) if d != 0 => {
                let drift_ms = d as f64 / 1000.0;
                title.push_str(&format!(" · {p50_ms:.1}ms ({drift_ms:+.1})"));
            }
            _ => title.push_str(&format!(" · {p50_ms:.1}ms")),
        }
    }
    if fps > 0.0 {
        title.push_str(&format!(" · {fps:.0} fps"));
    }
    if mbps > 0.0 {
        title.push_str(&format!(" · {mbps:.1} Mb/s"));
    }
    if let Some(hit) = stats.cache.hit_rate() {
        title.push_str(&format!(" · cache {:.0}%", hit * 100.0));
    }
    if stats.decode_errors > 0 || stats.undecoded_regions > 0 {
        title.push_str(&format!(
            " · STALE {}",
            stats.decode_errors + stats.undecoded_regions
        ));
    }
    title
}

/// The codec segment of the title bar: what painted pixels since the last title
/// refresh, dominant first.
///
/// Diffs the cumulative painted-bytes counters between two refreshes, so it names
/// what is carrying the picture *now*, not whatever once painted. A codec under a
/// tenth of the interval's paint is dropped as noise — a stray Uncompressed blit
/// rides alongside every stream and would otherwise flicker in and out of the
/// title. An idle interval returns `None` so the caller keeps the last active mix
/// instead of blanking the segment.
pub fn codec_note(
    painted: &std::collections::BTreeMap<String, u64>,
    painted_before: &std::collections::BTreeMap<String, u64>,
) -> Option<String> {
    let mut deltas: Vec<(&str, u64)> = painted
        .iter()
        .map(|(name, &bytes)| {
            let before = painted_before.get(name).copied().unwrap_or(0);
            (name.as_str(), bytes.saturating_sub(before))
        })
        .filter(|&(_, delta)| delta > 0)
        .collect();
    let total: u64 = deltas.iter().map(|&(_, delta)| delta).sum();
    if total == 0 {
        return None;
    }
    deltas.retain(|&(_, delta)| delta.saturating_mul(10) >= total);
    deltas.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let names: Vec<&str> = deltas
        .iter()
        .map(|&(name, _)| friendly_codec(name))
        .collect();
    Some(names.join("+"))
}

/// Title-bar wording for a stats codec key.
///
/// The stats maps key RFX Progressive as `WireToSurface2/RemoteFxProgressive`
/// (the wire PDU plus the `Codec2Type` debug name); the title has no room for
/// either half of that.
fn friendly_codec(name: &str) -> &str {
    match name.strip_prefix("WireToSurface2/").unwrap_or(name) {
        "RemoteFxProgressive" => "Progressive",
        other => other,
    }
}

/// A window bound to a surface store, not yet running.
///
/// Split from `run` so the caller can take a [`Waker`] before the loop takes over the
/// thread — `run` never returns until the window closes.
pub struct SessionWindow {
    event_loop: EventLoop<SessionEvent>,
    damage: DamageGate,
    config: WindowConfig,
    store: Arc<Mutex<SurfaceStore>>,
    input: WakingSender<InputEvent>,
    latest_mouse_move: Option<Arc<input::LatestMouseMove>>,
    stats: Option<StatsHandle>,
    on_exit: Option<Box<dyn FnMut()>>,
    commands: Option<WakingSender<SessionCommand>>,
    diagnostics: Option<DiagnosticsUis>,
    transients: Option<Box<dyn FnMut() -> TransientReport>>,
    /// Mirrors the window's fullscreen state for whoever outlives the loop — the exit
    /// path persists it so the next launch can restore it. An atomic rather than a
    /// return value because the Cmd+Q path never returns (see [`Self::on_exit`]).
    fullscreen_state: Arc<AtomicBool>,
}

impl SessionWindow {
    /// Build the event loop. **Must be called on the main thread** — winit requires it on
    /// macOS and Windows alike.
    ///
    /// There is exactly one event loop per process: winit sets a global flag on the first
    /// build and every later one fails with `RecreationAttempt`. Anything else that needs
    /// a window — the favourites launcher — must therefore run on *this* loop, which is
    /// why [`event_loop`](Self::event_loop) exists rather than each screen making its own.
    pub fn event_loop() -> Result<EventLoop<SessionEvent>, WindowError> {
        let event_loop = EventLoop::<SessionEvent>::with_user_event()
            .build()
            .map_err(|e| WindowError::EventLoop(e.to_string()))?;
        // Wait, not Poll: nothing here is animated, so the loop should sleep until the
        // OS or a producer has something to say.
        event_loop.set_control_flow(ControlFlow::Wait);
        Ok(event_loop)
    }

    /// Bind a window to the process's event loop.
    ///
    /// Takes the loop rather than building one so the launcher can have run on it first;
    /// see [`event_loop`](Self::event_loop).
    pub fn new(
        event_loop: EventLoop<SessionEvent>,
        config: WindowConfig,
        store: Arc<Mutex<SurfaceStore>>,
        input: WakingSender<InputEvent>,
    ) -> Result<Self, WindowError> {
        event_loop.set_control_flow(ControlFlow::Wait);
        let fullscreen_state = Arc::new(AtomicBool::new(config.fullscreen));
        Ok(SessionWindow {
            event_loop,
            damage: DamageGate::default(),
            config,
            store,
            input,
            latest_mouse_move: None,
            stats: None,
            on_exit: None,
            commands: None,
            diagnostics: None,
            transients: None,
            fullscreen_state,
        })
    }

    /// Coalesce only physical window motion. Scripted input, keys, buttons, and wheels
    /// keep using the reliable FIFO on both transports.
    pub fn with_latest_mouse_move(mut self, moves: Option<Arc<input::LatestMouseMove>>) -> Self {
        self.latest_mouse_move = moves;
        self
    }

    /// Wire up the channel for asking the session to do things — currently, to
    /// renegotiate its resolution when the window goes fullscreen.
    ///
    /// Optional: without it the fullscreen toggle still works, it just letterboxes.
    pub fn with_commands(mut self, commands: WakingSender<SessionCommand>) -> Self {
        self.commands = Some(commands);
        self
    }

    /// Give the window the three diagnostics UI bodies its Diagnostics menu opens.
    ///
    /// Optional: without it the menu items open nothing and say so on stderr — a
    /// probe-harness window has no stats to show.
    pub fn with_diagnostics(mut self, diagnostics: DiagnosticsUis) -> Self {
        self.diagnostics = Some(diagnostics);
        self
    }

    /// Poll `watch` for transient conditions (a lost audio device, a refused resize):
    /// fired toasts draw bottom-right for six seconds, and the returned warn line
    /// rides the Ctrl+Alt+S overlay while the condition lasts.
    pub fn with_transients(mut self, watch: Box<dyn FnMut() -> TransientReport>) -> Self {
        self.transients = Some(watch);
        self
    }

    /// A handle that always holds the window's current fullscreen state, usable after
    /// the loop has exited (including the Cmd+Q path, where `run` never returns).
    pub fn fullscreen_state(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.fullscreen_state)
    }

    /// Run `f` when the event loop is tearing down, however it was told to.
    ///
    /// This is not a convenience. On macOS winit installs a default menu whose Quit item
    /// is bound to AppKit's `terminate:`, and winit does not implement
    /// `applicationShouldTerminate:` — so Cmd+Q takes the default `NSTerminateNow` path:
    /// AppKit calls `exit(0)` from inside `-[NSApplication run]`. `run_app` never
    /// returns, so every statement after it is skipped, destructors do not run, and the
    /// RDP session is abandoned with no Shutdown Request. Abandoned sessions accumulate
    /// on a Windows host until it stops accepting logons — the exact failure this
    /// codebase has already been bitten by once.
    ///
    /// winit does emit `LoopExiting` before that `exit(0)`, which reaches
    /// [`ApplicationHandler::exiting`]. That callback is the only place a disconnect can
    /// still be sent, so it is where the hook runs. Closing the window normally reaches
    /// the same callback, so this path is not a rarely-exercised special case.
    pub fn on_exit(mut self, f: impl FnMut() + 'static) -> Self {
        self.on_exit = Some(Box::new(f));
        self
    }

    /// Show these counters when the user asks for the overlay.
    ///
    /// Optional so a window can be opened without any: the probe harness has no session
    /// stats to show, and an overlay of zeroes would be worse than no overlay.
    pub fn with_stats(mut self, stats: StatsHandle) -> Self {
        self.stats = Some(stats);
        self
    }

    /// A handle the session thread can use to nudge or close the window.
    pub fn waker(&self) -> Waker {
        Waker {
            proxy: self.event_loop.create_proxy(),
            damage: self.damage.clone(),
        }
    }

    /// Run until the window closes. Returns the still-usable event loop, so an
    /// epilogue (the Session-ended dialog) can run another on-demand cycle on it —
    /// winit permits exactly one loop per process, ever.
    ///
    /// On the macOS Cmd+Q path this never returns (AppKit exits the process); the
    /// `on_exit` hook is what still runs.
    pub fn run(self) -> Result<EventLoop<SessionEvent>, WindowError> {
        use winit::platform::run_on_demand::EventLoopExtRunOnDemand as _;
        let SessionWindow {
            mut event_loop,
            damage,
            config,
            store,
            input,
            latest_mouse_move,
            stats,
            on_exit,
            commands,
            diagnostics,
            transients,
            fullscreen_state,
        } = self;
        let mut app = SessionApp::new(config, store, input, stats);
        app.damage = damage;
        app.latest_mouse_move = latest_mouse_move;
        app.on_exit = on_exit;
        app.commands = commands;
        app.diagnostics = diagnostics;
        app.transients = transients;
        app.fullscreen_state = fullscreen_state;
        // Menu activations arrive on muda's own channel; forward them into the loop so
        // they are handled on the main thread with the rest of the window state. The
        // handler is process-global, which is fine: this process has one window.
        let menu_proxy = event_loop.create_proxy();
        muda::MenuEvent::set_event_handler(Some(move |event: muda::MenuEvent| {
            let _ = menu_proxy.send_event(SessionEvent::Menu(event.id().0.clone()));
        }));
        event_loop
            .run_app_on_demand(&mut app)
            .map_err(|e| WindowError::EventLoop(e.to_string()))?;
        match app.failure.take() {
            Some(e) => Err(e),
            None => Ok(event_loop),
        }
    }
}

fn window_attributes(config: &WindowConfig) -> WindowAttributes {
    Window::default_attributes()
        .with_title(config.title.clone())
        .with_inner_size(PhysicalSize::new(
            u32::from(config.session_width),
            u32::from(config.session_height),
        ))
        .with_fullscreen(config.fullscreen.then_some(Fullscreen::Borderless(None)))
        .with_decorations(!config.fullscreen)
}

/// The session resize a settled window size calls for, if any.
///
/// `None` for a degenerate size (minimised) and for a window already matching the
/// session — re-requesting the current resolution would cost a server Deactivate All
/// round for zero change. Encoder ceilings and MS-RDPEDISP bounds are the session
/// pump's job (`crate::session::service_resize`), not repeated here.
fn follow_request(
    width: u32,
    height: u32,
    session_width: u16,
    session_height: u16,
) -> Option<(u32, u32)> {
    if width == 0 || height == 0 {
        return None;
    }
    if width == u32::from(session_width) && height == u32::from(session_height) {
        return None;
    }
    Some((width, height))
}

struct SessionApp {
    config: WindowConfig,
    store: Arc<Mutex<SurfaceStore>>,
    damage: DamageGate,
    input: WakingSender<InputEvent>,
    latest_mouse_move: Option<Arc<input::LatestMouseMove>>,
    window: Option<Arc<Window>>,
    presenter: Option<crate::present::Presenter>,
    viewport: Viewport,
    /// Last pointer position, in session pixels. Buttons and scrolls need coordinates
    /// and winit does not repeat them.
    cursor: Option<(u16, u16)>,
    /// Sub-notch scroll travel held between wheel events. See
    /// [`input::WheelAccumulator`].
    wheel: input::WheelAccumulator,
    /// Keys forwarded down and not yet up, released en masse on focus loss so the
    /// server never keeps a modifier the OS stole the release of. See
    /// [`input::KeyLedger`].
    keys: input::KeyLedger,
    /// The store generation last put on screen. `None` until the first frame.
    presented: Option<u64>,
    failure: Option<WindowError>,
    /// Consecutive failed presents; reset by any successful one.
    present_failures: u32,
    /// Tells a resize the user asked for from one the system imposed.
    policy: WindowPolicy,
    /// Live session counters, when the caller wired any up.
    stats: Option<StatsHandle>,
    show_stats: bool,
    modifiers: ModifiersState,
    /// Runs on `LoopExiting` — the last point at which the session can be disconnected
    /// cleanly, including when AppKit is about to `exit(0)` under us. See
    /// [`SessionWindow::on_exit`].
    on_exit: Option<Box<dyn FnMut()>>,
    /// Monotonic origin for the policy's timestamps. Wall-clock would let a clock
    /// adjustment overnight — exactly when displays sleep — corrupt the timing.
    started: Instant,
    /// Channel for asking the session to renegotiate its resolution. `None` in windows
    /// with no session behind them (the probe harness).
    commands: Option<WakingSender<SessionCommand>>,
    /// Whether the window is currently fullscreen — ours to track, because winit reports
    /// transitions only as ordinary `Resized` events.
    fullscreen: bool,
    /// Whether the OS reports the window fully occluded (covered, minimised, or on a
    /// hidden Space). While true, damage does not request redraws — nobody can see
    /// them — and the session is asked to suppress server updates entirely.
    occluded: bool,
    /// Mirror of `fullscreen` readable after the loop dies. See
    /// [`SessionWindow::fullscreen_state`].
    fullscreen_state: Arc<AtomicBool>,
    /// The session size the user ran windowed at, restored when leaving fullscreen.
    /// Follows the window when a settled user resize renegotiates the resolution.
    windowed_session: (u16, u16),
    /// When a settled user resize should renegotiate the session resolution — the
    /// debounce deadline, re-armed by each `Resized` the policy judges to be the user
    /// and cancelled by anything that says otherwise (a display event, a restore, a
    /// fullscreen transition).
    resize_settle: Option<Instant>,
    /// Title diagnostics bookkeeping: last refresh, and the counters at that refresh so
    /// fps and bitrate are deltas rather than lifetime averages.
    last_title_refresh: Instant,
    title_frames: u64,
    title_bytes: u64,
    /// Painted-bytes counters at the last title refresh, for the codec diff.
    title_codec_painted: std::collections::BTreeMap<String, u64>,
    /// The last non-idle codec mix, kept so an idle second doesn't blank it.
    title_codec: Option<String>,
    /// Remote cursor shapes already turned into OS cursors, keyed by content hash.
    /// Windows re-sends the same few shapes constantly; rebuilding an `NSCursor` for
    /// each would churn for nothing.
    cursor_cache: std::collections::HashMap<u64, CustomCursor>,
    /// Whether the remote asked the pointer hidden, so a shape update can restore it.
    cursor_hidden: bool,
    /// UI bodies for the diagnostics windows, when the caller supplied any.
    diagnostics: Option<DiagnosticsUis>,
    /// Open auxiliary windows. At most one per [`AuxKind`].
    aux: Vec<(AuxKind, crate::ui::egui_host::AuxWindow)>,
    /// The About window's icon texture, loaded on its first frame. Belongs to that
    /// window's egui context, so it is dropped with the window.
    about_icon: Option<egui::TextureHandle>,
    /// The native session menu bar; dropping it removes the menu.
    menu: Option<session_menu::SessionMenuBar>,
    /// Last 1 Hz diagnostics refresh, used with `ControlFlow::WaitUntil`.
    last_diag_refresh: Instant,
    /// Transient-condition poll, when the caller wired one up.
    transients: Option<Box<dyn FnMut() -> TransientReport>>,
    /// Live toasts with their birth instants; expired ones drop on redraw.
    toasts: Vec<(Toast, Instant)>,
    /// The persistent warn line for the overlay, while a condition lasts.
    warn_line: Option<String>,
    /// A reveal is waiting for its repaint: when it was asked for, and the frame count
    /// at that moment. Cleared as soon as a frame lands, or when the stall is reported.
    repaint_awaited: Option<(Instant, u64)>,
    /// Whether the current stall has already been announced, so the log and the toast
    /// fire once per episode rather than once per tick.
    stall_announced: bool,
    /// Reused complete copy of the surface currently being presented. The store lock is
    /// held only while this buffer is refreshed; all expensive conversion follows after
    /// the copy.
    presentation: PresentationSnapshot,
    /// Completion time of the last successful present, used to pace 5K redraws without
    /// sleeping the event-loop thread.
    last_presented_at: Option<Instant>,
    /// A deferred 5K redraw waiting for the cadence deadline.
    redraw_deadline: Option<Instant>,
}

impl SessionApp {
    fn new(
        config: WindowConfig,
        store: Arc<Mutex<SurfaceStore>>,
        input: WakingSender<InputEvent>,
        stats: Option<StatsHandle>,
    ) -> Self {
        let viewport = Viewport::letterbox(
            u32::from(config.session_width),
            u32::from(config.session_height),
            config.session_width,
            config.session_height,
        );
        let policy = WindowPolicy::new(Geometry::new(
            u32::from(config.session_width),
            u32::from(config.session_height),
        ));
        let fullscreen = config.fullscreen;
        let windowed_session = (config.session_width, config.session_height);
        let show_stats = config.overlay_on_start;
        SessionApp {
            present_failures: 0,
            config,
            store,
            damage: DamageGate::default(),
            input,
            latest_mouse_move: None,
            window: None,
            presenter: None,
            viewport,
            cursor: None,
            wheel: input::WheelAccumulator::new(),
            keys: input::KeyLedger::new(),
            presented: None,
            failure: None,
            policy,
            started: Instant::now(),
            stats,
            show_stats,
            modifiers: ModifiersState::empty(),
            on_exit: None,
            commands: None,
            fullscreen,
            occluded: false,
            fullscreen_state: Arc::new(AtomicBool::new(fullscreen)),
            windowed_session,
            resize_settle: None,
            last_title_refresh: Instant::now(),
            title_frames: 0,
            title_bytes: 0,
            title_codec_painted: std::collections::BTreeMap::new(),
            title_codec: None,
            cursor_cache: std::collections::HashMap::new(),
            cursor_hidden: false,
            diagnostics: None,
            aux: Vec::new(),
            about_icon: None,
            menu: None,
            last_diag_refresh: Instant::now(),
            transients: None,
            toasts: Vec::new(),
            repaint_awaited: None,
            stall_announced: false,
            warn_line: None,
            presentation: PresentationSnapshot::default(),
            last_presented_at: None,
            redraw_deadline: None,
        }
    }

    /// Poll the transient watcher: absorb fresh toasts and the overlay warn line.
    /// Frames the session has actually painted, or 0 before stats are wired.
    fn painted_frames(&self) -> u64 {
        self.stats.as_ref().map_or(0, |s| s.snapshot().frames)
    }

    /// Drop any standing stall report. Called when a frame lands, or when the window
    /// goes away again — the condition is over either way.
    fn clear_stall(&mut self) {
        if self.stall_announced {
            self.stall_announced = false;
            self.warn_line = None;
        }
    }

    /// Report a reveal whose repaint never arrived.
    ///
    /// Runs on the same 1 Hz tick as the rest of the transient work. A reveal sends
    /// Suppress Output's allow form plus a full-desktop Refresh Rect; if the frame count
    /// has not moved [`REPAINT_GRACE`] later, the server owes us a desktop it never sent
    /// and the window is showing black or a stale frame with nothing to explain it. Say
    /// so once, in the log, on a toast, and on the overlay while it lasts.
    fn check_repaint_stall(&mut self) {
        let Some((asked, frames_then)) = self.repaint_awaited else {
            return;
        };
        if self.painted_frames() != frames_then {
            // The repaint arrived. Nothing to report, and nothing left to watch.
            self.repaint_awaited = None;
            self.clear_stall();
            return;
        }
        if !repaint_stalled(asked.elapsed(), frames_then, self.painted_frames()) {
            return;
        }
        let secs = asked.elapsed().as_secs();
        // The log line fires once. The overlay line is re-asserted each tick so its count
        // stays live, and only into a free slot — an audio warning already standing there
        // is not worth stomping for this.
        if !self.stall_announced {
            self.stall_announced = true;
            eprintln!(
                "display: asked the server to resume and repaint {secs}s ago and no frame \
                 has arrived; the window is showing stale or black pixels (the connection \
                 is otherwise healthy — see MDR-BUG-FLUX-00008)"
            );
        }
        // The card is held up for as long as the fault lasts, by refreshing its birth
        // instant rather than letting TOAST_TTL retire it. A six-second card for a
        // condition that persists for hours told the user nothing if they happened to be
        // looking elsewhere, which is exactly what happened in the field.
        let body = format!("repaint requested {secs}s ago, nothing arrived");
        match self
            .toasts
            .iter_mut()
            .find(|(t, _)| t.title == STALL_TOAST_TITLE)
        {
            Some((toast, born)) => {
                toast.body = body;
                *born = Instant::now();
            }
            None => self.toasts.push((
                Toast {
                    warn: false,
                    title: STALL_TOAST_TITLE.to_owned(),
                    body,
                },
                Instant::now(),
            )),
        }
        if self.warn_line.is_none() {
            self.warn_line = Some(format!("display  no repaint for {secs}s after reveal"));
        }
    }

    fn poll_transients(&mut self) {
        let Some(watch) = self.transients.as_mut() else {
            return;
        };
        let report = watch();
        let now = Instant::now();
        let changed = !report.toasts.is_empty() || report.warn_line != self.warn_line;
        for toast in report.toasts {
            self.toasts.push((toast, now));
        }
        self.warn_line = report.warn_line;
        let had = self.toasts.len();
        self.toasts.retain(|(_, born)| born.elapsed() < TOAST_TTL);
        if (changed || self.toasts.len() != had)
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }

    /// Open the diagnostics window of `kind`, if any diagnostics are wired up.
    fn open_diagnostics(&mut self, event_loop: &ActiveEventLoop, kind: DiagKind) {
        if self.diagnostics.is_none() {
            eprintln!("no diagnostics are wired into this window");
            return;
        }
        self.open_aux(event_loop, AuxKind::Diag(kind));
    }

    /// Open the auxiliary window of `kind`. A second request for one already open is
    /// ignored — nothing focuses a window portably, and one is enough.
    fn open_aux(&mut self, event_loop: &ActiveEventLoop, kind: AuxKind) {
        if self.aux.iter().any(|(k, _)| *k == kind) {
            return;
        }
        match crate::ui::egui_host::AuxWindow::open(event_loop, kind.title(), kind.size()) {
            Ok(win) => {
                self.aux.push((kind, win));
                event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + DIAG_REFRESH));
            }
            // An extra window that cannot open must never take the session down.
            Err(e) => eprintln!("could not open the {} window: {e}", kind.title()),
        }
    }

    /// Drop the aux window at `pos` and settle the loop's control flow.
    ///
    /// Every close path goes through here so the About window's texture cannot
    /// outlive the egui context that owns it.
    fn close_aux(&mut self, event_loop: &ActiveEventLoop, pos: usize) {
        let (kind, _) = self.aux.remove(pos);
        if kind == AuxKind::About {
            self.about_icon = None;
        }
        if self.aux.is_empty() && self.toasts.is_empty() {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    fn handle_menu(&mut self, event_loop: &ActiveEventLoop, id: &str) {
        match id {
            session_menu::DIAG_CACHE => self.open_diagnostics(event_loop, DiagKind::Cache),
            session_menu::DIAG_LATENCY => self.open_diagnostics(event_loop, DiagKind::Latency),
            session_menu::DIAG_CHANNELS => {
                self.open_diagnostics(event_loop, DiagKind::Channels);
            }
            session_menu::HELP_ABOUT => self.open_aux(event_loop, AuxKind::About),
            session_menu::HELP_SHORTCUTS => self.open_aux(event_loop, AuxKind::Shortcuts),
            session_menu::STATS_OVERLAY => {
                self.show_stats = !self.show_stats;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            session_menu::COPY_AVC444 => {
                // Local copy only — the clipboard channel then carries it into the
                // session for pasting into a PowerShell window on the host.
                let copied = arboard::Clipboard::new()
                    .and_then(|mut c| c.set_text(crate::hostscripts::ENABLE_AVC444));
                if let Err(e) = copied {
                    eprintln!("could not copy the AVC444 script: {e}");
                }
            }
            session_menu::COPY_60FPS => {
                let copied = arboard::Clipboard::new()
                    .and_then(|mut c| c.set_text(crate::hostscripts::ENABLE_60FPS));
                if let Err(e) = copied {
                    eprintln!("could not copy the 60 fps script: {e}");
                }
            }
            session_menu::COPY_SSH_SETUP => {
                // Unlike the other two this script is per-machine: it carries our
                // public key. Minting the key here is deliberate — the script is
                // useless without one, and this click is the request for it.
                match crate::sshsetup::ensure_key() {
                    Ok((pubkey, _)) => {
                        let script = crate::hostscripts::setup_ssh(&pubkey);
                        let copied = arboard::Clipboard::new().and_then(|mut c| c.set_text(script));
                        if let Err(e) = copied {
                            eprintln!("could not copy the SSH setup script: {e}");
                        }
                    }
                    Err(e) => eprintln!("could not prepare the SSH key: {e}"),
                }
            }
            session_menu::FULLSCREEN => self.set_fullscreen_mode(!self.fullscreen),
            session_menu::DISCONNECT => event_loop.exit(),
            _ => {}
        }
    }

    /// Mirror a remote pointer change onto the local window.
    ///
    /// Best-effort by design: a shape winit rejects (bad hotspot, wrong buffer length)
    /// falls back to the default arrow rather than touching the session. The pointer is
    /// cosmetic; the desktop is not.
    fn apply_remote_cursor(&mut self, event_loop: &ActiveEventLoop, update: CursorUpdate) {
        let Some(window) = self.window.clone() else {
            return;
        };
        match update {
            CursorUpdate::Default => {
                window.set_cursor(winit::window::CursorIcon::Default);
                self.set_cursor_hidden(&window, false);
            }
            CursorUpdate::Hidden => self.set_cursor_hidden(&window, true),
            CursorUpdate::Bitmap {
                width,
                height,
                hotspot_x,
                hotspot_y,
                rgba,
            } => {
                // A zero-sized shape is the protocol's "invisible pointer".
                if width == 0 || height == 0 {
                    self.set_cursor_hidden(&window, true);
                    return;
                }
                let key = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    (width, height, hotspot_x, hotspot_y).hash(&mut h);
                    rgba.hash(&mut h);
                    h.finish()
                };
                let cursor = match self.cursor_cache.get(&key) {
                    Some(c) => Some(c.clone()),
                    None => {
                        // Hotspot inside the image, or winit refuses the source.
                        let hx = hotspot_x.min(width - 1);
                        let hy = hotspot_y.min(height - 1);
                        CustomCursor::from_rgba(rgba, width, height, hx, hy)
                            .ok()
                            .map(|source| event_loop.create_custom_cursor(source))
                            .inspect(|c| {
                                // Distinct shapes number in the dozens; a runaway server
                                // gets a reset, not unbounded growth.
                                if self.cursor_cache.len() >= 128 {
                                    self.cursor_cache.clear();
                                }
                                self.cursor_cache.insert(key, c.clone());
                            })
                    }
                };
                match cursor {
                    Some(c) => window.set_cursor(c),
                    None => window.set_cursor(winit::window::CursorIcon::Default),
                }
                self.set_cursor_hidden(&window, false);
            }
        }
    }

    fn set_cursor_hidden(&mut self, window: &Window, hidden: bool) {
        if self.cursor_hidden != hidden {
            self.cursor_hidden = hidden;
            window.set_cursor_visible(!hidden);
        }
    }

    /// Ctrl+Alt+S toggles the overlay.
    ///
    /// Chosen because it is not a Windows shortcut worth losing: with the remote desktop
    /// focused, every keystroke belongs to it, so any local hotkey is a key the user can
    /// no longer send. Ctrl+Alt+Del is intercepted by the OS long before us and is not
    /// available to claim.
    fn is_stats_hotkey(&self, event: &winit::event::KeyEvent) -> bool {
        self.modifiers.control_key()
            && self.modifiers.alt_key()
            && matches!(&event.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("s"))
    }

    /// Ctrl+Alt+Enter toggles fullscreen.
    ///
    /// Echoes mstsc's Ctrl+Alt+Break — the classic RDP fullscreen toggle — with Enter
    /// standing in for the Break key Mac keyboards do not have. Ctrl+Alt+Enter is not a
    /// standard Windows shortcut, so claiming it locally costs the remote nothing.
    fn is_fullscreen_hotkey(&self, event: &winit::event::KeyEvent) -> bool {
        self.modifiers.control_key()
            && self.modifiers.alt_key()
            && matches!(&event.logical_key, Key::Named(NamedKey::Enter))
    }

    /// Enter or leave fullscreen, renegotiating the session resolution to match.
    ///
    /// This is the *only* place the session resolution follows the display, and it runs
    /// solely on the user's say-so — the hotkey, the startup restore of a fullscreen
    /// close, or the OS fullscreen button. A display-configuration change never lands
    /// here; those keep going through the geometry policy, which never touches the
    /// session (see the module note and CLAUDE.md).
    fn set_fullscreen_mode(&mut self, on: bool) {
        let Some(window) = self.window.clone() else {
            return;
        };
        // The transition sends its own resolution request; a follow armed by a drag
        // moments ago must not fire into the middle of it.
        self.resize_settle = None;
        self.fullscreen = on;
        self.fullscreen_state.store(on, Ordering::Relaxed);
        if on {
            window.set_fullscreen(Some(Fullscreen::Borderless(None)));
            window.set_decorations(false);
            self.request_native_resolution(&window);
        } else {
            window.set_fullscreen(None);
            window.set_decorations(true);
            self.request_windowed_resolution();
        }
        // The transition lands as ordinary `Resized` events; tell the policy a display
        // upheaval is in progress so it does not read them as the user dragging.
        let now = self.now_ms();
        self.policy.note_display_event(now);
    }

    /// Ask the session for this monitor's native pixel resolution, advertising the
    /// monitor's scale factor so the remote can render its UI at a matching size.
    fn request_native_resolution(&mut self, window: &Window) {
        if !self.config.dynamic_resolution {
            return; // Settings: the session resolution never follows the display.
        }
        let Some(monitor) = window.current_monitor() else {
            return;
        };
        let size = monitor.size();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let scale = (monitor.scale_factor() * 100.0).round() as u32;
        // The out-of-range-scale gating (MS-RDPEDISP allows 100–500) and the encoder
        // ceiling both live in the shared decision, so the fullscreen toggle and the
        // fullscreen-at-start connect always agree on what to ask for.
        let (width, height, scale_percent) = crate::session::fullscreen_request(
            size.width,
            size.height,
            scale,
            self.config.integer_fullscreen_fit,
        );
        self.send_command(SessionCommand::Resize {
            width,
            height,
            scale_percent,
        });
    }

    /// Ask the session to go back to the resolution the windowed session runs at.
    fn request_windowed_resolution(&mut self) {
        if !self.config.dynamic_resolution {
            return; // Symmetric with request_native_resolution.
        }
        self.send_command(SessionCommand::Resize {
            width: u32::from(self.windowed_session.0),
            height: u32::from(self.windowed_session.1),
            // Undo whatever scale fullscreen advertised, or the remote stays zoomed.
            scale_percent: Some(100),
        });
    }

    fn send_command(&mut self, command: SessionCommand) {
        if let Some(commands) = &self.commands {
            // A dead session is closing the window anyway; nothing useful to do here.
            let _ = commands.send(command);
        }
    }

    /// Refresh the title-bar diagnostics, at most once per [`TITLE_REFRESH`].
    fn maybe_refresh_title(&mut self) {
        let elapsed = self.last_title_refresh.elapsed();
        if elapsed < TITLE_REFRESH {
            return;
        }
        let (Some(window), Some(stats)) = (&self.window, &self.stats) else {
            return;
        };
        let snapshot = stats.snapshot();
        let secs = elapsed.as_secs_f64();
        let fps = snapshot.frames.saturating_sub(self.title_frames) as f64 / secs;
        let mbps = snapshot.bytes_in.saturating_sub(self.title_bytes) as f64 * 8.0 / secs / 1e6;
        self.last_title_refresh = Instant::now();
        self.title_frames = snapshot.frames;
        self.title_bytes = snapshot.bytes_in;
        if let Some(note) = codec_note(&snapshot.codec_painted, &self.title_codec_painted) {
            self.title_codec = Some(note);
        }
        self.title_codec_painted = snapshot.codec_painted.clone();
        window.set_title(&title_line(
            &self.config.title,
            self.viewport.session_width,
            self.viewport.session_height,
            &snapshot,
            fps,
            mbps,
            self.title_codec.as_deref(),
        ));
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Consult the geometry policy about a size: ask it back if the system chose it,
    /// arm the resolution-follow timer if the user did.
    ///
    /// Requesting is all we can do — every platform is free to ignore it, which is why the
    /// policy bounds its attempts rather than looping until the sizes agree.
    fn hold_geometry(&mut self, event_loop: &ActiveEventLoop, actual: Geometry) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let monitor = window
            .current_monitor()
            .map(|m| Geometry::new(m.size().width, m.size().height));
        let now = self.now_ms();
        match self.policy.on_resize(actual, now, monitor) {
            ResizeVerdict::Restore(target) => {
                // A restore in flight must never fire a follow: the size on screen is
                // the very one we are arguing with.
                self.resize_settle = None;
                tracing::debug!(
                    actual_w = actual.width,
                    actual_h = actual.height,
                    target_w = target.width,
                    target_h = target.height,
                    "restoring window geometry after a display change"
                );
                let _ = window.request_inner_size(PhysicalSize::new(target.width, target.height));
            }
            ResizeVerdict::UserResize => {
                if self.config.dynamic_resolution && self.config.follow_window_resize {
                    let deadline = Instant::now() + RESIZE_FOLLOW_SETTLE;
                    self.resize_settle = Some(deadline);
                    // The loop may be in `Wait` with nothing else due; without a timed
                    // wake an idle session would never see the deadline pass.
                    event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                }
            }
            ResizeVerdict::Ignore => {
                // Whatever this was — growth mid-upheaval, a restore landing, a spent
                // budget — it was not the user, so nothing may reach the session.
                self.resize_settle = None;
            }
        }
    }

    /// Fire the debounced resolution follow, if its deadline has passed.
    ///
    /// Called from `new_events`, so it runs whenever the loop wakes — including the
    /// `WaitUntil` armed for exactly this deadline.
    fn service_settled_resize(&mut self) {
        let due = self
            .resize_settle
            .is_some_and(|deadline| Instant::now() >= deadline);
        if !due {
            return;
        }
        self.resize_settle = None;
        // Belt and braces: transitions cancel the timer, but a fullscreen window's size
        // is the monitor's, never a drag's.
        if self.fullscreen {
            return;
        }
        let Some(window) = &self.window else {
            return;
        };
        let size = window.inner_size();
        let Some((width, height)) = follow_request(
            size.width,
            size.height,
            self.viewport.session_width,
            self.viewport.session_height,
        ) else {
            return;
        };
        // The next fullscreen round trip should come back to what the user last chose,
        // not to the connect-time size.
        self.windowed_session = (
            u16::try_from(width).unwrap_or(u16::MAX),
            u16::try_from(height).unwrap_or(u16::MAX),
        );
        self.send_command(SessionCommand::Resize {
            width,
            height,
            // Keep the scale the session already has: a mid-session scale change leaves
            // non-DPI-aware remote apps DWM-stretched and blurry until relaunch.
            scale_percent: None,
        });
    }

    /// A closed receiver means the session is gone, so there is nothing left to show.
    fn send(&mut self, event_loop: &ActiveEventLoop, event: InputEvent) {
        if !queue_input_event(&self.input, self.latest_mouse_move.as_deref(), event) {
            event_loop.exit();
        }
    }

    /// A frame we could not present. Survivable until it stops being occasional.
    ///
    /// The session is the expensive thing here: reconnecting costs the user a logon and,
    /// on a workstation host, can wedge the server. A dropped frame costs one repaint.
    fn note_present_failure(&mut self, event_loop: &ActiveEventLoop, detail: String) {
        self.present_failures = self.present_failures.saturating_add(1);
        if self.present_failures >= MAX_CONSECUTIVE_PRESENT_FAILURES {
            self.fail(event_loop, WindowError::Present(detail));
        }
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: WindowError) {
        if self.failure.is_none() {
            self.failure = Some(error);
        }
        event_loop.exit();
    }

    /// Read the store's generation, surviving a poisoned lock.
    ///
    /// A panic in a decoder thread must not freeze the display: the pixels are still
    /// whatever the last complete write left, which is exactly what we want to show.
    fn generation(&self) -> u64 {
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        store.generation()
    }

    fn cadence_dimensions(&self) -> (u16, u16) {
        if self.presentation.width != 0 && self.presentation.height != 0 {
            (self.presentation.width, self.presentation.height)
        } else {
            (self.config.session_width, self.config.session_height)
        }
    }

    /// Ask the window to present the newest generation, subject to the large-canvas
    /// cadence. The event loop remains free to handle input while a deadline is pending.
    fn request_damage_redraw(&mut self, event_loop: &ActiveEventLoop) {
        if self.occluded || self.presented == Some(self.generation()) {
            return;
        }
        let now = Instant::now();
        let (width, height) = self.cadence_dimensions();
        if let Some(deadline) = presentation_deadline(self.last_presented_at, now, width, height) {
            self.redraw_deadline = Some(
                self.redraw_deadline
                    .map_or(deadline, |current| current.max(deadline)),
            );
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            return;
        }
        self.redraw_deadline = None;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Fire one deferred redraw once its cadence deadline arrives.
    fn service_deferred_redraw(&mut self) {
        let Some(deadline) = self.redraw_deadline else {
            return;
        };
        if Instant::now() < deadline {
            return;
        }
        self.redraw_deadline = None;
        if !self.occluded
            && self.presented != Some(self.generation())
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if self.presenter.is_none() {
            return;
        }
        let size = window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return; // Minimised. Nothing to draw into.
        };

        // Gate large-canvas work while holding the same lock that makes the generation,
        // dimensions, and eventual snapshot coherent. A platform expose inside the
        // cadence window must not copy tens of MiB only to discard them afterward.
        let copy_result = {
            let store = self
                .store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            copy_presentation_when_due(
                &store,
                &mut self.presentation,
                self.presented,
                self.last_presented_at,
                Instant::now(),
                (self.config.session_width, self.config.session_height),
            )
        };
        let blank = match copy_result {
            Ok(PresentationCopy::Copied) => false,
            Ok(PresentationCopy::Empty) => true,
            // An active or aborted logical frame leaves the existing snapshot intact.
            // If this is the first ever expose there is no snapshot to retain, so the
            // normal empty-window path still paints black until the first commit.
            Ok(PresentationCopy::Retained) => {
                self.presentation.width == 0
                    || self.presentation.height == 0
                    || self.presentation.pixels.is_empty()
            }
            Err(deadline) => {
                self.redraw_deadline = Some(deadline);
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                return;
            }
        };
        let generation = self.presentation.generation;
        self.redraw_deadline = None;

        let Some(presenter) = self.presenter.as_mut() else {
            return;
        };
        if let Err(e) = presenter.resize(width, height) {
            // Transient: skip this frame. Only a sustained run of failures is fatal —
            // ending an RDP session because one present hiccuped is a bad trade.
            self.note_present_failure(event_loop, e);
            return;
        }
        let mut buffer = match presenter.frame() {
            Ok(b) => b,
            Err(e) => {
                self.note_present_failure(event_loop, e);
                return;
            }
        };

        if blank {
            // Nothing mapped to output yet: black, not stale garbage.
            buffer.fill(0);
        } else {
            self.viewport = Viewport::letterbox(
                size.width,
                size.height,
                self.presentation.width,
                self.presentation.height,
            );
            present_into(
                &mut buffer,
                size.width,
                size.height,
                &self.viewport,
                &self.presentation.pixels,
            );
        }

        // Explain the black rectangle rather than leaving the user to guess, in the two
        // cases where there is provably nothing to obscure (MDR-BUG-FLUX-00008):
        //
        // 1. No surface is mapped to output, so the window is blank because we have
        //    nothing at all — not because the desktop is dark.
        // 2. A repaint we asked for never arrived AND the frame is black. Either half
        //    alone must stay silent: a black desktop is legitimate (lock screen,
        //    full-screen terminal, screensaver), and a stall over a *stale* desktop is
        //    still showing the user real content that must not be painted over. Together
        //    they are not legitimate, and this was the case the field failure actually
        //    hit — the first version of this notice stayed silent through it.
        let stalled_and_dark =
            self.stall_announced && frame_looks_black(&buffer, BLACK_SAMPLE_STRIDE);
        if blank || stalled_and_dark {
            // Tartan, then the explanation over it. Overwriting the frame is safe on
            // exactly the same evidence that justifies writing text on it: either no
            // surface exists at all, or a repaint we asked for never arrived and the frame
            // samples as black. Anything else keeps its pixels untouched.
            fill_tartan(&mut buffer, size.width, size.height);
            let waiting = self.repaint_awaited.map(|(asked, _)| asked.elapsed());
            draw_blank_notice(&mut buffer, size.width, size.height, waiting);
        }

        if self.show_stats
            && let Some(stats) = &self.stats
        {
            let mut lines = stats.snapshot().overlay_lines();
            if let Some(warn) = &self.warn_line {
                // Transient conditions ride the overlay while they last (§8).
                lines.push(format!("! {warn}"));
            }
            draw_overlay(&mut buffer, size.width, size.height, &lines);
        }
        if !self.toasts.is_empty() {
            self.toasts.retain(|(_, born)| born.elapsed() < TOAST_TTL);
            let toasts: Vec<Toast> = self.toasts.iter().map(|(t, _)| t.clone()).collect();
            draw_toasts(&mut buffer, size.width, size.height, &toasts);
        }

        window.pre_present_notify();
        match buffer.present() {
            Ok(crate::present::PresentStatus::Presented) => {}
            Ok(crate::present::PresentStatus::Busy) => {
                let deadline = Instant::now() + PRESENT_BACKPRESSURE_RETRY;
                self.redraw_deadline = Some(deadline);
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                return;
            }
            Err(e) => {
                self.note_present_failure(event_loop, e);
                return;
            }
        }
        // Streak counts CONSECUTIVE failures: without this reset, 30 unrelated hiccups
        // across a long session would close a perfectly healthy window.
        self.present_failures = 0;
        self.presented = Some(generation);
        self.last_presented_at = Some(Instant::now());
        // Close the paint→present handoff measurement for this generation.
        if let Some(stats) = &self.stats {
            stats.update(|s| s.mark_presented(generation));
        }
    }
}

/// Put one window event on its transport queue.
///
/// Physical motion is a latest-value slot. Every other event remains reliable, and a
/// button or wheel event clears an older physical position because it carries its own
/// absolute coordinates.
fn queue_input_event(
    input: &WakingSender<InputEvent>,
    latest_mouse_move: Option<&input::LatestMouseMove>,
    event: InputEvent,
) -> bool {
    if let Some(moves) = latest_mouse_move {
        match event {
            InputEvent::MouseMove { x, y } => {
                if !moves.replace(x, y) {
                    return false;
                }
                input.wake();
                return true;
            }
            InputEvent::MouseButton { .. } | InputEvent::Scroll { .. } => {
                // These reliable events carry their own absolute position. Discard an
                // older pending move so it cannot land after them.
                moves.clear();
            }
            InputEvent::Key { .. } => {}
        }
    }
    input.send(event).is_ok()
}

impl ApplicationHandler<SessionEvent> for SessionApp {
    /// The loop is going away. On macOS this is the last code that runs before AppKit
    /// calls `exit(0)` for a Cmd+Q, so the disconnect has to happen here rather than
    /// after `run_app` returns — for a Cmd+Q, it never returns.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(f) = self.on_exit.as_mut() {
            f();
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // Resume can fire more than once; the window survives it.
        }

        let attributes = window_attributes(&self.config);

        let window = match event_loop.create_window(attributes) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.fail(event_loop, WindowError::Os(e.to_string()));
                return;
            }
        };

        let presenter = match crate::present::Presenter::new(window.clone()) {
            Ok(p) => p,
            Err(e) => {
                self.fail(event_loop, WindowError::Os(e));
                return;
            }
        };
        // One line of provenance for every measurement taken against this session:
        // a CPU or latency figure without the present backend named is ambiguous.
        eprintln!("present: {} backend", presenter.backend());

        // After the window, so AppKit is up and this is its main thread: an unbundled
        // session process otherwise sits in the Dock as a generic "exec" block.
        if let Some(label) = &self.config.dock_label {
            crate::dock::set_label(label);
        }

        window.request_redraw();
        self.window = Some(window.clone());
        self.presenter = Some(presenter);
        self.menu = Some(session_menu::install(&window));

        // Scripted runs cannot click a native menu, so `MDRDP_OPEN_DIAG=cache,latency,
        // channels,about,shortcuts` opens the named windows at startup — the automated
        // verification path for windows that are otherwise menu-only.
        if let Ok(names) = std::env::var("MDRDP_OPEN_DIAG") {
            for name in names.split(',') {
                match name.trim() {
                    "cache" => self.open_diagnostics(event_loop, DiagKind::Cache),
                    "latency" => self.open_diagnostics(event_loop, DiagKind::Latency),
                    "channels" => self.open_diagnostics(event_loop, DiagKind::Channels),
                    "about" => self.open_aux(event_loop, AuxKind::About),
                    "shortcuts" => self.open_aux(event_loop, AuxKind::Shortcuts),
                    "" => {}
                    other => eprintln!("MDRDP_OPEN_DIAG: unknown window {other:?}"),
                }
            }
        }

        // A window that *opens* fullscreen — a favourite, or a remembered fullscreen
        // close — negotiates its monitor's native resolution the same way the hotkey
        // does. Best-effort: on a server without Display Control it letterboxes.
        if self.fullscreen && self.config.negotiate_native_on_start {
            self.request_native_resolution(&window);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: SessionEvent) {
        match event {
            SessionEvent::Damaged => {
                // Clear before reading the generation. A producer that races with this
                // handler then claims and sends the next notification instead of being
                // lost behind the one currently being serviced.
                self.damage.clear();
                // The whole point of the generation counter: a nudge that turns out to
                // change nothing costs one atomic-ish read and no frame. An occluded
                // window presents to nobody, so damage accumulates silently until the
                // reveal's own request_redraw shows the latest frame (Occluded arm).
                self.request_damage_redraw(event_loop);
                // Damage is also the clock for the title diagnostics: numbers only move
                // when frames do.
                self.maybe_refresh_title();
                self.poll_transients();
                // Frames are moving again: this is what retires an outstanding repaint
                // watch, and clears a stall report if one was standing.
                self.check_repaint_stall();
            }
            SessionEvent::Cursor(update) => self.apply_remote_cursor(event_loop, update),
            SessionEvent::Close => event_loop.exit(),
            SessionEvent::Menu(id) => self.handle_menu(event_loop, &id),
        }
    }

    /// Drive the timed work — the 1 Hz refresh for open diagnostics windows and live
    /// toasts, and the settled-resize follow — and arm the wake for whichever is due
    /// next. With neither pending the loop returns to plain `Wait`.
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        self.service_settled_resize();
        self.service_deferred_redraw();

        // An outstanding repaint keeps the tick alive too: a stalled session produces no
        // damage by definition, so without this the one condition that most needs
        // reporting is the one nothing would ever poll for.
        let diag_active =
            !self.aux.is_empty() || !self.toasts.is_empty() || self.repaint_awaited.is_some();
        if diag_active
            && (matches!(cause, winit::event::StartCause::ResumeTimeReached { .. })
                || self.last_diag_refresh.elapsed() >= DIAG_REFRESH)
        {
            self.last_diag_refresh = Instant::now();
            for (_, win) in &self.aux {
                win.request_redraw();
            }
            self.poll_transients();
            self.check_repaint_stall();
            if !self.toasts.is_empty()
                && let Some(window) = &self.window
            {
                // Expiry is time-driven; without this nudge a toast would linger
                // until the next damage event.
                window.request_redraw();
            }
        }

        let diag_next = diag_active.then(|| self.last_diag_refresh + DIAG_REFRESH);
        let next = [diag_next, self.resize_settle, self.redraw_deadline]
            .into_iter()
            .flatten()
            .min();
        match next {
            Some(deadline) => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
            // A stale `WaitUntil` left set would spin the loop the moment its instant
            // passed, so the quiet state is restored explicitly.
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // Auxiliary windows first: their events must not fall through to the session
        // handling below (a stray CloseRequested would end the session).
        if let Some(pos) = self.aux.iter().position(|(_, w)| w.window_id() == id) {
            match event {
                WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                    self.close_aux(event_loop, pos);
                }
                WindowEvent::RedrawRequested => {
                    let (kind, win) = &mut self.aux[pos];
                    let mut close = false;
                    // Cmd+W (Ctrl+W elsewhere) and Escape close every aux window: over
                    // a fullscreen session the window may have no OS titlebar, so the
                    // button must not be the only way out.
                    let key_closed = |ui: &egui::Ui| {
                        ui.input(|i| {
                            i.key_pressed(egui::Key::Escape)
                                || (i.modifiers.command && i.key_pressed(egui::Key::W))
                        })
                    };
                    match *kind {
                        AuxKind::Diag(diag) => match self.diagnostics.as_mut() {
                            Some(uis) => {
                                let body = uis.ui_for(diag);
                                win.redraw(|ui| {
                                    close |= body(ui);
                                    close |= key_closed(ui);
                                });
                            }
                            None => win.redraw(|_| {}),
                        },
                        AuxKind::About => {
                            let icon = &mut self.about_icon;
                            win.redraw(|ui| {
                                close |= crate::ui::help::about_window(ui, icon);
                                close |= key_closed(ui);
                            });
                        }
                        AuxKind::Shortcuts => {
                            let groups = crate::ui::help::session_shortcuts();
                            win.redraw(|ui| {
                                close |= crate::ui::help::shortcuts_window(ui, &groups);
                                close |= key_closed(ui);
                            });
                        }
                    }
                    if close {
                        self.close_aux(event_loop, pos);
                    }
                }
                other => {
                    let (_, win) = &mut self.aux[pos];
                    win.on_window_event(&other);
                }
            }
            return;
        }
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => event_loop.exit(),

            WindowEvent::RedrawRequested => self.redraw(event_loop),

            // A resize changes the viewport immediately; whether it may also change the
            // session resolution is the geometry policy's call, made in `hold_geometry`
            // below — only a resize judged to be the user's own act arms the debounced
            // follow. See the module note and CLAUDE.md.
            WindowEvent::Resized(size) => {
                self.viewport = Viewport::letterbox(
                    size.width,
                    size.height,
                    self.viewport.session_width,
                    self.viewport.session_height,
                );
                // The OS can toggle fullscreen without us — the macOS green button, a
                // Mission Control gesture. Adopt the change as if the hotkey did it,
                // resolution renegotiation included.
                if let Some(window) = self.window.clone() {
                    let os_fullscreen = window.fullscreen().is_some();
                    if os_fullscreen != self.fullscreen {
                        // A transition's sizes are the transition's, not a drag's.
                        self.resize_settle = None;
                        self.fullscreen = os_fullscreen;
                        self.fullscreen_state
                            .store(os_fullscreen, Ordering::Relaxed);
                        if os_fullscreen {
                            self.request_native_resolution(&window);
                        } else {
                            self.request_windowed_resolution();
                        }
                        let now = self.now_ms();
                        self.policy.note_display_event(now);
                    }
                }
                // Fullscreen geometry belongs to the OS; the policy only defends the
                // size of a *windowed* window. Feeding it fullscreen sizes would teach
                // it that the monitor size is what the user wants.
                if !self.fullscreen {
                    self.hold_geometry(event_loop, Geometry::new(size.width, size.height));
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            // A screen locking or a monitor powering down shows up here. Both platforms
            // rearrange windows around these moments, so the policy uses them to read the
            // resizes that follow as the system's doing rather than the user's.
            WindowEvent::Occluded(occluded) => {
                // A drag cannot span an occlusion; whatever the timer was watching for
                // is over, and the sizes that follow a reveal are the system's.
                self.resize_settle = None;
                let now = self.now_ms();
                self.policy.note_occluded(occluded, now);
                if occluded != self.occluded {
                    self.occluded = occluded;
                    // Nobody can see a fully occluded window, so the server should stop
                    // encoding frames for it (MDR-BUG-FLUX-00005: an idle hidden session
                    // burned most of a core presenting invisible frames). Resuming asks
                    // for a full repaint, so nothing stale survives the reveal.
                    self.send_command(SessionCommand::SetVisibility { visible: !occluded });
                    // The reveal owes us a repaint. Start the clock on it, so silence
                    // where a whole desktop was asked for gets reported rather than
                    // shown as an unexplained black window (MDR-BUG-FLUX-00008).
                    self.repaint_awaited = if occluded {
                        None
                    } else {
                        Some((Instant::now(), self.painted_frames()))
                    };
                    self.clear_stall();
                }
                // Becoming visible is the first moment anything can actually be fixed:
                // the size may have been changed while the screen was off, and no further
                // `Resized` is guaranteed to arrive to prompt us.
                if !occluded && let Some(window) = self.window.clone() {
                    if !self.fullscreen {
                        let size = window.inner_size();
                        self.hold_geometry(event_loop, Geometry::new(size.width, size.height));
                    }
                    window.request_redraw();
                }
            }

            // A monitor swap or a move between displays of different density. The window
            // keeps its logical size and changes physical size, which the policy must not
            // mistake for the user resizing.
            WindowEvent::ScaleFactorChanged { .. } => {
                // The physical size is about to change without the user touching
                // anything; an armed follow must not adopt it.
                self.resize_settle = None;
                let now = self.now_ms();
                self.policy.note_display_event(now);
            }

            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),

            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                // Every other key belongs to the remote desktop, so the few local
                // hotkeys are claimed here and deliberately not forwarded — otherwise
                // they would also type into whatever has focus on the far end.
                if !is_synthetic && self.is_stats_hotkey(&event) {
                    if event.state == winit::event::ElementState::Pressed {
                        self.show_stats = !self.show_stats;
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                    return;
                }
                if !is_synthetic && self.is_fullscreen_hotkey(&event) {
                    if event.state == winit::event::ElementState::Pressed && !event.repeat {
                        self.set_fullscreen_mode(!self.fullscreen);
                    }
                    return;
                }
                // The ledger drops the synthetic presses the platform replays across a
                // focus change (forwarding them double-presses keys) but keeps synthetic
                // releases of keys we hold, and never sends a release the server has no
                // press for.
                if let Some(InputEvent::Key { scancode, down }) = input::from_key_event(&event)
                    && self.keys.on_key(scancode, down, is_synthetic)
                {
                    self.send(event_loop, InputEvent::Key { scancode, down });
                }
            }

            // Focus leaving with keys held is how modifiers get stuck: the OS chord that
            // stole focus (Cmd+Tab, Ctrl+Arrow switching Spaces) delivers the key-up to
            // whatever gains focus, never to us, and the server holds the key until the
            // same physical key is pressed again in-session. Release everything we
            // forwarded down, exactly as mstsc does on deactivate.
            WindowEvent::Focused(focused) => {
                if !focused {
                    for release in self.keys.release_all() {
                        self.send(event_loop, release);
                    }
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                if let Some(moved) =
                    input::from_cursor_moved(position.x, position.y, &self.viewport)
                {
                    if let InputEvent::MouseMove { x, y } = moved {
                        self.cursor = Some((x, y));
                    }
                    self.send(event_loop, moved);
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                // A click before the pointer has ever been seen has no coordinates to
                // carry, and RDP has no way to express one.
                if let Some(at) = self.cursor
                    && let Some(translated) = input::from_mouse_button(button, state, at)
                {
                    self.send(event_loop, translated);
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(at) = self.cursor {
                    for translated in self.wheel.translate(delta, at) {
                        self.send(event_loop, translated);
                    }
                }
            }

            _ => {}
        }
    }
}

mod session_menu {
    //! The session window's native menu bar: Session · View · Diagnostics · Help.
    //!
    //! macOS puts these in the system menu bar; Windows in the window's own bar.
    //! Ids are plain strings matched in `SessionApp::handle_menu`.

    use muda::{Menu, MenuItem, PredefinedMenuItem, Submenu};

    pub const DISCONNECT: &str = "session.disconnect";
    pub const COPY_AVC444: &str = "session.copy_avc444_script";
    pub const COPY_60FPS: &str = "session.copy_60fps_script";
    pub const COPY_SSH_SETUP: &str = "session.copy_ssh_setup_script";
    pub const FULLSCREEN: &str = "view.fullscreen";
    pub const DIAG_CACHE: &str = "diag.cache";
    pub const DIAG_LATENCY: &str = "diag.latency";
    pub const DIAG_CHANNELS: &str = "diag.channels";
    pub const STATS_OVERLAY: &str = "diag.overlay";
    pub const HELP_SHORTCUTS: &str = "help.shortcuts";
    pub const HELP_ABOUT: &str = "help.about";

    /// Holds the muda objects alive; dropping this removes the native menu.
    pub struct SessionMenuBar {
        _menu: Menu,
    }

    pub fn install(window: &winit::window::Window) -> SessionMenuBar {
        let menu = Menu::new();

        #[cfg(target_os = "macos")]
        {
            // About lives in the app menu on macOS, the way the platform expects —
            // off macOS it goes under Help with the shortcuts.
            let app = Submenu::new("mdrdp", true);
            let _ = app.append_items(&[
                &MenuItem::with_id(HELP_ABOUT, "About mdrdp", true, None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::quit(None),
            ]);
            let _ = menu.append(&app);
        }

        let session = Submenu::new("Session", true);
        let _ = session.append_items(&[
            &MenuItem::with_id(COPY_AVC444, "Copy AVC444 enable script", true, None),
            &MenuItem::with_id(COPY_60FPS, "Copy 60 fps enable script", true, None),
            &MenuItem::with_id(COPY_SSH_SETUP, "Copy SSH setup script", true, None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(DISCONNECT, "Disconnect", true, None),
        ]);
        let _ = menu.append(&session);

        let view = Submenu::new("View", true);
        let _ = view.append_items(&[&MenuItem::with_id(
            FULLSCREEN,
            "Toggle fullscreen",
            true,
            None,
        )]);
        let _ = menu.append(&view);

        let diagnostics = Submenu::new("Diagnostics", true);
        let _ = diagnostics.append_items(&[
            &MenuItem::with_id(DIAG_CACHE, "Bitmap cache…", true, None),
            &MenuItem::with_id(DIAG_LATENCY, "Latency and drift…", true, None),
            &MenuItem::with_id(DIAG_CHANNELS, "Channels and codecs…", true, None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(STATS_OVERLAY, "Stats overlay", true, None),
        ]);
        let _ = menu.append(&diagnostics);

        let help = Submenu::new("Help", true);
        let _ = help.append(&MenuItem::with_id(
            HELP_SHORTCUTS,
            "Keyboard shortcuts…",
            true,
            None,
        ));
        #[cfg(not(target_os = "macos"))]
        {
            let _ = help.append_items(&[
                &PredefinedMenuItem::separator(),
                &MenuItem::with_id(HELP_ABOUT, "About mdrdp", true, None),
            ]);
        }
        let _ = menu.append(&help);

        #[cfg(target_os = "macos")]
        {
            let _ = window;
            menu.init_for_nsapp();
        }
        #[cfg(target_os = "windows")]
        {
            use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
            if let Ok(handle) = window.window_handle()
                && let RawWindowHandle::Win32(h) = handle.as_raw()
            {
                // SAFETY: the HWND belongs to the live window on this thread.
                unsafe {
                    let _ = menu.init_for_hwnd(h.hwnd.get());
                }
            }
        }

        SessionMenuBar { _menu: menu }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{MouseButton, Scancode, ScrollAxis};
    use crate::surface::Rect;
    use std::sync::mpsc;

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];

    /// 2x2 RGBA: red, green / blue, white. Four distinct colours in four distinct
    /// positions, so a transposed or mirrored scaler cannot pass.
    fn quad() -> Vec<u8> {
        let mut v = Vec::new();
        for c in [RED, GREEN, BLUE, WHITE] {
            v.extend_from_slice(&c);
        }
        v
    }

    fn rgb(colour: [u8; 4]) -> u32 {
        (u32::from(colour[0]) << 16) | (u32::from(colour[1]) << 8) | u32::from(colour[2])
    }

    #[test]
    fn a_settled_size_matching_the_session_asks_for_nothing() {
        // Re-requesting the current resolution would cost a server Deactivate All
        // round for zero change — the drag ended where the session already is.
        assert_eq!(follow_request(1920, 1080, 1920, 1080), None);
        // Distinct axis values, so a swapped width/height comparison cannot pass.
        assert_eq!(follow_request(1080, 1920, 1920, 1080), Some((1080, 1920)));
    }

    #[test]
    fn a_settled_size_differing_from_the_session_is_requested_verbatim() {
        // Bounds and encoder ceilings belong to the session pump, not here.
        assert_eq!(follow_request(1800, 1124, 1920, 1080), Some((1800, 1124)));
    }

    #[test]
    fn a_degenerate_size_is_never_requested() {
        assert_eq!(follow_request(0, 1080, 1920, 1080), None);
        assert_eq!(follow_request(1920, 0, 1920, 1080), None);
    }

    #[test]
    fn damage_gate_allows_one_event_until_the_handler_clears_it() {
        let gate = DamageGate::default();
        assert!(gate.claim(), "the first damage must enqueue an event");
        assert!(
            !gate.claim(),
            "additional damage must coalesce while pending"
        );
        gate.clear();
        assert!(
            gate.claim(),
            "damage after handler entry must enqueue a new event"
        );
    }

    #[test]
    fn damage_gate_send_failure_rolls_back_the_pending_claim() {
        let gate = DamageGate::default();
        assert!(gate.claim());
        gate.send_failed();
        assert!(
            gate.claim(),
            "a failed proxy send must not strand the pending bit"
        );
    }

    #[test]
    fn reliable_buttons_and_scroll_clear_pending_physical_moves() {
        let (tx, rx) = mpsc::channel();
        let input = WakingSender::silent(tx);
        let latest = crate::input::LatestMouseMove::default();
        let button = InputEvent::MouseButton {
            button: MouseButton::Left,
            down: true,
            x: 40,
            y: 50,
        };
        assert!(latest.replace(1, 2));
        assert!(queue_input_event(&input, Some(&latest), button));
        assert_eq!(latest.take(), None);
        assert_eq!(rx.try_recv(), Ok(button));

        let scroll = InputEvent::Scroll {
            axis: ScrollAxis::Vertical,
            units: 120,
            x: 60,
            y: 70,
        };
        assert!(latest.replace(3, 4));
        assert!(queue_input_event(&input, Some(&latest), scroll));
        assert_eq!(latest.take(), None);
        assert_eq!(rx.try_recv(), Ok(scroll));

        // Keys remain reliable and do not clear the pending physical position; the
        // session drain will place the key before that final position.
        let key = InputEvent::Key {
            scancode: Scancode::plain(0x1e),
            down: true,
        };
        assert!(latest.replace(5, 6));
        assert!(queue_input_event(&input, Some(&latest), key));
        assert_eq!(rx.try_recv(), Ok(key));
        assert_eq!(latest.take(), Some(InputEvent::MouseMove { x: 5, y: 6 }));
    }

    #[test]
    fn only_large_surfaces_are_cadence_limited_and_the_first_paint_is_immediate() {
        let now = Instant::now();
        assert_eq!(presentation_deadline(None, now, 5120, 2880), None);
        assert_eq!(presentation_deadline(Some(now), now, 1920, 1080), None);
        assert_eq!(
            presentation_deadline(Some(now), now + Duration::from_millis(1), 5120, 2880),
            Some(now + DAMAGE_PRESENT_INTERVAL)
        );
        assert_eq!(
            presentation_deadline(Some(now), now + DAMAGE_PRESENT_INTERVAL, 5120, 2880),
            None
        );
    }

    #[test]
    fn an_early_large_redraw_does_not_copy_the_presentation_snapshot() {
        let mut store = SurfaceStore::new();
        store.create(1, 2561, 1);
        store
            .solid_fill(1, &[Rect::new(0, 0, 2561, 1)], RED)
            .expect("paint surface");
        store.map_to_output(1);

        let mut snapshot = PresentationSnapshot {
            width: 7,
            height: 9,
            pixels: vec![0xA5; 3],
            generation: 11,
        };
        let now = Instant::now();
        let result = copy_presentation_when_due(
            &store,
            &mut snapshot,
            Some(store.generation().wrapping_sub(1)),
            Some(now),
            now + Duration::from_millis(1),
            (1920, 1080),
        );

        assert_eq!(result, Err(now + DAMAGE_PRESENT_INTERVAL));
        assert_eq!(
            (snapshot.width, snapshot.height, snapshot.generation),
            (7, 9, 11)
        );
        assert_eq!(snapshot.pixels, vec![0xA5; 3]);
    }

    #[test]
    fn an_expose_during_a_frame_retains_the_last_snapshot() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 1);
        store
            .solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED)
            .expect("paint surface");
        store.map_to_output(1);

        let mut snapshot = PresentationSnapshot::default();
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        let generation = store.generation();

        store.begin_frame(17);
        store
            .solid_fill(1, &[Rect::new(0, 0, 1, 1)], BLUE)
            .expect("paint frame payload");
        assert_eq!(store.generation(), generation);

        let result = copy_presentation_when_due(
            &store,
            &mut snapshot,
            Some(generation),
            None,
            Instant::now(),
            (2, 1),
        );
        assert_eq!(result, Ok(PresentationCopy::Retained));
        assert_eq!(snapshot.pixels, [RED, RED].concat());
        assert_eq!(snapshot.generation, generation);
    }

    #[test]
    fn an_explicit_size_pins_the_resolution_against_drags() {
        let config = WindowConfig::new("pinned", 1280, 800).keeping_stated_resolution();
        assert!(!config.follow_window_resize);
        assert!(!config.negotiate_native_on_start);
        // The default, with no --size, follows the window.
        assert!(WindowConfig::new("free", 1280, 800).follow_window_resize);
    }

    #[test]
    fn fullscreen_config_builds_a_borderless_fullscreen_window() {
        let config = WindowConfig::new("fullscreen", 1600, 900).with_fullscreen(true);
        let attributes = window_attributes(&config);

        assert_eq!(
            attributes.fullscreen,
            Some(Fullscreen::Borderless(None)),
            "fullscreen must be requested when the config opts in"
        );
        assert!(
            !attributes.decorations,
            "fullscreen must not have window chrome"
        );
    }

    #[test]
    fn default_window_config_stays_windowed_and_decorated() {
        let config = WindowConfig::new("windowed", 1280, 800);
        let attributes = window_attributes(&config);

        assert_eq!(attributes.fullscreen, None);
        assert!(
            attributes.decorations,
            "existing windowed behavior stays unchanged"
        );
    }

    // --- viewport -------------------------------------------------------------------

    #[test]
    fn a_window_matching_the_session_needs_no_bars() {
        let v = Viewport::letterbox(1920, 1080, 1920, 1080);
        assert_eq!((v.dest_x, v.dest_y), (0, 0));
        assert_eq!((v.dest_width, v.dest_height), (1920, 1080));
    }

    #[test]
    fn a_wider_window_gets_pillars_and_a_taller_one_gets_bars() {
        // 1000x1000 window, 16:9 session -> the height binds; bars top and bottom.
        let wide_session = Viewport::letterbox(1000, 1000, 1600, 900);
        assert_eq!(wide_session.dest_width, 1000);
        assert_eq!(wide_session.dest_height, 563); // 900 * (1000/1600), rounded
        assert_eq!(wide_session.dest_x, 0);
        assert_eq!(wide_session.dest_y, (1000 - 563) / 2);

        // 1000x1000 window, 9:16 session -> the width binds; pillars left and right.
        let tall_session = Viewport::letterbox(1000, 1000, 900, 1600);
        assert_eq!(tall_session.dest_height, 1000);
        assert_eq!(tall_session.dest_width, 563);
        assert_eq!(tall_session.dest_x, (1000 - 563) / 2);
        assert_eq!(tall_session.dest_y, 0);
    }

    #[test]
    fn the_aspect_ratio_survives_scaling() {
        // A stretched image is the failure this guards: dest aspect must track source
        // aspect to within a pixel of rounding.
        let v = Viewport::letterbox(640, 480, 1920, 1080);
        let source = 1920.0 / 1080.0;
        let dest = f64::from(v.dest_width) / f64::from(v.dest_height);
        assert!(
            (source - dest).abs() < 0.01,
            "aspect drifted: {source} vs {dest}"
        );
        assert!(v.dest_width <= 640 && v.dest_height <= 480);
    }

    #[test]
    fn a_degenerate_window_or_session_yields_an_empty_viewport() {
        assert!(Viewport::letterbox(0, 1080, 1920, 1080).is_empty());
        assert!(Viewport::letterbox(1920, 0, 1920, 1080).is_empty());
        assert!(Viewport::letterbox(1920, 1080, 0, 1080).is_empty());
        assert!(Viewport::letterbox(1920, 1080, 1920, 0).is_empty());
        assert_eq!(Viewport::letterbox(0, 0, 0, 0).to_session(1.0, 1.0), None);
    }

    // --- pointer mapping ------------------------------------------------------------

    #[test]
    fn the_pointer_mapping_is_the_inverse_of_the_letterbox() {
        // 800x600 window, 400x300 session -> exactly 2x, no bars.
        let v = Viewport::letterbox(800, 600, 400, 300);
        assert_eq!(v.dest_width, 800);
        assert_eq!(v.to_session(0.0, 0.0), Some((0, 0)));
        assert_eq!(v.to_session(400.0, 300.0), Some((200, 150)));
        assert_eq!(v.to_session(799.0, 599.0), Some((399, 299)));
    }

    #[test]
    fn a_click_in_the_letterbox_bar_clamps_to_the_edge_rather_than_vanishing() {
        // 1000x1000 window, 1000x500 session -> 250px bars top and bottom.
        let v = Viewport::letterbox(1000, 1000, 1000, 500);
        assert_eq!(v.dest_y, 250);
        assert_eq!(v.to_session(500.0, 0.0), Some((500, 0)), "above the image");
        assert_eq!(
            v.to_session(500.0, 999.0),
            Some((500, 499)),
            "below the image"
        );
        assert_eq!(v.to_session(500.0, 250.0), Some((500, 0)), "top image row");
    }

    #[test]
    fn the_pointer_subtracts_a_horizontal_offset_as_well_as_a_vertical_one() {
        // Pillarboxing, the mirror of the case above. Dropping the `- dest_x` term is
        // invisible whenever the bars happen to be horizontal, so it needs its own test:
        // with 250px pillars every click would land 250 session pixels too far right.
        let v = Viewport::letterbox(1000, 1000, 500, 1000);
        assert_eq!((v.dest_x, v.dest_width), (250, 500));
        assert_eq!(v.to_session(250.0, 0.0), Some((0, 0)), "left image column");
        assert_eq!(v.to_session(500.0, 0.0), Some((250, 0)), "image centre");
        assert_eq!(
            v.to_session(749.0, 0.0),
            Some((499, 0)),
            "right image column"
        );
        assert_eq!(v.to_session(0.0, 0.0), Some((0, 0)), "in the left pillar");
        assert_eq!(
            v.to_session(999.0, 0.0),
            Some((499, 0)),
            "in the right pillar"
        );
    }

    #[test]
    fn a_scaled_window_does_not_map_one_to_one() {
        // The bug this catches is treating window pixels as session pixels: at 2x that
        // puts every click at half the distance from the origin it belongs at.
        let v = Viewport::letterbox(800, 600, 400, 300);
        assert_ne!(v.to_session(600.0, 400.0), Some((600, 400)));
        assert_eq!(v.to_session(600.0, 400.0), Some((300, 200)));
    }

    // --- presentation ----------------------------------------------------------------

    #[test]
    fn a_one_to_one_frame_is_copied_with_rgba_read_as_0rgb() {
        // Channel order is the classic silent bug: a BGR read swaps red and blue and
        // still produces a perfectly plausible-looking desktop.
        let v = Viewport::letterbox(2, 2, 2, 2);
        let mut dst = vec![0u32; 4];
        present_into(&mut dst, 2, 2, &v, &quad());
        assert_eq!(dst, vec![rgb(RED), rgb(GREEN), rgb(BLUE), rgb(WHITE)]);
    }

    #[test]
    fn the_alpha_byte_is_not_allowed_to_leak_into_the_high_bits() {
        // softbuffer requires the top 8 bits to be zero.
        let v = Viewport::letterbox(2, 2, 2, 2);
        let mut dst = vec![0u32; 4];
        present_into(&mut dst, 2, 2, &v, &quad());
        assert!(dst.iter().all(|p| p >> 24 == 0), "high byte must be clear");
    }

    #[test]
    fn scaling_up_replicates_each_source_pixel_into_its_own_quadrant() {
        // 2x2 source into a 4x4 window: each source pixel owns a 2x2 block. A
        // transposed or off-by-one mapping moves a colour into the wrong quadrant.
        let v = Viewport::letterbox(4, 4, 2, 2);
        assert_eq!((v.dest_width, v.dest_height), (4, 4));
        let mut dst = vec![0u32; 16];
        present_into(&mut dst, 4, 4, &v, &quad());

        let at = |x: usize, y: usize| dst[y * 4 + x];
        assert_eq!(at(0, 0), rgb(RED));
        assert_eq!(at(1, 1), rgb(RED));
        assert_eq!(at(2, 0), rgb(GREEN));
        assert_eq!(at(3, 1), rgb(GREEN));
        assert_eq!(at(0, 2), rgb(BLUE));
        assert_eq!(at(1, 3), rgb(BLUE));
        assert_eq!(at(2, 2), rgb(WHITE));
        assert_eq!(at(3, 3), rgb(WHITE));
    }

    #[test]
    fn the_letterbox_bars_are_black_and_the_image_is_centred() {
        // 4x2 window, 2x2 session -> 2x2 image centred with one-pixel pillars.
        let v = Viewport::letterbox(4, 2, 2, 2);
        assert_eq!((v.dest_x, v.dest_width), (1, 2));
        let mut dst = vec![0xDEAD_BEEF_u32; 8];
        present_into(&mut dst, 4, 2, &v, &quad());

        let at = |x: usize, y: usize| dst[y * 4 + x];
        assert_eq!(at(0, 0), 0, "left pillar");
        assert_eq!(at(3, 0), 0, "right pillar");
        assert_eq!(at(0, 1), 0, "left pillar, second row");
        assert_eq!(at(3, 1), 0, "right pillar, second row");
        assert_eq!(at(1, 0), rgb(RED));
        assert_eq!(at(2, 0), rgb(GREEN));
        assert_eq!(at(1, 1), rgb(BLUE));
        assert_eq!(at(2, 1), rgb(WHITE));
    }

    #[test]
    fn a_short_source_buffer_blanks_the_window_instead_of_reading_past_it() {
        let v = Viewport::letterbox(2, 2, 2, 2);
        let mut dst = vec![0xFFFF_FFFF_u32; 4];
        present_into(&mut dst, 2, 2, &v, &[0u8; 4]); // one pixel, four needed
        assert!(dst.iter().all(|&p| p == 0));
    }

    #[test]
    fn a_viewport_origin_outside_the_window_blanks_instead_of_panicking() {
        // `letterbox` cannot produce this, but `present_into` is public and the column
        // arithmetic subtracts `dest_x` — an origin past the right edge would underflow.
        let bogus = Viewport {
            dest_x: 10,
            dest_y: 0,
            dest_width: 2,
            dest_height: 2,
            session_width: 2,
            session_height: 2,
        };
        let mut dst = vec![0xFFFF_FFFF_u32; 4];
        present_into(&mut dst, 2, 2, &bogus, &quad());
        assert!(dst.iter().all(|&p| p == 0));
    }

    #[test]
    fn an_undersized_destination_blanks_instead_of_panicking() {
        let v = Viewport::letterbox(4, 4, 2, 2);
        let mut dst = vec![0xFFFF_FFFF_u32; 4]; // claims 4x4 but holds 2x2
        present_into(&mut dst, 4, 4, &v, &quad());
        assert!(dst.iter().all(|&p| p == 0));
    }

    #[test]
    fn the_overlay_dims_what_is_behind_it_and_writes_over_it() {
        let (w, h) = (400u32, 200u32);
        let mut buf = vec![0x00ff_ffffu32; (w * h) as usize];
        draw_overlay(&mut buf, w, h, &overlay_lines());

        // Inside the panel, pure white must have been darkened.
        let inside = buf[(20 * w + 20) as usize];
        assert_ne!(inside, 0x00ff_ffff, "the panel should dim its background");

        // Far outside the panel, the image is untouched.
        let outside = buf[((h - 1) * w + (w - 1)) as usize];
        assert_eq!(
            outside, 0x00ff_ffff,
            "the rest of the desktop is not dimmed"
        );
    }

    #[test]
    fn a_panel_wider_than_the_window_clips_instead_of_wrapping_onto_the_next_row() {
        // The text is far wider than this window, so the panel must be clipped at the
        // right edge. A buffer is a flat array: if the clamp is missing, writing past the
        // row end silently lands on the START of the row below. That is invisible to a
        // length check, so the oracle is the untouched left margin of the lower rows.
        let (w, h) = (100u32, 120u32);
        const SENTINEL: u32 = 0x00ff_ffff;
        let mut buf = vec![SENTINEL; (w * h) as usize];
        draw_overlay(&mut buf, w, h, &overlay_lines());

        // Column 0..MARGIN is left of the panel on every row, so nothing should touch it.
        for y in 0..h {
            for x in 0..12u32 {
                assert_eq!(
                    buf[(y * w + x) as usize],
                    SENTINEL,
                    "pixel ({x},{y}) was written; an overflowing row wrapped into it"
                );
            }
        }
    }

    #[test]
    fn an_overlay_with_nothing_to_say_changes_nothing() {
        let (w, h) = (64u32, 32u32);
        let mut buf = vec![0x0012_3456u32; (w * h) as usize];
        let before = buf.clone();
        draw_overlay(&mut buf, w, h, &[]);
        assert_eq!(buf, before);
    }

    #[test]
    fn a_zero_sized_window_does_not_panic() {
        let mut buf: Vec<u32> = Vec::new();
        draw_overlay(&mut buf, 0, 0, &overlay_lines());
        assert!(buf.is_empty());
    }

    fn overlay_lines() -> Vec<String> {
        vec!["latency p50 3.5ms".to_string(), "cache 75% hit".to_string()]
    }

    // --- blank-window notice and the repaint stall rule --------------------------------

    #[test]
    fn a_repaint_that_arrived_is_never_a_stall_however_long_it_took() {
        // The frame count moving is the whole proof that the server answered. Time alone
        // must not convict it — this is the guard against reporting a healthy session.
        assert!(!repaint_stalled(Duration::from_secs(600), 100, 101));
    }

    #[test]
    fn a_repaint_inside_its_grace_is_not_yet_a_stall() {
        assert!(!repaint_stalled(
            REPAINT_GRACE - Duration::from_millis(1),
            100,
            100
        ));
    }

    #[test]
    fn the_sett_is_symmetric_about_its_pivot() {
        // A tartan sett runs to a pivot and reflects. If the mirror is wrong the pattern
        // has a visible seam every repeat, which is the one thing that would make this
        // read as a bug rather than as a deliberate screen.
        let threads = tartan_threads();
        assert!(!threads.is_empty());
        for (i, colour) in threads.iter().enumerate() {
            let mirrored = threads[threads.len() - 1 - i];
            assert_eq!(
                *colour, mirrored,
                "thread {i} does not mirror its opposite; the sett has a seam"
            );
        }
    }

    #[test]
    fn the_tartan_covers_every_pixel_and_uses_more_than_one_colour() {
        const SENTINEL: u32 = 0x00de_adbe;
        let (w, h) = (200u32, 120u32);
        let mut buf = vec![SENTINEL; (w * h) as usize];
        fill_tartan(&mut buf, w, h);

        assert!(
            !buf.contains(&SENTINEL),
            "every pixel must be painted, or the black shows through"
        );
        let distinct: std::collections::BTreeSet<u32> = buf.iter().copied().collect();
        assert!(
            distinct.len() >= 4,
            "a tartan needs its stripes; got {} colours",
            distinct.len()
        );
    }

    #[test]
    fn the_weave_reads_from_both_warp_and_weft() {
        // The twill is what makes this a weave rather than vertical stripes. If the warp
        // and weft branches ever collapse to one, every column would be a single colour —
        // so the oracle is that some column is not uniform.
        let (w, h) = (200u32, 200u32);
        let mut buf = vec![0u32; (w * h) as usize];
        fill_tartan(&mut buf, w, h);
        let mixed_column = (0..w).any(|x| {
            let first = buf[x as usize];
            (0..h).any(|y| buf[(y * w + x) as usize] != first)
        });
        assert!(mixed_column, "no column varies; the weft never contributes");
    }

    #[test]
    fn a_zero_sized_window_does_not_panic_filling_tartan() {
        let mut buf: Vec<u32> = Vec::new();
        fill_tartan(&mut buf, 0, 0);
        assert!(buf.is_empty());
    }

    /// Render a corner of the tartan as letters, one per colour, for a human to look at.
    /// Not an assertion — the same reasoning as `font::render_ascii_art`: no pixel check
    /// can tell a convincing tartan from a mess, so this exists to be read.
    #[test]
    fn tartan_swatch_for_eyeballing() {
        let (w, h) = (96u32, 32u32);
        let mut buf = vec![0u32; (w * h) as usize];
        fill_tartan(&mut buf, w, h);
        let mut out = String::new();
        for y in 0..h {
            for x in 0..w {
                out.push(match buf[(y * w + x) as usize] {
                    0x0000_0000 => '.',
                    0x0010_2A4A => 'B',
                    0x000E_3B26 => 'G',
                    0x008E_1B1B => 'R',
                    0x00D8_D8D8 => 'W',
                    _ => '?',
                });
            }
            out.push('\n');
        }
        println!("{out}");
        assert!(!out.contains('?'), "unexpected colour in the weave");
    }

    #[test]
    fn a_black_frame_reads_as_black() {
        let buf = vec![0x0000_0000u32; 4096];
        assert!(frame_looks_black(&buf, BLACK_SAMPLE_STRIDE));
    }

    #[test]
    fn a_frame_with_any_sampled_colour_is_not_black() {
        // Whichever channel carries it: a red-only desktop must not read as blank, and a
        // per-channel test written as one combined comparison would let two of these
        // through.
        for bright in [0x00ff_0000u32, 0x0000_ff00, 0x0000_00ff] {
            let mut buf = vec![0x0000_0000u32; 4096];
            buf[0] = bright;
            assert!(
                !frame_looks_black(&buf, BLACK_SAMPLE_STRIDE),
                "{bright:#010x} should defeat the blackness test"
            );
        }
    }

    #[test]
    fn very_dark_but_not_quite_black_still_counts_as_black() {
        // A real "black" desktop is rarely all zeroes — compression and colour conversion
        // leave a little noise, and an exact-zero test would call a black screen "content"
        // and stay silent through the very failure this exists for.
        let buf = vec![0x000f_0f0fu32; 4096];
        assert!(frame_looks_black(&buf, BLACK_SAMPLE_STRIDE));
    }

    #[test]
    fn sampling_can_miss_colour_that_falls_between_samples() {
        // Documents the trade-off rather than pretending it away: this is a sampled test,
        // so a single bright pixel between two samples is not seen. That is acceptable
        // only because the result is never used alone — it gates a notice that already
        // requires an overdue repaint. If this ever becomes load-bearing on its own, the
        // stride is the thing to revisit.
        let mut buf = vec![0x0000_0000u32; 4096];
        buf[1] = 0x00ff_ffff;
        assert!(frame_looks_black(&buf, BLACK_SAMPLE_STRIDE));
    }

    #[test]
    fn a_repaint_that_never_arrives_becomes_a_stall() {
        assert!(repaint_stalled(REPAINT_GRACE, 100, 100));
    }

    #[test]
    fn the_blank_notice_reports_how_long_the_repaint_has_been_outstanding() {
        let lines = blank_notice_lines(Some(Duration::from_secs(14)));
        assert!(
            lines.iter().any(|l| l.contains("14s")),
            "the wait must be quantified, got: {lines:?}"
        );
    }

    #[test]
    fn the_blank_notice_uses_ascii_hyphens() {
        assert_eq!(
            blank_notice_lines(Some(Duration::from_secs(14))),
            vec![
                "Waiting for the server to send a picture.".to_owned(),
                "Repaint requested 14s ago - nothing has arrived yet.".to_owned(),
                "The connection is up - this is not a dropped session.".to_owned(),
            ]
        );
    }

    #[test]
    fn the_blank_notice_denies_the_thing_it_looks_like() {
        // Every version of this failure looks like a dropped connection and is not one.
        // If that line ever goes, the notice stops doing its job.
        for waiting in [None, Some(Duration::from_secs(9))] {
            let lines = blank_notice_lines(waiting);
            assert!(
                lines.iter().any(|l| l.contains("not a dropped session")),
                "the notice must say the link is alive, got: {lines:?}"
            );
        }
    }

    #[test]
    fn a_blank_notice_with_no_repaint_pending_says_nothing_ever_arrived() {
        let lines = blank_notice_lines(None);
        assert!(
            lines.iter().any(|l| l.contains("No desktop surface")),
            "got: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("Repaint requested")),
            "nothing was requested, so nothing is outstanding: {lines:?}"
        );
    }

    #[test]
    fn a_zero_sized_window_does_not_panic_drawing_the_blank_notice() {
        let mut buf: Vec<u32> = Vec::new();
        draw_blank_notice(&mut buf, 0, 0, Some(Duration::from_secs(3)));
        assert!(buf.is_empty());
    }

    #[test]
    fn the_blank_notice_survives_awkward_window_shapes() {
        // Centring computes a top that goes negative on a window shorter than the block,
        // and an x that goes negative on one narrower than the text. Both must clip
        // rather than panic or write out of bounds.
        const SENTINEL: u32 = 0x0012_3456;
        for (w, h) in [(60u32, 80u32), (1, 1), (2000, 3), (17, 400), (0, 0)] {
            let mut buf = vec![SENTINEL; (w * h) as usize];
            draw_blank_notice(&mut buf, w, h, Some(Duration::from_secs(7)));
            assert_eq!(buf.len(), (w * h) as usize, "{w}x{h} resized the buffer");
        }
    }

    #[test]
    fn the_blank_notice_centres_its_block_and_spills_into_no_row_above_it() {
        // The oracle is the untouched band above the text: it pins the vertical centring
        // and catches a glyph row drawn at the wrong offset, which a "did anything get
        // written" check cannot.
        let (w, h) = (600u32, 400u32);
        const SENTINEL: u32 = 0x0012_3456;
        let mut buf = vec![SENTINEL; (w * h) as usize];
        draw_blank_notice(&mut buf, w, h, None);

        // The notice dims a padded panel behind its text so the words stay readable over
        // tartan, so the clear band starts above that panel, not above the glyphs.
        const NOTICE_PAD: i32 = 12;
        let top = (h as i32 - font::CHAR_H * 3) / 2 - NOTICE_PAD;
        assert!(
            top > 0,
            "this window must be tall enough to have a clear band"
        );
        for y in 0..top as u32 {
            for x in 0..w {
                assert_eq!(
                    buf[(y * w + x) as usize],
                    SENTINEL,
                    "({x},{y}) is above the centred block and must be untouched"
                );
            }
        }
        // And the block itself must actually have been drawn, or the check above passes
        // for the wrong reason.
        let drawn = buf.iter().any(|&p| p != SENTINEL);
        assert!(drawn, "the notice drew nothing at all");
    }

    // --- toasts -----------------------------------------------------------------------

    #[test]
    fn a_toast_paints_its_card_bottom_right_with_the_accent_edge() {
        let (w, h) = (600u32, 400u32);
        let mut buf = vec![0u32; (w * h) as usize];
        draw_toasts(
            &mut buf,
            w,
            h,
            &[Toast {
                warn: true,
                title: "Audio device lost".into(),
                body: "playback stopped".into(),
            }],
        );
        // Card occupies the bottom-right: left edge at 600-16-400=184.
        let card_left = 184usize;
        let inside_y = (h - 20) as usize;
        assert_eq!(
            buf[inside_y * w as usize + card_left],
            0x00FFC93C,
            "the 2px left border carries the warn accent"
        );
        assert_eq!(
            buf[inside_y * w as usize + card_left + 10],
            0x001E2126,
            "the card body is bg.raised"
        );
        // Far left of the window is untouched desktop.
        assert_eq!(buf[inside_y * w as usize], 0);
    }

    #[test]
    fn a_danger_toast_carries_the_danger_accent() {
        let (w, h) = (600u32, 400u32);
        let mut buf = vec![0u32; (w * h) as usize];
        draw_toasts(
            &mut buf,
            w,
            h,
            &[Toast {
                warn: false,
                title: "t".into(),
                body: "b".into(),
            }],
        );
        let card_left = 184usize;
        let inside_y = (h - 20) as usize;
        assert_eq!(buf[inside_y * w as usize + card_left], 0x00FF5D4D);
    }

    #[test]
    fn stacked_toasts_do_not_overlap() {
        let (w, h) = (600u32, 400u32);
        let mut buf = vec![0u32; (w * h) as usize];
        let toast = |title: &str| Toast {
            warn: true,
            title: title.into(),
            body: "b".into(),
        };
        draw_toasts(&mut buf, w, h, &[toast("one"), toast("two")]);
        // Two cards: between them there must be an untouched gap row.
        let card_h = 14 * 2 + 16 * 2 + 4; // PAD*2 + 2 glyph rows + spacing
        let gap_y = (h as i32 - 16 - card_h - 5) as usize; // inside the 10px gap
        let card_left = 184usize;
        assert_eq!(
            buf[gap_y * w as usize + card_left + 10],
            0,
            "the gap between stacked cards stays desktop"
        );
    }

    #[test]
    fn a_zero_sized_window_and_no_toasts_are_no_ops() {
        let mut empty: Vec<u32> = Vec::new();
        draw_toasts(&mut empty, 0, 0, &[]);
        let (w, h) = (32u32, 32u32);
        let mut buf = vec![0xAAu32; (w * h) as usize];
        let before = buf.clone();
        draw_toasts(&mut buf, w, h, &[]);
        assert_eq!(buf, before);
    }

    // --- title diagnostics ------------------------------------------------------------

    #[test]
    fn the_title_carries_the_numbers_someone_glances_at() {
        use crate::stats::{BASELINE_SAMPLES, CacheStats, WINDOW};
        let mut s = SessionStats::new();
        for _ in 0..BASELINE_SAMPLES {
            s.latency.record(1_000);
        }
        for _ in 0..WINDOW {
            s.latency.record(3_500);
        }
        s.cache = CacheStats {
            hits: 3,
            misses: 1,
            ..Default::default()
        };
        let t = title_line(
            "mdrdp — Temper",
            2560,
            1440,
            &s,
            24.2,
            3.12,
            Some("Avc444v2"),
        );
        assert!(t.starts_with("mdrdp — Temper"), "got: {t}");
        assert!(t.contains("2560x1440"), "got: {t}");
        assert!(t.contains("· Avc444v2"), "got: {t}");
        assert!(t.contains("3.5ms"), "got: {t}");
        assert!(t.contains("(+2.5)"), "drift must be visible; got: {t}");
        assert!(t.contains("24 fps"), "got: {t}");
        assert!(t.contains("3.1 Mb/s"), "got: {t}");
        assert!(t.contains("cache 75%"), "got: {t}");
        assert!(!t.contains("STALE"), "nothing is stale here; got: {t}");
    }

    #[test]
    fn a_quiet_session_title_is_just_the_name_and_resolution() {
        let s = SessionStats::new();
        let t = title_line("mdrdp — box", 1920, 1080, &s, 0.0, 0.0, None);
        assert_eq!(t, "mdrdp — box — 1920x1080");
    }

    #[test]
    fn staleness_reaches_the_title_because_the_title_is_always_visible() {
        let mut s = SessionStats::new();
        s.decode_errors = 2;
        s.undecoded_regions = 3;
        let t = title_line("mdrdp — box", 800, 600, &s, 0.0, 0.0, None);
        assert!(t.contains("STALE 5"), "got: {t}");
    }

    #[test]
    fn codec_note_names_the_dominant_codec_and_drops_the_noise() {
        use std::collections::BTreeMap;
        // Before: AVC painted 1000 bytes, Uncompressed 50. Since: AVC painted 9000
        // more, Uncompressed 100 more (under a tenth of the interval), ClearCodec
        // nothing at all.
        let before = BTreeMap::from([
            ("Avc444v2".to_owned(), 1000),
            ("Uncompressed".to_owned(), 50),
            ("ClearCodec".to_owned(), 700),
        ]);
        let now = BTreeMap::from([
            ("Avc444v2".to_owned(), 10_000),
            ("Uncompressed".to_owned(), 150),
            ("ClearCodec".to_owned(), 700),
        ]);
        assert_eq!(codec_note(&now, &before), Some("Avc444v2".to_owned()));
    }

    #[test]
    fn codec_note_orders_a_real_mix_by_interval_paint_not_lifetime_totals() {
        use std::collections::BTreeMap;
        // Lifetime totals favour ClearCodec, but this interval Progressive painted
        // more — the note must follow the interval.
        let before = BTreeMap::from([
            ("ClearCodec".to_owned(), 90_000),
            ("WireToSurface2/RemoteFxProgressive".to_owned(), 10_000),
        ]);
        let now = BTreeMap::from([
            ("ClearCodec".to_owned(), 92_000),
            ("WireToSurface2/RemoteFxProgressive".to_owned(), 15_000),
        ]);
        assert_eq!(
            codec_note(&now, &before),
            Some("Progressive+ClearCodec".to_owned())
        );
    }

    #[test]
    fn an_idle_interval_yields_no_codec_note_rather_than_an_empty_one() {
        use std::collections::BTreeMap;
        let counters = BTreeMap::from([("ClearCodec".to_owned(), 5_000_u64)]);
        assert_eq!(codec_note(&counters, &counters), None);
        assert_eq!(codec_note(&BTreeMap::new(), &BTreeMap::new()), None);
    }

    #[test]
    fn a_viewport_extent_that_would_overflow_is_clamped_not_wrapped() {
        // The origin is inside the window but the extent is absurd. Plain addition
        // overflows: a panic in debug, a wrap to a small number in release that paints a
        // sliver and reports success.
        let (w, h) = (8u32, 4u32);
        let mut dst = vec![0xdead_beefu32; (w * h) as usize];
        let viewport = Viewport {
            dest_x: 1,
            dest_y: 1,
            dest_width: u32::MAX,
            dest_height: u32::MAX,
            session_width: 2,
            session_height: 2,
        };
        let src = vec![0u8; 2 * 2 * 4];
        present_into(&mut dst, w, h, &viewport, &src);
        // The point is that it neither panicked nor wrote outside the buffer.
        assert_eq!(dst.len(), (w * h) as usize);
    }

    #[test]
    fn a_store_frame_reaches_the_window_buffer_unchanged() {
        // End to end over the real store: fill a surface, map it to output, present it.
        let mut store = SurfaceStore::new();
        store.create(1, 2, 2);
        store.map_to_output(1);
        store
            .solid_fill(1, &[Rect::new(0, 0, 2, 2)], GREEN)
            .expect("fill");

        let session = store.output_surface().expect("output surface");
        let v = Viewport::letterbox(2, 2, session.width, session.height);
        let mut dst = vec![0u32; 4];
        present_into(&mut dst, 2, 2, &v, session.pixels());
        assert_eq!(dst, vec![rgb(GREEN); 4]);
    }
}
