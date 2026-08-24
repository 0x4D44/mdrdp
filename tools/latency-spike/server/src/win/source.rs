//! Stage 1, abstracted — where a captured desktop frame comes from.
//!
//! Two sources exist, and the pipeline above must not know which one it has:
//!
//! * [`super::dxgi::Capture`] — DXGI Desktop Duplication. Always available, and
//!   the default, because it works against any output on any host.
//! * [`super::idd_source::IddSource`] — the IddCx driver's shared texture pool.
//!   Removes duplication's ~7.5 ms present→acquire gap by reading the buffer the
//!   driver already holds, at the cost of only working against our own virtual
//!   display (HLD §6, decision 7).
//!
//! ## What the trait carries, and why nothing more
//!
//! The surface is exactly what `pipeline::run` and `pipeline::capture_loop`
//! already ask of `dxgi::Capture`, and it is small for a reason: every method here
//! is one the IDD source has to answer for real.
//!
//! * **The device and context are the source's.** The IDD consumer builds its
//!   D3D11 device on the *section's* `render_adapter_luid`, so it is not our
//!   choice which adapter the pipeline runs on — it is the driver's. The
//!   converter, the encoder's `IMFDXGIDeviceManager` and the rect readback all
//!   have to be built on whichever device the frames actually live on, or every
//!   copy becomes a cross-adapter transfer through system memory.
//! * **Geometry and names** feed the stats header, which is what makes an
//!   archived measurement say where it came from.
//! * **[`FrameSource::acquire`]** returns [`Acquired`] — a texture with a
//!   present-QPC stamp and optional coverage metadata, or a reason there is no
//!   frame. `present_qpc` stays in raw QPC ticks rather than microseconds because
//!   the pipeline converts every stamp once, at emission, through the one
//!   [`crate::stats::QpcClock`]; handing it microseconds here would mean two
//!   conversions of the same number on the same timebase.
//! * **[`FrameSource::read_rects`]** is the Increment 1 fast path's readback. It
//!   belongs on the source because it must run on the source's device and against
//!   a texture only the source knows the lifetime of.
//!
//! Both sources share [`RectReadback`] for the readback itself: the staging
//! surface, the per-rect `CopySubresourceRegion` and the packing are identical
//! once the device is a parameter, and duplicating them would mean two places to
//! get the row-pitch arithmetic wrong.

use super::Result;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX, D3D11_CPU_ACCESS_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

/// One changed region of a frame, in desktop coordinates, clamped to the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// One compositor-provided screen-to-screen copy, in desktop coordinates.
///
/// Sources refer to the preceding captured canvas. Every move source must therefore
/// be staged before any destination is written; dirty final pixels then land after
/// all moves, matching Desktop Duplication's metadata ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveRect {
    pub src_x: u32,
    pub src_y: u32,
    pub dst_x: u32,
    pub dst_y: u32,
    pub w: u32,
    pub h: u32,
}

/// Per-frame change metadata from whichever source produced the frame.
///
/// `None` at the [`FrameSource::acquire`] call site means **unavailable** — the
/// frame carried no metadata, a metadata call failed, or (IDD source) the coverage
/// list does not describe everything this consumer has missed. The fast-path
/// predicate must treat that as "assume everything changed", never as "zero rects".
///
/// `rects` contains final pixels which must be read from the current texture. `moves`
/// contains replayable copies from the preceding canvas; their destinations are not
/// duplicated in `rects`. IDD layout v2 supplies that ordered current-frame pair
/// only across an adjacent consumed frame; its accumulated fallback remains final
/// pixels and therefore has no moves.
#[derive(Debug, Clone, Default)]
pub struct ChangeInfo {
    pub rects: Vec<DirtyRect>,
    pub moves: Vec<MoveRect>,
}

impl ChangeInfo {
    /// Total bytes a raw BGRA readback of every rect would carry.
    pub fn dirty_bytes(&self) -> u64 {
        self.rects
            .iter()
            .map(|r| u64::from(r.w) * u64::from(r.h) * 4)
            .sum()
    }
}

