//! Stage 1, alternative — the IddCx driver's shared texture pool.
//!
//! Desktop Duplication costs a measured ~7.5 ms between DWM's present and our
//! `AcquireNextFrame` returning (HLD §1). The driver already holds each committed
//! buffer at `IddCxSwapChainReleaseAndAcquireBuffer`, so it copies that buffer into
//! a pool of three named shared textures and we read it from there: the gap
//! collapses to a copy plus a signal.
//!
//! The byte contract — the section layout, the object names, the seqlock and the
//! coverage invariant — lives in [`crate::idd_section`], portable and unit-tested
//! on macOS, because the C++ driver is written against the same numbers. This
//! module is only the Windows plumbing around it.
//!
//! The contract, in the order the code follows it:
//!
//! 1. `OpenFileMappingW(FILE_MAP_READ)` + `MapViewOfFile` on the one well-known
//!    name, [`crate::idd_section::SECTION_NAME`]. The section may not exist yet —
//!    the driver can start after the server — so this retries with a small backoff
//!    and logs once.
//! 2. Read the header under its seqlock. `layout_version` other than 1 is refused
//!    loudly; `generation == 0` means the section exists but no pool does, which is
//!    waited out.
//! 3. `D3D11CreateDevice` on the adapter the header's `render_adapter_luid` names,
//!    found with `IDXGIFactory4::EnumAdapterByLuid`. It has to be *that* adapter:
//!    a shared resource opened on any other one is a cross-adapter transfer through
//!    system memory, which is the opposite of the point.
//! 4. `ID3D11Device1::OpenSharedResourceByName` for each of the three textures and
//!    `OpenEventW(SYNCHRONIZE)` for each of the three events, at the
//!    generation-qualified names the header publishes.
//! 5. Per acquire: re-check the header, `WaitForMultipleObjects` on the three
//!    events, read every slot record under its seqlock, take the newest one that is
//!    newer than the last frame consumed, `AcquireSync` its keyed mutex, copy it to
//!    a private texture, `ReleaseSync`, and hand the private texture on.
//!
//! Two rules in that last step carry the whole design:
//!
//! * **The mutex is released before convert and encode.** The driver acquires with
//!   a zero timeout and skips a busy slot rather than blocking — blocking its
//!   swapchain thread would back-pressure DWM and reintroduce exactly the latency
//!   this source removes. Holding the mutex across the pipeline would therefore not
//!   stall us, it would silently drop frames at the source.
//! * **The keyed mutex's HRESULT is inspected by value, never by `FAILED()`.**
//!   `AcquireSync` returns `WAIT_TIMEOUT` (0x102) and `WAIT_ABANDONED` (0x80) as
//!   *SUCCEEDED* codes. A `FAILED()`-shaped check — which is what windows-rs's
//!   `Result`-returning wrapper gives — waves both through, and the first of them
//!   means we would copy a surface we do not hold.

use super::source::{Acquired, ChangeInfo, DirtyRect, FrameSource, RectReadback};
use super::{qpc, wide_to_string, Result};
use crate::agent::PoolObservation;
use crate::idd_section::{self, LayoutError, PoolHeader, SlotRecord};
use std::sync::atomic::{compiler_fence, Ordering};
use std::time::{Duration, Instant};
use windows::core::{Interface, HRESULT, HSTRING};
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HMODULE, LUID, WAIT_FAILED, WAIT_TIMEOUT as WAIT_EVENT_TIMEOUT,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11DeviceContext, ID3D11Multithread,
    ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory4, IDXGIKeyedMutex, DXGI_SHARED_RESOURCE_READ,
};
use windows::Win32::System::Memory::{
    MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS,
};
use windows::Win32::System::Threading::{
    OpenEventW, WaitForMultipleObjects, SYNCHRONIZATION_SYNCHRONIZE,
};

/// How long to wait for the driver's pool at startup before giving up and saying
/// so. The driver may legitimately start after the server, but a run that waits
/// forever with no output is worse than one that names the section it could not
/// find.
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);

/// Gap between open attempts while waiting for the section or a pool.
const OPEN_RETRY: Duration = Duration::from_millis(250);

/// How long a generation may keep refusing to open (`adopt`) before it stops
/// being a rebuild race and becomes a fault worth dying over.
const ADOPT_RETRY_LIMIT: Duration = Duration::from_secs(10);

