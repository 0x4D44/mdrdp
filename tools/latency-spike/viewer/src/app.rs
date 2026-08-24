//! The window: winit + softbuffer + `mdrdp::window::present_into`.
//!
//! Shaped after mdrdp's own presenter (`src/window.rs`, `SessionApp::redraw` at
//! line 1356): a decode thread paints into a shared slot, the window thread resizes
//! the softbuffer surface, runs `present_into` into its buffer and calls
//! `buffer.present()`. Same crates, same versions, same call order — which is what
//! makes a spike measurement and an mdrdp measurement comparable on the client side.
//!
//! Three deliberate differences, all because this is an instrument rather than a
//! client. The viewport is 1:1 (see [`crate::present`]); there is no overlay, since a
//! stats panel would repaint pixels inside the interval being measured; and the
//! converted picture is kept here in a persistent canvas so a present costs a
//! damage-only convert plus a copy instead of a full-surface convert (see
//! [`ViewerApp::canvas`] for why softbuffer cannot do that for us).

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::PhysicalKey;
use winit::window::{Window, WindowId};

use mdrdp::window::present_into;
use rhydra::input_proto::KeyKind;

use crate::clock::Clock;
use crate::input_link::InputLink;
use crate::interrupt;
use crate::keymap;
use crate::present::{one_to_one, present_region_into};
use crate::sink::{Damage, DamageRect, Frame, FrameSlot, PaintStamps};
use crate::stats::{FrameRecord, InputRecord, RectRecord, StatsLog};

/// The window before the first frame tells us the stream's coded size.
const PLACEHOLDER: PhysicalSize<u32> = PhysicalSize::new(640, 360);

/// How often the loop wakes to check the interrupt flag. It paints nothing — a redraw
/// only happens when a frame arrives or the window is resized — so this costs one
/// no-op iteration every 200 ms and buys a Ctrl-C that closes the sockets and flushes
/// the stats file instead of killing the process mid-line.
const INTERRUPT_POLL: Duration = Duration::from_millis(200);

/// One damaged frame rectangle, in window coordinates, clipped to what the window can
/// actually show — the shape `present_with_damage` documents.
///
/// `None` when nothing of it is visible, which is exactly when there is no damage to
/// declare. The clip repeats [`present_region_into`]'s, because the two must agree:
/// declaring damage the convert did not write would let a backend that honours damage
/// show a region the canvas never updated.
fn window_damage(
    viewport: &mdrdp::window::Viewport,
    window: (u32, u32),
    r: DamageRect,
) -> Option<softbuffer::Rect> {
    let (window_width, window_height) = window;
    if viewport.dest_x >= window_width || viewport.dest_y >= window_height {
        return None;
    }
    let x1 =
        r.x.saturating_add(r.w)
            .min(u32::from(viewport.session_width))
            .min(window_width - viewport.dest_x);
    let y1 =
        r.y.saturating_add(r.h)
            .min(u32::from(viewport.session_height))
            .min(window_height - viewport.dest_y);
    Some(softbuffer::Rect {
        x: viewport.dest_x + r.x,
        y: viewport.dest_y + r.y,
        width: NonZeroU32::new(x1.checked_sub(r.x)?)?,
        height: NonZeroU32::new(y1.checked_sub(r.y)?)?,
    })
}

/// Wakes the window thread when a frame lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserEvent {
    Frame,
}

type SbContext = softbuffer::Context<Arc<Window>>;
type SbSurface = softbuffer::Surface<Arc<Window>, Arc<Window>>;

pub struct ViewerApp {
    title: String,
    slot: Arc<FrameSlot>,
    stats: Arc<StatsLog>,
    input: Option<Arc<InputLink>>,
    clock: Clock,

    window: Option<Arc<Window>>,
    /// Held for as long as the surface: dropping the context invalidates it.
    _context: Option<SbContext>,
    surface: Option<SbSurface>,

