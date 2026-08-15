//! The EGFX graphics handler: PDUs in, pixels in the [`SurfaceStore`] out.
//!
//! `ironrdp-egfx` stops one step short of a picture. It parses every PDU and tracks
//! surfaces as *metadata*, but it stores no pixels, and it only decodes AVC420 and
//! uncompressed bitmaps. `temper` sends neither: it sends **ClearCodec**, which
//! `GraphicsPipelineClient::handle_wire_to_surface1` drops into its `_` arm and forwards
//! to [`GraphicsPipelineHandler::on_unhandled_pdu`]. That callback is the seam this
//! module plugs into.
//!
//! So the division of labour is:
//!
//! | Job                          | Owner                                    |
//! |------------------------------|------------------------------------------|
//! | Wire framing, ZGFX, parsing  | `ironrdp-egfx`                           |
//! | ClearCodec decode            | `ironrdp-graphics::clearcodec`           |
//! | Colour order, blits, cache   | this module + [`crate::surface`]         |
//!
//! **Two things here are load-bearing and easy to get wrong:**
//!
//! 1. **One decoder, for the whole session.** `ClearCodecDecoder` carries the V-bar and
//!    glyph caches, and the server encodes later frames as references into them. A
//!    decoder created per PDU decodes the first frame and then produces garbage or
//!    errors, so the instance lives in the handler and is never rebuilt.
//! 2. **A decode failure is a glitch, not a fault.** Every failure path here counts and
//!    carries on. A dropped region is a smear the next frame repaints; an error
//!    propagated into the DVC processor tears down the session.
//!
//! Nothing in this module reads, logs, or serialises pixel content. The counters are
//! protocol metadata only — codec ids, PDU kinds, error tallies.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ironrdp::pdu::geometry::ExclusiveRectangle;
use ironrdp_egfx::client::{BitmapUpdate, GraphicsPipelineHandler, Surface as EgfxSurface};
use ironrdp_egfx::pdu::{
    CacheToSurfacePdu, CapabilitiesV107Flags, CapabilitySet, Codec1Type, GfxPdu, Point,
    SolidFillPdu, SurfaceToCachePdu, SurfaceToSurfacePdu, WireToSurface1Pdu,
};
use ironrdp_graphics::clearcodec::ClearCodecDecoder;
use ironrdp_graphics::progressive::ProgressiveDecoder;
use serde::Serialize;

use crate::surface::{Rect, SurfaceStore};

/// Convert an EGFX rectangle to a surface rectangle.
///
/// `RDPGFX_RECT16` (MS-RDPEGFX 2.2.1.4.1) is **exclusive** on `right`/`bottom`, which is
/// why `ironrdp-egfx` types it as [`ExclusiveRectangle`], and [`Rect`] is exclusive too —
/// so this is a field-for-field copy with no ±1. Getting that wrong costs one row and one
/// column on every tile, which reads as a faint grid over the whole desktop.
pub fn rect_from_egfx(rect: &ExclusiveRectangle) -> Rect {
    Rect::new(rect.left, rect.top, rect.right, rect.bottom)
}

/// Side of a progressive tile, in pixels. MS-RDPRFX fixes the tile grid at 64x64.
pub const PROGRESSIVE_TILE: u16 = 64;

/// Where a progressive tile lands on its surface.
///
/// The decoder reports a tile by GRID index, not by pixel position, so this multiply is
/// the only thing standing between a correct frame and one where every tile is in the
/// wrong place — a failure that looks like corruption rather than like a bug in a
/// coordinate conversion.
pub fn progressive_tile_rect(x_idx: u16, y_idx: u16) -> Rect {
    let left = x_idx.saturating_mul(PROGRESSIVE_TILE);
    let top = y_idx.saturating_mul(PROGRESSIVE_TILE);
    Rect::new(
        left,
        top,
        left.saturating_add(PROGRESSIVE_TILE),
        top.saturating_add(PROGRESSIVE_TILE),
    )
}

/// What the graphics pipeline did, in counters the caller can poll cheaply.
///
/// Deliberately carries no pixel data and no rectangles — this is the "why does it feel
/// slow" surface, and it must stay safe to print, log and serialise.
#[derive(Debug, Default, Clone, Serialize)]
pub struct GfxStats {
    /// `EndFrame` PDUs seen — one per logical frame.
    pub frames_completed: u64,
    /// Every codec id seen on a surface command, counted by name.
    pub codec_ids_seen: BTreeMap<String, u64>,
    /// ClearCodec decodes that failed. Each one is a region that was not painted.
    pub decode_errors: u64,
    /// Why they failed, tallied by reason.
    ///
    /// A count alone cannot distinguish one systematic fault from a scattering of
    /// unrelated ones, and that difference decides whether you go and fix a decoder or go
    /// looking for a corrupt stream.
    pub decode_error_reasons: BTreeMap<String, u64>,
    /// Store operations refused (unknown surface, unknown cache slot, short source) or
    /// declined by us as out of range. Also a not-painted region.
    pub surface_errors: u64,
    /// PDUs that reached us with no handling of their own.
    pub unhandled_pdus: u64,
    /// Regions that arrived in a codec we can observe but not yet decode — today, RFX
    /// Progressive. Non-zero means part of the desktop is stale on screen, which is
    /// exactly the kind of silent rot the visibility requirement exists to surface.
    pub undecoded_regions: u64,
    pub surfaces_created: u64,
    pub surfaces_deleted: u64,
    /// The last `ResetGraphics` dimensions, if any.
    pub reset_graphics: Option<(u32, u32)>,
}

/// A cloneable read handle on [`GfxStats`].
///
/// The handler itself is moved into `GraphicsPipelineClient` and is unreachable
/// afterwards, so the caller keeps one of these instead.
#[derive(Debug, Clone, Default)]
pub struct GfxStatsHandle(Arc<Mutex<GfxStats>>);

impl GfxStatsHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// A point-in-time copy. Never aliases later mutation.
    pub fn snapshot(&self) -> GfxStats {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GfxStats> {
        // A poisoned stats mutex means some other thread panicked while counting. That
        // is not a reason to bring down a running session, so recover the counters.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn note<F: FnOnce(&mut GfxStats)>(&self, f: F) {
        f(&mut self.lock());
    }
}

/// Turns EGFX PDUs into pixels in a shared [`SurfaceStore`].
/// Where to dump the bytes of a tile the decoder rejected.
///
/// Off unless asked for. The ClearCodec decoder has never been run against a real
/// server's output and its bands path has no upstream tests, so the first failure is
/// likely to be the interesting one — and it is far cheaper to debug from the exact
/// bytes than to try to provoke it again live.
///
/// **This is session content.** Only the payload of tiles that FAILED to decode is
/// written, never a successful frame, and only to a path the operator names.
#[derive(Debug, Clone, Default)]
pub struct FailureCapture {
    dir: Option<std::path::PathBuf>,
    written: Arc<Mutex<u32>>,
}

impl FailureCapture {
    /// Capture into `dir`. Caller is stating they accept session bytes on disk.
    pub fn to_dir(dir: impl Into<std::path::PathBuf>) -> Self {
        FailureCapture {
            dir: Some(dir.into()),
            written: Arc::new(Mutex::new(0)),
        }
    }

    /// Cap the number of files, so one persistently bad stream cannot fill the disk.
    const MAX_FILES: u32 = 20;

    fn record(&self, width: u16, height: u16, bytes: &[u8]) {
        let Some(dir) = &self.dir else {
            return;
        };
        let Ok(mut n) = self.written.lock() else {
            return;
        };
        if *n >= Self::MAX_FILES {
            return;
        }
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let path = dir.join(format!("clearcodec-fail-{n:03}-{width}x{height}.bin"));
        if std::fs::write(&path, bytes).is_ok() {
            *n += 1;
        }
    }
}