/// Seqlock read attempts before a record is given up on for this tick. The driver
/// holds a record odd for a memcpy of at most 1 KB, so any real write finishes
/// inside one of these; the bound exists so a wedged writer cannot spin us.
const SEQLOCK_ATTEMPTS: u32 = 32;

/// Keyed-mutex key. One key both ways: the driver releases with it, we acquire and
/// release with it.
const MUTEX_KEY: u64 = 0;

/// How long to wait for a slot's keyed mutex. Small on purpose: a large surface copy
/// may outlive this wait, but a busy slot is safely skipped in favour of the next-newest
/// slot or the next wakeup.
const MUTEX_TIMEOUT_MS: u32 = 4;

/// `AcquireSync` succeeded but the previous owner died holding the mutex. A
/// *SUCCEEDED* HRESULT: `FAILED()` waves it through.
const HR_WAIT_ABANDONED: HRESULT = HRESULT(0x0000_0080_u32 as i32);

/// `AcquireSync` timed out — the mutex is **not** held. Also a SUCCEEDED HRESULT,
/// and the dangerous one: treating it as success copies a surface the driver is
/// writing.
const HR_WAIT_TIMEOUT: HRESULT = HRESULT(0x0000_0102_u32 as i32);

/// The one format the pipeline downstream of here understands: the converter's
/// input colour space and the rect readback's staging surface are both BGRA8.
const POOL_FORMAT: u32 = DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32;

/// The mapped `Global\mdrdp-idd` view.
struct Section {
    mapping: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

impl Section {
    /// Open and map the section, or say why not. `Err` here is always "not there
    /// yet" in practice; the caller retries.
    fn open() -> Result<Self> {
        let name = HSTRING::from(idd_section::SECTION_NAME);
        // SAFETY: `name` outlives the call. FILE_MAP_READ only: this process never
        // writes the driver's section, and asking for write access we do not need
        // would fail against a read-only ACE.
        let mapping = unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, &name) }?;
        // SAFETY: `mapping` is a live section handle; the view is unmapped in Drop.
        let view =
            unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, idd_section::SECTION_BYTES) };
        if view.Value.is_null() {
            let e = windows::core::Error::from_thread();
            // SAFETY: `mapping` is live and not used again.
            let _ = unsafe { CloseHandle(mapping) };
            return Err(format!("MapViewOfFile({}) failed: {e}", idd_section::SECTION_NAME).into());
        }
        Ok(Self { mapping, view })
    }

    /// One `u32` straight out of the mapping.
    fn seq_word(&self, offset: usize) -> u32 {
        debug_assert!(offset + 4 <= idd_section::SECTION_BYTES);
        // SAFETY: the view is `SECTION_BYTES` long and the offset is inside it and
        // 4-aligned (MapViewOfFile returns a page-aligned base, and every seqlock
        // word sits at a 4-aligned page offset). `read_volatile` is what stops the
        // compiler caching a word another process rewrites under us.
        unsafe {
            (self.view.Value as *const u8)
                .add(offset)
                .cast::<u32>()
                .read_volatile()
        }
    }

    /// Copy `len` bytes from `offset` out of the mapping.
    fn snapshot(&self, offset: usize, len: usize) -> Vec<u8> {
        debug_assert!(offset + len <= idd_section::SECTION_BYTES);
        let mut out = vec![0u8; len];
        // The fences pin this copy between the two sequence samples the caller
        // takes. Without them the compiler is free to sink or hoist the memcpy past
        // a `read_volatile`, and the seqlock would be checking a window the copy
        // never happened in.
        compiler_fence(Ordering::SeqCst);
        // SAFETY: the view is `SECTION_BYTES` long, `offset + len` is inside it, and
        // `out` is a fresh allocation of exactly `len` bytes that cannot overlap it.
        unsafe {
            std::ptr::copy_nonoverlapping(
                (self.view.Value as *const u8).add(offset),
                out.as_mut_ptr(),
                len,
            );
        }
        compiler_fence(Ordering::SeqCst);
        out
    }

    /// The seqlock read: sample, copy, sample again, accept only an unmoved even
    /// word. `None` means the writer never let go inside [`SEQLOCK_ATTEMPTS`].
    fn read_stable<T>(
        &self,
        offset: usize,
        len: usize,
        seq_offset: usize,
        parse: impl Fn(&[u8]) -> std::result::Result<T, LayoutError>,
    ) -> Option<std::result::Result<T, LayoutError>> {
        for _ in 0..SEQLOCK_ATTEMPTS {
            let before = self.seq_word(offset + seq_offset);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let body = self.snapshot(offset, len);
            if self.seq_word(offset + seq_offset) != before {
                std::hint::spin_loop();
                continue;
            }
            return Some(parse(&body));
        }
        None
    }

    fn read_header(&self) -> Option<std::result::Result<PoolHeader, LayoutError>> {
        self.read_stable(
            0,
            idd_section::HEADER_BYTES,
            idd_section::HEADER_SEQUENCE_OFFSET,
            idd_section::parse_header,
        )
    }

    fn read_slot(&self, slot: usize) -> Option<std::result::Result<SlotRecord, LayoutError>> {
        self.read_stable(
            idd_section::slot_offset(slot),
            idd_section::SLOT_BYTES,
            idd_section::SLOT_SEQUENCE_OFFSET,
            idd_section::parse_slot,
        )
    }
}

