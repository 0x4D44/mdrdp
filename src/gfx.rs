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
pub struct GfxHandler {
    store: Arc<Mutex<SurfaceStore>>,
    /// One instance for the session — see the module docs.
    decoder: ClearCodecDecoder,
    stats: GfxStatsHandle,
    /// Surfaces we have mirrored into the store. `ResetGraphics` implicitly destroys all
    /// surfaces (MS-RDPEGFX 3.3.5.14), and `SurfaceStore` has no bulk clear, so we need
    /// to know which ids to delete.
    live_surfaces: HashSet<u16>,
    /// Dimensions of each cached bitmap, mirrored so `CacheToSurface` can reject a
    /// destination point that would overflow a `u16` coordinate before the store does
    /// the arithmetic.
    cache_dims: HashMap<u16, (u16, u16)>,
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
            stats: GfxStatsHandle::new(),
            live_surfaces: HashSet::new(),
            cache_dims: HashMap::new(),
        }
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
            Err(_) => {
                // The error carries protocol field names, not pixels, but there is
                // nothing here a counter does not already say. One dropped tile is a
                // smear the next frame repaints; an error returned to the DVC processor
                // would end the session.
                self.stats
                    .note(|s| s.decode_errors = s.decode_errors.saturating_add(1));
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
        self.stats.note(|s| {
            *s.codec_ids_seen.entry(name).or_insert(0) += 1;
            s.undecoded_regions = s.undecoded_regions.saturating_add(1);
        });
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
                // RFX Progressive. Not decoded yet; counted so the mix is visible.
                let name = format!("WireToSurface2/{:?}", p.codec_id);
                self.stats.note(|s| {
                    *s.codec_ids_seen.entry(name).or_insert(0) += 1;
                    s.unhandled_pdus = s.unhandled_pdus.saturating_add(1);
                });
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
            s.undecoded_regions, 1,
            "and flagged as a region we cannot yet paint"
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