pub struct GfxHandler {
    store: Arc<Mutex<SurfaceStore>>,
    /// One instance for the session — see the module docs.
    decoder: ClearCodecDecoder,
    /// RFX Progressive, also one per session.
    ///
    /// It keeps per-context tile state: a progressive frame refines tiles an earlier
    /// frame established, so a decoder rebuilt per PDU would decode the first pass and
    /// then reject or corrupt every refinement — the same reason the ClearCodec decoder
    /// is long-lived.
    progressive: ProgressiveDecoder,
    stats: GfxStatsHandle,
    /// Surfaces we have mirrored into the store. `ResetGraphics` implicitly destroys all
    /// surfaces (MS-RDPEGFX 3.3.5.14), and `SurfaceStore` has no bulk clear, so we need
    /// to know which ids to delete.
    live_surfaces: HashSet<u16>,
    /// Dimensions of each cached bitmap, mirrored so `CacheToSurface` can reject a
    /// destination point that would overflow a `u16` coordinate before the store does
    /// the arithmetic.
    cache_dims: HashMap<u16, (u16, u16)>,
    /// Opt-in dump of tiles the decoder rejected. Default: off.
    capture: FailureCapture,
}

impl std::fmt::Debug for GfxHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // ClearCodecDecoder is not Debug, and its caches are session pixel content
        // anyway. Print structure, never content.
        f.debug_struct("GfxHandler")
            .field("live_surfaces", &self.live_surfaces.len())
            .field("cached_slots", &self.cache_dims.len())
            .finish_non_exhaustive()
    }
}

impl GfxHandler {
    pub fn new(store: Arc<Mutex<SurfaceStore>>) -> Self {
        Self {
            store,
            decoder: ClearCodecDecoder::new(),
            progressive: ProgressiveDecoder::new(),
            stats: GfxStatsHandle::new(),
            live_surfaces: HashSet::new(),
            cache_dims: HashMap::new(),
            capture: FailureCapture::default(),
        }
    }

    /// Dump the bytes of any tile the decoder rejects into `dir`.
    ///
    /// Writes session content, so it is opt-in and capped. Worth turning on for a first
    /// run against an unfamiliar server: the ClearCodec bands path has no upstream tests,
    /// and a captured payload is far cheaper to debug than a failure you must reproduce.
    #[must_use]
    pub fn capturing_failures_to(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.capture = FailureCapture::to_dir(dir);
        self
    }

    /// A handle the caller keeps after the handler is boxed into the graphics client.
    pub fn stats(&self) -> GfxStatsHandle {
        self.stats.clone()
    }

    fn with_store<R>(&self, f: impl FnOnce(&mut SurfaceStore) -> R) -> R {
        let mut guard = self
            .store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }

    /// Record a store result, counting a refusal rather than propagating it.
    fn absorb(&self, result: Result<(), crate::surface::SurfaceError>) {
        if result.is_err() {
            self.stats
                .note(|s| s.surface_errors = s.surface_errors.saturating_add(1));
        }
    }

    fn note_codec(&self, codec: Codec1Type) {
        let name = codec_name(codec);
        self.stats.note(|s| {
            *s.codec_ids_seen.entry(name.to_owned()).or_insert(0) += 1;
        });
    }

    /// Decode an RFX Progressive frame and blit every tile it updated.
    ///
    /// `ironrdp-graphics` carries a complete progressive decoder; like ClearCodec it is
    /// simply not wired into the client's decode path, so this is the seam. Without it a
    /// progressive region is counted and dropped, which shows on screen as part of the
    /// desktop frozen at whatever it last contained — the silent staleness the visibility
    /// requirement exists to surface.
    ///
    /// The decoder needs the *surface* dimensions to size its tile grid, not the region's,
    /// so a surface we do not know about cannot be decoded into.
    fn apply_wire_to_surface2(&mut self, pdu: &ironrdp_egfx::pdu::WireToSurface2Pdu) {
        let surface_id = pdu.surface_id;
        let Some((width, height)) =
            self.with_store(|store| store.get(surface_id).map(|s| (s.width, s.height)))
        else {
            // The server referenced a surface we never created. Counted as a store error
            // rather than a decode error: nothing was wrong with the bytes.
            self.stats
                .note(|s| s.surface_errors = s.surface_errors.saturating_add(1));
            return;
        };

        // Keyed by SURFACE, not by codec context.
        //
        // `ProgressiveDecoder` keeps a separate tile grid per `codec_context_id`, but
        // Windows rotates the context id for one surface while continuing to send
        // *refinement* passes. Keyed by context, a refinement lands on an empty grid and
        // reconstructs from partial coefficients. Progressive tile state belongs to the
        // surface being refined, so the surface id is the key.
        let tiles = match self.progressive.decode_bitmap(
            u32::from(surface_id),
            width,
            height,
            &pdu.bitmap_data,
        ) {
            Ok(tiles) => tiles,
            Err(e) => {
                // Still stale — the region did not get painted — so it is counted the same
                // way an undecodable region always was, but now with a reason attached.
                //
                // The context id is part of the reason on purpose: "missing CONTEXT block"
                // means the very first frame for a context arrived without one, so knowing
                // WHICH contexts fail distinguishes "the server opened a context we never
                // saw established" from "we lose the context we did establish".
                let reason = format!("ctx {}: {}", pdu.codec_context_id, e);
                self.stats.note(|s| {
                    s.undecoded_regions = s.undecoded_regions.saturating_add(1);
                    *s.decode_error_reasons.entry(reason).or_insert(0) += 1;
                });
                return;
            }
        };

        for tile in tiles {
            let rect = progressive_tile_rect(tile.x_idx, tile.y_idx);
            // `pixels` is a full 64x64 RGBA tile, so the source stride is the tile side
            // even when the destination is clipped at the surface edge. Passing the
            // clipped width instead shears the tile — the same trap `blit_rgba` documents.
            let result = self.with_store(|store| {
                store.blit_rgba(surface_id, rect, &tile.pixels, PROGRESSIVE_TILE)
            });
            if result.is_err() {
                self.stats
                    .note(|s| s.surface_errors = s.surface_errors.saturating_add(1));
            }
        }
    }

    /// Decode a ClearCodec tile and blit it into its surface.
    ///
    /// The decode happens **outside** the store lock: it is the expensive step, and
    /// holding the lock across it would stall the presenter for no reason.
    fn apply_wire_to_surface1(&mut self, pdu: &WireToSurface1Pdu) {
        self.note_codec(pdu.codec_id);

        if pdu.codec_id != Codec1Type::ClearCodec {
            // Some other codec we do not decode. Counted above; nothing to paint.
            self.stats
                .note(|s| s.unhandled_pdus = s.unhandled_pdus.saturating_add(1));
            return;
        }

        let dest = rect_from_egfx(&pdu.destination_rectangle);
        if dest.is_empty() {
            return;
        }

        let mut pixels = match self
            .decoder
            .decode(&pdu.bitmap_data, dest.width(), dest.height())
        {
            Ok(pixels) => pixels,
            Err(e) => {
                self.capture
                    .record(dest.width(), dest.height(), &pdu.bitmap_data);
                // The reason is recorded, not just the count. A bare counter said "177
                // tiles failed" and left no way to tell one cause from a hundred; the
                // error text carries protocol field names, never pixels, so it is safe
                // to keep. The tally is by reason so a single dominant fault is obvious.
                let reason = e.to_string();
                self.stats.note(|s| {
                    s.decode_errors = s.decode_errors.saturating_add(1);
                    *s.decode_error_reasons.entry(reason).or_insert(0) += 1;
                });
                // One dropped tile is a smear the next frame repaints; an error returned
                // to the DVC processor would end the session.
                return;
            }
        };

        SurfaceStore::bgra_to_rgba_in_place(&mut pixels);
        // The decoder produced rows at the UNCLIPPED rect width — that is what it was
        // asked for. Passing the clipped width instead shears the tile.
        let stride = dest.width();
        let result =
            self.with_store(|store| store.blit_rgba(pdu.surface_id, dest, &pixels, stride));
        self.absorb(result);
    }

