//! The EGFX graphics handler: PDUs in, pixels in the [`SurfaceStore`] out.
//!
//! `ironrdp-egfx` parses the wire and tracks surfaces as metadata, but it does not keep the
//! pixels presented by this client. Uncompressed updates are routed directly, while
//! ClearCodec and RFX Progressive reach callbacks that also report their exact painted
//! regions for AVC444 invalidation. This module turns all three paths into pixels.
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
    CacheToSurfacePdu, CapabilitiesV107Flags, CapabilitySet, Codec1Type, DeleteEncodingContextPdu,
    EvictCacheEntryPdu, GfxPdu, Point, SolidFillPdu, SurfaceToCachePdu, SurfaceToSurfacePdu,
    WireToSurface1Pdu,
};
use ironrdp_graphics::clearcodec::ClearCodecDecoder;
use ironrdp_graphics::progressive::ProgressiveDecoder;
use serde::Serialize;

use crate::stats::{SlotStatsHandle, UNKNOWN_CODEC};
use crate::surface::{BPP, Rect, SurfaceError, SurfaceStore};

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
    /// Bytes actually painted per codec (4 bytes per pixel of blitted region), keyed
    /// by the same names as `codec_ids_seen`. Only successful paints count — a failed
    /// decode painted nothing, and the diagnostics window must not pretend it did.
    pub codec_bytes_painted: BTreeMap<String, u64>,
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
    /// Surface refusals grouped by a stable, payload-free reason.
    pub surface_error_reasons: BTreeMap<String, u64>,
    /// PDUs that reached us with no handling of their own.
    pub unhandled_pdus: u64,
    /// Regions that arrived in an observed surface codec for which this client has no
    /// decoder. Non-zero means part of the desktop is stale on screen.
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
/// Off unless asked for. The exact rejected bytes make decoder faults reproducible without
/// another live session.
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
    /// The active wire encoding context most recently used successfully for each surface.
    /// Progressive tile state remains keyed by surface, so an obsolete delete must not
    /// retire a state that a rotated context is still refining.
    progressive_contexts: HashMap<u16, u32>,
    stats: GfxStatsHandle,
    /// Surfaces we have mirrored into the store. Surface lifetime ends only at an explicit
    /// `DeleteSurface`; `ResetGraphics` changes the output-buffer dimensions without
    /// destroying surfaces.
    live_surfaces: HashSet<u16>,
    /// Dimensions of each cached bitmap, mirrored so `CacheToSurface` can reject a
    /// destination point that would overflow a `u16` coordinate before the store does
    /// the arithmetic.
    cache_dims: HashMap<u16, (u16, u16)>,
    /// Per-slot cache accounting, for the diagnostics grid. Written here because this is
    /// where a slot's size and the codec that produced its pixels are both known; the
    /// store below sees the bytes but not the codec.
    slots: SlotStatsHandle,
    /// The codec that most recently painted each surface.
    ///
    /// `SurfaceToCache` copies pixels out of a surface and names no codec of its own, so
    /// the codec a slot is attributed to is the one that last wrote the surface it came
    /// from. Protocol constants only — never payload.
    surface_codec: HashMap<u16, &'static str>,
    /// Opt-in dump of tiles the decoder rejected. Default: off.
    capture: FailureCapture,
    /// Advertise the AVC capability ladder (V10.7 with AVC444 implied, V8.1 with
    /// AVC420, V8 fallback) instead of the AVC-free V10.7 set. Only set when the
    /// build carries an H.264 decoder — see [`crate::h264::hardware_decoder`]. The
    /// vendored client additionally filters the ladder against what the configured
    /// decoder actually supports (`H264Decoder::supports_yuv420`), so the two gates
    /// cannot drift into advertising something undecodable.
    avc: bool,
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
            progressive_contexts: HashMap::new(),
            stats: GfxStatsHandle::new(),
            live_surfaces: HashSet::new(),
            cache_dims: HashMap::new(),
            slots: SlotStatsHandle::new(),
            surface_codec: HashMap::new(),
            capture: FailureCapture::default(),
            avc: false,
        }
    }

    /// Dump the bytes of any tile the decoder rejects into `dir`.
    ///
    /// Writes session content, so it is opt-in and capped.
    #[must_use]
    pub fn capturing_failures_to(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.capture = FailureCapture::to_dir(dir);
        self
    }

    /// Advertise AVC support (AVC444 via V10.7, AVC420 via V8.1). Call only when a
    /// decoder is actually configured on the graphics client — advertising a codec
    /// nothing decodes blanks every video region.
    #[must_use]
    pub fn advertising_avc(mut self) -> Self {
        self.avc = true;
        self
    }

    /// A handle the caller keeps after the handler is boxed into the graphics client.
    pub fn stats(&self) -> GfxStatsHandle {
        self.stats.clone()
    }

    /// Per-cache-slot counters, for the diagnostics grid. Poll
    /// [`snapshot`](SlotStatsHandle::snapshot) from any thread.
    pub fn slot_stats(&self) -> SlotStatsHandle {
        self.slots.clone()
    }

    /// Remember which codec last painted a surface — see [`Self::surface_codec`].
    fn note_surface_codec(&mut self, surface_id: u16, codec: &'static str) {
        self.surface_codec.insert(surface_id, codec);
    }

    fn with_store<R>(&self, f: impl FnOnce(&mut SurfaceStore) -> R) -> R {
        let mut guard = self
            .store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }

    /// Record a store result, counting a refusal rather than propagating it.
    fn absorb(&self, result: Result<(), SurfaceError>) {
        if let Err(error) = result {
            let reason = match error {
                SurfaceError::NoSuchSurface(_) => "no_such_surface",
                SurfaceError::NoSuchCacheSlot(_) => "no_such_cache_slot",
                SurfaceError::ShortSource { .. } => "short_source",
                // Native-transport-only errors; the EGFX path never produces them.
                SurfaceError::SizeMismatch { .. } => "size_mismatch",
                SurfaceError::OutOfBounds { .. } => "out_of_bounds",
                SurfaceError::MoveSizeMismatch { .. } => "move_size_mismatch",
            };
            self.note_surface_error(reason, 1);
        }
    }

    fn note_surface_error(&self, reason: &'static str, count: u64) {
        if count == 0 {
            return;
        }
        self.stats.note(|stats| {
            stats.surface_errors = stats.surface_errors.saturating_add(count);
            let total = stats
                .surface_error_reasons
                .entry(reason.to_owned())
                .or_insert(0);
            *total = total.saturating_add(count);
        });
    }

    fn note_codec(&self, codec: Codec1Type) {
        let name = codec_name(codec);
        self.stats.note(|s| {
            *s.codec_ids_seen.entry(name.to_owned()).or_insert(0) += 1;
        });
    }

    /// Record `area_px` pixels painted by `name` (4 bytes each).
    fn note_painted(&self, name: &str, area_px: u64) {
        let bytes = area_px.saturating_mul(4);
        self.stats.note(|s| {
            let total = s.codec_bytes_painted.entry(name.to_owned()).or_insert(0);
            *total = total.saturating_add(bytes);
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
    fn apply_wire_to_surface2(
        &mut self,
        pdu: &ironrdp_egfx::pdu::WireToSurface2Pdu,
    ) -> Vec<ExclusiveRectangle> {
        let surface_id = pdu.surface_id;
        self.note_surface_codec(surface_id, codec2_name(pdu.codec_id));
        let Some((width, height)) =
            self.with_store(|store| store.get(surface_id).map(|s| (s.width, s.height)))
        else {
            // The server referenced a surface we never created. Counted as a store error
            // rather than a decode error: nothing was wrong with the bytes.
            self.note_surface_error("no_such_surface", 1);
            return Vec::new();
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
                return Vec::new();
            }
        };

        // Progressive state is keyed by surface so a rotated wire context can keep
        // refining the same tile grid. Remember the context only after a successful
        // decode: a malformed update must not make a later delete retire live state.
        self.progressive_contexts
            .insert(surface_id, pdu.codec_context_id);

        let mut painted_px: u64 = 0;
        let mut painted_regions = Vec::new();
        for tile in tiles {
            let rect = progressive_tile_rect(tile.x_idx, tile.y_idx);
            // `pixels` is a full 64x64 RGBA tile, so the source stride is the tile side
            // even when the destination is clipped at the surface edge. Passing the
            // clipped width instead shears the tile — the same trap `blit_rgba` documents.
            let result = self.with_store(|store| {
                store.blit_rgba(surface_id, rect, &tile.pixels, PROGRESSIVE_TILE)
            });
            if result.is_ok() {
                painted_px =
                    painted_px.saturating_add(u64::from(rect.width()) * u64::from(rect.height()));
                painted_regions.push(ExclusiveRectangle {
                    left: rect.left,
                    top: rect.top,
                    right: rect.right,
                    bottom: rect.bottom,
                });
            }
            self.absorb(result);
        }
        self.note_painted(&format!("WireToSurface2/{:?}", pdu.codec_id), painted_px);
        painted_regions
    }

    /// Decode a ClearCodec tile and blit it into its surface.
    ///
    /// The decode happens **outside** the store lock: it is the expensive step, and
    /// holding the lock across it would stall the presenter for no reason.
    fn apply_wire_to_surface1(&mut self, pdu: &WireToSurface1Pdu) -> Vec<ExclusiveRectangle> {
        self.note_codec(pdu.codec_id);
        self.note_surface_codec(pdu.surface_id, codec_name(pdu.codec_id));

        if pdu.codec_id != Codec1Type::ClearCodec {
            // Some other codec we do not decode. Counted above; nothing to paint.
            self.stats
                .note(|s| s.unhandled_pdus = s.unhandled_pdus.saturating_add(1));
            return Vec::new();
        }

        let dest = rect_from_egfx(&pdu.destination_rectangle);
        if dest.is_empty() {
            return Vec::new();
        }

        // Seed the decode with what is already on the surface. ClearCodec's layers need
        // not cover the whole tile, and every pixel they skip must keep its current
        // content — decoding over black and blitting the lot paints a black rectangle
        // wherever a tile carries only a small region.
        let existing = self
            .with_store(|store| {
                store
                    .get(pdu.surface_id)
                    .and_then(|surface| surface.extract_with_zero_padding(dest))
            })
            .map(|mut rgba| {
                // The surface stores RGBA; the ClearCodec decoder works in BGRA and its
                // output is swapped back below. Seeding without this swap would leave
                // every pixel the codec does NOT overwrite with red and blue exchanged.
                SurfaceStore::bgra_to_rgba_in_place(&mut rgba);
                rgba
            });

        let decoded = match self.decoder.decode_over_with_coverage(
            &pdu.bitmap_data,
            dest.width(),
            dest.height(),
            existing,
        ) {
            Ok(decoded) => decoded,
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
                return Vec::new();
            }
        };

        let mut pixels = decoded.pixels;
        let coverage: Vec<Rect> = decoded
            .written_regions
            .into_iter()
            .map(|region| {
                Rect::new(
                    dest.left.saturating_add(region.left),
                    dest.top.saturating_add(region.top),
                    dest.left.saturating_add(region.right),
                    dest.top.saturating_add(region.bottom),
                )
            })
            .collect();
        SurfaceStore::bgra_to_rgba_in_place(&mut pixels);
        // The decoder produced rows at the UNCLIPPED rect width — that is what it was
        // asked for. Passing the clipped width instead shears the tile.
        let stride = dest.width();
        let result = self.with_store(|store| {
            store.blit_rgba_with_coverage(pdu.surface_id, dest, &pixels, stride, &coverage)
        });
        match result {
            Ok(written) if written > 0 => {
                self.note_painted(codec_name(pdu.codec_id), (written / BPP) as u64);
                coverage
                    .into_iter()
                    .map(|rect| ExclusiveRectangle {
                        left: rect.left,
                        top: rect.top,
                        right: rect.right,
                        bottom: rect.bottom,
                    })
                    .collect()
            }
            Ok(_) => Vec::new(),
            Err(error) => {
                self.absorb(Err(error));
                Vec::new()
            }
        }
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
        if let Ok(Some((width, height))) = &result {
            self.cache_dims.insert(pdu.cache_slot, (*width, *height));
            let codec = self
                .surface_codec
                .get(&pdu.surface_id)
                .copied()
                .unwrap_or(UNKNOWN_CODEC);
            // The same dimensions `cache_dims` records, so the grid's size and its byte
            // figure can never disagree with each other.
            let bytes = u64::from(*width) * u64::from(*height) * BPP as u64;
            let (w, h, slot) = (*width, *height, pdu.cache_slot);
            self.slots.update(|s| s.fill(slot, w, h, codec, bytes));
        }
        self.absorb(result.map(|_| ()));
    }

    fn apply_cache_to_surface(&mut self, pdu: &CacheToSurfacePdu) {
        let Some((w, h)) = self.cache_dims.get(&pdu.cache_slot).copied() else {
            if !pdu.destination_points.is_empty() {
                self.note_surface_error("unknown_cache_dimensions", 1);
            }
            return;
        };
        let (points, skipped) = placeable_points(&pdu.destination_points, w, h);
        self.note_skipped(skipped);
        if points.is_empty() {
            return;
        }
        let result = self
            .with_store(|store| store.cache_to_surface(pdu.cache_slot, pdu.surface_id, &points));
        if result.is_ok() {
            // One hit per destination painted, matching the aggregate CacheStats. A
            // refused copy painted nothing, so it is not a hit.
            let (slot, times) = (pdu.cache_slot, points.len() as u32);
            self.slots.update(|s| s.hit(slot, times));
        }
        self.absorb(result);
    }

    fn note_skipped(&self, skipped: u64) {
        self.note_surface_error("coordinate_overflow", skipped);
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

/// A stable name per `WireToSurface2` codec id. Protocol constants, never payload.
fn codec2_name(codec: ironrdp_egfx::pdu::Codec2Type) -> &'static str {
    match codec {
        ironrdp_egfx::pdu::Codec2Type::RemoteFxProgressive => "RemoteFxProgressive",
    }
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
    /// Advertise the highest capability set whose codecs this build can actually decode.
    ///
    /// **Without an H.264 decoder:** V10.7 with AVC explicitly disabled, and nothing
    /// else. The upstream default advertises V10.7 with AVC implied, and
    /// `GraphicsPipelineClient::start` filters out every AVC-bearing set when no H.264
    /// decoder is configured — so a client without one silently drops to V8, an older
    /// pipeline than the server would otherwise use. Saying "V10.7, and no AVC please"
    /// survives that filter and keeps the modern pipeline.
    ///
    /// **With an H.264 decoder ([`advertising_avc`](GfxHandler::advertising_avc)):**
    /// the AVC ladder. V10.7 without AVC_DISABLED implies AVC444 — the codec modern
    /// Windows actually sends when AVC is on (measured on quench: Avc444v2 for the
    /// whole desktop) — then V8.1 with AVC420 for a downlevel server, then V8. The
    /// vendored client filters this ladder against `H264Decoder::supports_yuv420`,
    /// so a decoder without YUV output structurally falls back to the V8.1 rung.
    fn capabilities(&self) -> Vec<CapabilitySet> {
        if self.avc {
            return vec![
                CapabilitySet::V10_7 {
                    flags: CapabilitiesV107Flags::SMALL_CACHE,
                },
                CapabilitySet::V8_1 {
                    flags: ironrdp_egfx::pdu::CapabilitiesV81Flags::AVC420_ENABLED
                        | ironrdp_egfx::pdu::CapabilitiesV81Flags::SMALL_CACHE,
                },
                CapabilitySet::V8 {
                    flags: ironrdp_egfx::pdu::CapabilitiesV8Flags::SMALL_CACHE,
                },
            ];
        }
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
        // MS-RDPEGFX 3.3.5.14 changes the Graphics Output Buffer dimensions only. It does
        // not delete offscreen surfaces or bitmap-cache entries; those have explicit
        // DeleteSurface and EvictCacheEntry PDUs. Windows references existing cache slots
        // immediately after this reset, so clearing them here creates visible stale areas.
        //
        // ironrdp-egfx 0.3 clears its private surface metadata, but this handler's store is
        // the authoritative pixel state for mdrdp and must follow the wire specification.
        //
        // The ClearCodec caches survive the reset too — measured, not assumed: resetting
        // the decoder here produced 74 "V-bar cache miss on hit" failures on the very
        // next repaint, because Windows keeps referencing V-bars it cached before the
        // reset. FreeRDP resets codec state here (freerdp_client_codecs_reset), but its
        // ClearCodec reset does not break these hits in practice and ours measurably
        // does. Codec state dies with the SURFACE (see on_surface_deleted), never with
        // the reset.
        self.with_store(|store| {
            store.abort_frame();
            let _ = store.set_graphics_output_size(width, height);
        });
        self.stats
            .note(|s| s.reset_graphics = Some((width, height)));
    }

    fn on_frame_start(&mut self, frame_id: u32) {
        self.with_store(|store| store.begin_frame(frame_id));
    }

    fn on_frame_aborted(&mut self, _frame_id: u32) {
        self.with_store(SurfaceStore::abort_frame);
    }

    fn on_close(&mut self) {
        self.with_store(SurfaceStore::abort_frame);
    }

    fn on_surface_created(&mut self, surface: &EgfxSurface) {
        // CreateSurface may reuse an id without a preceding DeleteSurface. The new
        // incarnation must not inherit the old surface's progressive tile grid or
        // active wire context.
        self.progressive.delete_context(u32::from(surface.id));
        self.progressive_contexts.remove(&surface.id);
        self.live_surfaces.insert(surface.id);
        let (id, width, height) = (surface.id, surface.width, surface.height);
        self.with_store(|store| store.create(id, width, height));
        self.stats
            .note(|s| s.surfaces_created = s.surfaces_created.saturating_add(1));
    }

    fn on_surface_deleted(&mut self, surface_id: u16) {
        self.live_surfaces.remove(&surface_id);
        self.surface_codec.remove(&surface_id);
        self.with_store(|store| store.delete(surface_id));
        // Progressive tile state is keyed by surface id (see apply_wire_to_surface2), so
        // it dies with the surface — FreeRDP's gdi_DeleteSurface calls
        // progressive_delete_surface_context the same way. A recreated surface with the
        // same id must start from empty tile state, not refine the old surface's pixels.
        self.progressive.delete_context(u32::from(surface_id));
        self.progressive_contexts.remove(&surface_id);
        self.stats
            .note(|s| s.surfaces_deleted = s.surfaces_deleted.saturating_add(1));
    }

    /// Map the whole surface into the Graphics Output Buffer at the wire origin.
    fn on_surface_mapped(&mut self, surface_id: u16, origin_x: u32, origin_y: u32) {
        self.with_store(|store| {
            let Some((width, height)) = store
                .get(surface_id)
                .map(|surface| (surface.width, surface.height))
            else {
                return;
            };
            let _ = store.map_to_output_geometry(
                surface_id,
                width,
                height,
                origin_x,
                origin_y,
                u32::from(width),
                u32::from(height),
            );
        });
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
    fn on_map_surface_to_scaled_output(
        &mut self,
        pdu: &ironrdp_egfx::pdu::MapSurfaceToScaledOutputPdu,
    ) {
        self.with_store(|store| {
            let Some((width, height)) = store
                .get(pdu.surface_id)
                .map(|surface| (surface.width, surface.height))
            else {
                return;
            };
            let _ = store.map_to_output_geometry(
                pdu.surface_id,
                width,
                height,
                pdu.output_origin_x,
                pdu.output_origin_y,
                pdu.target_width,
                pdu.target_height,
            );
        });
    }

    /// `MapSurfaceToWindow` — see [`Self::on_map_surface_to_scaled_output`].
    ///
    /// Per-window placement belongs to RemoteApp/RAIL, which this desktop client does
    /// not implement. A RAIL mapping must not replace the Graphics Output Buffer.
    fn on_map_surface_to_window(&mut self, _pdu: &ironrdp_egfx::pdu::MapSurfaceToWindowPdu) {}

    /// `MapSurfaceToScaledWindow` — see [`Self::on_map_surface_to_scaled_output`].
    fn on_map_surface_to_scaled_window(
        &mut self,
        _pdu: &ironrdp_egfx::pdu::MapSurfaceToScaledWindowPdu,
    ) {
    }

    /// One AVC444/AVC444v2 PDU (one logical frame).
    ///
    /// Counted here, not per `BitmapUpdate`: the AVC444 path emits one update per
    /// region rect, and a per-update tally would count rects where every other
    /// codec counts PDUs — an incomparable codec mix.
    fn on_avc444_frame(&mut self, codec_id: Codec1Type) {
        self.note_codec(codec_id);
    }

    /// A codec payload the upstream client could not decode and skipped.
    ///
    /// The vendored client's resilience policy skips bad AVC frames rather than
    /// erroring the channel; without this seam those skips are invisible staleness —
    /// requirement 4 (visibility) reproduced inside the new codec. Counted with the
    /// same reason-tally shape the ClearCodec path uses.
    fn on_decode_failure(&mut self, codec_id: Codec1Type, reason: &str) {
        let reason = format!("{}: {reason}", codec_name(codec_id));
        self.stats.note(|s| {
            s.decode_errors = s.decode_errors.saturating_add(1);
            *s.decode_error_reasons.entry(reason).or_insert(0) += 1;
        });
    }

    /// Bitmaps the upstream client decoded itself (uncompressed, and AVC420 if a decoder
    /// is ever configured). Already RGBA by that API's contract, so it is blitted as-is.
    fn on_bitmap_updated(&mut self, update: &BitmapUpdate) {
        // AVC444 frames are already counted via on_avc444_frame (one per PDU, not
        // one per emitted rect).
        if !matches!(update.codec_id, Codec1Type::Avc444 | Codec1Type::Avc444v2) {
            self.note_codec(update.codec_id);
        }
        // Cache attribution wants the painter's codec either way.
        self.note_surface_codec(update.surface_id, codec_name(update.codec_id));
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
        if result.is_ok() {
            // Painted bytes are attributed per rect for every codec on this path
            // (AVC444 included) — the one-per-PDU rule above is about codec_ids_seen
            // only. Without this the title bar's codec segment never names AVC444:
            // codec_note diffs codec_bytes_painted, and a pure-AVC444 session left
            // that map empty.
            self.note_painted(
                codec_name(update.codec_id),
                u64::from(dest.width()) * u64::from(dest.height()),
            );
        }
        self.absorb(result);
    }

    fn on_frame_complete(&mut self, frame_id: u32) {
        self.with_store(|store| {
            let _ = store.commit_frame(frame_id);
        });
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

    fn on_evict_cache_entry(&mut self, pdu: &EvictCacheEntryPdu) {
        self.cache_dims.remove(&pdu.cache_slot);
        // The slot record survives the eviction on purpose: what it served while live is
        // what says whether dropping it cost anything.
        let slot = pdu.cache_slot;
        self.slots.update(|s| s.evict(slot));
        self.with_store(|store| store.evict_cache(pdu.cache_slot));
    }

    fn on_delete_encoding_context(&mut self, pdu: &DeleteEncodingContextPdu) {
        // Context ids can rotate while a surface's progressive tile state remains
        // live. Retire the surface only when the server deletes the context currently
        // associated with it; an obsolete delete must be harmless.
        if self.progressive_contexts.get(&pdu.surface_id).copied() == Some(pdu.codec_context_id) {
            self.progressive.delete_context(u32::from(pdu.surface_id));
            self.progressive_contexts.remove(&pdu.surface_id);
        }
    }

    /// The ClearCodec seam.
    ///
    /// The compositing PDUs are matched here as well as on their own callbacks. Upstream
    /// routes each of them to exactly one of the two — `handle_pdu` returns after calling
    /// the specific callback and never falls through — so this cannot double-apply, and
    /// it means the handler still behaves correctly if that routing ever changes.
    /// RFX Progressive arrives through the dedicated region-reporting callback, not
    /// through `on_unhandled_pdu`.
    ///
    /// `ironrdp-egfx` dispatches `WireToSurface2` to this dedicated callback and returns
    /// (client.rs:490-494), so a `WireToSurface2` arm inside `on_unhandled_pdu` is
    /// unreachable. An earlier version had exactly that, and it is why a previous
    /// measurement reported "ClearCodec only" — progressive PDUs were arriving and being
    /// swallowed by the empty upstream default.
    ///
    /// The callback records the wire codec name, then decodes and paints every updated
    /// progressive tile through the session-long decoder above.
    fn on_wire_to_surface2(&mut self, pdu: &ironrdp_egfx::pdu::WireToSurface2Pdu) {
        let name = format!("WireToSurface2/{:?}", pdu.codec_id);
        self.stats
            .note(|s| *s.codec_ids_seen.entry(name).or_insert(0) += 1);
        let _ = self.apply_wire_to_surface2(pdu);
    }

    fn on_wire_to_surface2_regions(
        &mut self,
        pdu: &ironrdp_egfx::pdu::WireToSurface2Pdu,
    ) -> Option<Vec<ExclusiveRectangle>> {
        let name = format!("WireToSurface2/{:?}", pdu.codec_id);
        self.stats
            .note(|s| *s.codec_ids_seen.entry(name).or_insert(0) += 1);
        Some(self.apply_wire_to_surface2(pdu))
    }

    fn on_unhandled_wire_to_surface1(
        &mut self,
        pdu: &WireToSurface1Pdu,
    ) -> Option<Vec<ExclusiveRectangle>> {
        Some(self.apply_wire_to_surface1(pdu))
    }

    fn on_unhandled_pdu(&mut self, pdu: &GfxPdu) {
        match pdu {
            GfxPdu::WireToSurface1(p) => {
                let _ = self.apply_wire_to_surface1(p);
            }
            GfxPdu::SolidFill(p) => self.apply_solid_fill(p),
            GfxPdu::SurfaceToSurface(p) => self.apply_surface_to_surface(p),
            GfxPdu::SurfaceToCache(p) => self.apply_surface_to_cache(p),
            GfxPdu::CacheToSurface(p) => self.apply_cache_to_surface(p),
            GfxPdu::MapSurfaceToOutput(p) => {
                self.on_surface_mapped(p.surface_id, p.output_origin_x, p.output_origin_y);
            }
            GfxPdu::WireToSurface2(p) => {
                let name = format!("WireToSurface2/{:?}", p.codec_id);
                self.stats
                    .note(|s| *s.codec_ids_seen.entry(name).or_insert(0) += 1);
                let _ = self.apply_wire_to_surface2(p);
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
    use ironrdp::pdu::geometry::InclusiveRectangle;
    use ironrdp_egfx::client::GraphicsPipelineClient;
    use ironrdp_egfx::decode::{DecodedFrame, DecoderError, DecoderResult, H264Decoder};
    use ironrdp_egfx::pdu::{
        Avc420BitmapStream, Avc444BitmapStream, CacheImportOfferPdu, Color, Encoding,
        MapSurfaceToOutputPdu, PixelFormat, Point, QuantQuality, SurfaceToSurfacePdu,
        WireToSurface1Pdu,
    };
    use ironrdp_graphics::clearcodec::{ClearCodecDecoder, ClearCodecEncoder, ClearCodecRect};

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

    /// Encode the tiny AVC444 container used by the pixel-level scroll regression below.
    /// The fake decoder reads each sub-stream's four-byte marker as `[Y, U, V, aux]`.
    fn avc444_payload(
        encoding: Encoding,
        stream1_rects: Vec<InclusiveRectangle>,
        stream1_data: &[u8],
        stream2: Option<(Vec<InclusiveRectangle>, &[u8])>,
    ) -> Vec<u8> {
        let quant_qualities = |rects: &[InclusiveRectangle]| {
            rects
                .iter()
                .map(|_| QuantQuality {
                    quantization_parameter: 22,
                    progressive: false,
                    quality: 100,
                })
                .collect()
        };
        let stream1 = Avc420BitmapStream {
            quant_qual_vals: quant_qualities(&stream1_rects),
            rectangles: stream1_rects,
            data: stream1_data,
        };
        let stream2 = stream2.map(|(rects, data)| Avc420BitmapStream {
            quant_qual_vals: quant_qualities(&rects),
            rectangles: rects,
            data,
        });
        let stream = Avc444BitmapStream {
            encoding,
            stream1,
            stream2,
        };
        let mut encoded = vec![0u8; stream.size()];
        use ironrdp::core::Encode as _;
        stream
            .encode(&mut ironrdp::core::WriteCursor::new(&mut encoded))
            .expect("AVC444 test stream must encode");
        encoded
    }

    fn avc444_pdu(
        encoding: Encoding,
        stream1_rects: Vec<InclusiveRectangle>,
        stream1_data: &[u8],
        stream2: Option<(Vec<InclusiveRectangle>, &[u8])>,
    ) -> GfxPdu {
        GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::Avc444v2,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 32, 16),
            bitmap_data: avc444_payload(encoding, stream1_rects, stream1_data, stream2),
        })
    }

    fn process_pdu(client: &mut GraphicsPipelineClient, pdu: GfxPdu) {
        use ironrdp_dvc::DvcProcessor as _;
        client
            .process(0, &encoded_gfx(&pdu))
            .expect("test EGFX PDU must process");
    }

    /// A deterministic YUV decoder for the end-to-end AVC444/SurfaceStore regression.
    /// Main frames are uniform; auxiliary v2 frames pack U in the first half of each
    /// row and V in the second half, so the resulting 4:4:4 colour is uniform too.
    struct TaggedYuvDecoder;

    impl H264Decoder for TaggedYuvDecoder {
        fn decode(&mut self, _data: &[u8]) -> DecoderResult<DecodedFrame> {
            Err(DecoderError::msg("test decoder only supplies YUV"))
        }

        fn decode_yuv420(
            &mut self,
            data: &[u8],
            out: &mut ironrdp_graphics::avc444::Yuv420Frame,
        ) -> DecoderResult<()> {
            let &[y, u, v, aux] = data else {
                return Err(DecoderError::msg("test marker must contain Y, U, V, aux"));
            };
            const WIDTH: usize = 32;
            const HEIGHT: usize = 16;
            out.width = WIDTH;
            out.height = HEIGHT;
            out.y = vec![y; WIDTH * HEIGHT];
            out.u = vec![u; (WIDTH / 2) * (HEIGHT / 2)];
            out.v = vec![v; (WIDTH / 2) * (HEIGHT / 2)];
            if aux != 0 {
                for row in out.y.chunks_exact_mut(WIDTH) {
                    row[..WIDTH / 2].fill(u);
                    row[WIDTH / 2..].fill(v);
                }
                for plane in [&mut out.u, &mut out.v] {
                    for row in plane.chunks_exact_mut(WIDTH / 2) {
                        row[..WIDTH / 4].fill(u);
                        row[WIDTH / 4..].fill(v);
                    }
                }
            }
            Ok(())
        }

        fn supports_yuv420(&self) -> bool {
            true
        }
    }

    fn sparse_clearcodec_stream(x: u16, y: u16, bgr: [u8; 3]) -> Vec<u8> {
        sparse_clearcodec_stream_with_glyph(x, y, bgr, None)
    }

    fn sparse_clearcodec_stream_with_glyph(
        x: u16,
        y: u16,
        bgr: [u8; 3],
        glyph_index: Option<u16>,
    ) -> Vec<u8> {
        let mut subcodec_data = Vec::new();
        subcodec_data.extend_from_slice(&x.to_le_bytes());
        subcodec_data.extend_from_slice(&y.to_le_bytes());
        subcodec_data.extend_from_slice(&1u16.to_le_bytes());
        subcodec_data.extend_from_slice(&1u16.to_le_bytes());
        subcodec_data.extend_from_slice(&3u32.to_le_bytes());
        subcodec_data.push(0x00); // SubcodecId::Raw
        subcodec_data.extend_from_slice(&bgr);

        let mut stream = vec![
            glyph_index.map_or(0, |_| 0x01), // FLAG_GLYPH_INDEX
            0x00,
        ]; // flags, sequence number
        if let Some(glyph_index) = glyph_index {
            stream.extend_from_slice(&glyph_index.to_le_bytes());
        }
        stream.extend_from_slice(&0u32.to_le_bytes()); // residual
        stream.extend_from_slice(&0u32.to_le_bytes()); // bands
        stream.extend_from_slice(&(subcodec_data.len() as u32).to_le_bytes());
        stream.extend_from_slice(&subcodec_data);
        stream
    }

    #[test]
    fn sparse_clearcodec_decoder_reports_explicit_writes_not_colour_differences() {
        let seed: Vec<u8> = [0x10u8, 0x20, 0x30, 0xFF]
            .iter()
            .copied()
            .cycle()
            .take(2 * 2 * BPP)
            .collect();
        let mut decoder = ClearCodecDecoder::new();
        let decoded = decoder
            .decode_over_with_coverage(
                &sparse_clearcodec_stream(1, 0, [0x10, 0x20, 0x30]),
                2,
                2,
                Some(seed.clone()),
            )
            .expect("sparse raw subcodec must decode");

        assert_eq!(decoded.pixels, seed, "the explicit write equals its seed");
        assert_eq!(
            decoded.written_regions,
            vec![ClearCodecRect {
                left: 1,
                top: 0,
                right: 2,
                bottom: 1,
            }],
            "coverage must come from stream writes, not a colour comparison"
        );
    }

    #[test]
    fn sparse_clearcodec_decode_is_not_cached_as_a_full_glyph() {
        let seed: Vec<u8> = [0x10u8, 0x20, 0x30, 0xFF]
            .iter()
            .copied()
            .cycle()
            .take(2 * 2 * BPP)
            .collect();
        let mut decoder = ClearCodecDecoder::new();
        let decoded = decoder
            .decode_over_with_coverage(
                &sparse_clearcodec_stream_with_glyph(0, 0, [0x01, 0x02, 0x03], Some(42)),
                2,
                2,
                Some(seed),
            )
            .expect("sparse glyph candidate must decode");
        assert_eq!(decoded.written_regions.len(), 1);

        let hit = [
            0x01 | 0x02, // FLAG_GLYPH_INDEX | FLAG_GLYPH_HIT
            0x01,
            42,
            0,
        ];
        assert!(
            decoder.decode_over_with_coverage(&hit, 2, 2, None).is_err(),
            "a sparse seeded bitmap must not be available as a full glyph hit"
        );
    }

    #[test]
    fn fully_supplied_glyph_rows_coalesce_and_still_replay_as_full_coverage() {
        let mut decoder = ClearCodecDecoder::new();
        let mut stream = vec![0x01, 0x00, 0x00, 0x00]; // glyph index 0
        stream.extend_from_slice(&4u32.to_le_bytes()); // residual
        stream.extend_from_slice(&0u32.to_le_bytes()); // bands
        stream.extend_from_slice(&0u32.to_le_bytes()); // subcodec
        stream.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]); // four BGR pixels

        let decoded = decoder
            .decode_over_with_coverage(&stream, 2, 2, None)
            .expect("full glyph candidate must decode");
        assert_eq!(
            decoded.written_regions,
            vec![ClearCodecRect {
                left: 0,
                top: 0,
                right: 2,
                bottom: 2,
            }]
        );

        let hit = [0x01 | 0x02, 0x01, 0x00, 0x00]; // glyph index 0
        let replayed = decoder
            .decode_over_with_coverage(&hit, 2, 2, None)
            .expect("fully supplied glyph must remain cacheable");
        assert_eq!(replayed.written_regions, decoded.written_regions);
        assert_eq!(replayed.pixels, decoded.pixels);
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
    fn capabilities_with_a_decoder_offer_the_avc_ladder() {
        // V10.7 WITHOUT AVC_DISABLED first: that is what invites the AVC444v2 the
        // build now decodes (and what modern Windows sends — measured on quench).
        // V8.1/AVC420 next for downlevel servers, V8 last. The vendored client
        // filters this ladder against the decoder's actual abilities, so a build
        // whose decoder cannot produce YUV structurally falls back to V8.1.
        let handler = GfxHandler::new(store()).advertising_avc();
        let caps = handler.capabilities();
        assert_eq!(caps.len(), 3, "V10.7, V8.1, V8");

        match &caps[0] {
            CapabilitySet::V10_7 { flags } => {
                assert!(
                    !flags.contains(CapabilitiesV107Flags::AVC_DISABLED),
                    "AVC444 is the point of this advertisement"
                );
                assert!(flags.contains(CapabilitiesV107Flags::SMALL_CACHE));
            }
            other => panic!("expected V10_7 first, got {other:?}"),
        }
        match &caps[1] {
            CapabilitySet::V8_1 { flags } => {
                assert!(
                    flags.contains(ironrdp_egfx::pdu::CapabilitiesV81Flags::AVC420_ENABLED),
                    "the downlevel rung still carries AVC420"
                );
            }
            other => panic!("expected V8_1 second, got {other:?}"),
        }
        assert!(
            matches!(&caps[2], CapabilitySet::V8 { .. }),
            "expected a V8 fallback, got {:?}",
            caps[2]
        );
    }

    #[test]
    fn a_decode_failure_reported_by_the_client_is_counted_with_its_reason() {
        let mut handler = GfxHandler::new(store());
        handler.on_decode_failure(Codec1Type::Avc444v2, "avc444 luma decode failed");
        handler.on_decode_failure(Codec1Type::Avc444v2, "avc444 luma decode failed");

        let s = handler.stats().snapshot();
        assert_eq!(s.decode_errors, 2);
        assert_eq!(
            s.decode_error_reasons
                .get("Avc444v2: avc444 luma decode failed"),
            Some(&2),
            "{:?}",
            s.decode_error_reasons
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
        assert_eq!(s.surface_error_reasons.get("no_such_surface"), Some(&1));
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
    fn reset_graphics_preserves_surfaces_and_cache_entries() {
        // MS-RDPEGFX 3.3.5.14 changes only the Graphics Output Buffer dimensions. The
        // server can legally reference an existing cache slot immediately afterwards.
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 4, 4));
        handler.on_surface_mapped(1, 0, 0);
        handler.on_solid_fill(&SolidFillPdu {
            surface_id: 1,
            fill_pixel: Color {
                b: 0x11,
                g: 0x22,
                r: 0x33,
                xa: 0,
            },
            rectangles: vec![rect(0, 0, 1, 1)],
        });
        handler.on_surface_to_cache(&SurfaceToCachePdu {
            surface_id: 1,
            cache_key: 0,
            cache_slot: 7,
            source_rectangle: rect(0, 0, 1, 1),
        });

        handler.on_reset_graphics(1920, 1080);
        handler.on_cache_to_surface(&CacheToSurfacePdu {
            cache_slot: 7,
            surface_id: 1,
            destination_points: vec![Point { x: 1, y: 0 }],
        });

        assert!(store.lock().unwrap().get(1).is_some());
        assert_eq!(pixel_at(&store, 1, 1, 0), [0x33, 0x22, 0x11, 0xFF]);
        assert_eq!(handler.stats().snapshot().surface_errors, 0);
        assert_eq!(
            handler.stats().snapshot().reset_graphics,
            Some((1920, 1080))
        );
    }

    #[test]
    fn evict_cache_entry_removes_the_slot_from_both_mirrors() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 2, 2));
        handler.on_surface_to_cache(&SurfaceToCachePdu {
            surface_id: 1,
            cache_key: 0,
            cache_slot: 7,
            source_rectangle: rect(0, 0, 1, 1),
        });
        assert_eq!(store.lock().unwrap().cache_stats().entries, 1);

        handler.on_evict_cache_entry(&EvictCacheEntryPdu { cache_slot: 7 });

        assert_eq!(store.lock().unwrap().cache_stats().entries, 0);
        assert!(!handler.cache_dims.contains_key(&7));
        assert_eq!(store.lock().unwrap().cache_stats().evictions, 1);
    }

    #[test]
    fn clipped_surface_to_cache_records_the_bitmap_that_was_stored() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 4, 4));
        handler.on_solid_fill(&SolidFillPdu {
            surface_id: 1,
            fill_pixel: Color {
                b: 0x11,
                g: 0x22,
                r: 0x33,
                xa: 0,
            },
            rectangles: vec![rect(0, 0, 4, 4)],
        });

        handler.on_surface_to_cache(&SurfaceToCachePdu {
            surface_id: 1,
            cache_key: 0,
            cache_slot: 7,
            source_rectangle: rect(2, 1, 6, 5),
        });

        assert_eq!(handler.cache_dims.get(&7), Some(&(2, 3)));
        let slot = *handler.slot_stats().snapshot().get(7).expect("slot 7");
        assert_eq!((slot.width, slot.height), (2, 3));
        assert_eq!(slot.bytes_stored, 2 * 3 * BPP as u64);

        handler.on_surface_to_cache(&SurfaceToCachePdu {
            surface_id: 1,
            cache_key: 0,
            cache_slot: 7,
            source_rectangle: rect(5, 5, 8, 7),
        });
        assert_eq!(
            handler.cache_dims.get(&7),
            Some(&(2, 3)),
            "a fully clipped no-op must not replace live cache metadata"
        );

        handler.on_solid_fill(&SolidFillPdu {
            surface_id: 1,
            fill_pixel: Color {
                b: 0,
                g: 0,
                r: 0,
                xa: 0,
            },
            rectangles: vec![rect(0, 0, 4, 4)],
        });
        handler.on_cache_to_surface(&CacheToSurfacePdu {
            cache_slot: 7,
            surface_id: 1,
            destination_points: vec![Point { x: 2, y: 1 }],
        });

        assert_eq!(pixel_at(&store, 1, 2, 1), [0x33, 0x22, 0x11, 0xFF]);
    }

    #[test]
    fn cache_slot_records_track_the_fill_every_hit_and_the_eviction() {
        // The per-slot grid's whole claim, end to end through the PDUs. Distinct numbers
        // throughout — a 10x4 tile, slot 7, three destinations — so a swapped width and
        // height, or hits counted per PDU instead of per destination, cannot pass.
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 32, 16));

        // Paint the region with ClearCodec, so the slot has a codec to be attributed to.
        let region_bgra: Vec<u8> = [0x10u8, 0x20, 0x30, 0xFF]
            .iter()
            .copied()
            .cycle()
            .take(10 * 4 * BPP)
            .collect();
        handler.on_unhandled_pdu(&GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::ClearCodec,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 10, 4),
            bitmap_data: ClearCodecEncoder::new().encode(&region_bgra, 10, 4),
        }));

        handler.on_surface_to_cache(&SurfaceToCachePdu {
            surface_id: 1,
            cache_key: 0,
            cache_slot: 7,
            source_rectangle: rect(0, 0, 10, 4),
        });

        let filled = handler.slot_stats().snapshot();
        let slot = *filled.get(7).expect("the fill must create a slot record");
        assert_eq!(slot.slot, 7);
        assert_eq!(slot.width, 10);
        assert_eq!(slot.height, 4);
        assert_eq!(slot.codec, "ClearCodec");
        assert_eq!(slot.state, crate::stats::SlotState::Live);
        assert_eq!(slot.bytes_stored, 10 * 4 * BPP as u64);
        assert_eq!(slot.hits, 0);
        assert_eq!(slot.bytes_served, 0);

        // One PDU, three destinations: three hits, three tiles' worth of bytes served.
        handler.on_cache_to_surface(&CacheToSurfacePdu {
            cache_slot: 7,
            surface_id: 1,
            destination_points: vec![
                Point { x: 0, y: 4 },
                Point { x: 10, y: 4 },
                Point { x: 20, y: 4 },
            ],
        });

        let hit = *handler.slot_stats().snapshot().get(7).expect("slot 7");
        assert_eq!(hit.hits, 3, "one hit per destination painted");
        assert_eq!(hit.bytes_served, 3 * 10 * 4 * BPP as u64);
        assert_eq!(hit.bytes_stored, 10 * 4 * BPP as u64);
        assert!(hit.last_hit.is_some());
        assert_eq!(handler.stats().snapshot().surface_errors, 0);

        handler.on_evict_cache_entry(&EvictCacheEntryPdu { cache_slot: 7 });

        let evicted = *handler
            .slot_stats()
            .snapshot()
            .get(7)
            .expect("an evicted slot is still reported");
        assert_eq!(evicted.state, crate::stats::SlotState::Evicted);
        assert_eq!(evicted.hits, 3, "eviction must not zero what it served");
        assert_eq!(evicted.bytes_served, 3 * 10 * 4 * BPP as u64);
        assert_eq!(evicted.width, 10);
        assert_eq!(evicted.height, 4);
    }

    #[test]
    fn a_cache_hit_on_a_slot_we_never_saw_filled_records_no_slot() {
        // Without a fill there is no stored size, so a record for it would report zero
        // bytes served for a hit that really did paint something — worse than absent.
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 8, 8));
        handler.on_cache_to_surface(&CacheToSurfacePdu {
            cache_slot: 7,
            surface_id: 1,
            destination_points: vec![Point { x: 0, y: 0 }],
        });
        assert!(handler.slot_stats().snapshot().is_empty());
        assert_eq!(
            handler
                .stats()
                .snapshot()
                .surface_error_reasons
                .get("unknown_cache_dimensions"),
            Some(&1),
            "and the miss is still counted where it always was"
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
        assert_eq!(
            stats.codec_bytes_painted.get("ClearCodec"),
            Some(&(2 * 2 * 4)),
            "a 2x2 blit paints 16 bytes under its codec"
        );

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
    fn clearcodec_right_bottom_overhang_seeds_the_full_requested_extent() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 3, 3));

        // Distinct pixels make a clipped seed's missing stride visible. The requested
        // 3x3 tile starts at (1,1), so only its 2x2 top-left corner is in the surface.
        let seed: Vec<u8> = (0..3)
            .flat_map(|y| (0..3).flat_map(move |x| [0x10 + x, 0x20 + y, 0x30, 0xFF]))
            .collect();
        store
            .lock()
            .unwrap()
            .blit_rgba(1, Rect::new(0, 0, 3, 3), &seed, 3)
            .unwrap();

        // Only the top-left pixel of the requested tile is supplied. The other visible
        // pixels must come from the full-stride seed, not from the decoder's black fill.
        handler.on_unhandled_pdu(&GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::ClearCodec,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(1, 1, 4, 4),
            bitmap_data: sparse_clearcodec_stream(0, 0, [0x01, 0x02, 0x03]),
        }));

        assert_eq!(pixel_at(&store, 1, 1, 1), [0x03, 0x02, 0x01, 0xFF]);
        assert_eq!(pixel_at(&store, 1, 2, 1), [0x12, 0x21, 0x30, 0xFF]);
        assert_eq!(pixel_at(&store, 1, 1, 2), [0x11, 0x22, 0x30, 0xFF]);
        assert_eq!(pixel_at(&store, 1, 2, 2), [0x12, 0x22, 0x30, 0xFF]);
    }

    #[test]
    fn oversized_clearcodec_destination_keeps_the_decoder_error_path() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 1, 1));

        handler.on_unhandled_pdu(&GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::ClearCodec,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 8193, 1),
            bitmap_data: sparse_clearcodec_stream(0, 0, [0x01, 0x02, 0x03]),
        }));

        let stats = handler.stats().snapshot();
        assert_eq!(stats.decode_errors, 1);
        assert_eq!(stats.surface_errors, 0);
        assert_eq!(pixel_at(&store, 1, 0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn sparse_clearcodec_tiles_do_not_complete_a_replacement_until_pixels_are_written() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 4, 4));
        handler.on_solid_fill(&SolidFillPdu {
            surface_id: 1,
            fill_pixel: Color {
                b: 0xC0,
                g: 0xB0,
                r: 0xA0,
                xa: 0,
            },
            rectangles: vec![rect(0, 0, 4, 4)],
        });
        handler.on_surface_mapped(1, 0, 0);

        // A same-size CreateSurface starts a new zero-filled incarnation while the
        // old, fully painted output remains the presentation fallback.
        handler.on_surface_created(&egfx_surface(1, 4, 4));
        handler.on_surface_mapped(1, 0, 0);

        for (x, y, bgr) in [
            (0, 0, [0x01, 0x02, 0x03]),
            (2, 0, [0x04, 0x05, 0x06]),
            (0, 2, [0x07, 0x08, 0x09]),
            (2, 2, [0x0A, 0x0B, 0x0C]),
        ] {
            handler.on_unhandled_pdu(&GfxPdu::WireToSurface1(WireToSurface1Pdu {
                surface_id: 1,
                codec_id: Codec1Type::ClearCodec,
                pixel_format: PixelFormat::XRgb,
                destination_rectangle: rect(x, y, x + 2, y + 2),
                bitmap_data: sparse_clearcodec_stream(0, 0, bgr),
            }));
        }

        let guard = store.lock().unwrap();
        let visible = guard
            .presentation_surface()
            .expect("the old output must remain presentable");
        assert!(
            visible
                .pixels()
                .chunks_exact(BPP)
                .all(|pixel| pixel == [0xA0, 0xB0, 0xC0, 0xFF]),
            "sparse writes must not retire the old output before replacement coverage is full"
        );
        drop(guard);

        let full_bgra: Vec<u8> = [0x11u8, 0x22, 0x33, 0xFF]
            .iter()
            .copied()
            .cycle()
            .take(4 * 4 * BPP)
            .collect();
        handler.on_unhandled_pdu(&GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::ClearCodec,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 4, 4),
            bitmap_data: ClearCodecEncoder::new().encode(&full_bgra, 4, 4),
        }));

        let guard = store.lock().unwrap();
        let visible = guard
            .presentation_surface()
            .expect("the fully supplied replacement must be presentable");
        assert!(
            visible
                .pixels()
                .chunks_exact(BPP)
                .all(|pixel| pixel == [0x33, 0x22, 0x11, 0xFF])
        );
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
        assert_eq!(
            stats.codec_bytes_painted.get("ClearCodec"),
            None,
            "a failed decode painted nothing and must not claim bytes"
        );
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
        assert_eq!(stats.surface_error_reasons.get("no_such_surface"), Some(&1));
        assert_eq!(
            stats.codec_bytes_painted.get("ClearCodec"),
            None,
            "a refused surface write must not claim painted bytes"
        );
    }

    #[test]
    fn empty_clearcodec_coverage_does_not_report_painted_bytes() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 2, 2));

        let mut empty_composite = vec![0x00, 0x00]; // flags, sequence number
        empty_composite.extend_from_slice(&0u32.to_le_bytes()); // residual
        empty_composite.extend_from_slice(&0u32.to_le_bytes()); // bands
        empty_composite.extend_from_slice(&0u32.to_le_bytes()); // subcodec
        handler.on_unhandled_pdu(&GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::ClearCodec,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 2, 2),
            bitmap_data: empty_composite,
        }));

        let stats = handler.stats().snapshot();
        assert_eq!(stats.decode_errors, 0);
        assert_eq!(stats.surface_errors, 0);
        assert_eq!(
            stats.codec_bytes_painted.get("ClearCodec"),
            None,
            "zero explicit coverage must not claim painted bytes"
        );
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
    fn avc444_scroll_copy_then_partial_lc1_lc2_keeps_old_and_new_rgba_pixels() {
        // MDR-BUG-FLU-00119: a same-surface scroll/copy changes the visible destination
        // without changing the AVC decoder's reference chain. A later partial LC=1/LC=2
        // update must not let stale chroma state repaint the copied-but-uncovered pixels
        // at a fixed coordinate. Exercise the real GraphicsPipelineClient, GfxHandler and
        // SurfaceStore so the assertions inspect final RGBA pixels, not plane metadata.
        let store = store();
        let handler = GfxHandler::new(Arc::clone(&store));
        let mut client =
            GraphicsPipelineClient::new(Box::new(handler), Some(Box::new(TaggedYuvDecoder)));

        const FULL: InclusiveRectangle = InclusiveRectangle {
            left: 0,
            top: 0,
            right: 32,
            bottom: 16,
        };
        const OLD: InclusiveRectangle = InclusiveRectangle {
            left: 0,
            top: 0,
            right: 8,
            bottom: 4,
        };
        const OLD_COPY: ExclusiveRectangle = ExclusiveRectangle {
            left: 0,
            top: 0,
            right: 8,
            bottom: 4,
        };
        const NEW: InclusiveRectangle = InclusiveRectangle {
            left: 8,
            top: 0,
            right: 16,
            bottom: 4,
        };
        const NEW_LEFT: InclusiveRectangle = InclusiveRectangle {
            left: 8,
            top: 0,
            right: 12,
            bottom: 4,
        };

        process_pdu(
            &mut client,
            GfxPdu::CreateSurface(ironrdp_egfx::pdu::CreateSurfacePdu {
                surface_id: 1,
                width: 32,
                height: 16,
                pixel_format: PixelFormat::XRgb,
            }),
        );

        // Establish a full AVC444 baseline (A), then give the old scroll location its
        // distinct colour (B) through the normal LC=1/LC=2 alternation.
        process_pdu(
            &mut client,
            avc444_pdu(
                Encoding::LUMA_AND_CHROMA,
                vec![FULL],
                &[100, 120, 140, 0],
                Some((vec![FULL], &[0, 120, 140, 1])),
            ),
        );
        process_pdu(
            &mut client,
            avc444_pdu(Encoding::LUMA, vec![OLD], &[80, 130, 150, 0], None),
        );
        process_pdu(
            &mut client,
            avc444_pdu(Encoding::CHROMA, vec![OLD], &[0, 130, 150, 1], None),
        );

        // Scroll by copying the old region into a new coordinate on the SAME surface.
        // SurfaceStore extracts before writing, so this is also safe for overlap.
        process_pdu(
            &mut client,
            GfxPdu::SurfaceToSurface(SurfaceToSurfacePdu {
                source_surface_id: 1,
                destination_surface_id: 1,
                source_rectangle: OLD_COPY,
                destination_points: vec![Point { x: 8, y: 0 }],
            }),
        );
        let expected_b = [114, 69, 83, 0xFF];
        assert_eq!(pixel_at(&store, 1, 1, 1), expected_b, "old scroll pixel");
        assert_eq!(pixel_at(&store, 1, 13, 1), expected_b, "copied new pixel");

        // Only the left half of the copied destination gets a fresh LC=1 luma pass.
        // LC=2 covers the whole destination. Regional validity must suppress the stale
        // right-half AVC buffer and leave its visible copied RGBA pixels untouched.
        process_pdu(
            &mut client,
            avc444_pdu(Encoding::LUMA, vec![NEW_LEFT], &[60, 110, 170, 0], None),
        );
        process_pdu(
            &mut client,
            avc444_pdu(Encoding::CHROMA, vec![NEW], &[0, 110, 170, 1], None),
        );

        assert_eq!(
            pixel_at(&store, 1, 1, 1),
            expected_b,
            "old coordinate must retain the pre-scroll content"
        );
        assert_eq!(
            pixel_at(&store, 1, 9, 1),
            [126, 43, 26, 0xFF],
            "the LC=1/LC=2-covered new pixels must use the fresh colour"
        );
        assert_eq!(
            pixel_at(&store, 1, 13, 1),
            expected_b,
            "the copied new pixels outside partial LC=1 must not become a fixed ghost"
        );
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
        assert_eq!(
            handler
                .stats()
                .snapshot()
                .surface_error_reasons
                .get("coordinate_overflow"),
            Some(&1)
        );
    }

    #[test]
    fn frames_and_unknown_pdus_are_counted_without_touching_pixels() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 2, 2));
        let generation_before = store.lock().unwrap().generation();

        handler.on_frame_complete(1);
        handler.on_frame_complete(2);
        handler.on_unhandled_pdu(&GfxPdu::CacheImportOffer(CacheImportOfferPdu {
            cache_entries: Vec::new(),
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

    #[test]
    fn split_bitmap_payloads_stay_hidden_until_end_frame() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 2, 1));
        handler.on_surface_mapped(1, 0, 0);
        handler.on_solid_fill(&SolidFillPdu {
            surface_id: 1,
            fill_pixel: Color {
                b: 0,
                g: 0,
                r: 255,
                xa: 0,
            },
            rectangles: vec![rect(0, 0, 2, 1)],
        });

        let mut snapshot = crate::surface::PresentationSnapshot::default();
        assert_eq!(
            store.lock().unwrap().copy_presentation_state(&mut snapshot),
            crate::surface::PresentationCopy::Copied
        );
        let before = store.lock().unwrap().generation();
        handler.on_frame_start(21);

        for (left, color) in [(0, [0, 0, 255, 255]), (1, [0, 255, 0, 255])] {
            handler.on_bitmap_updated(&BitmapUpdate::new(
                1,
                rect(left, 0, left + 1, 1),
                Codec1Type::Uncompressed,
                color.to_vec(),
                1,
                1,
            ));
        }
        assert_eq!(store.lock().unwrap().generation(), before);
        assert_eq!(
            store.lock().unwrap().copy_presentation_state(&mut snapshot),
            crate::surface::PresentationCopy::Retained
        );
        assert_eq!(snapshot.pixels, vec![255, 0, 0, 255, 255, 0, 0, 255]);

        handler.on_frame_complete(21);
        assert_eq!(store.lock().unwrap().generation(), before + 1);
        assert_eq!(
            store.lock().unwrap().copy_presentation_state(&mut snapshot),
            crate::surface::PresentationCopy::Copied
        );
        assert_eq!(snapshot.pixels, vec![0, 0, 255, 255, 0, 255, 0, 255]);
    }

    /// Output mapping PDUs must display a surface; unsupported RAIL mappings must not
    /// replace the desktop output.
    ///
    /// This is the regression for the bug that made the client show a black window
    /// against a real server. `ironrdp-egfx` dispatches scaled output to its own defaulted
    /// trait method, so implementing `on_surface_mapped` alone silently loses it. The two
    /// window callbacks target RAIL windows and must remain separate from desktop output.
    ///
    /// The older test below covers the raw `on_unhandled_pdu` arm, which upstream never
    /// takes; it passed throughout and proved nothing about real behaviour.
    #[test]
    fn output_maps_display_and_rail_maps_do_not_steal_the_desktop() {
        use ironrdp_egfx::pdu::{
            MapSurfaceToScaledOutputPdu, MapSurfaceToScaledWindowPdu, MapSurfaceToWindowPdu,
        };

        // Each closure exercises one of the two Graphics Output Buffer callbacks.
        type Map = (&'static str, fn(&mut GfxHandler));
        let output_cases: [Map; 2] = [
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
        ];

        for (name, apply) in output_cases {
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

        let store = store();
        {
            let mut store = store.lock().unwrap();
            store.create(7, 4, 4);
            store.create(8, 4, 4);
            store
                .solid_fill(7, &[Rect::new(0, 0, 4, 4)], [255, 0, 0, 255])
                .unwrap();
            store
                .solid_fill(8, &[Rect::new(0, 0, 4, 4)], [0, 0, 255, 255])
                .unwrap();
            store.map_to_output(7);
        }
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_map_surface_to_window(&MapSurfaceToWindowPdu {
            surface_id: 8,
            window_id: 1,
            mapped_width: 4,
            mapped_height: 4,
        });
        assert_eq!(
            store.lock().unwrap().output_surface().unwrap().pixels(),
            [255, 0, 0, 255].repeat(16)
        );

        handler.on_map_surface_to_scaled_window(&MapSurfaceToScaledWindowPdu {
            surface_id: 8,
            window_id: 1,
            mapped_width: 4,
            mapped_height: 4,
            target_width: 8,
            target_height: 8,
        });
        assert_eq!(
            store.lock().unwrap().output_surface().unwrap().pixels(),
            [255, 0, 0, 255].repeat(16)
        );
    }

    #[test]
    fn scaled_output_mapping_reaches_the_presentation_geometry() {
        use crate::surface::{PresentationCopy, PresentationSnapshot};
        use ironrdp_egfx::pdu::MapSurfaceToScaledOutputPdu;

        let store = store();
        {
            let mut store = store.lock().unwrap();
            store.create(7, 2, 2);
            store
                .solid_fill(7, &[Rect::new(0, 0, 2, 2)], [1, 2, 3, 255])
                .unwrap();
        }
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_reset_graphics(6, 3);
        handler.on_map_surface_to_scaled_output(&MapSurfaceToScaledOutputPdu {
            surface_id: 7,
            output_origin_x: 1,
            output_origin_y: 0,
            target_width: 4,
            target_height: 2,
        });

        let mut snapshot = PresentationSnapshot::default();
        assert_eq!(
            store.lock().unwrap().copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!((snapshot.width, snapshot.height), (6, 3));
        assert_eq!(snapshot.mapping.dest_x, 1);
        assert_eq!(snapshot.mapping.dest_width, 4);
        assert_eq!(snapshot.mapping.dest_height, 2);
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

    /// An AVC444 paint must land in `codec_bytes_painted`, or the title bar's codec
    /// segment (which diffs that map) never names the codec carrying the picture.
    /// A live quench session (2026-08-17) painted 117 MB of Avc444v2 and left the
    /// map empty — this is that regression pinned.
    #[test]
    fn a_decoded_bitmap_update_attributes_its_painted_bytes_to_its_codec() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));
        handler.on_surface_created(&egfx_surface(1, 4, 4));

        handler.on_bitmap_updated(&BitmapUpdate::new(
            1,
            rect(0, 0, 4, 2),
            Codec1Type::Avc444v2,
            vec![0xAAu8; 4 * 2 * BPP],
            4,
            2,
        ));

        let stats = handler.stats().snapshot();
        assert_eq!(
            stats.codec_bytes_painted.get("Avc444v2"),
            Some(&(4 * 2 * BPP as u64)),
            "the blitted rect's bytes must be attributed to the painting codec"
        );
        // The one-per-PDU codec tally stays with on_avc444_frame; this path must
        // not double-count it.
        assert_eq!(stats.codec_ids_seen.get("Avc444v2"), None);
    }

    /// A refused blit painted nothing and must not claim bytes.
    #[test]
    fn a_bitmap_update_for_a_missing_surface_attributes_nothing() {
        let store = store();
        let mut handler = GfxHandler::new(Arc::clone(&store));

        handler.on_bitmap_updated(&BitmapUpdate::new(
            9,
            rect(0, 0, 2, 2),
            Codec1Type::Uncompressed,
            vec![0u8; 2 * 2 * BPP],
            2,
            2,
        ));

        let stats = handler.stats().snapshot();
        assert!(stats.codec_bytes_painted.is_empty());
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

    /// `decode_over` accepts a seed of the right size and ignores a wrong-sized one.
    ///
    /// ClearCodec's layers need not cover the whole tile, and FreeRDP composites straight
    /// into the destination so uncovered pixels keep what is on screen. We now seed the
    /// decode with the surface's current content instead of black.
    ///
    /// The preservation property itself is NOT pinned here: `ClearCodecEncoder` only
    /// produces fully-covering streams, so a partial-coverage fixture cannot be built
    /// from it. That half is evidenced by live capture — before this change a tile
    /// carrying one small subcodec region blacked out the rest of its rectangle, which is
    /// visible as dark blocks in the earlier screenshots and absent afterwards.
    #[test]
    fn clearcodec_decode_over_takes_a_correctly_sized_seed() {
        use ironrdp_graphics::clearcodec::{ClearCodecDecoder, ClearCodecEncoder};

        // BGRA in, as the encoder expects: B=0x10, G=0x20, R=0x30.
        let tile: Vec<u8> = [0x10u8, 0x20, 0x30, 0xFF]
            .iter()
            .copied()
            .cycle()
            .take(2 * 2 * 4)
            .collect();
        let encoded = ClearCodecEncoder::new().encode(&tile, 2, 2);

        // A seed of the right length is accepted; the covered pixels still decode.
        let seed = vec![0x7Au8; 2 * 2 * 4];
        let out = ClearCodecDecoder::new()
            .decode_over(&encoded, 2, 2, Some(seed))
            .expect("a correctly sized seed must be accepted");
        assert_eq!(out.len(), 2 * 2 * 4);
        assert_eq!(
            &out[0..3],
            &[0x10, 0x20, 0x30],
            "covered pixels decode normally"
        );

        // A wrong-sized seed must not be used, and must not fail the decode.
        let out2 = ClearCodecDecoder::new()
            .decode_over(&encoded, 2, 2, Some(vec![0u8; 3]))
            .expect("a mismatched seed falls back rather than erroring");
        assert_eq!(out2.len(), 2 * 2 * 4);
        assert_eq!(&out2[0..3], &[0x10, 0x20, 0x30]);
    }

    #[test]
    fn a_short_vbar_replayed_near_the_band_edge_is_clipped_to_the_band() {
        use ironrdp_graphics::clearcodec::{ShortVBar, VBarCache};

        let short = ShortVBar {
            y_on: 3,
            pixel_count: 3,
            pixels: vec![
                1, 2, 3, // the one row that fits
                4, 5, 6, // beyond the band
                7, 8, 9, // beyond the band
            ],
        };

        let full = VBarCache::reconstruct_full_vbar(&short, 4, 10, 20, 30);

        assert_eq!(full.pixels.len(), 4 * 3, "a four-row band stays four rows");
        assert_eq!(&full.pixels[0..9], &[10, 20, 30, 10, 20, 30, 10, 20, 30]);
        assert_eq!(&full.pixels[9..12], &[1, 2, 3]);
    }

    #[test]
    fn clearcodec_nscodec_subregion_is_decoded_instead_of_silently_skipped() {
        use ironrdp_graphics::clearcodec::ClearCodecDecoder;

        // A 1x1 NSCodec bitmap: four one-byte raw Y/Co/Cg/A planes. With CLL=1,
        // Y=100, Co=10 and Cg=-5 reconstruct to BGR=(95,95,115).
        let mut nscodec = Vec::new();
        for _ in 0..4 {
            nscodec.extend_from_slice(&1u32.to_le_bytes());
        }
        nscodec.extend_from_slice(&[1, 0, 0, 0]); // CLL, no subsampling, reserved
        nscodec.extend_from_slice(&[100, 10, 251, 200]);

        let mut subcodec = Vec::new();
        subcodec.extend_from_slice(&0u16.to_le_bytes()); // x
        subcodec.extend_from_slice(&0u16.to_le_bytes()); // y
        subcodec.extend_from_slice(&1u16.to_le_bytes()); // width
        subcodec.extend_from_slice(&1u16.to_le_bytes()); // height
        subcodec.extend_from_slice(&u32::try_from(nscodec.len()).unwrap().to_le_bytes());
        subcodec.push(1); // NSCodec
        subcodec.extend_from_slice(&nscodec);

        let mut clear = vec![0, 0]; // flags, sequence
        clear.extend_from_slice(&0u32.to_le_bytes()); // residual bytes
        clear.extend_from_slice(&0u32.to_le_bytes()); // band bytes
        clear.extend_from_slice(&u32::try_from(subcodec.len()).unwrap().to_le_bytes());
        clear.extend_from_slice(&subcodec);

        let decoded = ClearCodecDecoder::new().decode(&clear, 1, 1).unwrap();
        assert_eq!(decoded, [95, 95, 115, 200]);
    }

    #[test]
    fn clearcodec_nscodec_rle_planes_expand_to_the_declared_bitmap() {
        use ironrdp_graphics::clearcodec::ClearCodecDecoder;

        // Nine identical bytes encode as a five-byte run followed by the mandatory raw
        // four-byte tail: value, value, run-minus-two, then four raw bytes.
        let plane = |value: u8| [value, value, 3, value, value, value, value];
        let planes = [plane(100), plane(10), plane(251), plane(200)];
        let mut nscodec = Vec::new();
        for plane in &planes {
            nscodec.extend_from_slice(&u32::try_from(plane.len()).unwrap().to_le_bytes());
        }
        nscodec.extend_from_slice(&[1, 0, 0, 0]);
        for plane in &planes {
            nscodec.extend_from_slice(plane);
        }

        let mut subcodec = Vec::new();
        subcodec.extend_from_slice(&0u16.to_le_bytes());
        subcodec.extend_from_slice(&0u16.to_le_bytes());
        subcodec.extend_from_slice(&3u16.to_le_bytes());
        subcodec.extend_from_slice(&3u16.to_le_bytes());
        subcodec.extend_from_slice(&u32::try_from(nscodec.len()).unwrap().to_le_bytes());
        subcodec.push(1);
        subcodec.extend_from_slice(&nscodec);

        let mut clear = vec![0, 0];
        clear.extend_from_slice(&0u32.to_le_bytes());
        clear.extend_from_slice(&0u32.to_le_bytes());
        clear.extend_from_slice(&u32::try_from(subcodec.len()).unwrap().to_le_bytes());
        clear.extend_from_slice(&subcodec);

        let decoded = ClearCodecDecoder::new().decode(&clear, 3, 3).unwrap();
        assert_eq!(decoded, [95, 95, 115, 200].repeat(9));
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

    /// A tile flagged RFX_TILE_DIFFERENCE carries a DELTA, and must be ADDED to the
    /// coefficients already held — not replace them.
    ///
    /// FreeRDP does this with `add_16s_inplace(buffer, current, ...)` inside
    /// `progressive_rfx_dwt_2d_decode` when `coeffDiff` is set. We overwrote instead, so
    /// such a tile discarded everything the earlier passes had built.
    ///
    /// Only a few tiles per frame carry the flag, which is why it surfaced as exactly one
    /// wrong tile on an otherwise correct screen: in a captured frame, tile (11,0) had
    /// flags=0x01 while its neighbour had 0x00, and that single tile was visibly offset.
    #[test]
    fn a_difference_tile_adds_to_the_retained_coefficients() {
        use ironrdp::pdu::codecs::rfx::progressive::ComponentCodecQuant;
        use ironrdp_graphics::progressive::{TileState, encode_first_pass};

        // A quantiser the encoder and decoder agree on, and a recognisable ramp so a
        // replace and an add cannot look alike.
        let quant = ComponentCodecQuant::LOSSLESS;
        let mut coeffs = [0i16; 4096];
        for (i, c) in coeffs.iter_mut().enumerate() {
            *c = ((i % 7) as i16) - 3;
        }
        let mut encoded = vec![0u8; 32768];
        let len =
            encode_first_pass(&mut coeffs, &mut encoded, &quant, &quant, true).expect("encode");
        let data = &encoded[..len];

        // Baseline: a normal (non-difference) first pass.
        let mut plain = TileState::new();
        plain
            .decode_first(
                [data; 3],
                [&quant; 3],
                [quant; 3],
                [0; 3],
                0xFF,
                true,
                false,
            )
            .expect("decode");
        let baseline = plain.coefficients[0];

        // Same tile decoded again as a DIFFERENCE on top of that state.
        plain
            .decode_first([data; 3], [&quant; 3], [quant; 3], [0; 3], 0xFF, true, true)
            .expect("decode");

        let doubled: Vec<i16> = baseline.iter().map(|v| v.wrapping_add(*v)).collect();
        assert_eq!(
            &plain.coefficients[0][..],
            &doubled[..],
            "a difference tile must add to what was already there"
        );
        assert_ne!(
            &plain.coefficients[0][..],
            &baseline[..],
            "and must not simply replace it"
        );
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
        // Two complete symbols: +1 (`1001`), then -1 (`111`). Restarting before the
        // second call would read +1 again.
        let data = [0b1001_1110u8];
        let mut shared = SrlReader::new(&data);
        let first = shared.read(3);
        let second = shared.read(3);

        let mut fresh = SrlReader::new(&data);
        assert_eq!(
            fresh.read(3),
            first,
            "a fresh reader sees the same first symbol"
        );
        assert_eq!((first, second), (1, -1));
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

    /// A minimal valid RFX Progressive stream (SYNC + CONTEXT + empty frame) that
    /// establishes one codec context in the decoder without painting anything.
    fn minimal_progressive_stream() -> Vec<u8> {
        use ironrdp::pdu::codecs::rfx::RfxRectangle;
        use ironrdp::pdu::codecs::rfx::progressive::{
            ProgressiveBlock, ProgressiveContextPdu, ProgressiveFrameBeginPdu,
            ProgressiveFrameEndPdu, ProgressiveRegion, ProgressiveSyncPdu,
            encode_progressive_stream,
        };
        let region = ProgressiveRegion {
            tile_size: 0x40,
            rects: vec![RfxRectangle {
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            }],
            quant_vals: vec![],
            quant_prog_vals: vec![],
            flags: 0,
            tiles: vec![],
        };
        encode_progressive_stream(&[
            ProgressiveBlock::Sync(ProgressiveSyncPdu),
            ProgressiveBlock::Context(ProgressiveContextPdu {
                context_id: 0,
                tile_size: 0x0040,
                flags: 0,
            }),
            ProgressiveBlock::FrameBegin(ProgressiveFrameBeginPdu {
                frame_index: 0,
                region_count: 1,
            }),
            ProgressiveBlock::Region(region),
            ProgressiveBlock::FrameEnd(ProgressiveFrameEndPdu),
        ])
        .expect("encode progressive stream")
    }

    fn decode_progressive_into_with_context(
        handler: &mut GfxHandler,
        surface_id: u16,
        codec_context_id: u32,
    ) {
        let pdu = ironrdp_egfx::pdu::WireToSurface2Pdu {
            surface_id,
            codec_context_id,
            codec_id: ironrdp_egfx::pdu::Codec2Type::RemoteFxProgressive,
            pixel_format: ironrdp_egfx::pdu::PixelFormat::XRgb,
            bitmap_data: minimal_progressive_stream(),
        };
        handler.on_wire_to_surface2(&pdu);
        assert_eq!(
            handler.stats().snapshot().undecoded_regions,
            0,
            "the fixture stream must decode, or the test observes nothing"
        );
    }

    fn decode_progressive_into(handler: &mut GfxHandler, surface_id: u16) {
        decode_progressive_into_with_context(handler, surface_id, 0);
    }

    /// Progressive tile state is keyed by surface id, so a deleted surface must take its
    /// state with it — a recreated surface with the same id would otherwise refine the
    /// OLD surface's pixels into the new one. FreeRDP's gdi_DeleteSurface does the same
    /// via progressive_delete_surface_context.
    #[test]
    fn deleting_a_surface_drops_its_progressive_tile_state() {
        let mut handler = GfxHandler::new(store());
        handler.on_surface_created(&egfx_surface(3, 64, 64));
        decode_progressive_into(&mut handler, 3);
        assert_eq!(handler.progressive.context_count(), 1);

        handler.on_surface_deleted(3);
        assert_eq!(
            handler.progressive.context_count(),
            0,
            "the surface's tile state must die with the surface"
        );
    }

    #[test]
    fn deleting_an_active_progressive_context_drops_its_surface_state() {
        let mut handler = GfxHandler::new(store());
        handler.on_surface_created(&egfx_surface(5, 64, 64));
        decode_progressive_into_with_context(&mut handler, 5, 11);
        assert_eq!(handler.progressive_contexts.get(&5), Some(&11));

        handler.on_delete_encoding_context(&DeleteEncodingContextPdu {
            surface_id: 5,
            codec_context_id: 11,
        });

        assert_eq!(handler.progressive.context_count(), 0);
        assert!(!handler.progressive_contexts.contains_key(&5));
    }

    #[test]
    fn deleting_an_obsolete_rotated_context_keeps_live_progressive_state() {
        let mut handler = GfxHandler::new(store());
        handler.on_surface_created(&egfx_surface(6, 64, 64));
        decode_progressive_into_with_context(&mut handler, 6, 21);
        decode_progressive_into_with_context(&mut handler, 6, 22);
        assert_eq!(handler.progressive.context_count(), 1);
        assert_eq!(handler.progressive_contexts.get(&6), Some(&22));

        handler.on_delete_encoding_context(&DeleteEncodingContextPdu {
            surface_id: 6,
            codec_context_id: 21,
        });

        assert_eq!(handler.progressive.context_count(), 1);
        assert_eq!(handler.progressive_contexts.get(&6), Some(&22));
    }

    #[test]
    fn recreating_a_surface_id_clears_progressive_state_and_context_mapping() {
        let mut handler = GfxHandler::new(store());
        handler.on_surface_created(&egfx_surface(7, 64, 64));
        decode_progressive_into_with_context(&mut handler, 7, 31);
        assert_eq!(handler.progressive.context_count(), 1);

        handler.on_surface_created(&egfx_surface(7, 32, 32));

        assert_eq!(handler.progressive.context_count(), 0);
        assert!(!handler.progressive_contexts.contains_key(&7));
    }

    #[test]
    fn deleting_one_surface_context_does_not_touch_another_surface() {
        let mut handler = GfxHandler::new(store());
        handler.on_surface_created(&egfx_surface(8, 64, 64));
        handler.on_surface_created(&egfx_surface(9, 64, 64));
        decode_progressive_into_with_context(&mut handler, 8, 41);
        decode_progressive_into_with_context(&mut handler, 9, 41);

        handler.on_delete_encoding_context(&DeleteEncodingContextPdu {
            surface_id: 8,
            codec_context_id: 41,
        });

        assert_eq!(handler.progressive.context_count(), 1);
        assert!(!handler.progressive_contexts.contains_key(&8));
        assert_eq!(handler.progressive_contexts.get(&9), Some(&41));
    }

    /// A reset must NOT discard codec state for surviving surfaces. Measured on quench:
    /// resetting the ClearCodec decoder at ResetGraphics produced 74 "V-bar cache miss
    /// on hit" failures on the next repaint — the server keeps referencing state it
    /// established before the reset. Codec state dies with the surface, never the reset.
    #[test]
    fn reset_graphics_keeps_codec_state_for_surviving_surfaces() {
        let mut handler = GfxHandler::new(store());
        handler.on_surface_created(&egfx_surface(4, 64, 64));
        decode_progressive_into(&mut handler, 4);
        assert_eq!(handler.progressive.context_count(), 1);

        handler.on_reset_graphics(1920, 1080);
        assert_eq!(
            handler.progressive.context_count(),
            1,
            "a surviving surface keeps its codec state across a reset"
        );
    }
}