    /// The most recent decoded frame, retained so a resize or an expose can repaint
    /// without waiting for the next one.
    current: Option<Frame>,
    /// The stream size the window has already been sized to, so the resize request
    /// fires once per resolution rather than once per frame.
    sized_to: Option<(u32, u32)>,
    /// One warning per unpresentable frame geometry, not one per frame.
    warned_geometry: bool,

    /// The converted picture, in the window's pixels and softbuffer's `0x00RRGGBB`
    /// packing, retained across presents.
    ///
    /// It exists because softbuffer cannot: its macOS backend allocates a fresh zeroed
    /// buffer on every `buffer_mut()` and its `present_with_damage` ignores the damage
    /// it is given, so *nothing* survives a present down there and every present would
    /// otherwise re-convert the whole surface. Keeping the converted pixels here turns
    /// a rect update's present into a damage-only convert plus one sequential
    /// `copy_from_slice`, and an expose into the copy alone.
    canvas: Vec<u32>,
    /// The window size `canvas` was built for. A mismatch means every pixel in it is
    /// at the wrong offset, so it must be rebuilt rather than patched.
    canvas_size: (u32, u32),
    /// Whether `canvas` currently holds the picture that is on screen. False before
    /// the first convert and after anything that invalidates the whole surface (a
    /// resize, a scale change) — the one bit that decides "patch" against "rebuild".
    canvas_valid: bool,
}

impl ViewerApp {
    pub fn new(
        title: String,
        slot: Arc<FrameSlot>,
        stats: Arc<StatsLog>,
        input: Option<Arc<InputLink>>,
    ) -> Self {
        Self {
            title,
            slot,
            stats,
            input,
            clock: Clock::new(),
            window: None,
            _context: None,
            surface: None,
            current: None,
            sized_to: None,
            warned_geometry: false,
            canvas: Vec::new(),
            canvas_size: (0, 0),
            canvas_valid: false,
        }
    }

    fn title_for(&self, size: Option<(u32, u32)>) -> String {
        match size {
            Some((w, h)) => format!("{} — {w}x{h}", self.title),
            None => format!("{} — waiting for stream", self.title),
        }
    }

    /// Forward one key transition, and record that we did.
    fn forward_key(&mut self, code: winit::keyboard::KeyCode, state: ElementState) {
        let Some(link) = self.input.as_ref() else {
            return;
        };
        let Some(vk) = keymap::virtual_key(code) else {
            return; // Unmapped keys never reach the wire — see `keymap`.
        };
        let kind = match state {
            ElementState::Pressed => KeyKind::Down,
            ElementState::Released => KeyKind::Up,
        };
        // Stamped before the write, so the recorded time can only be early, never late.
        let sent_us = self.clock.now_us();
        let result = link.send(kind, vk);
        match result {
            Ok(seq) => self
                .stats
                .record(&InputRecord::new(seq, vk, kind.as_str(), sent_us)),
            Err(e) => {
                eprintln!("input: send failed: {e}; keystrokes are disabled");
                // The link has already failed closed. Drop our handle so auto-repeat
                // cannot turn one socket failure into unbounded window-thread logging.
                self.input = None;
            }
        }
    }

    fn redraw(&mut self) {
        self.paint(false);
    }