impl Drop for Section {
    fn drop(&mut self) {
        // SAFETY: both were produced by this type's own `open` and are used
        // nowhere else afterwards.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.mapping);
        }
    }
}

/// Read the section for health purposes and let go again (HLD tranche 4 §5).
///
/// This is the agent's per-tick observation, and it is deliberately a *fresh*
/// open every time. Holding the mapping open would keep the kernel object alive
/// across a driver rebuild — the very hazard `IddSource::reopen` documents, where
/// `OpenFileMappingW` on a name this process still holds hands back the old world
/// — and an agent that pinned a dead generation would be reporting on a section
/// nobody writes to while insisting it was fine.
///
/// Costs one open/map/copy/unmap per tick (every 2 s) and needs no viewer, no
/// capture server, and no D3D device.
pub(super) fn observe_pool() -> PoolObservation {
    let section = match Section::open() {
        Ok(s) => s,
        // The name not existing is a real, actionable reading: no driver
        // instance is publishing. Any other open failure is us failing to look.
        Err(e) => {
            let text = e.to_string();
            return if text.contains("cannot find the file")
                || text.contains("does not exist")
                || text.contains("(os error 2)")
            {
                PoolObservation::Absent
            } else {
                PoolObservation::Unreadable(format!("opening the section failed: {text}"))
            };
        }
    };

    let header = match section.read_header() {
        // The writer never let go: transient by construction.
        None => return PoolObservation::Unreadable("the section stayed mid-write".to_owned()),
        Some(Err(e)) => {
            return PoolObservation::Unreadable(format!("the section header did not parse: {e}"))
        }
        Some(Ok(h)) => h,
    };
    if header.generation == 0 {
        // The driver's own `AdvertiseNoPool`. Not ignorance — a statement.
        return PoolObservation::NoPool;
    }

    // The driver advances `frame_seq` as the compositor presents, so the newest
    // slot of this generation is the liveness counter. A slot that will not read
    // is skipped rather than counted as zero: a torn slot must not look like a
    // stalled display.
    let frame_seq = (0..idd_section::SLOT_COUNT)
        .filter_map(|slot| match section.read_slot(slot) {
            Some(Ok(record)) if record.generation == header.generation => Some(record.frame_seq),
            _ => None,
        })
        .max()
        .unwrap_or(0);

    PoolObservation::Present {
        generation: header.generation,
        frame_seq,
    }
}

/// One slot's named objects, opened on our device.
struct Slot {
    texture: ID3D11Texture2D,
    mutex: IDXGIKeyedMutex,
    event: HANDLE,
}

/// The three slots of one generation.
struct Pool {
    generation: u32,
    /// The random per-build suffix from the header. Pool identity is the PAIR
    /// (generation, suffix): generation numbers can repeat across driver
    /// instances (teardown advertises 0, and a new instance adopting the
    /// section seeds its counter from that), so equality on the number alone
    /// would let a consumer keep a dead pool's textures while believing it is
    /// current (re-review, D1). 64 random bits make the pair unique.
    suffix: u64,
    slots: Vec<Slot>,
    /// The slot events, in slot order, ready for `WaitForMultipleObjects`.
    events: Vec<HANDLE>,
}