    fn apply_solid_fill(&mut self, pdu: &SolidFillPdu) {
        let rects: Vec<Rect> = pdu.rectangles.iter().map(rect_from_egfx).collect();
        // RDPGFX_COLOR32 is BGRA on the wire and its alpha byte is reserved, so the
        // store gets an opaque RGBA colour.
        let rgba = [pdu.fill_pixel.r, pdu.fill_pixel.g, pdu.fill_pixel.b, 0xFF];
        let result = self.with_store(|store| store.solid_fill(pdu.surface_id, &rects, rgba));
        self.absorb(result);
    }

    fn apply_surface_to_surface(&mut self, pdu: &SurfaceToSurfacePdu) {
        let src = rect_from_egfx(&pdu.source_rectangle);
        let (points, skipped) =
            placeable_points(&pdu.destination_points, src.width(), src.height());
        self.note_skipped(skipped);
        if points.is_empty() {
            return;
        }
        let result = self.with_store(|store| {
            store.surface_to_surface(
                pdu.source_surface_id,
                src,
                pdu.destination_surface_id,
                &points,
            )
        });
        self.absorb(result);
    }

    fn apply_surface_to_cache(&mut self, pdu: &SurfaceToCachePdu) {
        let src = rect_from_egfx(&pdu.source_rectangle);
        let result =
            self.with_store(|store| store.surface_to_cache(pdu.surface_id, src, pdu.cache_slot));
        if result.is_ok() {
            self.cache_dims
                .insert(pdu.cache_slot, (src.width(), src.height()));
        }
        self.absorb(result);
    }

    fn apply_cache_to_surface(&mut self, pdu: &CacheToSurfacePdu) {
        let (w, h) = self
            .cache_dims
            .get(&pdu.cache_slot)
            .copied()
            .unwrap_or((0, 0));
        let (points, skipped) = placeable_points(&pdu.destination_points, w, h);
        self.note_skipped(skipped);
        if points.is_empty() {
            // Either every point was out of range, or we never saw the SurfaceToCache
            // that filled this slot. Either way the store would refuse it.
            if !pdu.destination_points.is_empty() {
                self.stats
                    .note(|s| s.surface_errors = s.surface_errors.saturating_add(1));
            }
            return;
        }
        let result = self
            .with_store(|store| store.cache_to_surface(pdu.cache_slot, pdu.surface_id, &points));
        self.absorb(result);
    }

    fn note_skipped(&self, skipped: u64) {
        if skipped > 0 {
            self.stats
                .note(|s| s.surface_errors = s.surface_errors.saturating_add(skipped));
        }
    }
}

/// Keep only the destination points where the copy fits inside `u16` coordinates.
///
/// `SurfaceStore` builds each destination rectangle as `x + width`, and in a debug build
/// a `u16` overflow there is a **panic**, which would poison the store mutex and take the
/// session with it. A server has no reason to send such a point, which is exactly why it
/// must not be able to. Returns the surviving points and the number dropped.
fn placeable_points(points: &[Point], width: u16, height: u16) -> (Vec<(u16, u16)>, u64) {
    if width == 0 || height == 0 {
        return (Vec::new(), 0);
    }
    let mut kept = Vec::with_capacity(points.len());
    let mut skipped = 0u64;
    for point in points {
        match (point.x.checked_add(width), point.y.checked_add(height)) {
            (Some(_), Some(_)) => kept.push((point.x, point.y)),
            _ => skipped = skipped.saturating_add(1),
        }
    }
    (kept, skipped)
}

/// A stable name per codec id. Protocol constants, never payload.
fn codec_name(codec: Codec1Type) -> &'static str {
    match codec {
        Codec1Type::Uncompressed => "Uncompressed",
        Codec1Type::RemoteFx => "RemoteFx",
        Codec1Type::ClearCodec => "ClearCodec",
        Codec1Type::Planar => "Planar",
        Codec1Type::Avc420 => "Avc420",
        Codec1Type::Alpha => "Alpha",
        Codec1Type::Avc444 => "Avc444",
        Codec1Type::Avc444v2 => "Avc444v2",
    }
}

impl GraphicsPipelineHandler for GfxHandler {
    /// Advertise **V10.7 with AVC explicitly disabled**, and nothing else.
    ///
    /// This single-set offer is deliberate. The upstream default advertises V10.7 with
    /// AVC implied, and `GraphicsPipelineClient::start` filters out every AVC-bearing set
    /// when no H.264 decoder is configured — so a client without one silently drops to
    /// V8, an older pipeline than the server would otherwise use. Saying "V10.7, and no
    /// AVC please" survives that filter and keeps the modern pipeline.
    fn capabilities(&self) -> Vec<CapabilitySet> {
        // V8 is kept as a fallback: it carries no AVC so it survives the same filter,
        // and without it a server that cannot confirm V10.7 has nothing to select. Our
        // one measured server confirms V10.7; this is for every other one.
        vec![
            CapabilitySet::V10_7 {
                flags: CapabilitiesV107Flags::AVC_DISABLED | CapabilitiesV107Flags::SMALL_CACHE,
            },
            CapabilitySet::V8 {
                flags: ironrdp_egfx::pdu::CapabilitiesV8Flags::SMALL_CACHE,
            },
        ]
    }

    fn on_reset_graphics(&mut self, width: u32, height: u32) {
        // Per MS-RDPEGFX the reset implicitly destroys every surface, and the upstream
        // client clears its own table here. Mirror that, or a stale surface stays mapped
        // to output and the window keeps showing the previous desktop.
        let ids: Vec<u16> = self.live_surfaces.drain().collect();
        self.with_store(|store| {
            for id in ids {
                store.delete(id);
            }
        });
        // Clear BOTH sides. Clearing only the local mirror leaves the store holding
        // pixels for slots the handler no longer knows about, so a later CacheToSurface
        // computes (0,0), drops every point, and counts an error instead of painting.
        self.cache_dims.clear();
        self.with_store(|store| store.clear_cache());
        self.stats
            .note(|s| s.reset_graphics = Some((width, height)));
    }

    fn on_surface_created(&mut self, surface: &EgfxSurface) {
        self.live_surfaces.insert(surface.id);
        let (id, width, height) = (surface.id, surface.width, surface.height);
        self.with_store(|store| store.create(id, width, height));
        self.stats
            .note(|s| s.surfaces_created = s.surfaces_created.saturating_add(1));
    }

    fn on_surface_deleted(&mut self, surface_id: u16) {
        self.live_surfaces.remove(&surface_id);
        self.with_store(|store| store.delete(surface_id));
        self.stats
            .note(|s| s.surfaces_deleted = s.surfaces_deleted.saturating_add(1));
    }

    /// The `MapSurfaceToOutput` callback. `origin_x`/`origin_y` place the surface within
    /// the output; the presenter owns that offset, so the store only records *which*
    /// surface is the visible one.
    fn on_surface_mapped(&mut self, surface_id: u16, _origin_x: u32, _origin_y: u32) {
        self.with_store(|store| store.map_to_output(surface_id));
    }