    /// Present the current picture. `fresh_only` skips the whole pass when the slot
    /// holds nothing new — the wake path uses it so a redundant wake does not re-blit
    /// 8 MB to show the pixels already on screen. OS-driven paths (expose, resize)
    /// pass `false` and re-present unconditionally.
    fn paint(&mut self, fresh_only: bool) {
        let Some(window) = self.window.clone() else {
            return;
        };

        // Consume the newest decoded frame, if one arrived. Its stats line is closed
        // below, after the present; a frame that was displaced before we got here was
        // already recorded as dropped by the decode thread.
        let fresh = self.slot.take();
        if fresh_only && fresh.is_none() {
            return;
        }
        let had_fresh = fresh.is_some();
        if let Some(frame) = fresh {
            let size = (frame.width, frame.height);
            self.current = Some(frame);
            if self.sized_to != Some(size) {
                // The window opens at a placeholder size because the coded size is not
                // known until the first frame decodes. Physical pixels: on a Retina
                // display that makes the window look half-size in points while keeping
                // one stream pixel on one device pixel, which is what a 1:1 present
                // needs.
                let _ = window.request_inner_size(PhysicalSize::new(size.0, size.1));
                window.set_title(&self.title_for(Some(size)));
                self.sized_to = Some(size);
            }
        }

        let size = window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return; // Minimised. Nothing to draw into.
        };
        // Borrowed here rather than at the top: `title_for` above needs `&self`, and
        // holding `&mut self.surface` across it would borrow the whole struct.
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        if let Err(e) = surface.resize(width, height) {
            eprintln!("present: resize failed, skipping this frame: {e}");
            return;
        }
        let mut buffer = match surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("present: buffer_mut failed, skipping this frame: {e}");
                return;
            }
        };

        // softbuffer's buffer decides the geometry we must fill, and it is not
        // required to agree with the size read above: a `Resized` can land between
        // the two, and a `copy_from_slice` across a length mismatch is a panic. Trust
        // the buffer, re-reading the window once to name its shape; if even that does
        // not describe it, show black and come back on the redraw rather than
        // shipping a mis-strided picture.
        let mut win = (size.width, size.height);
        if buffer.len() != win.0 as usize * win.1 as usize {
            let now = window.inner_size();
            if buffer.len() == now.width as usize * now.height as usize {
                win = (now.width, now.height);
            } else {
                buffer.fill(0);
                self.canvas_valid = false;
                window.pre_present_notify();
                if let Err(e) = buffer.present() {
                    eprintln!("present: present failed: {e}");
                }
                window.request_redraw();
                return;
            }
            // Whatever the canvas holds is at the previous window's offsets.
            self.canvas_valid = false;
        }

        // The canvas is the window's size or it is nothing: every pixel in it is
        // addressed by the window stride.
        let canvas_pixels = win.0 as usize * win.1 as usize;
        if self.canvas_size != win || self.canvas.len() != canvas_pixels {
            self.canvas.resize(canvas_pixels, 0);
            self.canvas_size = win;
            self.canvas_valid = false;
        }

        let viewport = self
            .current
            .as_ref()
            .and_then(|f| one_to_one(win.0, win.1, f.width, f.height));
        let presentable = viewport.is_some();
        let mut partial = false;
        let mut damage: Vec<softbuffer::Rect> = Vec::new();

        // What this paint owes the canvas. Nothing, when no frame arrived and the
        // canvas is still valid: the picture is already converted and the present
        // below is a pure copy — which is what makes an expose or a redundant redraw
        // free rather than an 8 MB convert.
        if had_fresh || !self.canvas_valid {
            match (self.current.as_ref(), viewport) {
                (Some(frame), Some(viewport)) => {
                    // The partial path needs both a fresh frame that says what it
                    // changed and a canvas that is already the picture it changed
                    // *from*. Without the second, "since last time" has no referent
                    // and only a full convert is honest.
                    let rects = match &frame.damage {
                        Damage::Rects(rects) if self.canvas_valid && had_fresh => Some(rects),
                        _ => None,
                    };
                    match rects {
                        Some(rects) => {
                            for r in rects {
                                present_region_into(
                                    &mut self.canvas,
                                    win.0,
                                    win.1,
                                    &viewport,
                                    &frame.rgba,
                                    *r,
                                );
                                damage.extend(window_damage(&viewport, win, *r));
                            }
                            partial = true;
                        }
                        None => {
                            present_into(&mut self.canvas, win.0, win.1, &viewport, &frame.rgba);
                            self.canvas_valid = true;
                        }
                    }
                }
                (Some(frame), None) => {
                    if !self.warned_geometry {
                        self.warned_geometry = true;
                        eprintln!(
                            "present: {}x{} cannot be presented; showing black",
                            frame.width, frame.height
                        );
                    }
                    // Black is a picture too: the canvas holds it, so a later expose
                    // does not have to work it out again.
                    self.canvas.fill(0);
                    self.canvas_valid = true;
                }
                // Nothing decoded yet: black, not whatever the buffer last held.
                (None, _) => {
                    self.canvas.fill(0);
                    self.canvas_valid = true;
                }
            }
        }

        // The present itself: one sequential copy, never a convert. Lengths were
        // reconciled above, so this cannot panic.
        buffer.copy_from_slice(&self.canvas);

        window.pre_present_notify();
        // softbuffer 0.4.8's macOS backend throws the damage away and presents the
        // whole surface, so this is a no-op there today — but it is the truthful call
        // for what changed, and it becomes the fast path unchanged the day a backend
        // honours it (or phase 2 replaces the presenter).
        let presented = if partial {
            buffer.present_with_damage(&damage)
        } else {
            buffer.present()
        };
        if let Err(e) = presented {
            eprintln!("present: present failed: {e}");
            return;
        }
        let present_done_us = self.clock.now_us();

        // One record per presented snapshot, written where its last stage actually
        // completes — in whichever shape the message that painted it uses.
        if let Some(frame) = self.current.as_mut() {
            if presentable && frame.stamps_pending {
                frame.stamps_pending = false;
                match frame.stamps {
                    PaintStamps::Au(s) => {
                        self.stats
                            .record(&FrameRecord::new(&s, Some(present_done_us), partial))
                    }
                    PaintStamps::Rects(s) => {
                        self.stats
                            .record(&RectRecord::painted(&s, Some(present_done_us), partial))
                    }
                }
            }
        }
    }
}

