//! Stage 1 — DXGI Desktop Duplication.
//!
//! The contract, in the order the code follows it:
//!
//! 1. `CreateDXGIFactory1` → `EnumAdapters1` → `EnumOutputs` to find the display.
//!    `--list-outputs` prints this list; `--output N` indexes it.
//! 2. `D3D11CreateDevice` on **that adapter** (driver type must be `UNKNOWN` when an
//!    adapter is named) with `VIDEO_SUPPORT`, because the same device also drives the
//!    colour conversion and is handed to the hardware encoder.
//! 3. `IDXGIOutput1::DuplicateOutput` for the duplication interface.
//! 4. `AcquireNextFrame` in a loop with a short timeout. Three outcomes matter:
//!    * `DXGI_ERROR_WAIT_TIMEOUT` — nothing was presented. Not an error.
//!    * `LastPresentTime == 0` — a pointer-only update. There is no new desktop
//!      image, so encoding it would burn a frame on nothing.
//!    * `DXGI_ERROR_ACCESS_LOST` — the desktop switched (secure desktop, session
//!      change, mode change, or the IDD driver reconfiguring). Recoverable: release
//!      the duplication and make a new one. The HLD calls this out as *the* expected
//!      interruption, not an exceptional one.
//! 5. `ReleaseFrame` before the next `AcquireNextFrame`. Holding two is an error and
//!    holding one blocks the compositor, so the frame is released at the top of the
//!    next acquire rather than at some later convenient point.

use super::{qpc, wide_to_string, Result};
use std::time::Duration;
use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication,
    IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_UNSUPPORTED,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_MOVE_RECT, DXGI_OUTPUT_DESC,
};

/// `DXGI_ERROR_UNAVAILABLE` — another process already holds the duplication, or the
/// desktop is mid-switch. Transient, so it is retried rather than reported.
const DXGI_ERROR_UNAVAILABLE: windows::core::HRESULT =
    windows::core::HRESULT(0x887A_0022_u32 as i32);

/// One display, as `--list-outputs` reports it.
#[derive(Debug, Clone)]
pub struct OutputInfo {
    /// Flat index across all adapters — this is what `--output` takes.
    pub index: usize,
    pub adapter_index: u32,
    pub output_index: u32,
    pub adapter: String,
    pub device_name: String,
    pub width: u32,
    pub height: u32,
    pub attached: bool,
    pub rotation: &'static str,
}

fn rotation_name(r: i32) -> &'static str {
    match r {
        1 => "identity",
        2 => "90",
        3 => "180",
        4 => "270",
        _ => "unspecified",
    }
}

fn describe(desc: &DXGI_OUTPUT_DESC) -> (String, u32, u32, bool, &'static str) {
    let rect = desc.DesktopCoordinates;
    (
        wide_to_string(&desc.DeviceName),
        (rect.right - rect.left).max(0) as u32,
        (rect.bottom - rect.top).max(0) as u32,
        desc.AttachedToDesktop.as_bool(),
        rotation_name(desc.Rotation.0),
    )
}

/// Enumerate every output on every adapter, in a stable order.
pub fn enumerate() -> Result<Vec<OutputInfo>> {
    // SAFETY: the factory call writes only through the out-pointer windows-rs owns.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
    let mut list = Vec::new();
    let mut adapter_index = 0u32;
    loop {
        // SAFETY: `factory` is live. DXGI_ERROR_NOT_FOUND is how DXGI ends an
        // enumeration, so it is the loop's terminating condition, not a failure.
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(e.into()),
        };
        // SAFETY: `adapter` is a live COM interface.
        let adapter_desc = unsafe { adapter.GetDesc1() }?;
        let adapter_name = wide_to_string(&adapter_desc.Description);

        let mut output_index = 0u32;
        loop {
            // SAFETY: as above.
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(o) => o,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            };
            // SAFETY: `output` is a live COM interface.
            let desc = unsafe { output.GetDesc() }?;
            let (device_name, width, height, attached, rotation) = describe(&desc);
            list.push(OutputInfo {
                index: list.len(),
                adapter_index,
                output_index,
                adapter: adapter_name.clone(),
                device_name,
                width,
                height,
                attached,
                rotation,
            });
            output_index += 1;
        }
        adapter_index += 1;
    }
    Ok(list)
}