    /// `MapSurfaceToScaledOutput` — the same job as [`Self::on_surface_mapped`].
    ///
    /// A server may map a surface with any of four PDUs, and each one is dispatched by
    /// `ironrdp-egfx` to its OWN defaulted trait method. Implementing only
    /// `on_surface_mapped` therefore leaves the other three as silent no-ops: the session
    /// connects, decodes frames, reports healthy counters — and shows a black window,
    /// because nothing is ever mapped to output. That is what this client did against a
    /// real server, and no counter could see it, because a defaulted trait method never
    /// reaches `on_unhandled_pdu`.
    ///
    /// The scale factor is ignored on purpose: the renderer letterboxes the surface into
    /// the window itself, so the only thing the store needs is *which* surface is visible.
    fn on_map_surface_to_scaled_output(
        &mut self,
        pdu: &ironrdp_egfx::pdu::MapSurfaceToScaledOutputPdu,
    ) {
        self.with_store(|store| store.map_to_output(pdu.surface_id));
    }

    /// `MapSurfaceToWindow` — see [`Self::on_map_surface_to_scaled_output`].
    ///
    /// Per-window mapping belongs to RemoteApp/RAIL, which is out of scope; treating it
    /// as "this surface is the visible one" is still better than showing nothing.
    fn on_map_surface_to_window(&mut self, pdu: &ironrdp_egfx::pdu::MapSurfaceToWindowPdu) {
        self.with_store(|store| store.map_to_output(pdu.surface_id));
    }

    /// `MapSurfaceToScaledWindow` — see [`Self::on_map_surface_to_scaled_output`].
    fn on_map_surface_to_scaled_window(
        &mut self,
        pdu: &ironrdp_egfx::pdu::MapSurfaceToScaledWindowPdu,
    ) {
        self.with_store(|store| store.map_to_output(pdu.surface_id));
    }

    /// Bitmaps the upstream client decoded itself (uncompressed, and AVC420 if a decoder
    /// is ever configured). Already RGBA by that API's contract, so it is blitted as-is.
    fn on_bitmap_updated(&mut self, update: &BitmapUpdate) {
        self.note_codec(update.codec_id);
        if update.data.is_empty() {
            return;
        }
        let dest = rect_from_egfx(&update.destination_rectangle);
        if dest.is_empty() {
            return;
        }
        let stride = dest.width();
        let result =
            self.with_store(|store| store.blit_rgba(update.surface_id, dest, &update.data, stride));
        self.absorb(result);
    }

    fn on_frame_complete(&mut self, _frame_id: u32) {
        self.stats
            .note(|s| s.frames_completed = s.frames_completed.saturating_add(1));
    }

    fn on_solid_fill(&mut self, pdu: &SolidFillPdu) {
        self.apply_solid_fill(pdu);
    }

    fn on_surface_to_surface(&mut self, pdu: &SurfaceToSurfacePdu) {
        self.apply_surface_to_surface(pdu);
    }

    fn on_surface_to_cache(&mut self, pdu: &SurfaceToCachePdu) {
        self.apply_surface_to_cache(pdu);
    }

    fn on_cache_to_surface(&mut self, pdu: &CacheToSurfacePdu) {
        self.apply_cache_to_surface(pdu);
    }

    /// The ClearCodec seam.
    ///
    /// The compositing PDUs are matched here as well as on their own callbacks. Upstream
    /// routes each of them to exactly one of the two — `handle_pdu` returns after calling
    /// the specific callback and never falls through — so this cannot double-apply, and
    /// it means the handler still behaves correctly if that routing ever changes.
    /// RFX Progressive arrives HERE, not via `on_unhandled_pdu`.
    ///
    /// `ironrdp-egfx` dispatches `WireToSurface2` to this dedicated callback and returns
    /// (client.rs:490-494), so a `WireToSurface2` arm inside `on_unhandled_pdu` is
    /// unreachable. An earlier version had exactly that, and it is why a previous
    /// measurement reported "ClearCodec only" — progressive PDUs were arriving and being
    /// swallowed by the empty upstream default.
    ///
    /// Counting only, for now: decoding progressive needs the surface/tile plumbing that
    /// is P3a's remaining work. Counting it at least makes it visible instead of silent.
    fn on_wire_to_surface2(&mut self, pdu: &ironrdp_egfx::pdu::WireToSurface2Pdu) {
        let name = format!("WireToSurface2/{:?}", pdu.codec_id);
        self.stats
            .note(|s| *s.codec_ids_seen.entry(name).or_insert(0) += 1);
        self.apply_wire_to_surface2(pdu);
    }