impl Pool {
    fn open(device: &ID3D11Device1, header: &PoolHeader) -> Result<Self> {
        let mut slots = Vec::with_capacity(idd_section::SLOT_COUNT);
        let mut events = Vec::with_capacity(idd_section::SLOT_COUNT);
        for i in 0..idd_section::SLOT_COUNT {
            let texture_name = idd_section::texture_name(header.generation, header.name_suffix, i);
            // SAFETY: the name outlives the call; the returned interface is owned
            // by windows-rs. READ access only — this process never writes the
            // driver's surfaces.
            let texture: ID3D11Texture2D = unsafe {
                device.OpenSharedResourceByName(
                    &HSTRING::from(texture_name.as_str()),
                    DXGI_SHARED_RESOURCE_READ.0,
                )
            }
            .map_err(|e| format!("OpenSharedResourceByName({texture_name}): {e}"))?;
            let mutex: IDXGIKeyedMutex = texture.cast().map_err(|e| {
                format!(
                    "{texture_name} carries no keyed mutex, which the pool contract requires: {e}"
                )
            })?;

            let event_name = idd_section::event_name(header.generation, header.name_suffix, i);
            // SAFETY: the name outlives the call; the handle is closed in Drop.
            // SYNCHRONIZE is all a waiter needs — we never signal or reset.
            let event = unsafe {
                OpenEventW(
                    SYNCHRONIZATION_SYNCHRONIZE,
                    false,
                    &HSTRING::from(event_name.as_str()),
                )
            }
            .map_err(|e| format!("OpenEventW({event_name}): {e}"))?;

            events.push(event);
            slots.push(Slot {
                texture,
                mutex,
                event,
            });
        }
        Ok(Self {
            generation: header.generation,
            suffix: header.name_suffix,
            slots,
            events,
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        // SAFETY: the handle came from `Pool::open`'s own `OpenEventW` and is not
        // used again. `Pool::events` holds copies of the same handles, never closed
        // separately. Ownership sits on the slot rather than the pool so a
        // `Pool::open` that fails partway (the retrying adopt path) still closes
        // the events of the slots it did build.
        unsafe {
            let _ = CloseHandle(self.event);
        }
    }
}

/// What one attempt at a slot produced.
enum Taken {
    /// The slot's pixels are in the private texture, and this is the record that
    /// describes exactly those pixels — re-read under the keyed mutex, because the
    /// driver may have republished the slot between our scan and our acquire. The
    /// scan's record chose the slot; this one is the truth about its contents.
    /// Pairing the scan's record with the acquired pixels would let the coverage
    /// list under-claim, which the client's exactness invariant turns into
    /// permanently stale canvas regions.
    Copied(idd_section::SlotRecord),
    /// The mutex was held by the driver for longer than we will wait.
    Busy,
    /// `WAIT_ABANDONED`: the previous owner died holding it, so the surface's
    /// contents are not to be trusted.
    Poisoned,
}

/// The IddCx shared-pool frame source.
pub struct IddSource {
    device: ID3D11Device,
    device1: ID3D11Device1,
    context: ID3D11DeviceContext,
    adapter: String,
    output_name: String,
    width: u32,
    height: u32,
    /// The adapter the pool lives on. A generation that moves to a different one
    /// cannot be adopted: the converter and the encoder are already built on this
    /// device.
    luid: u64,
    /// `None` only transiently, inside [`IddSource::reopen`]: `OpenFileMappingW`
    /// on a name we still hold open returns the same kernel object, so the old
    /// mapping must be gone before a reopen can observe a new world (review, M4).
    section: Option<Section>,
    pool: Pool,
    /// Our own copy target. One texture, not a pool: the copy into it and the
    /// converter's read out of it are both submitted to the same immediate
    /// context, which orders them, so frame N's copy cannot land inside frame
    /// N-1's blit.
    private: ID3D11Texture2D,
    readback: RectReadback,
    /// Highest `frame_seq` this source has actually handed to the pipeline in the
    /// current generation. The coverage invariant is decided against it, and it
    /// resets to 0 on every rebuild.
    last_consumed: u64,
    /// A malformed slot record is logged once per generation, not per frame.
    warned_bad_slot: bool,
    /// This display's top-left corner in the virtual desktop, physical pixels —
    /// read from GDI at open, since the pool header carries no placement. Read
    /// once, like the duplication source's: a display that moves under a live
    /// server needs a restart for the injector to follow it.
    origin: (i32, i32),
    /// Start of the current streak of `Pool::open` failures inside [`adopt`];
    /// `None` while healthy. See `ADOPT_RETRY_LIMIT`.
    adopt_failing_since: Option<Instant>,
}

impl IddSource {
    /// Open the driver's pool, waiting for it if the driver has not started yet.
    pub fn open() -> Result<Self> {
        let (section, header) = wait_for_pool()?;
        if header.dxgi_format != POOL_FORMAT {
            return Err(format!(
                "the IDD pool publishes DXGI format {}; this pipeline converts and reads back \
                 BGRA8 ({POOL_FORMAT}) only",
                header.dxgi_format
            )
            .into());
        }
        if header.width == 0 || header.height == 0 {
            return Err(format!(
                "the IDD pool reports a {}x{} surface",
                header.width, header.height
            )
            .into());
        }
        if header.width % 2 != 0 || header.height % 2 != 0 {
            return Err(format!(
                "the IDD pool is {}x{}; NV12 needs even dimensions in both axes",
                header.width, header.height
            )
            .into());
        }

        let (device, context, adapter) = create_device(header.render_adapter_luid)?;
        let device1: ID3D11Device1 = device.cast().map_err(|e| {
            format!("this D3D11 device exposes no ID3D11Device1, so it cannot open a shared resource by name: {e}")
        })?;
        let pool = Pool::open(&device1, &header)?;
        let private = create_private_texture(&device, header.width, header.height)?;
        // Where this display sits in the virtual desktop, for the mouse injector.
        // A display that GDI cannot place falls back to the desktop origin, which
        // is what the injector assumed unconditionally before — so an unreadable
        // placement is no worse than the old behaviour, but it is worth saying.
        let origin = super::agent_ops::idd_display_origin().unwrap_or_else(|| {
            eprintln!(
                "capture: the IDD display's desktop placement is unreadable; \
                 mouse input will be mapped as though it sat at (0, 0)"
            );
            (0, 0)
        });
        eprintln!("capture: IDD display origin ({}, {})", origin.0, origin.1);

        Ok(Self {
            device,
            device1,
            context,
            adapter,
            output_name: pool_name(header.generation),
            width: header.width,
            height: header.height,
            luid: header.render_adapter_luid,
            section: Some(section),
            pool,
            private,
            readback: RectReadback::new(header.width, header.height),
            last_consumed: 0,
            warned_bad_slot: false,
            adopt_failing_since: None,
            origin,
        })
    }

    /// Adopt a pool the header now describes, keeping our device.
    ///
    /// The device stays because the shared textures are opened *onto* it by name;
    /// what cannot change under us is the adapter or the geometry, because the
    /// converter and the encoder were built against both. Refusing loudly beats
    /// silently encoding a differently-sized desktop.
    ///
    /// `Ok(true)` = adopted; `Ok(false)` = the pool would not open just now (a
    /// rebuild race) — keep the old pool and retry next tick; `Err` = a fault
    /// worth dying over.
    fn adopt(&mut self, header: &PoolHeader) -> Result<bool> {
        if header.render_adapter_luid != self.luid {
            return Err(format!(
                "the IDD pool moved to adapter LUID {:#x} from {:#x}; the converter and encoder \
                 are built on the old one, so this needs a server restart",
                header.render_adapter_luid, self.luid
            )
            .into());
        }
        if header.width != self.width || header.height != self.height {
            return Err(format!(
                "the IDD pool changed from {}x{} to {}x{}; mid-session resolution change is out \
                 of scope for this build (HLD §2), so this needs a server restart",
                self.width, self.height, header.width, header.height
            )
            .into());
        }
        if header.dxgi_format != POOL_FORMAT {
            return Err(format!(
                "the rebuilt IDD pool publishes DXGI format {}, not BGRA8 ({POOL_FORMAT})",
                header.dxgi_format
            )
            .into());
        }
        // The new pool opens BEFORE the old one drops — deliberately. Names are
        // generation-qualified so they cannot collide, and an open that fails
        // must leave the old pool in place: a second rebuild can land between our
        // header read and these opens (Windows issues unassign/assign pairs in
        // quick succession on a mode change), and killing the server over that
        // race would be wrong. The failure is retried tick by tick and only
        // escalates once it has persisted long enough to be a real fault.
        match Pool::open(&self.device1, header) {
            Ok(pool) => {
                self.pool = pool;
                self.adopt_failing_since = None;
            }
            Err(e) => {
                let since = *self.adopt_failing_since.get_or_insert_with(Instant::now);
                if since.elapsed() > ADOPT_RETRY_LIMIT {
                    return Err(format!(
                        "the IDD pool (generation {}) has refused to open for {:?}: {e}",
                        header.generation, ADOPT_RETRY_LIMIT
                    )
                    .into());
                }
                return Ok(false);
            }
        }
        self.output_name = pool_name(header.generation);
        // A rebuilt pool has its own frame numbering, and we have seen none of it.
        // The coverage invariant then refuses metadata until this consumer has a
        // baseline again — the first frame after a rebuild always takes the
        // full-frame path, falling out of the general rule.
        self.last_consumed = 0;
        self.warned_bad_slot = false;
        Ok(true)
    }

    /// The section itself went away — a `pnputil` redeploy, or the driver
    /// unloading. Remap it and adopt whatever pool comes back. `Ok(false)` =
    /// remapped but the pool would not open this tick; the header mismatch
    /// persists, so the next tick retries the adopt.
    fn reopen(&mut self) -> Result<bool> {
        // The old mapping goes first: `OpenFileMappingW` on a name this process
        // still holds open returns the same kernel object, and a "reopen" that
        // remaps the dead section would wait 30 s staring at its own stale header.
        self.section = None;
        let (section, header) = wait_for_pool()?;
        self.section = Some(section);
        self.adopt(&header)
    }

    fn section(&self) -> &Section {
        self.section
            .as_ref()
            .expect("the section is None only inside reopen; reopen's own error path propagates and drops this source before anything else can call in")
    }

    fn read_slot_records(&mut self) -> Vec<Option<SlotRecord>> {
        let mut out = Vec::with_capacity(idd_section::SLOT_COUNT);
        for i in 0..idd_section::SLOT_COUNT {
            match self.section().read_slot(i) {
                Some(Ok(record)) => out.push(Some(record)),
                Some(Err(e)) => {
                    if !self.warned_bad_slot {
                        self.warned_bad_slot = true;
                        eprintln!(
                            "capture: IDD slot {i} record is malformed and is being skipped: {e}"
                        );
                    }
                    out.push(None);
                }
                // The writer never let go: not an error, just not this tick's slot.
                None => out.push(None),
            }
        }
        out
    }

    /// Acquire one slot's keyed mutex, copy it out, and release.
    ///
    /// The copy happens **inside** the mutex and convert/encode happen outside it:
    /// see the module docs for why holding it longer drops frames at the driver.
    fn take_slot(&self, slot: usize) -> Result<Taken> {
        let mutex = &self.pool.slots[slot].mutex;
        // Called through the vtable, not through windows-rs's `Result`-returning
        // wrapper, because the wrapper's `.ok()` maps only *negative* HRESULTs to
        // `Err` — and both outcomes that mean "you do not hold this mutex" are
        // positive. See the module docs.
        // SAFETY: `mutex` is a live interface on a texture we own a reference to.
        let hr = unsafe {
            (Interface::vtable(mutex).AcquireSync)(
                Interface::as_raw(mutex),
                MUTEX_KEY,
                MUTEX_TIMEOUT_MS,
            )
        };
        if hr == HR_WAIT_TIMEOUT {
            return Ok(Taken::Busy);
        }
        if hr.is_err() {
            return Err(windows::core::Error::from(hr).into());
        }
        let abandoned = hr == HR_WAIT_ABANDONED;
        let mut record = None;
        if !abandoned {
            // Re-read the record while the mutex is held: the driver writes it
            // inside the mutex too, so what we read here describes exactly the
            // pixels we are about to copy. A record that will not parse under the
            // mutex means the writer died mid-write — the slot is as untrustworthy
            // as an abandoned one.
            record = match self.section().read_slot(slot) {
                // A record stamped by another pool build describes another pool's
                // texture; pairing it with this one's pixels is exactly the
                // cross-generation confusion the stamp exists to stop.
                Some(Ok(r)) if r.generation == self.pool.generation => Some(r),
                Some(Ok(_)) | Some(Err(_)) | None => None,
            };
            if record.is_some() {
                // SAFETY: both textures are live, share the BGRA8 format and the pool's
                // dimensions (checked at open and at every adopt), and the slot's
                // surface is held by the keyed mutex for the duration of the copy.
                unsafe {
                    self.context
                        .CopyResource(&self.private, &self.pool.slots[slot].texture);
                }
            }
        }
        // SAFETY: exactly one release for the acquire above, on the same key. An
        // abandoned mutex is still *held* by us, so it is released either way —
        // skipping this would poison the slot permanently.
        unsafe { mutex.ReleaseSync(MUTEX_KEY) }?;
        Ok(match record {
            Some(r) if !abandoned => Taken::Copied(r),
            _ => Taken::Poisoned,
        })
    }

    fn acquire_frame(&mut self, timeout_ms: u32) -> Result<Acquired> {
        match self.section().read_header() {
            Some(Ok(header))
                if header.generation == self.pool.generation
                    && header.name_suffix == self.pool.suffix => {}
            // A torn-down pool with none published yet. Nothing to consume; the
            // generation bump that follows is what triggers the rebuild. Sleep the
            // timeout out first: this arm blocks on nothing, and an unpaced return
            // spins the capture loop flat out for the whole teardown gap
            // (re-review, D2).
            Some(Ok(header)) if header.generation == 0 => {
                std::thread::sleep(Duration::from_millis(u64::from(timeout_ms)));
                return Ok(Acquired::Timeout);
            }
            Some(Ok(header)) => {
                eprintln!(
                    "capture: IDD pool rebuilt (generation {} → {})",
                    self.pool.generation, header.generation
                );
                return Ok(if self.adopt(&header)? {
                    Acquired::Recreated
                } else {
                    // The new generation's objects were not openable this tick —
                    // a rebuild race. The mismatch persists, so we land here
                    // again next tick — paced, or the retry loop hammers
                    // OpenSharedResourceByName flat out (re-review, D2).
                    std::thread::sleep(Duration::from_millis(u64::from(timeout_ms)));
                    Acquired::Timeout
                });
            }
            // Mid-write, or a seqlock that never settled: a rebuild in flight.
            // Nothing to do but come back next tick.
            Some(Err(LayoutError::MidWrite(_))) | Some(Err(LayoutError::Uninitialised)) | None => {
                return Ok(Acquired::Timeout)
            }
            // A layout this build does not speak. Refusing is the whole point of
            // the version field.
            Some(Err(e)) => return Err(e.into()),
        }

        // SAFETY: every handle in `events` is a live event opened by `Pool::open`
        // and closed only in its Drop. `bwaitall = false` — any one publish wakes us.
        let wait = unsafe { WaitForMultipleObjects(&self.pool.events, false, timeout_ms) };
        if wait == WAIT_FAILED {
            // A named object vanished under us — the driver unloaded, or a redeploy
            // is in flight. Same recovery as a generation bump.
            let e = windows::core::Error::from_thread();
            eprintln!("capture: IDD slot events went away ({e}); reopening the pool");
            return Ok(if self.reopen()? {
                Acquired::Recreated
            } else {
                Acquired::Timeout
            });
        }
        // WAIT_TIMEOUT is not a shortcut out: an earlier wakeup may have consumed
        // the event of a slot we did not take, so the records are scanned either
        // way and the timeout only decides how long we waited to do it.
        debug_assert!(wait == WAIT_EVENT_TIMEOUT || wait.0 < idd_section::SLOT_COUNT as u32);

        let records = self.read_slot_records();
        let last_consumed = self.last_consumed;
        for slot in idd_section::ready_slots(&records, self.pool.generation, last_consumed) {
            // The scan's record chooses the slot; the record that describes the
            // copied pixels is the one `take_slot` re-reads under the keyed mutex,
            // because the driver may republish a slot between scan and acquire.
            let record = match self.take_slot(slot)? {
                Taken::Copied(r) => r,
                Taken::Busy | Taken::Poisoned => continue,
            };
            if record.frame_seq <= last_consumed {
                // Only possible if the driver's frame counter went backwards —
                // a rebuild we have not adopted yet. The generation check next
                // tick owns that; consuming it here would regress `last_consumed`.
                continue;
            }
            let acquire_qpc = qpc::now();
            let change =
                idd_section::coverage_for(&record, last_consumed).map(|rects| ChangeInfo {
                    rects: rects
                        .iter()
                        .filter_map(|r| idd_section::clamp(r, self.width, self.height))
                        .map(|c| DirtyRect {
                            x: c.x,
                            y: c.y,
                            w: c.w,
                            h: c.h,
                        })
                        .collect(),
                    // The driver publishes one already-unioned coverage list, so the
                    // dirty/move split does not survive to us.
                    move_rects: 0,
                });
            self.last_consumed = record.frame_seq;
            return Ok(Acquired::Frame {
                texture: self.private.clone(),
                present_qpc: record.present_qpc,
                acquire_qpc,
                change,
            });
        }
        Ok(Acquired::Timeout)
    }
}

impl FrameSource for IddSource {
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

    fn output_name(&self) -> &str {
        &self.output_name
    }

    fn kind(&self) -> &'static str {
        "idd"
    }

    /// Overrides the trait's `(0, 0)` default: the IDD display is routinely not
    /// the desktop's top-left one, and the default silently mis-aimed every
    /// click by the display's offset (§5.2).
    fn origin(&self) -> (i32, i32) {
        self.origin
    }

    fn acquire(&mut self, timeout_ms: u32) -> Result<Acquired> {
        self.acquire_frame(timeout_ms)
    }

    /// The validity window is the next `acquire`: this reads back the private
    /// texture, which the next slot copy overwrites.
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

fn pool_name(generation: u32) -> String {
    format!("{} generation {generation}", idd_section::SECTION_NAME)
}

/// Wait for the section to exist and for it to publish a pool.
///
/// Logs at most once per state so a server started before the driver says what it
/// is waiting for without filling the terminal.
fn wait_for_pool() -> Result<(Section, PoolHeader)> {
    let deadline = Instant::now() + OPEN_TIMEOUT;
    let mut said_waiting = false;
    let mut last_reason = format!("{} does not exist yet", idd_section::SECTION_NAME);
    loop {
        match Section::open() {
            Ok(section) => match section.read_header() {
                Some(Ok(header)) if header.generation != 0 => {
                    eprintln!(
                        "capture: IDD pool generation {} — {}x{} on adapter LUID {:#x}",
                        header.generation, header.width, header.height, header.render_adapter_luid
                    );
                    return Ok((section, header));
                }
                Some(Ok(_)) => last_reason = "the driver has published no pool yet".to_owned(),
                // A layout mismatch is not something waiting fixes.
                Some(Err(e @ LayoutError::UnsupportedVersion(_)))
                | Some(Err(e @ LayoutError::SlotCount(_))) => return Err(e.into()),
                Some(Err(e)) => last_reason = e.to_string(),
                None => last_reason = "the header seqlock never settled".to_owned(),
            },
            Err(e) => last_reason = e.to_string(),
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "waited {}s for {}: {last_reason}. Is mdrdp-idd installed and started?",
                OPEN_TIMEOUT.as_secs(),
                idd_section::SECTION_NAME
            )
            .into());
        }
        if !said_waiting {
            said_waiting = true;
            eprintln!(
                "capture: waiting up to {}s for {} ({last_reason})",
                OPEN_TIMEOUT.as_secs(),
                idd_section::SECTION_NAME
            );
        }
        std::thread::sleep(OPEN_RETRY);
    }
}