/// One changed region of a frame, in desktop coordinates, clamped to the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Per-frame change metadata from the duplication.
///
/// `None` at the `acquire` call site means **unavailable** — the frame carried
/// no metadata, or a metadata call failed — which the fast-path predicate must
/// treat as "assume everything changed", never as "zero rects".
///
/// Accumulated frames (`AccumulatedFrames > 1`) are fine, deliberately: this
/// scheme never replays moves — it reads the **final pixels** of the current
/// frame at every covered rectangle — so what it needs from the metadata is
/// coverage, not a replayable sequence. Any pixel that changed across the
/// accumulated presents is inside some accumulated dirty rect or some move's
/// destination, and the union of those is exactly what this returns (move rects
/// contribute their *destination* rectangles). Order never matters to coverage.
/// The first cut gated on `AccumulatedFrames == 1`; the §5a telemetry showed a
/// keystroke on the 240 Hz IDD is a ~100 ms burst of presents the ~8 ms capture
/// loop cannot drain one-by-one, so that gate starved the fast path to a 4-in-49
/// hit rate while the accumulated union stayed small and correct.
#[derive(Debug, Clone, Default)]
pub struct ChangeInfo {
    pub rects: Vec<DirtyRect>,
    /// How many of `rects` came from move regions (diagnostic).
    pub move_rects: u32,
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

/// What one `acquire` call produced.
pub enum Acquired {
    Frame {
        texture: ID3D11Texture2D,
        /// `DXGI_OUTDUPL_FRAME_INFO::LastPresentTime` — when the compositor
        /// presented it, i.e. before we heard about it.
        present_qpc: i64,
        /// When `AcquireNextFrame` returned to us.
        acquire_qpc: i64,
        /// Change metadata, when this frame carried a trustworthy set.
        change: Option<ChangeInfo>,
    },
    /// Nothing was presented within the timeout.
    Timeout,
    /// Only the mouse pointer moved; the desktop image is unchanged.
    PointerOnly,
    /// The duplication was lost and has been rebuilt. The next frame is a fresh
    /// start, so the caller should ask the encoder for a keyframe.
    Recreated,
}

/// The capture stage: a D3D11 device plus the duplication interface for one output.
pub struct Capture {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub width: u32,
    pub height: u32,
    pub adapter: String,
    pub device_name: String,
    output: IDXGIOutput1,
    /// `None` only while a lost duplication is being replaced. It must be dropped
    /// before `DuplicateOutput` is called again — DXGI refuses a second duplication
    /// of an output that already has one outstanding.
    dupl: Option<IDXGIOutputDuplication>,
    /// True while a frame is checked out and owes a `ReleaseFrame`.
    holding: bool,
    /// Full-desktop CPU-readable copy target for [`Capture::read_rects`], created on
    /// first use. Lazy because a run that never takes the rect fast path (a large
    /// desktop, `--no-rects`, a driver that reports full-frame dirty) should not pay
    /// for an 8 MB staging surface it will never map.
    staging: Option<ID3D11Texture2D>,
}

impl Capture {
    /// Open the output named by `info`, creating the D3D11 device on its adapter.
    pub fn open(info: &OutputInfo) -> Result<Self> {
        // SAFETY: every handle below is used only while live, and every
        // out-parameter is owned by windows-rs.
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
        let adapter: IDXGIAdapter1 = unsafe { factory.EnumAdapters1(info.adapter_index) }?;
        let output = unsafe { adapter.EnumOutputs(info.output_index) }?;
        let output: IDXGIOutput1 = output.cast()?;
        // SAFETY: `output` is a live COM interface.
        let desc = unsafe { output.GetDesc() }?;
        let (device_name, width, height, attached, _) = describe(&desc);
        if !attached {
            return Err(format!(
                "output {} ({device_name}) is not attached to the desktop; \
                 Desktop Duplication has nothing to duplicate",
                info.index
            )
            .into());
        }
        if width == 0 || height == 0 {
            return Err(format!("output {} reports a {width}x{height} desktop", info.index).into());
        }
        if width % 2 != 0 || height % 2 != 0 {
            return Err(format!(
                "output {} is {width}x{height}; NV12 needs even dimensions in both axes",
                info.index
            )
            .into());
        }

        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let levels = [D3D_FEATURE_LEVEL_11_0];
        // SAFETY: all out-parameters are live `Option`s that windows-rs fills.
        // D3D_DRIVER_TYPE_UNKNOWN is mandatory when an adapter is supplied.
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }?;
        let device = device.ok_or("D3D11CreateDevice returned no device")?;
        let context = context.ok_or("D3D11CreateDevice returned no context")?;

        // A hardware MFT runs its own threads against this device. Without
        // multithread protection the driver is free to corrupt state under us, and
        // the symptom is an intermittent hang rather than an error.
        if let Ok(mt) = device.cast::<ID3D11Multithread>() {
            // SAFETY: `mt` is a live interface on our own device.
            let _ = unsafe { mt.SetMultithreadProtected(true) };
        }