    fn on_unhandled_pdu(&mut self, pdu: &GfxPdu) {
        match pdu {
            GfxPdu::WireToSurface1(p) => self.apply_wire_to_surface1(p),
            GfxPdu::SolidFill(p) => self.apply_solid_fill(p),
            GfxPdu::SurfaceToSurface(p) => self.apply_surface_to_surface(p),
            GfxPdu::SurfaceToCache(p) => self.apply_surface_to_cache(p),
            GfxPdu::CacheToSurface(p) => self.apply_cache_to_surface(p),
            GfxPdu::MapSurfaceToOutput(p) => {
                let id = p.surface_id;
                self.with_store(|store| store.map_to_output(id));
            }
            GfxPdu::WireToSurface2(p) => {
                let name = format!("WireToSurface2/{:?}", p.codec_id);
                self.stats
                    .note(|s| *s.codec_ids_seen.entry(name).or_insert(0) += 1);
                self.apply_wire_to_surface2(p);
            }
            _ => self
                .stats
                .note(|s| s.unhandled_pdus = s.unhandled_pdus.saturating_add(1)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_egfx::pdu::{Color, EvictCacheEntryPdu, MapSurfaceToOutputPdu, PixelFormat, Point};
    use ironrdp_graphics::clearcodec::ClearCodecEncoder;

    const BPP: usize = 4;

    fn store() -> Arc<Mutex<SurfaceStore>> {
        Arc::new(Mutex::new(SurfaceStore::new()))
    }

    fn egfx_surface(id: u16, width: u16, height: u16) -> EgfxSurface {
        // `Surface` is #[non_exhaustive], so it cannot be built field-by-field from
        // outside its crate. Round-tripping a CreateSurface PDU through the real client
        // is the only honest way to get one, and it also proves our mirror matches what
        // the client itself would report.
        use ironrdp_dvc::DvcProcessor as _;
        use ironrdp_egfx::client::GraphicsPipelineClient;
        use ironrdp_egfx::pdu::CreateSurfacePdu;

        struct Capture(std::sync::mpsc::Sender<EgfxSurface>);
        impl GraphicsPipelineHandler for Capture {
            fn on_surface_created(&mut self, surface: &EgfxSurface) {
                let _ = self.0.send(surface.clone());
            }
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let mut client = GraphicsPipelineClient::new(Box::new(Capture(tx)), None);
        let payload = encoded_gfx(&GfxPdu::CreateSurface(CreateSurfacePdu {
            surface_id: id,
            width,
            height,
            pixel_format: PixelFormat::XRgb,
        }));
        client
            .process(0, &payload)
            .expect("CreateSurface must parse");
        rx.try_recv()
            .expect("client must report the created surface")
    }

    /// ZGFX-wrap an encoded PDU so `GraphicsPipelineClient::process` accepts it.
    fn encoded_gfx(pdu: &GfxPdu) -> Vec<u8> {
        use ironrdp::core::Encode as _;
        let mut body = vec![0u8; pdu.size()];
        pdu.encode(&mut ironrdp::core::WriteCursor::new(&mut body))
            .expect("encode");
        ironrdp_graphics::zgfx::wrap_uncompressed(&body)
    }

    fn rect(left: u16, top: u16, right: u16, bottom: u16) -> ExclusiveRectangle {
        ExclusiveRectangle {
            left,
            top,
            right,
            bottom,
        }
    }

    fn pixel_at(store: &Arc<Mutex<SurfaceStore>>, id: u16, x: u16, y: u16) -> [u8; 4] {
        let guard = store.lock().unwrap();
        let surface = guard.get(id).expect("surface");
        let off = (y as usize * surface.width as usize + x as usize) * BPP;
        surface.pixels()[off..off + BPP].try_into().unwrap()
    }

    #[test]
    fn egfx_rectangles_are_exclusive_so_conversion_is_field_for_field() {
        // If this ever grew a ±1, every tile would lose (or duplicate) its last row and
        // column — a faint grid over the whole desktop.
        let converted = rect_from_egfx(&rect(10, 20, 30, 45));
        assert_eq!(converted, Rect::new(10, 20, 30, 45));
        assert_eq!(converted.width(), 20);
        assert_eq!(converted.height(), 25);
    }

    #[test]
    fn capabilities_prefer_v10_7_without_avc_and_keep_a_v8_fallback() {
        // V10.7 first, so the modern pipeline wins where it is available. AVC must be
        // explicitly disabled in it, because GraphicsPipelineClient::start filters out
        // every AVC-bearing set when no H.264 decoder is configured — which would
        // silently drop us to V8.
        //
        // V8 second, so a server that cannot confirm V10.7 still has something to pick.
        // It carries no AVC, so it survives the same filter.
        let handler = GfxHandler::new(store());
        let caps = handler.capabilities();
        assert_eq!(caps.len(), 2, "V10.7 preferred, V8 fallback");

        match &caps[0] {
            CapabilitySet::V10_7 { flags } => {
                assert!(
                    flags.contains(CapabilitiesV107Flags::AVC_DISABLED),
                    "without AVC_DISABLED this set is filtered out and we drop to V8"
                );
                assert!(flags.contains(CapabilitiesV107Flags::SMALL_CACHE));
            }
            other => panic!("expected V10_7 first, got {other:?}"),
        }
        assert!(
            matches!(&caps[1], CapabilitySet::V8 { .. }),
            "expected a V8 fallback, got {:?}",
            caps[1]
        );
    }

    #[test]
    fn progressive_arrives_via_on_wire_to_surface2_and_is_counted() {
        // The regression this guards is subtle and cost a wrong published measurement:
        // ironrdp-egfx dispatches WireToSurface2 to its own callback and returns, so a
        // WireToSurface2 arm inside on_unhandled_pdu is unreachable. Progressive PDUs
        // then vanish silently and a codec census reports "ClearCodec only".
        let mut handler = GfxHandler::new(store());
        let pdu = ironrdp_egfx::pdu::WireToSurface2Pdu {
            surface_id: 1,
            codec_context_id: 0,
            codec_id: ironrdp_egfx::pdu::Codec2Type::RemoteFxProgressive,
            pixel_format: ironrdp_egfx::pdu::PixelFormat::XRgb,
            bitmap_data: vec![0u8; 8],
        };
        handler.on_wire_to_surface2(&pdu);

        let s = handler.stats().snapshot();
        assert_eq!(
            s.codec_ids_seen
                .get("WireToSurface2/RemoteFxProgressive")
                .copied(),
            Some(1),
            "progressive must be counted: {:?}",
            s.codec_ids_seen
        );
        assert_eq!(
            s.surface_errors, 1,
            "a surface we never created is a store problem, not a codec one"
        );
        assert_eq!(
            s.undecoded_regions, 0,
            "nothing was wrong with the bytes, so this is not an undecodable region"
        );
    }

    #[test]
    fn a_progressive_stream_we_cannot_decode_is_counted_with_its_reason() {
        // The region stays stale either way, but "we could not decode it" and "we do not
        // have that surface" are different problems, and the reason tally is what makes a
        // systematic decoder fault distinguishable from a scattering of unrelated ones.
        let store = store();
        store
            .lock()
            .unwrap()
            .create(1, PROGRESSIVE_TILE, PROGRESSIVE_TILE);
        let mut handler = GfxHandler::new(store);

        let pdu = ironrdp_egfx::pdu::WireToSurface2Pdu {
            surface_id: 1,
            codec_context_id: 0,
            codec_id: ironrdp_egfx::pdu::Codec2Type::RemoteFxProgressive,
            pixel_format: ironrdp_egfx::pdu::PixelFormat::XRgb,
            // Not a progressive stream. The decoder must refuse it rather than paint.
            bitmap_data: vec![0u8; 8],
        };
        handler.on_wire_to_surface2(&pdu);

        let s = handler.stats().snapshot();
        assert_eq!(s.undecoded_regions, 1, "the region was not painted");
        assert_eq!(
            s.surface_errors, 0,
            "the surface was fine; the bytes were not"
        );
        assert_eq!(
            s.decode_error_reasons.values().sum::<u64>(),
            1,
            "the reason must be recorded, not just the count: {:?}",
            s.decode_error_reasons
        );
    }

    #[test]
    fn surface_lifecycle_is_mirrored_into_the_store() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));

        handler.on_surface_created(&egfx_surface(7, 8, 4));
        assert!(store.lock().unwrap().get(7).is_some());
        assert!(
            store.lock().unwrap().output_surface().is_none(),
            "creating must not map to output"
        );

        handler.on_surface_mapped(7, 0, 0);
        assert!(store.lock().unwrap().output_surface().is_some());

        handler.on_surface_deleted(7);
        assert!(store.lock().unwrap().get(7).is_none());

        let stats = handler.stats().snapshot();
        assert_eq!(stats.surfaces_created, 1);
        assert_eq!(stats.surfaces_deleted, 1);
    }

    #[test]
    fn reset_graphics_destroys_every_surface_it_created() {
        // The spec destroys all surfaces on reset. Miss this and a stale surface stays
        // mapped to output, so the window keeps showing the previous desktop.
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 4, 4));
        handler.on_surface_created(&egfx_surface(2, 4, 4));
        handler.on_surface_mapped(1, 0, 0);

        handler.on_reset_graphics(1920, 1080);

