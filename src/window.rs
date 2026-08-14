//! The session window: presents the surface store, captures input, knows no RDP.
//!
//! Three decisions worth stating up front, because each is load-bearing:
//!
//! - **The window never resizes the session.** `CLAUDE.md` is explicit: session
//!   resolution is decoupled from window size, and a display-configuration change must
//!   never reflow the remote desktop. So a resize changes only the [`Viewport`] — the
//!   letterboxed rectangle the session image is scaled into. Nothing here can send a
//!   resize; there is no path to the protocol from this module at all.
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

use std::num::NonZeroU32;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use crate::input::{self, InputEvent, PointerMap};
use crate::surface::SurfaceStore;

/// What the caller must decide before a window exists.
#[derive(Debug, Clone)]
pub struct WindowConfig {
    pub title: String,
    /// Session pixels. The window opens at this size and never forces it back.
    pub session_width: u16,
    pub session_height: u16,
}

impl WindowConfig {
    pub fn new(title: impl Into<String>, session_width: u16, session_height: u16) -> Self {
        WindowConfig {
            title: title.into(),
            session_width,
            session_height,
        }
    }
}

/// Messages a producer thread can push into the event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    /// The surface store may have changed. The loop checks the generation and only then
    /// asks for a redraw.
    Damaged,
    /// The session ended; close the window.
    Close,
}

/// A `Send` handle onto a running window, for the thread that owns the RDP session.
#[derive(Debug, Clone)]
pub struct Waker(EventLoopProxy<SessionEvent>);

impl Waker {
    /// Tell the window the store may have changed. `false` means the window is gone.
    pub fn damaged(&self) -> bool {
        self.0.send_event(SessionEvent::Damaged).is_ok()
    }

