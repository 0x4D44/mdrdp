//! Getting a finished frame onto the screen.
//!
//! Two backends behind one enum. The portable one is softbuffer, which is correct
//! everywhere but expensive on macOS: its CoreGraphics backend allocates a fresh
//! zeroed buffer every frame, wraps it in a DeviceRGB `CGImage`, and CoreAnimation
//! then re-renders the whole thing through a CPU colorspace conversion on every
//! present — ~22 ms of main-thread CPU per frame at 1440p, which multiplied by a
//! server streaming 30 fps burned most of a core in an idle session
//! (MDR-BUG-FLUX-00005).
//!
//! The macOS backend hands CoreAnimation an IOSurface instead: the compositor reads
//! it directly on the GPU, colorspace conversion included, so the CPU cost per frame
//! is one strided copy out of the staging buffer and nothing else. Softbuffer remains
//! the fallback if the IOSurface path cannot be built, and `MDRDP_PRESENT=soft`
//! forces it for A/B measurement and triage.
//!
//! The staging buffer persists across frames, but nothing stale can leak: every
//! frame is fully written by `present_into` (letterbox bars included) or blanked by
//! the no-surface branch before it is presented.

use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use winit::window::Window;

type SbContext = softbuffer::Context<Arc<Window>>;
type SbSurface = softbuffer::Surface<Arc<Window>, Arc<Window>>;

/// A window's presenter: yields a `u32` pixel buffer per frame and puts it on screen.
///
/// Pixel format is softbuffer's everywhere: `0x00RRGGBB`, one `u32` per window pixel,
/// top 8 bits zero. The IOSurface path forces the alpha byte on during its copy out.
pub enum Presenter {
    Soft {
        // Held for as long as the surface: dropping the context invalidates it.
        _context: SbContext,
        surface: SbSurface,
    },
    #[cfg(target_os = "macos")]
    Layer(macos::LayerPresenter),
}

impl Presenter {
    /// Build the best presenter this platform offers for `window`.
    ///
    /// Never fails just because the fast path is unavailable: the IOSurface layer is
    /// attempted first on macOS and softbuffer is the answer everywhere else, or when
    /// the layer cannot be built, or when `MDRDP_PRESENT=soft` asks for the comparison.
    pub fn new(window: Arc<Window>) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        {
            let forced_soft = std::env::var("MDRDP_PRESENT").is_ok_and(|v| v == "soft");
            if !forced_soft {
                match macos::LayerPresenter::new(&window) {
                    Ok(p) => return Ok(Self::Layer(p)),
                    Err(e) => {
                        eprintln!("present: IOSurface layer unavailable ({e}); using softbuffer")
                    }
                }
            }
        }
        let context = softbuffer::Context::new(window.clone()).map_err(|e| e.to_string())?;
        let surface = softbuffer::Surface::new(&context, window).map_err(|e| e.to_string())?;
        Ok(Self::Soft {
            _context: context,
            surface,
        })
    }

    /// Which backend is live, for the connect report and triage.
    pub fn backend(&self) -> &'static str {
        match self {
            Self::Soft { .. } => "softbuffer",
            #[cfg(target_os = "macos")]
            Self::Layer(_) => "iosurface",
        }
    }

    /// Match the frame buffer to the window's physical size.
    pub fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), String> {
        match self {
            Self::Soft { surface, .. } => surface.resize(width, height).map_err(|e| e.to_string()),
            #[cfg(target_os = "macos")]
            Self::Layer(layer) => {
                layer.resize(width.get(), height.get());
                Ok(())
            }
        }
    }

    /// Borrow this frame's pixel buffer. Present it with [`FrameBuf::present`].
    pub fn frame(&mut self) -> Result<FrameBuf<'_>, String> {
        match self {
            Self::Soft { surface, .. } => Ok(FrameBuf::Soft(
                surface.buffer_mut().map_err(|e| e.to_string())?,
            )),
            #[cfg(target_os = "macos")]
            Self::Layer(layer) => Ok(FrameBuf::Layer(layer)),
        }
    }
}

/// One frame's pixels, dereferencing to `[u32]` rows of the window's width.
pub enum FrameBuf<'a> {
    Soft(softbuffer::Buffer<'a, Arc<Window>, Arc<Window>>),
    #[cfg(target_os = "macos")]
    Layer(&'a mut macos::LayerPresenter),
}

/// Result of a successful presentation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentStatus {
    /// The frame was handed to the platform compositor.
    Presented,
    /// Every safe backing surface is still owned by the compositor; retry later.
    Busy,
}

