//! Auxiliary egui windows on an existing winit event loop.
//!
//! The session process already owns its one `EventLoop` (`window.rs`), so eframe
//! cannot host the diagnostics windows there — this module can (2026-08-16 HLD,
//! decision 2). Each [`AuxWindow`] is a winit window with its own glutin GL context,
//! `egui_glow` painter, and `egui-winit` state; the owning `ApplicationHandler` routes
//! `WindowEvent`s by id and calls [`AuxWindow::redraw`] with the screen's UI closure.
//!
//! Nothing here knows what the windows show. Content stays with the caller, so the
//! diagnostics screens are testable pure functions over their stats snapshots.

use std::num::NonZeroU32;
use std::sync::Arc;

use egui_glow::EguiGlow;
use egui_glow::glow;
use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextAttributesBuilder, PossiblyCurrentContext};
use glutin::display::GetGlDisplay as _;
use glutin::prelude::*;
use glutin::surface::{Surface, SurfaceAttributesBuilder, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow as _};
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes, WindowId};

/// One egui-rendered auxiliary window.
pub struct AuxWindow {
    window: Arc<Window>,
    gl_context: PossiblyCurrentContext,
    gl_surface: Surface<WindowSurface>,
    egui_glow: EguiGlow,
}

impl AuxWindow {
    /// Open a fixed-size aux window on the running loop.
    ///
    /// GL context creation is fallible on principle (a headless or exhausted GPU
    /// environment), and a diagnostics window that cannot open must never take the
    /// session down — callers report the error and carry on.
    pub fn open(
        event_loop: &ActiveEventLoop,
        title: &str,
        size: [f32; 2],
    ) -> Result<AuxWindow, String> {
        let attributes = WindowAttributes::default()
            .with_title(title)
            .with_inner_size(winit::dpi::LogicalSize::new(size[0], size[1]))
            .with_resizable(false);

        let template = ConfigTemplateBuilder::new().with_alpha_size(8);
        let (window, gl_config) = DisplayBuilder::new()
            .with_window_attributes(Some(attributes))
            .build(event_loop, template, |mut configs| {
                configs.next().expect("at least one GL config")
            })
            .map_err(|e| format!("GL display: {e}"))?;
        let window = Arc::new(window.ok_or("GL display built without a window")?);

        let context_attributes = ContextAttributesBuilder::new().build(None);
        let not_current = unsafe {
            gl_config
                .display()
                .create_context(&gl_config, &context_attributes)
                .map_err(|e| format!("GL context: {e}"))?
        };
        let surface_attributes = window
            .build_surface_attributes(SurfaceAttributesBuilder::new())
            .map_err(|e| format!("GL surface attributes: {e}"))?;
        let gl_surface = unsafe {
            gl_config
                .display()
                .create_window_surface(&gl_config, &surface_attributes)
                .map_err(|e| format!("GL surface: {e}"))?
        };
        let gl_context = not_current
            .make_current(&gl_surface)
            .map_err(|e| format!("GL make_current: {e}"))?;
        let gl = unsafe {
            glow::Context::from_loader_function_cstr(|s| {
                gl_config.display().get_proc_address(s).cast()
            })
        };

        let egui_glow = EguiGlow::new(event_loop, Arc::new(gl), None, None, true);
        crate::ui::theme::apply(&egui_glow.egui_ctx);

        window.request_redraw();
        Ok(AuxWindow {
            window,
            gl_context,
            gl_surface,
            egui_glow,
        })
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    pub fn request_redraw(&self) {
        self.window.request_redraw();
    }

    /// Feed a winit event for this window. Returns whether egui wants a repaint.
    pub fn on_window_event(&mut self, event: &WindowEvent) -> bool {
        if let WindowEvent::Resized(size) = event
            && let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        {
            let _ = self.gl_context.make_current(&self.gl_surface);
            self.gl_surface.resize(&self.gl_context, w, h);
        }
        let response = self.egui_glow.on_window_event(&self.window, event);
        if response.repaint {
            self.window.request_redraw();
        }
        response.repaint
    }

    /// Run the UI closure and present the frame.
    pub fn redraw(&mut self, run_ui: impl FnMut(&mut egui::Ui)) {
        if self.gl_context.make_current(&self.gl_surface).is_err() {
            return; // Surface lost; the next event or close will sort it out.
        }
        self.egui_glow.run(&self.window, run_ui);
        let bg = crate::ui::theme::BG_WINDOW;
        unsafe {
            use glow::HasContext as _;
            let gl = self.egui_glow.painter.gl();
            gl.clear_color(
                f32::from(bg.r()) / 255.0,
                f32::from(bg.g()) / 255.0,
                f32::from(bg.b()) / 255.0,
                1.0,
            );
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        self.egui_glow.paint(&self.window);
        let _ = self.gl_surface.swap_buffers(&self.gl_context);
    }
}

impl Drop for AuxWindow {
    fn drop(&mut self) {
        let _ = self.gl_context.make_current(&self.gl_surface);
        self.egui_glow.destroy();
    }
}