/// Build a D3D11 device on the adapter the pool's LUID names.
///
/// It must be that adapter: `OpenSharedResourceByName` against a device on any
/// other one either fails or silently routes every copy through system memory.
fn create_device(luid: u64) -> Result<(ID3D11Device, ID3D11DeviceContext, String)> {
    let adapter_luid = LUID {
        LowPart: luid as u32,
        HighPart: (luid >> 32) as i32,
    };
    // SAFETY: the factory call writes only through the out-pointer windows-rs owns.
    let factory: IDXGIFactory4 = unsafe { CreateDXGIFactory1() }?;
    // SAFETY: `factory` is live; the LUID is a plain value.
    let adapter: IDXGIAdapter1 =
        unsafe { factory.EnumAdapterByLuid(adapter_luid) }.map_err(|e| {
            format!(
                "no adapter with LUID {luid:#x}, which the IDD pool says its textures live on: {e}"
            )
        })?;
    // SAFETY: `adapter` is a live COM interface.
    let desc = unsafe { adapter.GetDesc1() }?;
    let name = wide_to_string(&desc.Description);

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

    // A hardware MFT runs its own threads against this device. Without multithread
    // protection the driver is free to corrupt state under us, and the symptom is
    // an intermittent hang rather than an error.
    if let Ok(mt) = device.cast::<ID3D11Multithread>() {
        // SAFETY: `mt` is a live interface on our own device.
        let _ = unsafe { mt.SetMultithreadProtected(true) };
    }
    Ok((device, context, name))
}

/// The copy target the pipeline actually sees. Never shared, never mapped: the
/// converter reads it as a video input view and the rect readback copies out of it.
fn create_private_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        // What a video-processor input view wants, plus the render-target flag the
        // duplication's own textures carry — matching them keeps the converter on
        // the same path it takes under `--source dxgi`.
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture: Option<ID3D11Texture2D> = None;
    // SAFETY: `desc` is fully initialised; the initial-data pointer is None because
    // the surface is filled by a copy, not by us.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }?;
    texture.ok_or_else(|| {
        "CreateTexture2D returned no private texture"
            .to_owned()
            .into()
    })
}
