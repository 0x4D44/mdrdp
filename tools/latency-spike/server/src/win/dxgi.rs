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
//!
//! This is one of the two [`FrameSource`] implementations — the always-available
//! one, and the pipeline's default. The other reads the IddCx driver's shared
//! texture pool ([`super::idd_source`]); the shapes they both produce
//! ([`Acquired`], [`ChangeInfo`]) live in [`super::source`].

use super::source::{Acquired, ChangeInfo, DirtyRect, FrameSource, RectReadback};
use super::{qpc, wide_to_string, Result};
use std::time::Duration;
use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
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

/// `origin_x, origin_y` are `DesktopCoordinates`'s top-left corner — this output's
/// own offset into the virtual desktop, physical pixels. `enumerate` ignores them
/// (`--list-outputs` has never printed placement); `Capture::open` keeps them, for
/// the mouse-move injector (§5.2, review S-M5) — see [`super::source::FrameSource::origin`].
fn describe(desc: &DXGI_OUTPUT_DESC) -> (String, u32, u32, i32, i32, bool, &'static str) {
    let rect = desc.DesktopCoordinates;
    (
        wide_to_string(&desc.DeviceName),
        (rect.right - rect.left).max(0) as u32,
        (rect.bottom - rect.top).max(0) as u32,
        rect.left,
        rect.top,
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
            let (device_name, width, height, _origin_x, _origin_y, attached, rotation) =
                describe(&desc);
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

/// The capture stage: a D3D11 device plus the duplication interface for one output.
///
/// Accumulated frames (`AccumulatedFrames > 1`) are accepted deliberately — see
/// [`ChangeInfo`] for why coverage is order-free. The §5a telemetry showed a
/// keystroke on the 240 Hz IDD is a ~100 ms burst of presents the ~8 ms capture
/// loop cannot drain one-by-one, so the first cut's `AccumulatedFrames == 1` gate
/// starved the fast path to a 4-in-49 hit rate while the accumulated union stayed
/// small and correct.
pub struct Capture {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub width: u32,
    pub height: u32,
    pub adapter: String,
    pub device_name: String,
    /// This output's own offset into the virtual desktop (§5.2) — see
    /// [`super::source::FrameSource::origin`].
    origin_x: i32,
    origin_y: i32,
    output: IDXGIOutput1,
    /// `None` only while a lost duplication is being replaced. It must be dropped
    /// before `DuplicateOutput` is called again — DXGI refuses a second duplication
    /// of an output that already has one outstanding.
    dupl: Option<IDXGIOutputDuplication>,
    /// True while a frame is checked out and owes a `ReleaseFrame`.
    holding: bool,
    /// The raw fast path's GPU→CPU readback, shared with the IDD source.
    readback: RectReadback,
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
        let (device_name, width, height, origin_x, origin_y, attached, _) = describe(&desc);
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
            origin_x,
            origin_y,
            output,
            dupl: Some(dupl),
            holding: false,
            readback: RectReadback::new(width, height),
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

    fn acquire_frame(&mut self, timeout_ms: u32) -> Result<Acquired> {
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

    /// Release the dead duplication, then build a new one. The order is the whole
    /// point: DXGI will not hand out a second duplication while the first is alive.
    fn rebuild(&mut self) -> Result<()> {
        self.holding = false;
        self.dupl = None;
        self.dupl = Some(Self::duplicate(&self.output, &self.device)?);
        Ok(())
    }
}

impl FrameSource for Capture {
    fn device(&self) -> &ID3D11Device {
        &self.device
    }

    fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn adapter(&self) -> &str {
        &self.adapter
    }

    fn origin(&self) -> (i32, i32) {
        (self.origin_x, self.origin_y)
    }

    fn output_name(&self) -> &str {
        &self.device_name
    }

    fn kind(&self) -> &'static str {
        "dxgi"
    }

    fn acquire(&mut self, timeout_ms: u32) -> Result<Acquired> {
        self.acquire_frame(timeout_ms)
    }

    /// The held frame is the validity window: `texture` and the metadata `change`
    /// was built from both die at the `ReleaseFrame` the next `acquire` performs.
    fn read_rects(
        &mut self,
        texture: &ID3D11Texture2D,
        change: &ChangeInfo,
    ) -> Result<Vec<crate::rects::Rect>> {
        // Split borrow: the readback needs the device and the context by reference
        // at the same time as itself, which one `&mut self` method cannot express.
        let Self {
            readback,
            device,
            context,
            ..
        } = self;
        readback.read(device, context, texture, change)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // Leaving a frame checked out keeps the compositor waiting on us, which is
        // rude to whatever runs next on the host.
        let _ = self.release_held();
    }
}