/// What one [`FrameSource::acquire`] call produced.
pub enum Acquired {
    Frame {
        texture: ID3D11Texture2D,
        /// When the compositor presented it — i.e. before we heard about it.
        /// `DXGI_OUTDUPL_FRAME_INFO::LastPresentTime` for duplication, the slot
        /// record's `present_qpc` for the IDD pool. QPC ticks either way.
        present_qpc: i64,
        /// When the source handed it to us.
        acquire_qpc: i64,
        /// Change metadata, when this frame carried a trustworthy set.
        change: Option<ChangeInfo>,
    },
    /// Nothing was presented within the timeout.
    Timeout,
    /// Only the mouse pointer moved; the desktop image is unchanged.
    PointerOnly,
    /// The source was lost and has been rebuilt: a lost duplication, or an IDD
    /// pool whose generation moved under us. The next frame is a fresh start, so
    /// the caller asks the encoder for a keyframe and suppresses one frame's rects
    /// (HLD decision 15 — the rebuilt source's metadata describes change against a
    /// baseline the viewer never saw).
    Recreated,
}

/// Where the pipeline's frames come from.
pub trait FrameSource {
    /// The device every frame this source hands out lives on. The converter and
    /// the encoder's device manager are built from it.
    fn device(&self) -> &ID3D11Device;
    /// The immediate context of [`FrameSource::device`].
    fn context(&self) -> &ID3D11DeviceContext;
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    /// Adapter description, for the stats header.
    fn adapter(&self) -> &str;
    /// Where this source's captured display sits in the virtual desktop, in
    /// physical pixels — `DXGI_OUTPUT_DESC::DesktopCoordinates`'s top-left corner
    /// for duplication (HLD tranche 3 §5.2). The mouse-move injector adds this to
    /// the wire's display-local `x, y` before mapping into
    /// `SendInput`'s virtual-desktop absolute space
    /// ([`crate::input_proto::map_to_virtual_desk`]).
    ///
    /// Defaults to `(0, 0)`: the IDD source's shared-pool header carries no desktop
    /// coordinates, so a moved IDD virtual display needs revisiting when that
    /// becomes reachable.
    fn origin(&self) -> (i32, i32) {
        (0, 0)
    }
    /// What this source is capturing, for the stats header and the startup line:
    /// a `\\.\DISPLAY5` for duplication, the pool generation for the IDD source.
    fn output_name(&self) -> &str;
    /// `dxgi` or `idd` — recorded in the stats header so an archived run names the
    /// path it was measured on.
    fn kind(&self) -> &'static str;
    /// Whether the viewer should hide its immediate local pointer. Sources that
    /// do not publish cursor state keep the platform cursor visible.
    fn hide_local_cursor(&self) -> bool {
        false
    }
    /// Wait up to `timeout_ms` for the next frame.
    fn acquire(&mut self, timeout_ms: u32) -> Result<Acquired>;
    /// Read the pixels behind `change`'s rects back to the CPU, tightly packed.
    ///
    /// **Validity window: the frame must still be current.** `texture` is the one
    /// the last [`FrameSource::acquire`] returned, and it stays readable only until
    /// the next one. The pipeline calls this inside that window, before the frame
    /// enters the converter — which is also where the latency win is.
    ///
    /// Each returned [`crate::rects::Rect`] carries exactly `w * h * 4` bytes of
    /// BGRA, `w * 4` per row, top-down: `rects::encode`'s pixel contract.
    fn read_rects(
        &mut self,
        texture: &ID3D11Texture2D,
        change: &ChangeInfo,
    ) -> Result<Vec<crate::rects::Rect>>;
}

/// The GPU→CPU readback both sources use for the raw dirty-rect fast path.
///
/// One full-desktop staging texture, created on first use. Full desktop size
/// rather than per-rect: a staging texture sized to the rect would have to be
/// recreated whenever a rect grew, and creating a texture is far more expensive
/// than copying into a corner of an existing one. Copying at the rect's own
/// desktop coordinates then keeps source and destination coordinates identical, so
/// there is no offset arithmetic to get wrong.
///
/// Lazy because a run that never takes the fast path (`--no-rects`, a desktop too
/// large for the wire's `u16` coordinates, a driver that reports full-frame dirty)
/// should not pay for an 8 MB surface it will never map.
pub struct RectReadback {
    width: u32,
    height: u32,
    staging: Option<ID3D11Texture2D>,
}

