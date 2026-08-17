//! The window: winit + softbuffer + `mdrdp::window::present_into`.
//!
//! Shaped after mdrdp's own presenter (`src/window.rs`, `SessionApp::redraw` at
//! line 1356): a decode thread paints into a shared slot, the window thread resizes
//! the softbuffer surface, runs `present_into` into its buffer and calls
//! `buffer.present()`. Same crates, same versions, same call order — which is what
//! makes a spike measurement and an mdrdp measurement comparable on the client side.
//!
//! Two deliberate differences, both because this is an instrument rather than a
//! client. The viewport is 1:1 (see [`crate::present`]), and there is no overlay: a
//! stats panel would repaint pixels inside the interval being measured.

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
use spike_server::input_proto::KeyKind;

use crate::clock::Clock;
use crate::input_link::InputLink;
use crate::interrupt;
use crate::keymap;
use crate::present::one_to_one;
use crate::sink::{Frame, FrameSlot};
use crate::stats::{FrameRecord, InputRecord, StatsLog};

/// The window before the first frame tells us the stream's coded size.
const PLACEHOLDER: PhysicalSize<u32> = PhysicalSize::new(640, 360);

/// How often the loop wakes to check the interrupt flag. It paints nothing — a redraw
/// only happens when a frame arrives or the window is resized — so this costs one
/// no-op iteration every 200 ms and buys a Ctrl-C that closes the sockets and flushes
/// the stats file instead of killing the process mid-line.
const INTERRUPT_POLL: Duration = Duration::from_millis(200);

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
        }
    }

    fn title_for(&self, size: Option<(u32, u32)>) -> String {
        match size {
            Some((w, h)) => format!("{} — {w}x{h}", self.title),
            None => format!("{} — waiting for stream", self.title),
        }
    }

    /// Forward one key transition, and record that we did.
    fn forward_key(&self, code: winit::keyboard::KeyCode, state: ElementState) {
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
        match link.send(kind, vk) {
            Ok(seq) => self
                .stats
                .record(&InputRecord::new(seq, vk, kind.as_str(), sent_us)),
            Err(e) => eprintln!("input: send failed: {e}"),
        }
    }

    fn redraw(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };

        // Consume the newest decoded frame, if one arrived. Its stats line is closed
        // below, after the present; a frame that was displaced before we got here was
        // already recorded as dropped by the decode thread.
        let fresh = self.slot.take();
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

        let mut presentable = false;
        match self.current.as_ref() {
            Some(frame) => match one_to_one(size.width, size.height, frame.width, frame.height) {
                Some(viewport) => {
                    present_into(&mut buffer, size.width, size.height, &viewport, &frame.rgba);
                    presentable = true;
                }
                None => {
                    if !self.warned_geometry {
                        self.warned_geometry = true;
                        eprintln!(
                            "present: {}x{} cannot be presented; showing black",
                            frame.width, frame.height
                        );
                    }
                    buffer.fill(0);
                }
            },
            // Nothing decoded yet: black, not whatever the buffer last held.
            None => buffer.fill(0),
        }

        window.pre_present_notify();
        if let Err(e) = buffer.present() {
            eprintln!("present: present failed: {e}");
            return;
        }
        let present_done_us = self.clock.now_us();

        // One frame record per frame, written where its last stage actually completes.
        if let Some(frame) = self.current.as_mut() {
            if presentable && frame.stamps_pending {
                frame.stamps_pending = false;
                self.stats
                    .record(&FrameRecord::new(&frame.stamps, Some(present_done_us)));
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
            UserEvent::Frame => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
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