        assert!(store.lock().unwrap().get(1).is_none());
        assert!(store.lock().unwrap().get(2).is_none());
        assert!(store.lock().unwrap().output_surface().is_none());
        assert_eq!(
            handler.stats().snapshot().reset_graphics,
            Some((1920, 1080))
        );
    }

    #[test]
    fn a_clearcodec_tile_is_decoded_swapped_to_rgba_and_placed_at_the_destination() {
        // The end-to-end claim of this module, checked with a colour whose channels are
        // all different: a missing BGRA->RGBA swap, a wrong offset, or a wrong stride
        // each produce a different, visible failure here.
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 8, 8));

        // Encoder input is BGRA: B=0x10, G=0x20, R=0x30.
        let tile_bgra: Vec<u8> = [0x10u8, 0x20, 0x30, 0xFF]
            .iter()
            .copied()
            .cycle()
            .take(2 * 2 * BPP)
            .collect();
        let bitmap_data = ClearCodecEncoder::new().encode(&tile_bgra, 2, 2);

        handler.on_unhandled_pdu(&GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::ClearCodec,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(3, 5, 5, 7),
            bitmap_data,
        }));

        let stats = handler.stats().snapshot();
        assert_eq!(stats.decode_errors, 0, "the round trip must decode");
        assert_eq!(stats.surface_errors, 0);
        assert_eq!(stats.codec_ids_seen.get("ClearCodec"), Some(&1));

        // Stored as RGBA: R=0x30, G=0x20, B=0x10, opaque.
        for (x, y) in [(3, 5), (4, 5), (3, 6), (4, 6)] {
            assert_eq!(
                pixel_at(&store, 1, x, y),
                [0x30, 0x20, 0x10, 0xFF],
                "({x},{y}) must hold the decoded pixel in RGBA order"
            );
        }
        // And nothing outside the destination rectangle moved.
        assert_eq!(pixel_at(&store, 1, 2, 5), [0, 0, 0, 0]);
        assert_eq!(pixel_at(&store, 1, 5, 5), [0, 0, 0, 0]);
        assert_eq!(pixel_at(&store, 1, 3, 4), [0, 0, 0, 0]);
        assert_eq!(pixel_at(&store, 1, 3, 7), [0, 0, 0, 0]);
    }

    #[test]
    fn a_decode_failure_is_counted_and_the_session_carries_on() {
        // A dropped region is a glitch; a propagated error is a dead client.
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 4, 4));

        handler.on_unhandled_pdu(&GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::ClearCodec,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 4, 4),
            bitmap_data: vec![0xFF; 3], // truncated stream: cannot parse
        }));

        let stats = handler.stats().snapshot();
        assert_eq!(stats.decode_errors, 1, "the failure must be counted");
        assert_eq!(stats.codec_ids_seen.get("ClearCodec"), Some(&1));
        assert_eq!(pixel_at(&store, 1, 0, 0), [0, 0, 0, 0], "nothing painted");

        // ...and the handler still works afterwards.
        let tile = ClearCodecEncoder::new().encode(&[0x01, 0x02, 0x03, 0xFF], 1, 1);
        handler.on_unhandled_pdu(&GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::ClearCodec,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 1, 1),
            bitmap_data: tile,
        }));
        assert_eq!(handler.stats().snapshot().decode_errors, 1);
        assert_eq!(pixel_at(&store, 1, 0, 0), [0x03, 0x02, 0x01, 0xFF]);
    }

    #[test]
    fn a_blit_to_an_unknown_surface_is_counted_not_propagated() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        // No surface 9 was ever created.
        let tile = ClearCodecEncoder::new().encode(&[0x01, 0x02, 0x03, 0xFF], 1, 1);
        handler.on_unhandled_pdu(&GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 9,
            codec_id: Codec1Type::ClearCodec,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 1, 1),
            bitmap_data: tile,
        }));

        let stats = handler.stats().snapshot();
        assert_eq!(stats.decode_errors, 0, "the decode itself succeeded");
        assert_eq!(stats.surface_errors, 1, "the store refusal is the count");
    }

    #[test]
    fn solid_fill_reorders_the_wire_colour_and_forces_opaque_alpha() {
        // RDPGFX_COLOR32 is BGRA with a reserved alpha byte. Copying it straight through
        // would paint blue where the server asked for red, and an alpha of 0x00 would
        // paint an invisible rectangle.
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 4, 4));

        handler.on_solid_fill(&SolidFillPdu {
            surface_id: 1,
            fill_pixel: Color {
                b: 0x11,
                g: 0x22,
                r: 0x33,
                xa: 0x00,
            },
            rectangles: vec![rect(1, 1, 3, 2)],
        });

        assert_eq!(pixel_at(&store, 1, 1, 1), [0x33, 0x22, 0x11, 0xFF]);
        assert_eq!(pixel_at(&store, 1, 2, 1), [0x33, 0x22, 0x11, 0xFF]);
        assert_eq!(pixel_at(&store, 1, 0, 1), [0, 0, 0, 0], "outside the rect");
        assert_eq!(
            pixel_at(&store, 1, 1, 2),
            [0, 0, 0, 0],
            "bottom is exclusive"
        );
    }

    #[test]
    fn surface_and_cache_copies_move_the_right_pixels() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 8, 8));
        handler.on_surface_created(&egfx_surface(2, 8, 8));

        // Seed a 2x1 block at (0,0) of surface 1.
        handler.on_solid_fill(&SolidFillPdu {
            surface_id: 1,
            fill_pixel: Color {
                b: 0x11,
                g: 0x22,
                r: 0x33,
                xa: 0xFF,
            },
            rectangles: vec![rect(0, 0, 2, 1)],
        });

        handler.on_surface_to_surface(&SurfaceToSurfacePdu {
            source_surface_id: 1,
            destination_surface_id: 2,
            source_rectangle: rect(0, 0, 2, 1),
            destination_points: vec![Point { x: 4, y: 3 }],
        });
        assert_eq!(pixel_at(&store, 2, 4, 3), [0x33, 0x22, 0x11, 0xFF]);
        assert_eq!(pixel_at(&store, 2, 5, 3), [0x33, 0x22, 0x11, 0xFF]);
        assert_eq!(pixel_at(&store, 2, 6, 3), [0, 0, 0, 0], "copied width is 2");

        handler.on_surface_to_cache(&SurfaceToCachePdu {
            surface_id: 1,
            cache_key: 0,
            cache_slot: 5,
            source_rectangle: rect(0, 0, 2, 1),
        });
        handler.on_cache_to_surface(&CacheToSurfacePdu {
            cache_slot: 5,
            surface_id: 2,
            destination_points: vec![Point { x: 0, y: 7 }],
        });
        assert_eq!(pixel_at(&store, 2, 0, 7), [0x33, 0x22, 0x11, 0xFF]);
        assert_eq!(pixel_at(&store, 2, 1, 7), [0x33, 0x22, 0x11, 0xFF]);

        assert_eq!(handler.stats().snapshot().surface_errors, 0);
    }

    #[test]
    fn a_destination_point_that_would_overflow_a_coordinate_is_dropped_not_panicked() {
        // SurfaceStore builds `x + width` as a u16; in a debug build the overflow is a
        // panic, which would poison the store mutex and end the session. Filtering here
        // is what stops a malformed PDU from doing that.
        let (kept, skipped) = placeable_points(
            &[Point { x: 65_000, y: 0 }, Point { x: 10, y: 20 }],
            1_000,
            1,
        );
        assert_eq!(kept, vec![(10, 20)]);
        assert_eq!(skipped, 1);

        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 8, 8));
        handler.on_surface_to_surface(&SurfaceToSurfacePdu {
            source_surface_id: 1,
            destination_surface_id: 1,
            source_rectangle: rect(0, 0, 1_000, 1),
            destination_points: vec![Point { x: 65_000, y: 0 }],
        });
        assert_eq!(handler.stats().snapshot().surface_errors, 1);
    }

    #[test]
    fn frames_and_unknown_pdus_are_counted_without_touching_pixels() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 2, 2));
        let generation_before = store.lock().unwrap().generation();

        handler.on_frame_complete(1);
        handler.on_frame_complete(2);
        handler.on_unhandled_pdu(&GfxPdu::EvictCacheEntry(EvictCacheEntryPdu {
            cache_slot: 3,
        }));

        let stats = handler.stats().snapshot();
        assert_eq!(stats.frames_completed, 2);
        assert_eq!(stats.unhandled_pdus, 1);
        assert_eq!(
            store.lock().unwrap().generation(),
            generation_before,
            "counting must not mutate the store"
        );
    }

    /// EVERY mapping PDU must end with a surface on screen.
    ///
    /// This is the regression for the bug that made the client show a black window
    /// against a real server. `ironrdp-egfx` dispatches each of the four map PDUs to its
    /// OWN defaulted trait method, so implementing `on_surface_mapped` alone leaves the
    /// other three as silent no-ops — and silent is exact: they never reach
    /// `on_unhandled_pdu`, so `unhandled_pdus` stays 0 and every other counter looks
    /// healthy while nothing is displayed.
    ///
    /// The older test below covers the raw `on_unhandled_pdu` arm, which upstream never
    /// takes; it passed throughout and proved nothing about real behaviour.
    #[test]
    fn every_map_surface_pdu_results_in_something_on_screen() {
        use ironrdp_egfx::pdu::{
            MapSurfaceToScaledOutputPdu, MapSurfaceToScaledWindowPdu, MapSurfaceToWindowPdu,
        };

        // Each closure exercises one of the four callbacks on a fresh handler.
        type Map = (&'static str, fn(&mut GfxHandler));
        let cases: [Map; 4] = [
            ("MapSurfaceToOutput", |h| h.on_surface_mapped(7, 0, 0)),
            ("MapSurfaceToScaledOutput", |h| {
                h.on_map_surface_to_scaled_output(&MapSurfaceToScaledOutputPdu {
                    surface_id: 7,
                    output_origin_x: 0,
                    output_origin_y: 0,
                    target_width: 800,
                    target_height: 600,
                })
            }),
            ("MapSurfaceToWindow", |h| {
                h.on_map_surface_to_window(&MapSurfaceToWindowPdu {
                    surface_id: 7,
                    window_id: 1,
                    mapped_width: 800,
                    mapped_height: 600,
                })
            }),
            ("MapSurfaceToScaledWindow", |h| {
                h.on_map_surface_to_scaled_window(&MapSurfaceToScaledWindowPdu {
                    surface_id: 7,
                    window_id: 1,
                    mapped_width: 800,
                    mapped_height: 600,
                    target_width: 800,
                    target_height: 600,
                })
            }),
        ];

        for (name, apply) in cases {
            let store = store();
            store.lock().unwrap().create(7, 4, 4);
            let mut handler = GfxHandler::new(Arc::clone(&store));
            assert!(
                store.lock().unwrap().output_surface().is_none(),
                "{name}: nothing should be mapped before the PDU"
            );

            apply(&mut handler);

            assert!(
                store.lock().unwrap().output_surface().is_some(),
                "{name} left nothing mapped to output — the window would render black"
            );
        }
    }

    #[test]
    fn map_surface_to_output_arriving_as_a_raw_pdu_still_sets_the_output() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(4, 2, 2));
        handler.on_unhandled_pdu(&GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
            surface_id: 4,
            output_origin_x: 0,
            output_origin_y: 0,
        }));
        assert!(store.lock().unwrap().output_surface().is_some());
    }

    #[test]
    fn a_non_clearcodec_wire_to_surface_1_is_counted_and_dropped() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 4, 4));

        handler.on_unhandled_pdu(&GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::Avc444,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 4, 4),
            bitmap_data: vec![0u8; 16],
        }));

        let stats = handler.stats().snapshot();
        assert_eq!(stats.codec_ids_seen.get("Avc444"), Some(&1));
        assert_eq!(stats.unhandled_pdus, 1);
        assert_eq!(stats.decode_errors, 0, "we never fed it to the decoder");
        assert_eq!(pixel_at(&store, 1, 0, 0), [0, 0, 0, 0]);
    }

    /// The DWT's i32 -> i16 narrowing must SATURATE, not wrap.
    ///
    /// The inverse transform narrows an i32 intermediate to i16 at roughly twenty sites.
    /// Truncating (`value as i16`) wraps, so a value one past the rail flips sign — a
    /// bright sample becomes a dark one — and the lifting steps then spread that error
    /// into its neighbours. FreeRDP clamps at every one of these sites (`clampi16`,
    /// libfreerdp/codec/progressive.c:591).
    #[test]
    fn the_dwt_narrowing_saturates_rather_than_wrapping() {
        use ironrdp_graphics::dwt_extrapolate::t;

        // In range: unchanged.
        assert_eq!(t(0), 0);
        assert_eq!(t(1234), 1234);
        assert_eq!(t(-1234), -1234);
        assert_eq!(t(i32::from(i16::MAX)), i16::MAX);
        assert_eq!(t(i32::from(i16::MIN)), i16::MIN);

        // One past the rail is where wrapping and saturation disagree: `as i16` gives
        // i16::MIN here, a full sign flip.
        assert_eq!(t(i32::from(i16::MAX) + 1), i16::MAX);
        assert_eq!(t(i32::from(i16::MIN) - 1), i16::MIN);

        // Far out of range, both directions.
        assert_eq!(t(1_000_000), i16::MAX);
        assert_eq!(t(-1_000_000), i16::MIN);
    }

    /// A single-entry RLEX palette still uses ONE stop-index bit, not zero.
    ///
    /// FreeRDP computes `numBits = CLEAR_LOG2_FLOOR[paletteCount - 1] + 1`, and
    /// CLEAR_LOG2_FLOOR[0] is 0, so a one-colour palette gives numBits = 1. Treating it as
    /// zero bits took a different parsing path that read ONE byte per segment instead of
    /// the two the format always carries — the packed index/depth byte, then the run
    /// length — so twice as many segments were produced and the region overran with
    /// "rlex: suite exceeds region pixel count".
    ///
    /// Here the payload is: paletteCount = 1, one BGR colour, then a single segment whose
    /// packed byte is 0 (stopIndex 0, suiteDepth 0) and whose run length is 5.
    #[test]
    fn rlex_single_entry_palette_reads_two_bytes_per_segment() {
        use ironrdp::pdu::codecs::clearcodec::decode_rlex;

        let data = [1u8, 0x10, 0x20, 0x30, 0x00, 0x05];
        let rlex = decode_rlex(&data).expect("a one-colour palette must decode");

        assert_eq!(rlex.palette.len(), 1);
        assert_eq!(
            rlex.segments.len(),
            1,
            "two bytes is ONE segment; reading a byte at a time yields two"
        );
        assert_eq!(rlex.segments[0].run_length, 5);
        assert_eq!(rlex.segments[0].start_index, 0);
        assert_eq!(rlex.segments[0].stop_index, 0);
    }

    /// ClearCodec SHORT_VBAR_CACHE_MISS: yOn is the LOW 8 bits, yOff is bits 13:8.
    ///
    /// The two were transposed — yOn was read from bits 13:6 and yOff from bits 5:0. That
    /// makes yOn range to 255 while yOff caps at 63, so the `yOff < yOn` validity check
    /// fires for any yOn above 63 and the tile is rejected outright. Against a live host
    /// that rejected 84 of 222 ClearCodec commands, and the v-bars those tiles would have
    /// cached never existed, so a further 92 failed later with "cache miss on hit".
    ///
    /// This word encodes yOn = 10, yOff = 20 as FreeRDP reads it
    /// (libfreerdp/codec/clear.c): `(20 << 8) | 10` = 0x140A. Under the old reading it
    /// decodes as yOn = 80, yOff = 10 — and is refused.
    #[test]
    fn clearcodec_short_vbar_takes_y_on_from_the_low_byte() {
        use ironrdp::pdu::codecs::clearcodec::decode_bands_layer;

        let mut data = Vec::new();
        data.extend_from_slice(&0u16.to_le_bytes()); // xStart
        data.extend_from_slice(&0u16.to_le_bytes()); // xEnd  -> one v-bar
        data.extend_from_slice(&0u16.to_le_bytes()); // yStart
        data.extend_from_slice(&51u16.to_le_bytes()); // yEnd -> band height 52
        data.extend_from_slice(&[0x11, 0x22, 0x33]); // background cb, cg, cr
        data.extend_from_slice(&0x140Au16.to_le_bytes()); // SHORT_VBAR_CACHE_MISS
        data.extend_from_slice(&[0x40u8; 30]); // (20 - 10) pixels * 3 bytes

        let bands = decode_bands_layer(&data).expect("the band must decode");
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].vbars.len(), 1);

        match &bands[0].vbars[0] {
            ironrdp::pdu::codecs::clearcodec::VBar::ShortCacheMiss(m) => {
                assert_eq!(m.y_on, 10, "yOn comes from the low 8 bits");
                assert_eq!(m.y_off_delta, 10, "yOff (20) - yOn (10)");
                assert_eq!(m.pixel_data.len(), 30);
            }
            other => panic!("expected a short cache miss, got {other:?}"),
        }
    }

    /// The SRL reader, hand-traced from FreeRDP's `progressive_rfx_srl_read`.
    ///
    /// For `data = 0b1000_1000` and num_bits = 3:
    ///   bit 1 = 1  -> escape; the next symbol is unary; k = kp / 8 = 8 / 8 = 1
    ///   bit 2 = 0  -> short-run length 0, so fall through to unary
    ///   bit 3 = 0  -> sign bit, positive; kp := 8 - 6 = 2
    ///   mag starts at 1, max = (1 << 3) - 1 = 7
    ///   bit 4 = 0  -> mag := 2
    ///   bit 5 = 1  -> stop
    /// giving +2, having consumed exactly five bits.
    ///
    /// The previous implementation started kp at 0 (so k = 0, and bit 2 was never read)
    /// and decoded magnitudes as a Golomb-Rice quotient plus remainder bits — a different
    /// value AND a different bit count, which desynchronised the stream for every
    /// coefficient after it.
    #[test]
    fn srl_reads_a_bounded_unary_magnitude() {
        use ironrdp_graphics::progressive::SrlReader;
        let data = [0b1000_1000u8];
        let mut srl = SrlReader::new(&data);
        assert_eq!(srl.read(3), 2);
    }

    /// A leading `0` is a run of `1 << k` zeros, and with kp starting at 8 that k is 1.
    ///
    /// So the first TWO reads come from one bit. With kp = 0 the run would be a single
    /// zero and the second read would consume a bit FreeRDP does not — the streams
    /// diverge on the very first symbol of every component.
    #[test]
    fn srl_zero_run_length_follows_kp_starting_at_eight() {
        use ironrdp_graphics::progressive::SrlReader;
        let data = [0b0100_0000u8];
        let mut srl = SrlReader::new(&data);
        assert_eq!(srl.read(3), 0, "first zero of the run");
        assert_eq!(srl.read(3), 0, "second zero, consuming no further bit");
    }

    /// num_bits == 1 is the degenerate case: a sign bit, magnitude always 1.
    #[test]
    fn srl_with_one_bit_returns_plus_or_minus_one() {
        use ironrdp_graphics::progressive::SrlReader;
        // escape, run length 0, then sign = 1 (negative).
        let data = [0b1010_0000u8];
        let mut srl = SrlReader::new(&data);
        assert_eq!(srl.read(1), -1);
    }

    /// The reader is stateful, which is what lets ONE stream serve all ten bands of a
    /// component. A fresh reader restarts; a shared one must not.
    #[test]
    fn srl_state_persists_across_reads() {
        use ironrdp_graphics::progressive::SrlReader;
        let data = [0b1000_1000u8, 0b1000_1000u8];
        let mut shared = SrlReader::new(&data);
        let first = shared.read(3);
        let second = shared.read(3);

        let mut fresh = SrlReader::new(&data);
        assert_eq!(
            fresh.read(3),
            first,
            "a fresh reader sees the same first symbol"
        );
        assert_ne!(
            (first, second),
            (first, first),
            "the second read must continue the stream, not restart it"
        );
    }

    /// The RFX YCbCr->RGB conversion, pinned against hand-computed values.
    ///
    /// Every expected value below is worked out by hand from FreeRDP's formula, not read
    /// off our own implementation — an assertion checked against a constant the code also
    /// derives can only ever agree with itself.
    #[test]
    fn rfx_ycbcr_matches_hand_computed_values() {
        use ironrdp_graphics::progressive::rfx_ycbcr_to_rgb;

        // Zero coefficients: Y = 4096 << 16, so every channel is 4096 >> 5 = 128.
        assert_eq!(rfx_ycbcr_to_rgb(0, 0, 0), (128, 128, 128));

        // cr = 1000, by hand:
        //   R = ((1000*91916 + 4096*65536) >> 16) >> 5 = (360351456 >> 16) >> 5 = 5498 >> 5 = 171
        //   G = ((268435456 - 1000*46820)   >> 16) >> 5 = (221615456 >> 16) >> 5 = 3381 >> 5 = 105
        //   B = (268435456 >> 16) >> 5 = 128
        assert_eq!(rfx_ycbcr_to_rgb(0, 0, 1000), (171, 105, 128));
    }

    /// The scale is what stops a bright coefficient clipping.
    ///
    /// This is the regression for the bug that made every photographic region render as a
    /// flat block of its own average colour. Without the >> 5, a luma of 4000 becomes
    /// 4000 + 128 = 4128 and clamps to 255 — as does almost every other coefficient, so
    /// all detail collapses to the same saturated value. With it, 4000 is a mid-tone.
    #[test]
    fn a_bright_luma_is_scaled_rather_than_clipped() {
        use ironrdp_graphics::progressive::rfx_ycbcr_to_rgb;

        // (4000 + 4096) >> 5 = 8096 >> 5 = 253 — bright, but NOT saturated.
        assert_eq!(rfx_ycbcr_to_rgb(4000, 0, 0), (253, 253, 253));

        // A mid coefficient must land mid-range, not at the top of it.
        let (r, _, _) = rfx_ycbcr_to_rgb(2000, 0, 0);
        assert_eq!(r, 190, "(2000 + 4096) >> 5 = 190");
        assert!(r < 255, "the old +128 form clipped this to 255");
    }

    #[test]
    fn luma_at_the_bottom_of_the_range_is_black_not_wrapped() {
        use ironrdp_graphics::progressive::rfx_ycbcr_to_rgb;
        // Y = (-4096 + 4096) << 16 = 0.
        assert_eq!(rfx_ycbcr_to_rgb(-4096, 0, 0), (0, 0, 0));
        // Below that must clamp, never wrap to white.
        assert_eq!(rfx_ycbcr_to_rgb(-8000, 0, 0), (0, 0, 0));
    }

    #[test]
    fn a_progressive_tile_lands_at_its_grid_position() {
        // Grid indices, not pixels. Hand-computed: tile (0,0) is the origin, tile (2,3)
        // starts at (128, 192) and is one tile wide and tall.
        assert_eq!(progressive_tile_rect(0, 0), Rect::new(0, 0, 64, 64));
        let r = progressive_tile_rect(2, 3);
        assert_eq!(r, Rect::new(128, 192, 192, 256));
        assert_eq!(r.width(), PROGRESSIVE_TILE);
        assert_eq!(r.height(), PROGRESSIVE_TILE);
    }

    #[test]
    fn adjacent_progressive_tiles_touch_without_overlapping_or_gapping() {
        // Exclusive rectangles: tile n's right edge is tile n+1's left edge. A ±1 here is
        // a one-pixel seam or a one-pixel double-draw across the whole desktop.
        let a = progressive_tile_rect(0, 0);
        let b = progressive_tile_rect(1, 0);
        assert_eq!(a.right, b.left);
        let c = progressive_tile_rect(0, 1);
        assert_eq!(a.bottom, c.top);
    }

    #[test]
    fn a_progressive_tile_at_the_far_edge_of_the_grid_does_not_wrap() {
        // u16 grid indices near the top of the range must saturate rather than wrap into
        // the top-left corner, which would paint far-edge tiles over the wrong region.
        let r = progressive_tile_rect(u16::MAX, u16::MAX);
        assert_eq!(r.left, u16::MAX);
        assert_eq!(r.right, u16::MAX);
        assert!(
            r.is_empty(),
            "a saturated tile paints nothing rather than wrapping"
        );
    }

    #[test]
    fn stats_snapshots_are_copies_shared_across_handles() {
        let handler = GfxHandler::new(store());
        let handle = handler.stats();
        let before = handle.snapshot();
        handler.stats.note(|s| s.frames_completed += 1);
        assert_eq!(before.frames_completed, 0, "snapshot must not alias");
        assert_eq!(handle.snapshot().frames_completed, 1);
    }
}