impl RectReadback {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            staging: None,
        }
    }

    fn ensure_staging(&mut self, device: &ID3D11Device) -> Result<()> {
        if self.staging.is_some() {
            return Ok(());
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: self.width,
            Height: self.height,
            MipLevels: 1,
            ArraySize: 1,
            // Both sources hand over BGRA8; a staging copy must match its source's
            // format exactly or `CopySubresourceRegion` refuses it.
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            // A staging resource is bindable to no pipeline stage at all.
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: `desc` is fully initialised; the initial-data pointer is None
        // because the surface is filled by a copy, not by us.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }?;
        self.staging = Some(texture.ok_or("CreateTexture2D returned no staging texture")?);
        Ok(())
    }

    /// See [`FrameSource::read_rects`] — this is its body, with the device and the
    /// context as parameters so both sources can share it.
    ///
    /// Wire coordinates are `u16`, so the caller must have established that the
    /// desktop fits (the pipeline checks once at startup); the `debug_assert`
    /// below states that contract rather than re-deriving it per frame.
    pub fn read(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        texture: &ID3D11Texture2D,
        change: &ChangeInfo,
    ) -> Result<Vec<crate::rects::Rect>> {
        if change.rects.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_staging(device)?;
        let staging = self.staging.as_ref().ok_or("staging texture missing")?;

        for r in &change.rects {
            // Checked before the Map below, where an unwinding assert would leak
            // the mapping.
            debug_assert!(
                r.x + r.w <= u16::MAX as u32 && r.y + r.h <= u16::MAX as u32,
                "rect {r:?} does not fit the u16 wire fields; the caller gates on desktop size"
            );
            let region = D3D11_BOX {
                left: r.x,
                top: r.y,
                front: 0,
                right: r.x + r.w,
                bottom: r.y + r.h,
                back: 1,
            };
            // SAFETY: both textures are live, share the BGRA8 format and the
            // desktop size, and `region` is inside both — the source clamped the
            // rect against exactly these dimensions. The copy is same-coordinate,
            // so the destination cannot overrun either.
            unsafe {
                context.CopySubresourceRegion(
                    staging,
                    0,
                    r.x,
                    r.y,
                    0,
                    texture,
                    0,
                    Some(&region as *const D3D11_BOX),
                );
            }
        }

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: `staging` is live, subresource 0 is its only one (MipLevels and
        // ArraySize are both 1), and `mapped` is a live local for the call.
        unsafe { context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }?;

        // Nothing between here and the `Unmap` can fail or return early: the copy
        // loop only allocates and memcpys. That is what keeps the mapping from
        // leaking without a guard type — a leaked map wedges the device.
        let pitch = mapped.RowPitch as usize;
        let mut out = Vec::with_capacity(change.rects.len());
        for r in &change.rects {
            let row_bytes = r.w as usize * 4;
            let mut pixels = vec![0u8; row_bytes * r.h as usize];
            for row in 0..r.h as usize {
                let src_offset = (r.y as usize + row) * pitch + r.x as usize * 4;
                // SAFETY: the mapping covers `height` rows of `RowPitch` bytes and
                // the rect is inside the desktop, so `src_offset .. + row_bytes` is
                // inside it too. The slice is read and dropped before `Unmap`.
                let src = unsafe {
                    std::slice::from_raw_parts(
                        (mapped.pData as *const u8).add(src_offset),
                        row_bytes,
                    )
                };
                pixels[row * row_bytes..(row + 1) * row_bytes].copy_from_slice(src);
            }
            out.push(crate::rects::Rect {
                x: r.x as u16,
                y: r.y as u16,
                w: r.w as u16,
                h: r.h as u16,
                pixels,
            });
        }

        // SAFETY: exactly one `Unmap` for the `Map` above, on the same subresource.
        unsafe { context.Unmap(staging, 0) };
        Ok(out)
    }
}