impl FrameBuf<'_> {
    pub fn present(self) -> Result<PresentStatus, String> {
        match self {
            Self::Soft(buffer) => buffer
                .present()
                .map(|()| PresentStatus::Presented)
                .map_err(|e| e.to_string()),
            #[cfg(target_os = "macos")]
            Self::Layer(layer) => layer.present(),
        }
    }
}

impl Deref for FrameBuf<'_> {
    type Target = [u32];
    fn deref(&self) -> &[u32] {
        match self {
            Self::Soft(buffer) => buffer,
            #[cfg(target_os = "macos")]
            Self::Layer(layer) => layer.staging(),
        }
    }
}

impl DerefMut for FrameBuf<'_> {
    fn deref_mut(&mut self) -> &mut [u32] {
        match self {
            Self::Soft(buffer) => &mut *buffer,
            #[cfg(target_os = "macos")]
            Self::Layer(layer) => layer.staging_mut(),
        }
    }
}

/// Copy `height` rows of `width` `0x00RRGGBB` pixels into a strided BGRA byte
/// destination, forcing the alpha byte on.
///
/// Pure so the stride and alpha handling are testable without an IOSurface. The
/// destination stride is in BYTES (IOSurface rows are padded to an alignment the
/// caller does not choose); each row writes exactly `width * 4` bytes at its stride
/// offset and leaves the padding untouched. Little-endian `0x00RRGGBB` in memory is
/// B,G,R,X — the BGRA layout the surface was created with — so forcing the top byte
/// to 0xFF is the whole format conversion.
///
/// Out-of-bounds geometry blanks nothing and copies nothing: the caller sized both
/// buffers, so a mismatch is a bug upstream, and a short frame beats a panic on the
/// present path.
#[cfg(any(target_os = "macos", test))]
fn copy_rows_bgra(src: &[u32], width: usize, height: usize, dst: &mut [u8], dst_stride: usize) {
    let row_bytes = width * 4;
    if dst_stride < row_bytes {
        return;
    }
    for y in 0..height {
        let Some(s) = src.get(y * width..(y + 1) * width) else {
            return;
        };
        let Some(d) = dst.get_mut(y * dst_stride..y * dst_stride + row_bytes) else {
            return;
        };
        for (px, out) in s.iter().zip(d.chunks_exact_mut(4)) {
            out.copy_from_slice(&(px | 0xFF00_0000).to_le_bytes());
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn pick_surface_for_write(
    pool_len: usize,
    last: Option<usize>,
    mut is_in_use: impl FnMut(usize) -> bool,
) -> Option<usize> {
    (0..pool_len)
        .filter(|i| Some(*i) != last)
        .find(|i| !is_in_use(*i))
}

#[cfg(target_os = "macos")]
pub mod macos {
    use super::{PresentStatus, copy_rows_bgra, pick_surface_for_write};
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool};
    use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint};
    use objc2_foundation::NSObject;
    use objc2_io_surface::{
        IOSurfaceLockOptions, IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceHeight,
        kIOSurfacePixelFormat, kIOSurfaceWidth,
    };
    use objc2_quartz_core::{CALayer, CATransaction, kCAGravityTopLeft};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    /// 'BGRA' — the 32-bit little-endian layout `0x00RRGGBB` already is in memory.
    const PIXEL_FORMAT_BGRA: i32 = i32::from_be_bytes(*b"BGRA");

    /// Three surfaces: one on glass, one the compositor may still be reading, one
    /// free to write. `is_in_use` picks; a smaller pool stalls or tears.
    const POOL: usize = 3;

    /// Presents by handing CoreAnimation an IOSurface as the layer contents.
    ///
    /// Main-thread only, like the window that owns it: every method touches CALayer.
    pub struct LayerPresenter {
        /// Our sublayer of the view's root layer; its contents is the frame.
        layer: Retained<CALayer>,
        /// The view's own layer, read each present to keep frame and scale in step.
        root: Retained<CALayer>,
        /// The frame being drawn, `0x00RRGGBB`, `width * height` long. Persists
        /// across frames so there is no per-frame allocation.
        staging: Vec<u32>,
        width: u32,
        height: u32,
        /// Lazily (re)built when the size changes.
        surfaces: Vec<CFRetained<IOSurfaceRef>>,
        /// Index of the surface currently on glass, never picked for the next write.
        last: Option<usize>,
        /// Last geometry pushed to the layer, so an unchanged present sets nothing.
        synced: Option<((f64, f64), f64)>,
    }

    impl LayerPresenter {
        pub fn new(window: &Window) -> Result<Self, String> {
            let handle = window.window_handle().map_err(|e| e.to_string())?.as_raw();
            let RawWindowHandle::AppKit(handle) = handle else {
                return Err("not an AppKit window".to_owned());
            };
            // SAFETY: winit's `WindowHandle` guarantees a valid `NSView` pointer for
            // the lifetime of the borrow. `NSObject` + `msg_send` keeps objc2-app-kit
            // out of the tree, exactly as softbuffer's own backend does it.
            let view: &NSObject = unsafe { handle.ns_view.cast().as_ref() };
            let _: () = unsafe { msg_send![view, setWantsLayer: Bool::YES] };
            let root: Option<Retained<CALayer>> = unsafe { msg_send![view, layer] };
            let root = root.ok_or_else(|| "the view refused to become layer-backed".to_owned())?;

            let layer = CALayer::new();
            // Top-left origin to match the buffer's row order.
            layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
            layer.setGeometryFlipped(true);
            layer.setContentsGravity(unsafe { kCAGravityTopLeft });
            // The alpha byte is forced on during the copy, and telling the compositor
            // so lets it skip blending the desktop behind a desktop.
            layer.setOpaque(true);
            root.addSublayer(&layer);

            Ok(Self {
                layer,
                root,
                staging: Vec::new(),
                width: 0,
                height: 0,
                surfaces: Vec::new(),
                last: None,
                synced: None,
            })
        }

        pub fn resize(&mut self, width: u32, height: u32) {
            if (width, height) == (self.width, self.height) {
                return;
            }
            self.width = width;
            self.height = height;
            self.staging.clear();
            self.staging.resize((width as usize) * (height as usize), 0);
            // The pool is rebuilt at the new size on the next present; the old
            // surfaces stay alive under the compositor until then.
            self.surfaces.clear();
            self.last = None;
        }

        pub fn staging(&self) -> &[u32] {
            &self.staging
        }

        pub fn staging_mut(&mut self) -> &mut [u32] {
            &mut self.staging
        }

        /// Copy the staging frame into a free surface and put it on glass.
        pub fn present(&mut self) -> Result<PresentStatus, String> {
            if self.width == 0 || self.height == 0 {
                return Ok(PresentStatus::Presented); // Minimised; nothing to show it to.
            }
            if self.surfaces.is_empty() {
                self.surfaces = make_pool(self.width, self.height)?;
            }

            // Never rewrite the surface currently on glass: CoreAnimation may
            // short-circuit a `setContents` naming the object it already shows, so
            // in-place writes could silently stop updating the screen.
            let Some(pick) = pick_surface_for_write(self.surfaces.len(), self.last, |i| {
                self.surfaces[i].is_in_use()
            }) else {
                return Ok(PresentStatus::Busy);
            };
            {
                let surface = &self.surfaces[pick];
                // SAFETY: lock gives exclusive CPU access to the surface memory;
                // base_address/bytes_per_row are valid while it is held, and the slice
                // is bounded by rows * stride, which the allocation always covers.
                unsafe {
                    if surface.lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) != 0 {
                        return Err("IOSurfaceLock failed".to_owned());
                    }
                    let stride = surface.bytes_per_row();
                    let bytes = std::slice::from_raw_parts_mut(
                        surface.base_address().cast::<u8>().as_ptr(),
                        stride * (self.height as usize),
                    );
                    copy_rows_bgra(
                        &self.staging,
                        self.width as usize,
                        self.height as usize,
                        bytes,
                        stride,
                    );
                    surface.unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut());
                }
            }

            // One transaction: geometry (if it moved) and contents land together,
            // with the implicit quarter-second contents animation disabled.
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            self.sync_geometry();
            // An IOSurface is a CFType, and any CFType is a valid `id` for
            // `-[CALayer setContents:]` (the property is documented to accept one).
            let cf: &CFType = &self.surfaces[pick];
            let contents: &AnyObject = cf.as_ref();
            unsafe { self.layer.setContents(Some(contents)) };
            CATransaction::commit();
            self.last = Some(pick);
            Ok(PresentStatus::Presented)
        }

        /// Keep our sublayer covering the view and rendering at its scale.
        ///
        /// Polled at present time instead of observed by KVO: this presenter is only
        /// ever visible through what a present shows, so geometry from any moment
        /// between frames is indistinguishable from geometry read at the frame.
        fn sync_geometry(&mut self) {
            let bounds = self.root.bounds();
            let scale = self.root.contentsScale();
            let key = ((bounds.size.width, bounds.size.height), scale);
            if self.synced == Some(key) {
                return;
            }
            self.synced = Some(key);
            self.layer.setFrame(bounds);
            self.layer.setContentsScale(scale);
        }
    }

    impl Drop for LayerPresenter {
        fn drop(&mut self) {
            self.layer.removeFromSuperlayer();
        }
    }

    fn make_pool(width: u32, height: u32) -> Result<Vec<CFRetained<IOSurfaceRef>>, String> {
        (0..POOL)
            .map(|_| {
                // Stride is deliberately not requested: IOSurface pads rows to its own
                // alignment, and `copy_rows_bgra` honours whatever it chose.
                let keys: &[&CFString] = &[
                    unsafe { kIOSurfaceWidth },
                    unsafe { kIOSurfaceHeight },
                    unsafe { kIOSurfaceBytesPerElement },
                    unsafe { kIOSurfacePixelFormat },
                ];
                let width = CFNumber::new_i64(i64::from(width));
                let height = CFNumber::new_i64(i64::from(height));
                let bpe = CFNumber::new_i64(4);
                let format = CFNumber::new_i32(PIXEL_FORMAT_BGRA);
                let values: &[&CFNumber] = &[&width, &height, &bpe, &format];
                let properties = CFDictionary::from_slices(keys, values);
                // SAFETY: the dictionary holds exactly the documented creation keys.
                unsafe { IOSurfaceRef::new(properties.as_opaque()) }
                    .ok_or_else(|| format!("IOSurfaceCreate refused {width:?}x{height:?}"))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_land_at_the_destination_stride_not_the_width() {
        // The whole reason the copy exists: IOSurface rows are padded. Writing rows
        // at width*4 instead of the stride shears the image diagonally — every row
        // after the first lands a few pixels early.
        let src: Vec<u32> = vec![0x00AA_BBCC, 0x0011_2233, 0x0044_5566, 0x0077_8899];
        let mut dst = vec![0u8; 12 * 2]; // 2 rows of 2px, stride 12 (one px padding)
        copy_rows_bgra(&src, 2, 2, &mut dst, 12);
        // Row 0 at offset 0: BGRA of 0x00AABBCC is CC,BB,AA,FF.
        assert_eq!(&dst[0..4], &[0xCC, 0xBB, 0xAA, 0xFF]);
        assert_eq!(&dst[4..8], &[0x33, 0x22, 0x11, 0xFF]);
        assert_eq!(&dst[8..12], &[0, 0, 0, 0], "row padding stays untouched");
        // Row 1 starts at the STRIDE, not at width*4.
        assert_eq!(&dst[12..16], &[0x66, 0x55, 0x44, 0xFF]);
        assert_eq!(&dst[16..20], &[0x99, 0x88, 0x77, 0xFF]);
    }

    #[test]
    fn the_alpha_byte_is_forced_on() {
        // The pixel pipeline keeps the top byte zero (softbuffer's contract). An
        // IOSurface composited with alpha 0 is an invisible window — the desktop
        // shows through where the session should be.
        let src = vec![0u32; 1];
        let mut dst = vec![0u8; 4];
        copy_rows_bgra(&src, 1, 1, &mut dst, 4);
        assert_eq!(dst[3], 0xFF);
    }

    #[test]
    fn a_stride_shorter_than_a_row_copies_nothing_rather_than_shearing() {
        let src = vec![0xFFFF_FFFFu32; 4];
        let mut dst = vec![0u8; 16];
        copy_rows_bgra(&src, 2, 2, &mut dst, 4); // stride 4 < row 8
        assert!(dst.iter().all(|b| *b == 0));
    }

    #[test]
    fn a_short_source_stops_at_the_boundary_instead_of_panicking() {
        let src = vec![0u32; 3]; // one pixel short of 2x2
        let mut dst = vec![0xEEu8; 16];
        copy_rows_bgra(&src, 2, 2, &mut dst, 8);
        // Row 0 copied, row 1 left alone.
        assert_eq!(dst[3], 0xFF);
        assert_eq!(dst[8], 0xEE);
    }

    #[test]
    fn a_busy_surface_pool_never_selects_compositor_owned_memory() {
        let in_use = [true, true, true];
        assert_eq!(
            pick_surface_for_write(in_use.len(), Some(0), |i| in_use[i]),
            None
        );
    }
}