    /// Ask the window to close. `false` means it already has.
    pub fn close(&self) -> bool {
        self.0.send_event(SessionEvent::Close).is_ok()
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
    if !fits || !origin_inside || viewport.is_empty() || src.len() < needed {
        dst.fill(0);
        return;
    }

    let dest_right = (viewport.dest_x + viewport.dest_width).min(window_width);
    let dest_bottom = (viewport.dest_y + viewport.dest_height).min(window_height);

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

/// A window bound to a surface store, not yet running.
///
/// Split from `run` so the caller can take a [`Waker`] before the loop takes over the
/// thread — `run` never returns until the window closes.
pub struct SessionWindow {
    event_loop: EventLoop<SessionEvent>,
    config: WindowConfig,
    store: Arc<Mutex<SurfaceStore>>,
    input: Sender<InputEvent>,
}

impl SessionWindow {
    /// Build the event loop. **Must be called on the main thread** — winit requires it on
    /// macOS and Windows alike.
    pub fn new(
        config: WindowConfig,
        store: Arc<Mutex<SurfaceStore>>,
        input: Sender<InputEvent>,
    ) -> Result<Self, WindowError> {
        let event_loop = EventLoop::<SessionEvent>::with_user_event()
            .build()
            .map_err(|e| WindowError::EventLoop(e.to_string()))?;
        // Wait, not Poll: nothing here is animated, so the loop should sleep until the
        // OS or a producer has something to say.
        event_loop.set_control_flow(ControlFlow::Wait);
        Ok(SessionWindow {
            event_loop,
            config,
            store,
            input,
        })
    }

    /// A handle the session thread can use to nudge or close the window.
    pub fn waker(&self) -> Waker {
        Waker(self.event_loop.create_proxy())
    }

    /// Run until the window closes. Consumes the loop; returns on the main thread.
    pub fn run(self) -> Result<(), WindowError> {
        let SessionWindow {
            event_loop,
            config,
            store,
            input,
        } = self;
        let mut app = SessionApp::new(config, store, input);
        event_loop
            .run_app(&mut app)
            .map_err(|e| WindowError::EventLoop(e.to_string()))?;
        match app.failure.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

type SbContext = softbuffer::Context<Arc<Window>>;
type SbSurface = softbuffer::Surface<Arc<Window>, Arc<Window>>;

struct SessionApp {
    config: WindowConfig,
    store: Arc<Mutex<SurfaceStore>>,
    input: Sender<InputEvent>,
    window: Option<Arc<Window>>,
    // Held for as long as the surface: dropping the context invalidates it.
    _context: Option<SbContext>,
    surface: Option<SbSurface>,
    viewport: Viewport,
    /// Last pointer position, in session pixels. Buttons and scrolls need coordinates
    /// and winit does not repeat them.
    cursor: Option<(u16, u16)>,
    /// The store generation last put on screen. `None` until the first frame.
    presented: Option<u64>,
    failure: Option<WindowError>,
}

impl SessionApp {
    fn new(
        config: WindowConfig,
        store: Arc<Mutex<SurfaceStore>>,
        input: Sender<InputEvent>,
    ) -> Self {
        let viewport = Viewport::letterbox(
            u32::from(config.session_width),
            u32::from(config.session_height),
            config.session_width,
            config.session_height,
        );
        SessionApp {
            config,
            store,
            input,
            window: None,
            _context: None,
            surface: None,
            viewport,
            cursor: None,
            presented: None,
            failure: None,
        }
    }

    /// A closed receiver means the session is gone, so there is nothing left to show.
    fn send(&mut self, event_loop: &ActiveEventLoop, event: InputEvent) {
        if self.input.send(event).is_err() {
            event_loop.exit();
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

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let (Some(window), Some(surface)) = (self.window.clone(), self.surface.as_mut()) else {
            return;
        };
        let size = window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return; // Minimised. Nothing to draw into.
        };

        if let Err(e) = surface.resize(width, height) {
            self.fail(event_loop, WindowError::Present(e.to_string()));
            return;
        }
        let mut buffer = match surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => {
                self.fail(event_loop, WindowError::Present(e.to_string()));
                return;
            }
        };

        // The store lock is held only for the scale-and-convert pass, never across
        // `present` — presenting blocks on a copy on macOS, and the decoder thread must
        // not wait on that.
        let generation = {
            let store = self
                .store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let generation = store.generation();
            match store.output_surface() {
                Some(session) => {
                    self.viewport =
                        Viewport::letterbox(size.width, size.height, session.width, session.height);
                    present_into(
                        &mut buffer,
                        size.width,
                        size.height,
                        &self.viewport,
                        session.pixels(),
                    );
                }
                // Nothing mapped to output yet: black, not stale garbage.
                None => buffer.fill(0),
            }
            generation
        };

        window.pre_present_notify();
        if let Err(e) = buffer.present() {
            self.fail(event_loop, WindowError::Present(e.to_string()));
            return;
        }
        self.presented = Some(generation);
    }
}

impl ApplicationHandler<SessionEvent> for SessionApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // Resume can fire more than once; the window survives it.
        }

        let attributes = Window::default_attributes()
            .with_title(self.config.title.clone())
            .with_inner_size(PhysicalSize::new(
                u32::from(self.config.session_width),
                u32::from(self.config.session_height),
            ));

        let window = match event_loop.create_window(attributes) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.fail(event_loop, WindowError::Os(e.to_string()));
                return;
            }
        };

        let context = match softbuffer::Context::new(window.clone()) {
            Ok(c) => c,
            Err(e) => {
                self.fail(event_loop, WindowError::Os(e.to_string()));
                return;
            }
        };
        let surface = match softbuffer::Surface::new(&context, window.clone()) {
            Ok(s) => s,
            Err(e) => {
                self.fail(event_loop, WindowError::Os(e.to_string()));
                return;
            }
        };

        window.request_redraw();
        self.window = Some(window);
        self._context = Some(context);
        self.surface = Some(surface);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: SessionEvent) {
        match event {
            SessionEvent::Damaged => {
                // The whole point of the generation counter: a nudge that turns out to
                // change nothing costs one atomic-ish read and no frame.
                if self.presented != Some(self.generation())
                    && let Some(window) = &self.window
                {
                    window.request_redraw();
                }
            }
            SessionEvent::Close => event_loop.exit(),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => event_loop.exit(),

            WindowEvent::RedrawRequested => self.redraw(event_loop),

            // A resize changes the viewport and nothing else. The session keeps its
            // resolution — see the module note and CLAUDE.md.
            WindowEvent::Resized(size) => {
                self.viewport = Viewport::letterbox(
                    size.width,
                    size.height,
                    self.viewport.session_width,
                    self.viewport.session_height,
                );
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                // Synthetic events are the platform replaying key state across a focus
                // change. Forwarding them double-presses keys.
                if !is_synthetic && let Some(translated) = input::from_key_event(&event) {
                    self.send(event_loop, translated);
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
                    for translated in input::from_mouse_wheel(delta, at) {
                        self.send(event_loop, translated);
                    }
                }
            }

            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::Rect;

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