impl ApplicationHandler<UserEvent> for ViewerApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // Resume can fire more than once; the window survives it.
        }
        let attributes = Window::default_attributes()
            .with_title(self.title_for(None))
            .with_inner_size(PLACEHOLDER);
        let window = match event_loop.create_window(attributes) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("window: creation failed: {e}");
                event_loop.exit();
                return;
            }
        };
        let context = match softbuffer::Context::new(window.clone()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("window: softbuffer context failed: {e}");
                event_loop.exit();
                return;
            }
        };
        let surface = match softbuffer::Surface::new(&context, window.clone()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("window: softbuffer surface failed: {e}");
                event_loop.exit();
                return;
            }
        };
        window.request_redraw();
        self.window = Some(window);
        self._context = Some(context);
        self.surface = Some(surface);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            // Present now, on the wake, rather than `request_redraw`. On macOS a
            // requested redraw rides the view's display cycle, which taxed every
            // wake with up to a compositor interval of waiting — measured as
            // paint→present p50 11.3 ms on the Increment 1 typing runs, against
            // 5.4 ms for the decode path whose ~6 ms of work absorbed the phase.
            // Presenting directly from the wake removes the wait; fresh-only, so
            // a wake that lost its frame to a newer one costs nothing.
            UserEvent::Frame => self.paint(true),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                // The canvas is indexed by the window stride and offset by the
                // viewport, so a new geometry makes every pixel in it wrong. Drop it
                // here rather than trying to detect the change at paint time: this is
                // where the change is actually announced.
                self.canvas_valid = false;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // Auto-repeat is forwarded too: an OS repeat is a genuine extra key-down
                // transition, and `SendInput` on the far side reproduces exactly that.
                if let PhysicalKey::Code(code) = event.physical_key {
                    self.forward_key(code, event.state);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if interrupt::is_set() {
            eprintln!("interrupted; closing");
            event_loop.exit();
            return;
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + INTERRUPT_POLL));
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(link) = &self.input {
            link.shutdown();
        }
        self.stats.flush();
    }
}