        let dupl = Self::duplicate(&output, &device)?;
        Ok(Self {
            device,
            context,
            width,
            height,
            adapter: info.adapter.clone(),
            device_name,
            output,
            dupl: Some(dupl),
            holding: false,
            staging: None,
        })
    }

    /// `DuplicateOutput` with a bounded retry on the transient "someone else has it"
    /// error, which is what a desktop switch looks like from here for a few hundred
    /// milliseconds.
    fn duplicate(output: &IDXGIOutput1, device: &ID3D11Device) -> Result<IDXGIOutputDuplication> {
        let mut last = None;
        for attempt in 0..20u32 {
            // SAFETY: both arguments are live interfaces.
            match unsafe { output.DuplicateOutput(device) } {
                Ok(d) => return Ok(d),
                Err(e) if e.code() == DXGI_ERROR_UNAVAILABLE => {
                    last = Some(e);
                    std::thread::sleep(Duration::from_millis(50 * (attempt + 1).min(4) as u64));
                }
                Err(e) if e.code() == DXGI_ERROR_UNSUPPORTED => {
                    return Err(format!(
                        "DuplicateOutput: DXGI_ERROR_UNSUPPORTED. The output is driven by an \
                         adapter this device cannot duplicate (a hybrid-graphics or \
                         indirect-display mismatch). {e}"
                    )
                    .into());
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(format!(
            "DuplicateOutput stayed DXGI_ERROR_UNAVAILABLE across 20 attempts: {}",
            last.map(|e| e.to_string()).unwrap_or_default()
        )
        .into())
    }

    fn release_held(&mut self) -> Result<()> {
        if !self.holding {
            return Ok(());
        }
        self.holding = false;
        let Some(dupl) = self.dupl.as_ref() else {
            return Ok(());
        };
        // SAFETY: `dupl` is live and we are holding exactly one frame.
        match unsafe { dupl.ReleaseFrame() } {
            Ok(()) => Ok(()),
            // Losing access while holding a frame is the same recoverable event.
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Acquire the next desktop frame, or say why there isn't one.
    pub fn acquire(&mut self, timeout_ms: u32) -> Result<Acquired> {
        self.release_held()?;
        let dupl = self
            .dupl
            .as_ref()
            .ok_or("duplication interface missing")?
            .clone();

        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: both out-parameters are live locals for the duration of the call.
        let outcome = unsafe { dupl.AcquireNextFrame(timeout_ms, &mut info, &mut resource) };
        let acquire_qpc = qpc::now();

        match outcome {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(Acquired::Timeout),
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                drop(dupl);
                self.rebuild()?;
                return Ok(Acquired::Recreated);
            }
            Err(e) => return Err(e.into()),
        }
        self.holding = true;

        // A zero present time means the only thing that changed was the pointer.
        // Encoding it would spend a frame's budget re-sending an identical image.
        if info.LastPresentTime == 0 {
            return Ok(Acquired::PointerOnly);
        }
        let Some(resource) = resource else {
            return Ok(Acquired::PointerOnly);
        };
        let change = self.read_change_info(&dupl, &info);
        let texture: ID3D11Texture2D = resource.cast()?;
        Ok(Acquired::Frame {
            texture,
            present_qpc: info.LastPresentTime,
            acquire_qpc,
            change,
        })
    }

    /// Fetch the frame's dirty/move rects while it is still held. `None` means the
    /// metadata is unavailable: the frame carried none, or a metadata call failed.
    /// Accumulated frames are accepted — the union is coverage, and coverage is all
    /// this scheme needs (see [`ChangeInfo`]); the buffers below are sized to
    /// `TotalMetadataBufferSize`, which spans the whole accumulated set.
    fn read_change_info(
        &self,
        dupl: &IDXGIOutputDuplication,
        info: &DXGI_OUTDUPL_FRAME_INFO,
    ) -> Option<ChangeInfo> {
        if info.TotalMetadataBufferSize == 0 {
            return None;
        }
        let capacity = info.TotalMetadataBufferSize as usize;

        let mut dirty: Vec<RECT> = vec![RECT::default(); capacity / std::mem::size_of::<RECT>()];
        let mut dirty_bytes_required = 0u32;
        // SAFETY: the buffer is `capacity` bytes of RECTs and we pass that size; the
        // frame is held (this runs between AcquireNextFrame and ReleaseFrame), which
        // is the API's validity window for metadata.
        unsafe {
            dupl.GetFrameDirtyRects(
                (dirty.len() * std::mem::size_of::<RECT>()) as u32,
                dirty.as_mut_ptr(),
                &mut dirty_bytes_required,
            )
        }
        .ok()?;
        dirty.truncate(dirty_bytes_required as usize / std::mem::size_of::<RECT>());

        let mut moves: Vec<DXGI_OUTDUPL_MOVE_RECT> =
            vec![
                DXGI_OUTDUPL_MOVE_RECT::default();
                capacity / std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>()
            ];
        let mut move_bytes_required = 0u32;
        // SAFETY: as above, with the move-rect element size.
        unsafe {
            dupl.GetFrameMoveRects(
                (moves.len() * std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>()) as u32,
                moves.as_mut_ptr(),
                &mut move_bytes_required,
            )
        }
        .ok()?;
        moves
            .truncate(move_bytes_required as usize / std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>());

        let mut out = ChangeInfo {
            rects: Vec::with_capacity(dirty.len() + moves.len()),
            move_rects: moves.len() as u32,
        };
        for r in &dirty {
            if let Some(dr) = self.clamp(r) {
                out.rects.push(dr);
            }
        }
        for m in &moves {
            if let Some(dr) = self.clamp(&m.DestinationRect) {
                out.rects.push(dr);
            }
        }
        Some(out)
    }

    /// Desktop-clamp one RECT; degenerate or fully off-screen rects vanish.
    fn clamp(&self, r: &RECT) -> Option<DirtyRect> {
        let x0 = r.left.max(0) as u32;
        let y0 = r.top.max(0) as u32;
        let x1 = (r.right.max(0) as u32).min(self.width);
        let y1 = (r.bottom.max(0) as u32).min(self.height);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(DirtyRect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }

    /// Create the readback surface if this is the first rect frame.
    ///
    /// Full desktop size rather than per-rect: a staging texture sized to the rect
    /// would have to be recreated whenever a rect grew, and creating a texture is
    /// far more expensive than copying into a corner of an existing one. Copying at
    /// the rect's own desktop coordinates then keeps source and destination
    /// coordinates identical, so there is no offset arithmetic to get wrong.
    fn ensure_staging(&mut self) -> Result<()> {
        if self.staging.is_some() {
            return Ok(());
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: self.width,
            Height: self.height,
            MipLevels: 1,
            ArraySize: 1,
            // The duplication hands over BGRA8; a staging copy must match its
            // source's format exactly or `CopySubresourceRegion` refuses it.
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
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture)) }?;
        self.staging = Some(texture.ok_or("CreateTexture2D returned no staging texture")?);
        Ok(())
    }

    /// Read the pixels behind `change`'s rects back to the CPU, tightly packed.
    ///
    /// **Validity window: the frame must still be held.** `texture` is only alive
    /// between the `AcquireNextFrame` that produced it and the `ReleaseFrame` the
    /// *next* [`Capture::acquire`] performs, and the same is true of the metadata
    /// `change` was built from. The pipeline calls this inside that window, before
    /// the frame enters the converter — which is also where the latency win is.
    ///
    /// Each returned [`crate::rects::Rect`] carries exactly `w * h * 4` bytes of
    /// BGRA, `w * 4` per row, top-down: `rects::encode`'s pixel contract.
    ///
    /// Wire coordinates are `u16`, so the caller must have established that the
    /// desktop fits (it checks once at startup); the `debug_assert` below states
    /// that contract rather than re-deriving it per frame.
    pub fn read_rects(
        &mut self,
        texture: &ID3D11Texture2D,
        change: &ChangeInfo,
    ) -> Result<Vec<crate::rects::Rect>> {
        if change.rects.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_staging()?;
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
            // SAFETY: both textures are live, share the BGRA8 format and the desktop
            // size, and `region` is inside both — `clamp` built the rect against
            // exactly these dimensions. The copy is same-coordinate, so the
            // destination cannot overrun either.
            unsafe {
                self.context.CopySubresourceRegion(
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
        unsafe {
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }?;

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
        unsafe { self.context.Unmap(staging, 0) };
        Ok(out)
    }

    /// Release the dead duplication, then build a new one. The order is the whole
    /// point: DXGI will not hand out a second duplication while the first is alive.
    fn rebuild(&mut self) -> Result<()> {
        self.holding = false;
        self.dupl = None;
        self.dupl = Some(Self::duplicate(&self.output, &self.device)?);
        Ok(())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // Leaving a frame checked out keeps the compositor waiting on us, which is
        // rude to whatever runs next on the host.
        let _ = self.release_held();
    }
}
