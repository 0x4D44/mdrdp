//! Client-side EGFX implementation
//!
//! This module provides client-side support for the Graphics Pipeline Extension
//! ([MS-RDPEGFX]), including H.264 AVC420 decode and surface management.
//!
//! # Protocol Compliance
//!
//! This implementation follows MS-RDPEGFX client requirements:
//!
//! - **Capability Negotiation**: Advertises V8 through V10.7 ([2.2.3])
//! - **Surface Management**: Tracks server-created surfaces ([3.3.1.6])
//! - **Frame Acknowledgment**: Sends `FrameAcknowledge` after `EndFrame` ([3.3.5.12])
//! - **Codec Dispatch**: Routes `WireToSurface1` by `codec_id` ([3.3.5.2])
//!
//! # Architecture
//!
//! ```text
//! Server                                  Client
//!    |                                       |
//!    |--- CapabilitiesConfirm -------------->|
//!    |--- ResetGraphics -------------------->|
//!    |--- CreateSurface -------------------->|
//!    |--- MapSurfaceToOutput --------------->|
//!    |                                       |
//!    |  (For each frame:)                    |
//!    |--- StartFrame ----------------------->|
//!    |--- WireToSurface1 (H.264) ----------->|  -> H264Decoder::decode()
//!    |--- EndFrame ------------------------->|  -> FrameAcknowledge
//!    |                                       |
//!    |<---------- FrameAcknowledge ----------|
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use ironrdp_egfx::client::{GraphicsPipelineClient, GraphicsPipelineHandler, BitmapUpdate};
//! use ironrdp_egfx::decode::H264Decoder;
//!
//! struct MyHandler;
//!
//! impl GraphicsPipelineHandler for MyHandler {
//!     fn on_bitmap_updated(&mut self, update: &BitmapUpdate) {
//!         // Render decoded bitmap to screen
//!     }
//! }
//!
//! let client = GraphicsPipelineClient::new(Box::new(MyHandler), None);
//! ```
//!
//! [MS-RDPEGFX]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/da5c75f9-cd99-450c-98c4-014a496942b0
//! [2.2.3]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/b5e09f90-6dde-47ca-8ec1-7dcdd5dc70b0
//! [3.3.1.6]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/83cb08ff-c97f-4d08-b834-7aa69cdea6c5
//! [3.3.5.2]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/90aba3e3-d4a8-4af1-b1bb-a94e2313bbf0
//! [3.3.5.12]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/e3c80bff-3e4e-4e65-b7c2-c2cd6b1fb4f5

use std::collections::BTreeMap;

use ironrdp_core::{Decode as _, ReadCursor, impl_as_any};
use ironrdp_dvc::{DvcClientProcessor, DvcMessage, DvcProcessor};
use ironrdp_graphics::zgfx;
use ironrdp_pdu::geometry::{ExclusiveRectangle, InclusiveRectangle, Rectangle as _};
use ironrdp_pdu::{PduResult, decode_cursor, decode_err, pdu_other_err};
use tracing::{debug, trace, warn};

use ironrdp_graphics::avc444::{Yuv420Frame, Yuv444Buffer};

use crate::CHANNEL_NAME;
use crate::decode::H264Decoder;
use crate::pdu::{
    Avc420BitmapStream, Avc444BitmapStream, CacheImportReplyPdu, CacheToSurfacePdu, CapabilitiesAdvertisePdu,
    CapabilitiesV8Flags, CapabilitiesV81Flags, CapabilitiesV107Flags, CapabilitySet, Codec1Type,
    DeleteEncodingContextPdu, Encoding, EvictCacheEntryPdu, FrameAcknowledgePdu, GfxPdu,
    MapSurfaceToScaledOutputPdu, MapSurfaceToScaledWindowPdu, MapSurfaceToWindowPdu, PixelFormat, QueueDepth,
    RawCapabilitySet, SolidFillPdu, SurfaceToCachePdu, SurfaceToSurfacePdu, WireToSurface2Pdu,
};

/// Max capacity to keep for decompressed buffer when cleared.
const MAX_DECOMPRESSED_BUFFER_CAPACITY: usize = 16384; // 16 KiB

// ============================================================================
// Surface Management
// ============================================================================

/// Client-side surface state
///
/// Per [MS-RDPEGFX 3.3.1.6], the client maintains an "Offscreen Surfaces
/// ADM element" tracking surfaces created by the server.
///
/// [MS-RDPEGFX 3.3.1.6]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/83cb08ff-c97f-4d08-b834-7aa69cdea6c5
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Surface {
    /// Surface identifier (assigned by server)
    pub id: u16,
    /// Surface width in pixels
    pub width: u16,
    /// Surface height in pixels
    pub height: u16,
    /// Pixel format
    pub pixel_format: PixelFormat,
    /// Whether this surface is mapped to an output
    pub is_mapped: bool,
    /// Output X origin (if mapped)
    pub output_origin_x: u32,
    /// Output Y origin (if mapped)
    pub output_origin_y: u32,
}

// ============================================================================
// Codec Capabilities
// ============================================================================

/// Codec capabilities determined from negotiated capability set
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CodecCapabilities {
    /// AVC420 (H.264 4:2:0) is available
    pub avc420: bool,
    /// AVC444 (H.264 4:4:4) is available
    pub avc444: bool,
    /// Small cache mode
    pub small_cache: bool,
    /// Thin client mode
    pub thin_client: bool,
}

impl CodecCapabilities {
    fn from_capability_set(cap: &CapabilitySet) -> Self {
        // Mirrors the server-side extraction logic
        match cap {
            CapabilitySet::V8 { flags } => Self {
                avc420: false,
                avc444: false,
                small_cache: flags.contains(CapabilitiesV8Flags::SMALL_CACHE),
                thin_client: flags.contains(CapabilitiesV8Flags::THIN_CLIENT),
            },
            CapabilitySet::V8_1 { flags } => Self {
                avc420: flags.contains(CapabilitiesV81Flags::AVC420_ENABLED),
                avc444: false,
                small_cache: flags.contains(CapabilitiesV81Flags::SMALL_CACHE),
                thin_client: flags.contains(CapabilitiesV81Flags::THIN_CLIENT),
            },
            CapabilitySet::V10 { flags } | CapabilitySet::V10_2 { flags } => Self {
                avc420: !flags.contains(crate::pdu::CapabilitiesV10Flags::AVC_DISABLED),
                avc444: !flags.contains(crate::pdu::CapabilitiesV10Flags::AVC_DISABLED),
                small_cache: flags.contains(crate::pdu::CapabilitiesV10Flags::SMALL_CACHE),
                thin_client: false,
            },
            CapabilitySet::V10_1 => Self {
                avc420: true,
                avc444: true,
                small_cache: false,
                thin_client: false,
            },
            CapabilitySet::V10_3 { flags } => Self {
                avc420: !flags.contains(crate::pdu::CapabilitiesV103Flags::AVC_DISABLED),
                avc444: !flags.contains(crate::pdu::CapabilitiesV103Flags::AVC_DISABLED),
                small_cache: false,
                thin_client: flags.contains(crate::pdu::CapabilitiesV103Flags::AVC_THIN_CLIENT),
            },
            CapabilitySet::V10_4 { flags }
            | CapabilitySet::V10_5 { flags }
            | CapabilitySet::V10_6 { flags }
            | CapabilitySet::V10_6Err { flags } => Self {
                avc420: !flags.contains(crate::pdu::CapabilitiesV104Flags::AVC_DISABLED),
                avc444: !flags.contains(crate::pdu::CapabilitiesV104Flags::AVC_DISABLED),
                small_cache: flags.contains(crate::pdu::CapabilitiesV104Flags::SMALL_CACHE),
                thin_client: flags.contains(crate::pdu::CapabilitiesV104Flags::AVC_THIN_CLIENT),
            },
            CapabilitySet::V10_7 { flags } => Self {
                avc420: !flags.contains(CapabilitiesV107Flags::AVC_DISABLED),
                avc444: !flags.contains(CapabilitiesV107Flags::AVC_DISABLED),
                small_cache: flags.contains(CapabilitiesV107Flags::SMALL_CACHE),
                thin_client: flags.contains(CapabilitiesV107Flags::AVC_THIN_CLIENT),
            },
        }
    }
}

// ============================================================================
// Bitmap Update
// ============================================================================

/// Decoded bitmap data for a surface region
///
/// Delivered to [`GraphicsPipelineHandler::on_bitmap_updated`] when
/// a `WireToSurface1` PDU is processed with decoded pixel data.
#[derive(Debug)]
#[non_exhaustive]
pub struct BitmapUpdate {
    /// Surface this update applies to
    pub surface_id: u16,
    /// Destination rectangle within the surface (exclusive `right`/`bottom`)
    pub destination_rectangle: ExclusiveRectangle,
    /// Codec that produced this update
    pub codec_id: Codec1Type,
    /// RGBA pixel data (4 bytes per pixel), row-major
    ///
    /// Dimensions match `width * height * 4` bytes.
    /// May be empty if decode was skipped (no decoder configured).
    pub data: Vec<u8>,
    /// Width of the decoded data in pixels
    pub width: u16,
    /// Height of the decoded data in pixels
    pub height: u16,
}

// ============================================================================
// Handler Trait
// ============================================================================

/// Handler trait for client-side EGFX events
///
/// Implement this trait to receive decoded bitmap data and surface
/// lifecycle notifications from the EGFX pipeline.
///
/// All methods have default no-op implementations so you only need
/// to override the ones relevant to your use case.
pub trait GraphicsPipelineHandler: Send {
    /// Returns the capability sets to advertise to the server
    ///
    /// The default advertises V10.7 (AVC420+AVC444), V8.1 (AVC420 only),
    /// and V8 (no AVC) as fallback.
    ///
    /// Note: AVC-capable versions are automatically filtered out at
    /// advertisement time if no H.264 decoder is configured on the
    /// [`GraphicsPipelineClient`]. If all returned sets require AVC
    /// and no decoder is available, a V8-only fallback is used.
    fn capabilities(&self) -> Vec<CapabilitySet> {
        vec![
            CapabilitySet::V10_7 {
                flags: CapabilitiesV107Flags::SMALL_CACHE,
            },
            CapabilitySet::V8_1 {
                flags: CapabilitiesV81Flags::AVC420_ENABLED | CapabilitiesV81Flags::SMALL_CACHE,
            },
            CapabilitySet::V8 {
                flags: CapabilitiesV8Flags::SMALL_CACHE,
            },
        ]
    }

    /// Called when the server confirms negotiated capabilities
    fn on_capabilities_confirmed(&mut self, _caps: &CapabilitySet) {}

    /// Called when the server resets the graphics output buffer
    fn on_reset_graphics(&mut self, _width: u32, _height: u32) {}

    /// Called when a surface is created by the server
    fn on_surface_created(&mut self, _surface: &Surface) {}

    /// Called when a surface is deleted by the server
    fn on_surface_deleted(&mut self, _surface_id: u16) {}

    /// Called when a surface is mapped to an output position
    fn on_surface_mapped(&mut self, _surface_id: u16, _origin_x: u32, _origin_y: u32) {}

    /// Called when decoded bitmap data is available for a surface
    ///
    /// This is the primary output path. The `update` contains the
    /// surface ID, destination rectangle, and RGBA pixel data.
    fn on_bitmap_updated(&mut self, _update: &BitmapUpdate) {}

    /// Called when a codec payload could not be decoded and was skipped
    ///
    /// mdrdp patch: the client's resilience policy is to skip a bad frame rather
    /// than error the channel, but a silent skip is invisible staleness — exactly
    /// what the visibility requirement exists to surface. `reason` is a stable,
    /// payload-free label suitable for tallying.
    fn on_decode_failure(&mut self, _codec_id: Codec1Type, _reason: &'static str) {}

    /// Called when a logical frame is complete
    ///
    /// All bitmap updates between the corresponding `StartFrame`
    /// and this notification belong to the same logical frame.
    fn on_frame_complete(&mut self, _frame_id: u32) {}

    /// Called when the EGFX channel is closed
    fn on_close(&mut self) {}

    // ========================================================================
    // Additional PDU handlers (server→client)
    // ========================================================================

    /// Called when the server fills a surface region with a solid color
    ///
    /// Per [MS-RDPEGFX 3.3.5.4].
    ///
    /// [MS-RDPEGFX 3.3.5.4]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/d696ab07-fd47-42f6-a601-c8b6fae26577
    fn on_solid_fill(&mut self, _pdu: &SolidFillPdu) {}

    /// Called when the server copies pixels between surfaces
    ///
    /// Per [MS-RDPEGFX 3.3.5.5].
    ///
    /// [MS-RDPEGFX 3.3.5.5]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/0b19d058-fff0-43e5-8671-8c4186d60529
    fn on_surface_to_surface(&mut self, _pdu: &SurfaceToSurfacePdu) {}

    /// Called when the server caches a surface region
    ///
    /// Per [MS-RDPEGFX 3.3.5.6].
    ///
    /// [MS-RDPEGFX 3.3.5.6]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/01108b9f-a888-4e5c-b790-42d5c5985998
    fn on_surface_to_cache(&mut self, _pdu: &SurfaceToCachePdu) {}

    /// Called when the server renders cached content to a surface
    ///
    /// Per [MS-RDPEGFX 3.3.5.7].
    ///
    /// [MS-RDPEGFX 3.3.5.7]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/78c00bcd-f5cb-4c33-8d6c-f4cd50facfab
    fn on_cache_to_surface(&mut self, _pdu: &CacheToSurfacePdu) {}

    /// Called when the server evicts a cache entry
    ///
    /// Per [MS-RDPEGFX 3.3.5.8].
    ///
    /// [MS-RDPEGFX 3.3.5.8]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/9dd32c5c-fabc-497b-81be-776fa581a4f6
    fn on_evict_cache_entry(&mut self, _pdu: &EvictCacheEntryPdu) {}

    /// Called when the server maps a surface to a RAIL window
    ///
    /// Per [MS-RDPEGFX 2.2.2.20].
    ///
    /// [MS-RDPEGFX 2.2.2.20]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/2ec1357c-ee65-4d9b-89f3-8fc49348c92a
    fn on_map_surface_to_window(&mut self, _pdu: &MapSurfaceToWindowPdu) {}

    /// Called when the server maps a surface to a scaled output
    ///
    /// Per [MS-RDPEGFX 2.2.2.22].
    ///
    /// [MS-RDPEGFX 2.2.2.22]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/3fcc3e63-e5a2-4b18-a572-26bbeb87b3aa
    fn on_map_surface_to_scaled_output(&mut self, _pdu: &MapSurfaceToScaledOutputPdu) {}

    /// Called when the server maps a surface to a scaled RAIL window
    ///
    /// Per [MS-RDPEGFX 2.2.2.23].
    ///
    /// [MS-RDPEGFX 2.2.2.23]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/22fc0ec7-38ce-4d9d-ad6d-93a0e9f3c38c
    fn on_map_surface_to_scaled_window(&mut self, _pdu: &MapSurfaceToScaledWindowPdu) {}

    /// Called for progressive codec (RFX Progressive) bitmap data
    ///
    /// Per [MS-RDPEGFX 3.3.5.3].
    ///
    /// [MS-RDPEGFX 3.3.5.3]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/e6dbb3a7-3de0-44a5-a1ee-9de90f75e7e0
    fn on_wire_to_surface2(&mut self, _pdu: &WireToSurface2Pdu) {}

    /// Called when the server deletes a progressive encoding context
    ///
    /// Per [MS-RDPEGFX 2.2.2.3].
    ///
    /// [MS-RDPEGFX 2.2.2.3]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/bd0c64d4-07b3-47e5-9f7b-ba5c14a3a2e2
    fn on_delete_encoding_context(&mut self, _pdu: &DeleteEncodingContextPdu) {}

    /// Called when the server replies to a cache import offer
    ///
    /// Per [MS-RDPEGFX 2.2.2.17].
    ///
    /// [MS-RDPEGFX 2.2.2.17]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/7c7a0a5d-50c1-44b9-a2e7-44b47ce1e49d
    fn on_cache_import_reply(&mut self, _pdu: &CacheImportReplyPdu) {}

    /// Called for PDUs that have no specific handler
    ///
    /// This is a catch-all for any GfxPdu variant not matched above.
    fn on_unhandled_pdu(&mut self, _pdu: &GfxPdu) {}
}

// ============================================================================
// Client State Machine
// ============================================================================

/// Client state machine states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientState {
    /// Waiting for server `CapabilitiesConfirm`
    WaitingForConfirm,
    /// Channel is active, processing frames
    Active,
    /// Channel has been closed
    Closed,
}

// ============================================================================
// Graphics Pipeline Client
// ============================================================================

/// Client for the Graphics Pipeline Virtual Channel (EGFX)
///
/// This client handles capability negotiation, surface tracking,
/// H.264 AVC420 decode, and frame acknowledgment per [MS-RDPEGFX].
///
/// [MS-RDPEGFX]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/da5c75f9-cd99-450c-98c4-014a496942b0
pub struct GraphicsPipelineClient {
    handler: Box<dyn GraphicsPipelineHandler>,
    h264_decoder: Option<Box<dyn H264Decoder>>,

    decompressor: zgfx::Decompressor,
    decompressed_buffer: Vec<u8>,

    state: ClientState,
    negotiated_caps: Option<CapabilitySet>,
    codec_caps: CodecCapabilities,

    surfaces: BTreeMap<u16, Surface>,
    current_frame_id: Option<u32>,
    frames_queued: u32,
    total_frames_decoded: u32,

    /// Per-surface persistent YUV444 state for AVC444/AVC444v2.
    ///
    /// LC=1 (luma-only) and LC=2 (chroma-only) updates change only part of the
    /// frame, so the combined state must outlive individual PDUs. Keyed by surface;
    /// dies with the surface (and on CreateSurface id reuse), survives ResetGraphics
    /// like the surfaces themselves.
    avc444_buffers: BTreeMap<u16, Yuv444Buffer>,
    /// Decoded-frame scratch (main, aux) — an LC=0 update needs both alive at once.
    yuv_scratch: (Yuv420Frame, Yuv420Frame),
    /// RGBA conversion scratch, recycled across updates.
    rgba_scratch: Vec<u8>,
    /// Opt-in dump of raw AVC444 container payloads (dir, files written so far).
    /// **Session content** — only ever set by an operator who asked for it, and
    /// capped at [`Self::AVC_CAPTURE_MAX`] files.
    avc_capture: Option<(std::path::PathBuf, u32)>,
}

impl GraphicsPipelineClient {
    /// Cap on captured AVC payload files, so a long session cannot fill a disk.
    const AVC_CAPTURE_MAX: u32 = 32;

    /// Create a new `GraphicsPipelineClient`
    ///
    /// If `h264_decoder` is `None`, AVC420 frames are logged and skipped.
    pub fn new(handler: Box<dyn GraphicsPipelineHandler>, h264_decoder: Option<Box<dyn H264Decoder>>) -> Self {
        Self {
            handler,
            h264_decoder,
            decompressor: zgfx::Decompressor::new(),
            decompressed_buffer: Vec::new(),
            state: ClientState::WaitingForConfirm,
            negotiated_caps: None,
            codec_caps: CodecCapabilities::default(),
            surfaces: BTreeMap::new(),
            current_frame_id: None,
            frames_queued: 0,
            total_frames_decoded: 0,
            avc444_buffers: BTreeMap::new(),
            yuv_scratch: (Yuv420Frame::default(), Yuv420Frame::default()),
            rgba_scratch: Vec::new(),
            avc_capture: None,
        }
    }

    /// Dump the first [`Self::AVC_CAPTURE_MAX`] raw AVC444 container payloads into
    /// `dir`, for offline replay of a decode anomaly. mdrdp patch. Writes **session
    /// content**; opt-in by construction.
    #[must_use]
    pub fn capturing_avc_payloads_to(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.avc_capture = Some((dir.into(), 0));
        self
    }

    /// Write one captured payload, silently stopping at the cap or on I/O trouble
    /// (capture must never affect the session).
    fn capture_avc_payload(&mut self, codec_id: Codec1Type, payload: &[u8]) {
        let Some((dir, written)) = &mut self.avc_capture else {
            return;
        };
        if *written >= Self::AVC_CAPTURE_MAX || std::fs::create_dir_all(&*dir).is_err() {
            return;
        }
        let path = dir.join(format!("avc444-{written:03}-{codec_id:?}.bin"));
        if std::fs::write(path, payload).is_ok() {
            *written += 1;
        }
    }

    // ========================================================================
    // State Queries
    // ========================================================================

    /// Check if the client has completed capability negotiation
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.state == ClientState::Active
    }

    /// Get the negotiated capability set
    #[must_use]
    pub fn negotiated_capabilities(&self) -> Option<&CapabilitySet> {
        self.negotiated_caps.as_ref()
    }

    /// Get codec capabilities determined from negotiation
    #[must_use]
    pub fn codec_capabilities(&self) -> &CodecCapabilities {
        &self.codec_caps
    }

    /// Get a surface by ID
    #[must_use]
    pub fn get_surface(&self, surface_id: u16) -> Option<&Surface> {
        self.surfaces.get(&surface_id)
    }

    /// Get the total number of frames decoded
    #[must_use]
    pub fn total_frames_decoded(&self) -> u32 {
        self.total_frames_decoded
    }

    // ========================================================================
    // PDU Handlers
    // ========================================================================

    fn handle_pdu(&mut self, pdu: GfxPdu) -> PduResult<Vec<DvcMessage>> {
        match pdu {
            GfxPdu::CapabilitiesConfirm(confirm) => {
                self.handle_capabilities_confirm(confirm.0);
                Ok(vec![])
            }
            GfxPdu::ResetGraphics(reset) => {
                self.handle_reset_graphics(reset.width, reset.height);
                Ok(vec![])
            }
            GfxPdu::CreateSurface(create) => {
                self.handle_create_surface(create.surface_id, create.width, create.height, create.pixel_format);
                Ok(vec![])
            }
            GfxPdu::DeleteSurface(delete) => {
                self.handle_delete_surface(delete.surface_id);
                Ok(vec![])
            }
            GfxPdu::MapSurfaceToOutput(map) => {
                self.handle_map_surface(map.surface_id, map.output_origin_x, map.output_origin_y);
                Ok(vec![])
            }
            GfxPdu::StartFrame(start) => {
                self.current_frame_id = Some(start.frame_id);
                self.frames_queued = self.frames_queued.saturating_add(1);
                trace!(frame_id = start.frame_id, "StartFrame");
                Ok(vec![])
            }
            GfxPdu::WireToSurface1(wire) => {
                self.handle_wire_to_surface1(wire)?;
                Ok(vec![])
            }
            GfxPdu::WireToSurface2(pdu) => {
                trace!("WireToSurface2 (progressive codec)");
                self.handler.on_wire_to_surface2(&pdu);
                Ok(vec![])
            }
            GfxPdu::EndFrame(end) => self.handle_end_frame(end.frame_id),

            // Surface operations
            GfxPdu::SolidFill(pdu) => {
                trace!(surface_id = pdu.surface_id, "SolidFill");
                self.handler.on_solid_fill(&pdu);
                Ok(vec![])
            }
            GfxPdu::SurfaceToSurface(pdu) => {
                trace!(
                    src = pdu.source_surface_id,
                    dst = pdu.destination_surface_id,
                    "SurfaceToSurface"
                );
                self.handler.on_surface_to_surface(&pdu);
                Ok(vec![])
            }

            // Cache operations
            GfxPdu::SurfaceToCache(pdu) => {
                trace!(
                    surface_id = pdu.surface_id,
                    cache_slot = pdu.cache_slot,
                    "SurfaceToCache"
                );
                self.handler.on_surface_to_cache(&pdu);
                Ok(vec![])
            }
            GfxPdu::CacheToSurface(pdu) => {
                trace!(
                    cache_slot = pdu.cache_slot,
                    surface_id = pdu.surface_id,
                    "CacheToSurface"
                );
                self.handler.on_cache_to_surface(&pdu);
                Ok(vec![])
            }
            GfxPdu::EvictCacheEntry(pdu) => {
                trace!(cache_slot = pdu.cache_slot, "EvictCacheEntry");
                self.handler.on_evict_cache_entry(&pdu);
                Ok(vec![])
            }
            GfxPdu::CacheImportReply(pdu) => {
                trace!("CacheImportReply");
                self.handler.on_cache_import_reply(&pdu);
                Ok(vec![])
            }

            // Surface mapping variants
            GfxPdu::MapSurfaceToWindow(pdu) => {
                trace!(
                    surface_id = pdu.surface_id,
                    window_id = pdu.window_id,
                    "MapSurfaceToWindow"
                );
                self.handler.on_map_surface_to_window(&pdu);
                Ok(vec![])
            }
            GfxPdu::MapSurfaceToScaledOutput(pdu) => {
                trace!(surface_id = pdu.surface_id, "MapSurfaceToScaledOutput");
                self.handler.on_map_surface_to_scaled_output(&pdu);
                Ok(vec![])
            }
            GfxPdu::MapSurfaceToScaledWindow(pdu) => {
                trace!(surface_id = pdu.surface_id, "MapSurfaceToScaledWindow");
                self.handler.on_map_surface_to_scaled_window(&pdu);
                Ok(vec![])
            }

            // Progressive codec context management
            GfxPdu::DeleteEncodingContext(pdu) => {
                trace!(
                    surface_id = pdu.surface_id,
                    codec_context_id = pdu.codec_context_id,
                    "DeleteEncodingContext"
                );
                self.handler.on_delete_encoding_context(&pdu);
                Ok(vec![])
            }

            // Catch-all for any remaining PDUs
            other => {
                self.handler.on_unhandled_pdu(&other);
                Ok(vec![])
            }
        }
    }

    fn handle_capabilities_confirm(&mut self, cap: RawCapabilitySet) {
        // Server confirms a single capability set. If we cannot interpret it
        // (unknown version, or malformed body), we still transition to Active
        // to avoid hanging the session, but we keep `negotiated_caps` empty
        // and skip the typed callback so consumers don't observe a confirm
        // they can't reason about.
        let cap = match cap.parsed() {
            Ok(Some(typed)) => typed,
            Ok(None) => {
                warn!(
                    version = cap.version.0,
                    "Server confirmed an unknown EGFX capability version; proceeding with defaults"
                );
                self.state = ClientState::Active;
                return;
            }
            Err(e) => {
                warn!(error = %e, "Failed to parse server's EGFX capabilities confirmation");
                self.state = ClientState::Active;
                return;
            }
        };

        self.codec_caps = CodecCapabilities::from_capability_set(&cap);
        self.state = ClientState::Active;
        let cap = self.negotiated_caps.insert(cap);

        debug!(
            version = ?cap.version(),
            avc420 = self.codec_caps.avc420,
            avc444 = self.codec_caps.avc444,
            "EGFX capabilities confirmed"
        );

        self.handler.on_capabilities_confirmed(cap);
    }

    fn handle_reset_graphics(&mut self, width: u32, height: u32) {
        // mdrdp patch: MS-RDPEGFX 3.3.5.14 resizes only the Graphics Output Buffer.
        // Surfaces and bitmap-cache entries have explicit delete/evict PDUs and survive
        // ResetGraphics; clearing this table makes later Map/DeleteSurface PDUs vanish.

        // Reset frame tracking state so subsequent FrameAcknowledge PDUs
        // don't report stale queue depth from a previous stream.
        // Capability state (negotiated_caps, codec_caps) is NOT reset here:
        // per spec, capabilities are negotiated via CapabilitiesConfirm before
        // ResetGraphics, and a ResetGraphics does not re-negotiate capabilities.
        self.current_frame_id = None;
        self.frames_queued = 0;

        // Reset decoder state for new stream
        if let Some(ref mut decoder) = self.h264_decoder {
            decoder.reset();
        }

        debug!(width, height, "Graphics reset");
        self.handler.on_reset_graphics(width, height);
    }

    fn handle_create_surface(&mut self, surface_id: u16, width: u16, height: u16, pixel_format: PixelFormat) {
        if width == 0 || height == 0 {
            warn!(surface_id, width, height, "Ignoring CreateSurface with zero dimensions");
            return;
        }

        let surface = Surface {
            id: surface_id,
            width,
            height,
            pixel_format,
            is_mapped: false,
            output_origin_x: 0,
            output_origin_y: 0,
        };

        // A CreateSurface may reuse an id without a DeleteSurface (the ordinary
        // resolution-change sequence). Any retained AVC444 state belongs to the old
        // surface and its old dimensions.
        self.avc444_buffers.remove(&surface_id);

        debug!(surface_id, width, height, ?pixel_format, "Surface created");
        self.handler.on_surface_created(&surface);
        self.surfaces.insert(surface_id, surface);
    }

    fn handle_delete_surface(&mut self, surface_id: u16) {
        self.avc444_buffers.remove(&surface_id);
        if self.surfaces.remove(&surface_id).is_some() {
            debug!(surface_id, "Surface deleted");
            self.handler.on_surface_deleted(surface_id);
        } else {
            warn!(surface_id, "DeleteSurface for unknown surface");
        }
    }

    fn handle_map_surface(&mut self, surface_id: u16, origin_x: u32, origin_y: u32) {
        if let Some(surface) = self.surfaces.get_mut(&surface_id) {
            surface.is_mapped = true;
            surface.output_origin_x = origin_x;
            surface.output_origin_y = origin_y;
            debug!(surface_id, origin_x, origin_y, "Surface mapped to output");
            self.handler.on_surface_mapped(surface_id, origin_x, origin_y);
        } else {
            warn!(surface_id, "MapSurfaceToOutput for unknown surface");
        }
    }

    fn handle_wire_to_surface1(&mut self, pdu: crate::pdu::WireToSurface1Pdu) -> PduResult<()> {
        let surface = self
            .surfaces
            .get(&pdu.surface_id)
            .ok_or_else(|| pdu_other_err!("unknown surface in WireToSurface1"))?;

        // Validate rectangle ordering (left <= right, top <= bottom)
        let rect = &pdu.destination_rectangle;
        if rect.left > rect.right || rect.top > rect.bottom {
            warn!(
                left = rect.left,
                top = rect.top,
                right = rect.right,
                bottom = rect.bottom,
                "invalid destination rectangle ordering"
            );
            return Err(pdu_other_err!("invalid destination rectangle ordering"));
        }

        // Validate destination rectangle against surface bounds. The rectangle
        // uses exclusive `right`/`bottom`, so a full-surface update has
        // `right == surface.width` and `bottom == surface.height`, which is valid.
        if rect.right > surface.width || rect.bottom > surface.height {
            warn!(
                surface_id = pdu.surface_id,
                rect_right = rect.right,
                rect_bottom = rect.bottom,
                surface_width = surface.width,
                surface_height = surface.height,
                "WireToSurface1 destination rectangle exceeds surface bounds"
            );
        }

        match pdu.codec_id {
            Codec1Type::Avc420 => {
                self.decode_avc420(pdu.surface_id, &pdu.destination_rectangle, &pdu.bitmap_data)?;
            }
            codec @ (Codec1Type::Avc444 | Codec1Type::Avc444v2) => {
                self.decode_avc444(pdu.surface_id, codec, &pdu.bitmap_data);
            }
            Codec1Type::Uncompressed => {
                self.handle_uncompressed(pdu);
            }
            _ => {
                trace!(codec_id = ?pdu.codec_id, "Forwarding unsupported codec to handler");
                self.handler.on_unhandled_pdu(&GfxPdu::WireToSurface1(pdu));
            }
        }

        Ok(())
    }

    fn decode_avc420(&mut self, surface_id: u16, dest_rect: &ExclusiveRectangle, bitmap_data: &[u8]) -> PduResult<()> {
        let mut cursor = ReadCursor::new(bitmap_data);
        let stream = Avc420BitmapStream::decode(&mut cursor).map_err(|e| decode_err!(e))?;

        let Some(ref mut decoder) = self.h264_decoder else {
            debug!("No H.264 decoder configured, skipping AVC420 frame");
            return Ok(());
        };

        // mdrdp patch: a failed decode of one access unit skips that frame instead of
        // erroring the whole channel — the region stays stale until the next IDR heals
        // it, which beats tearing the session down over a single bad frame. (A hostile
        // or buggy server gets a dropped frame and a warning, never a stall.)
        let frame = match decoder.decode(stream.data) {
            Ok(frame) => frame,
            Err(e) => {
                warn!(error = %e, "H.264 decode failed; skipping this frame");
                self.handler.on_decode_failure(Codec1Type::Avc420, "h264 decode failed");
                return Ok(());
            }
        };

        let dest_width = dest_rect.width();
        let dest_height = dest_rect.height();

        // Decoded frame must be at least as large as the destination rectangle.
        // Larger is expected (macroblock alignment) and handled by cropping.
        // Smaller means the server sent mismatched dimensions — a skipped (and
        // counted) frame, not a dead channel: the module doctrine is that a dropped
        // region is a glitch the next frame repaints.
        if frame.width() < u32::from(dest_width) || frame.height() < u32::from(dest_height) {
            warn!(
                frame_width = frame.width(),
                frame_height = frame.height(),
                dest_width,
                dest_height,
                "decoded frame smaller than destination rectangle; skipping this frame"
            );
            self.handler
                .on_decode_failure(Codec1Type::Avc420, "decoded frame smaller than destination");
            return Ok(());
        }

        let cropped_data = crop_decoded_frame(frame.data(), frame.width(), frame.height(), dest_width, dest_height);

        let update = BitmapUpdate {
            surface_id,
            destination_rectangle: dest_rect.clone(),
            codec_id: Codec1Type::Avc420,
            data: cropped_data,
            width: dest_width,
            height: dest_height,
        };

        self.handler.on_bitmap_updated(&update);
        Ok(())
    }

    /// Decode an AVC444/AVC444v2 update (MS-RDPEGFX 2.2.4.5/2.2.4.6).
    ///
    /// mdrdp patch. The update carries one or two H.264 4:2:0 sub-streams (LC field):
    /// a main (luma) frame and an auxiliary chroma-packing frame, combined in YUV
    /// space into a persistent per-surface YUV444 buffer, then converted to RGBA per
    /// region rect. Both sub-streams go through the SAME decoder sequentially — the
    /// Windows encoder produces a jointly-encoded, single-decoder-compatible pair
    /// (FreeRDP decodes it the same way).
    ///
    /// Every failure skips the frame (logged + counted via `on_decode_failure`),
    /// never errors the channel.
    fn decode_avc444(&mut self, surface_id: u16, codec_id: Codec1Type, bitmap_data: &[u8]) {
        let Some(surface) = self.surfaces.get(&surface_id) else {
            return; // caller validated; defensive
        };
        let (surf_w, surf_h) = (surface.width, surface.height);

        self.capture_avc_payload(codec_id, bitmap_data);
        let started = std::time::Instant::now();
        let mut decode_us: u128 = 0;

        let mut cursor = ReadCursor::new(bitmap_data);
        let stream = match Avc444BitmapStream::decode(&mut cursor) {
            Ok(stream) => stream,
            Err(e) => {
                warn!(error = %e, "AVC444 bitmap stream parse failed; skipping this frame");
                self.handler.on_decode_failure(codec_id, "avc444 stream parse failed");
                return;
            }
        };

        if self.h264_decoder.is_none() {
            debug!("No H.264 decoder configured, skipping AVC444 frame");
            return;
        }

        // Wire rects are RDPGFX_RECT16: EXCLUSIVE right/bottom despite the
        // `InclusiveRectangle` typing (upstream artifact — the destRect handling in
        // this file and FreeRDP both read them exclusive). Convert field-for-field
        // and drop malformed or out-of-surface rects: a reversed rect must not
        // underflow, and an oversized one must not index past the 444 buffer.
        let (stream1_rects, dropped1) = valid_avc_rects(&stream.stream1.rectangles, surf_w, surf_h);
        let (stream2_rects, dropped2) = match stream.stream2.as_ref() {
            Some(stream2) => valid_avc_rects(&stream2.rectangles, surf_w, surf_h),
            None => (Vec::new(), 0),
        };
        if dropped1 + dropped2 > 0 {
            warn!(dropped = dropped1 + dropped2, "AVC444 update carried malformed region rects");
            self.handler.on_decode_failure(codec_id, "avc444 malformed region rects");
        }

        // Decode the sub-streams (sequentially, one decoder). Which passes run and
        // which rect list feeds each is the LC contract, mirrored from FreeRDP's
        // avc444_decompress. NOTE: Encoding is a bitflags type whose
        // LUMA_AND_CHROMA value is 0, so dispatch MUST be by equality, never
        // `.contains()`.
        enum Passes {
            LumaAndChroma,
            LumaOnly,
            ChromaOnly,
        }
        let passes = match stream.encoding {
            e if e == Encoding::LUMA_AND_CHROMA => Passes::LumaAndChroma,
            e if e == Encoding::LUMA => Passes::LumaOnly,
            e if e == Encoding::CHROMA => Passes::ChromaOnly,
            _ => {
                // The parser rejects encoding values > 2 already.
                self.handler.on_decode_failure(codec_id, "avc444 reserved LC value");
                return;
            }
        };

        let decoder = self
            .h264_decoder
            .as_mut()
            .expect("checked above that a decoder is configured");

        // The chroma passes index the aux frame through the geometry the encoder
        // packed against: the 16-aligned surface. A decoded frame at any other size
        // (an SPS-cropped unaligned frame) would shear the whole frame's chroma, so
        // those degrade to luma-only 4:2:0 output instead (counted below).
        let aligned_w = usize::from(surf_w).div_ceil(16) * 16;
        let aligned_h = usize::from(surf_h).div_ceil(16) * 16;

        let mut chroma_skipped: Option<&'static str> = None;
        let mut emit_rects: Vec<ExclusiveRectangle>;

        match passes {
            Passes::LumaAndChroma => {
                let Some(stream2) = stream.stream2.as_ref() else {
                    // Parser guarantees stream2 for LC=0; defensive.
                    self.handler.on_decode_failure(codec_id, "avc444 missing chroma stream");
                    return;
                };
                let decode_started = std::time::Instant::now();
                if let Err(e) = decoder.decode_yuv420(stream.stream1.data, &mut self.yuv_scratch.0) {
                    warn!(error = %e, "AVC444 luma stream decode failed; skipping this frame");
                    self.handler.on_decode_failure(codec_id, "avc444 luma decode failed");
                    return;
                }
                decode_us += decode_started.elapsed().as_micros();
                let decode_started = std::time::Instant::now();
                if let Err(e) = decoder.decode_yuv420(stream2.data, &mut self.yuv_scratch.1) {
                    warn!(error = %e, "AVC444 chroma stream decode failed; applying luma only");
                    chroma_skipped = Some("avc444 chroma decode failed");
                }
                decode_us += decode_started.elapsed().as_micros();

                let main = &self.yuv_scratch.0;
                let aux = &self.yuv_scratch.1;
                let buffer = self
                    .avc444_buffers
                    .entry(surface_id)
                    .or_insert_with(|| Yuv444Buffer::new(surf_w, surf_h));
                buffer.apply_luma(main, &stream1_rects);
                if chroma_skipped.is_none() {
                    if aux.width == main.width
                        && aux.height == main.height
                        && aux.width == aligned_w
                        && aux.height == aligned_h
                    {
                        match codec_id {
                            Codec1Type::Avc444 => buffer.apply_chroma_v1(aux, &stream2_rects),
                            _ => buffer.apply_chroma_v2(aux, &stream2_rects),
                        }
                    } else {
                        warn!(
                            main_w = main.width,
                            main_h = main.height,
                            aux_w = aux.width,
                            aux_h = aux.height,
                            aligned_w,
                            aligned_h,
                            "AVC444 frame geometry does not match the packing geometry; luma only"
                        );
                        chroma_skipped = Some("avc444 frame geometry mismatch");
                    }
                }
                emit_rects = stream1_rects;
                for rect in stream2_rects {
                    if !emit_rects.contains(&rect) {
                        emit_rects.push(rect);
                    }
                }
            }
            Passes::LumaOnly => {
                let decode_started = std::time::Instant::now();
                if let Err(e) = decoder.decode_yuv420(stream.stream1.data, &mut self.yuv_scratch.0) {
                    warn!(error = %e, "AVC444 luma stream decode failed; skipping this frame");
                    self.handler.on_decode_failure(codec_id, "avc444 luma decode failed");
                    return;
                }
                decode_us += decode_started.elapsed().as_micros();
                let main = &self.yuv_scratch.0;
                self.avc444_buffers
                    .entry(surface_id)
                    .or_insert_with(|| Yuv444Buffer::new(surf_w, surf_h))
                    .apply_luma(main, &stream1_rects);
                emit_rects = stream1_rects;
            }
            Passes::ChromaOnly => {
                // LC=2: the chroma frame travels in stream1, with stream1's rects.
                let decode_started = std::time::Instant::now();
                if let Err(e) = decoder.decode_yuv420(stream.stream1.data, &mut self.yuv_scratch.1) {
                    warn!(error = %e, "AVC444 chroma stream decode failed; skipping this frame");
                    self.handler.on_decode_failure(codec_id, "avc444 chroma decode failed");
                    return;
                }
                decode_us += decode_started.elapsed().as_micros();
                let aux = &self.yuv_scratch.1;
                if aux.width == aligned_w && aux.height == aligned_h {
                    let buffer = self
                        .avc444_buffers
                        .entry(surface_id)
                        .or_insert_with(|| Yuv444Buffer::new(surf_w, surf_h));
                    match codec_id {
                        Codec1Type::Avc444 => buffer.apply_chroma_v1(aux, &stream1_rects),
                        _ => buffer.apply_chroma_v2(aux, &stream1_rects),
                    }
                    emit_rects = stream1_rects;
                } else {
                    warn!(
                        aux_w = aux.width,
                        aux_h = aux.height,
                        aligned_w,
                        aligned_h,
                        "AVC444 chroma-only frame geometry mismatch; skipping this frame"
                    );
                    self.handler.on_decode_failure(codec_id, "avc444 frame geometry mismatch");
                    return;
                }
            }
        }

        if let Some(reason) = chroma_skipped {
            self.handler.on_decode_failure(codec_id, reason);
        }

        // Paint exactly the regions this update combined — never the PDU destRect,
        // which may exceed them (repainting non-AVC surface content from the stale
        // 444 buffer) or miss chroma-only rects outside it. FreeRDP paints the same
        // region lists.
        let Some(buffer) = self.avc444_buffers.get(&surface_id) else {
            return;
        };
        for rect in emit_rects {
            let mut data = core::mem::take(&mut self.rgba_scratch);
            buffer.to_rgba_into(&rect, &mut data);
            let width = rect.width();
            let height = rect.height();
            let update = BitmapUpdate {
                surface_id,
                destination_rectangle: rect,
                codec_id,
                data,
                width,
                height,
            };
            self.handler.on_bitmap_updated(&update);
            self.rgba_scratch = update.data;
        }

        // Per-frame cost split, for the "why does it feel slow" question. Protocol
        // metadata only, debug level (a per-frame line is too hot for info).
        let total_us = started.elapsed().as_micros();
        debug!(
            codec = ?codec_id,
            total_us,
            decode_us,
            combine_convert_us = total_us.saturating_sub(decode_us),
            "AVC444 frame processed"
        );
    }

    fn handle_uncompressed(&mut self, pdu: crate::pdu::WireToSurface1Pdu) {
        let dest_width = pdu.destination_rectangle.width();
        let dest_height = pdu.destination_rectangle.height();

        // Convert wire-format pixels to RGBA.
        // BitmapUpdate.data is always RGBA8888 regardless of codec -- this is
        // the convention so that handlers get a uniform pixel format.
        // Uncompressed wire format is 32-bit LE (0xAARRGGBB → bytes [B, G, R, A]).
        let rgba_data = convert_uncompressed_to_rgba(&pdu.bitmap_data);

        let update = BitmapUpdate {
            surface_id: pdu.surface_id,
            destination_rectangle: pdu.destination_rectangle,
            codec_id: Codec1Type::Uncompressed,
            data: rgba_data,
            width: dest_width,
            height: dest_height,
        };

        self.handler.on_bitmap_updated(&update);
    }

    #[expect(clippy::as_conversions, reason = "Box<GfxPdu> to Box<dyn DvcEncode> coercion")]
    fn handle_end_frame(&mut self, frame_id: u32) -> PduResult<Vec<DvcMessage>> {
        self.total_frames_decoded = self.total_frames_decoded.wrapping_add(1);
        self.current_frame_id = None;
        self.frames_queued = self.frames_queued.saturating_sub(1);

        self.handler.on_frame_complete(frame_id);

        // Per [3.3.5.12]: client MUST send FrameAcknowledge after EndFrame
        let ack = GfxPdu::FrameAcknowledge(FrameAcknowledgePdu {
            queue_depth: QueueDepth::from_u32(self.frames_queued),
            frame_id,
            total_frames_decoded: self.total_frames_decoded,
        });

        trace!(frame_id, "Sending FrameAcknowledge");
        Ok(vec![Box::new(ack) as DvcMessage])
    }
}

impl_as_any!(GraphicsPipelineClient);

impl DvcProcessor for GraphicsPipelineClient {
    fn channel_name(&self) -> &str {
        CHANNEL_NAME
    }

    fn start(&mut self, _channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        let caps = advertised_capabilities(self.handler.capabilities(), self.h264_decoder.as_deref());

        let pdu = GfxPdu::CapabilitiesAdvertise(CapabilitiesAdvertisePdu::from_typed(&caps));

        #[expect(clippy::as_conversions, reason = "Box<GfxPdu> to Box<dyn DvcEncode> coercion")]
        Ok(vec![Box::new(pdu) as DvcMessage])
    }

    fn close(&mut self, _channel_id: u32) {
        self.state = ClientState::Closed;
        self.handler.on_close();
    }

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        // ZGFX decompress
        self.decompressed_buffer.clear();
        self.decompressed_buffer.shrink_to(MAX_DECOMPRESSED_BUFFER_CAPACITY);
        self.decompressor
            .decompress(payload, &mut self.decompressed_buffer)
            .map_err(|e| decode_err!(e))?;

        // Decode all PDUs first (cursor borrows decompressed_buffer)
        let mut pdus = Vec::new();
        {
            let mut cursor = ReadCursor::new(self.decompressed_buffer.as_slice());
            while !cursor.is_empty() {
                let pdu: GfxPdu = decode_cursor(&mut cursor).map_err(|e| decode_err!(e))?;
                pdus.push(pdu);
            }
        }

        // Process decoded PDUs
        let mut responses: Vec<DvcMessage> = Vec::new();
        for pdu in pdus {
            let pdu_responses = self.handle_pdu(pdu)?;
            responses.extend(pdu_responses);
        }

        Ok(responses)
    }
}

impl DvcClientProcessor for GraphicsPipelineClient {}

// ============================================================================
// Frame Cropping
// ============================================================================

/// Filter the handler's capability sets down to what the decoder can decode.
///
/// The advertisement and the decode capability must not drift apart:
/// - no decoder: drop every AVC-bearing set;
/// - decoder without YUV 4:2:0 output: drop AVC444-bearing sets (a V10.x set
///   without AVC_DISABLED implies AVC444) — a V8.1/AVC420 set survives;
/// - decoder with YUV output: advertise as the handler asked.
///
/// If nothing survives, fall back to V8 (never AVC-bearing, always decodable).
fn advertised_capabilities(
    handler_caps: Vec<CapabilitySet>,
    decoder: Option<&dyn H264Decoder>,
) -> Vec<CapabilitySet> {
    let filtered: Vec<CapabilitySet> = handler_caps
        .into_iter()
        .filter(|cap| {
            let implied = CodecCapabilities::from_capability_set(cap);
            match decoder {
                Some(decoder) if decoder.supports_yuv420() => true,
                Some(_) => !implied.avc444,
                None => !implied.avc420,
            }
        })
        .collect();

    if filtered.is_empty() {
        debug!("All advertised capabilities require unavailable AVC decode; falling back to V8");
        vec![CapabilitySet::V8 {
            flags: CapabilitiesV8Flags::SMALL_CACHE,
        }]
    } else {
        filtered
    }
}

/// Validate and convert AVC metablock region rects.
///
/// RDPGFX_RECT16 is exclusive on `right`/`bottom` despite arriving typed as
/// [`InclusiveRectangle`] (upstream typing artifact). Wire data is untrusted:
/// reversed rects would underflow width arithmetic and oversized ones would index
/// past the per-surface buffers, so both are dropped. Returns the surviving rects
/// and the number dropped.
fn valid_avc_rects(rects: &[InclusiveRectangle], surf_w: u16, surf_h: u16) -> (Vec<ExclusiveRectangle>, usize) {
    let mut out = Vec::with_capacity(rects.len());
    let mut dropped = 0usize;
    for rect in rects {
        if rect.left < rect.right && rect.top < rect.bottom && rect.right <= surf_w && rect.bottom <= surf_h {
            out.push(ExclusiveRectangle {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            });
        } else {
            dropped += 1;
        }
    }
    (out, dropped)
}

/// Convert uncompressed 32bpp little-endian pixels to RGBA8888
///
/// The wire format for uncompressed graphics is 0xAARRGGBB in a 32-bit
/// little-endian word, which corresponds to bytes [B, G, R, A]. This
/// reorders to [R, G, B, 0xFF], treating all pixels as fully opaque.
fn convert_uncompressed_to_rgba(src: &[u8]) -> Vec<u8> {
    let mut dst = Vec::with_capacity(src.len());
    for pixel in src.chunks_exact(4) {
        let b = pixel[0];
        let g = pixel[1];
        let r = pixel[2];
        dst.extend_from_slice(&[r, g, b, 0xFF]);
    }
    dst
}

/// Crop a decoded RGBA frame to target dimensions
///
/// H.264 frames are macroblock-aligned (16x16), so decoded frames
/// may be larger than the destination rectangle. This function
/// extracts the top-left region matching the target size.
fn crop_decoded_frame(
    data: &[u8],
    decoded_width: u32,
    decoded_height: u32,
    target_width: u16,
    target_height: u16,
) -> Vec<u8> {
    let tw = u32::from(target_width);
    let th = u32::from(target_height);

    if decoded_width == 0 || decoded_height == 0 || tw == 0 || th == 0 {
        return Vec::new();
    }

    // If dimensions match, return as-is
    if decoded_width == tw && decoded_height == th {
        return data.to_vec();
    }

    let src_stride = decoded_width.saturating_mul(4);
    let dst_stride = tw.saturating_mul(4);
    let rows = th.min(decoded_height);

    #[expect(clippy::as_conversions, reason = "product of u32 values bounded by frame dimensions")]
    let mut cropped = Vec::with_capacity((dst_stride as usize).saturating_mul(rows as usize));

    for row in 0..rows {
        #[expect(clippy::as_conversions, reason = "row * src_stride bounded by frame size")]
        let src_start = (row.saturating_mul(src_stride)) as usize;
        #[expect(clippy::as_conversions, reason = "bounded by frame dimensions")]
        let copy_len = dst_stride.min(src_stride) as usize;
        let src_end = src_start.saturating_add(copy_len);
        if src_end <= data.len() {
            cropped.extend_from_slice(&data[src_start..src_end]);
        }
    }

    #[expect(clippy::as_conversions, reason = "dst_stride * rows bounded by frame dimensions")]
    let expected_len = (dst_stride as usize).saturating_mul(rows as usize);
    if cropped.len() < expected_len {
        tracing::warn!(
            expected = expected_len,
            actual = cropped.len(),
            "Decoded frame data truncated during crop"
        );
    }

    cropped
}

/// Unit tests that require access to private fields (state, surfaces, frame tracking).
/// Integration tests exercising the public DVC API are in ironrdp-testsuite-core/tests/egfx/client.rs.
#[cfg(test)]
mod tests {
    use super::*;

    struct TestHandler;
    impl GraphicsPipelineHandler for TestHandler {
        fn on_capabilities_confirmed(&mut self, _caps: &CapabilitySet) {}
        fn on_reset_graphics(&mut self, _width: u32, _height: u32) {}
        fn on_surface_created(&mut self, _surface: &Surface) {}
        fn on_surface_deleted(&mut self, _surface_id: u16) {}
        fn on_surface_mapped(&mut self, _surface_id: u16, _x: u32, _y: u32) {}
        fn on_bitmap_updated(&mut self, _update: &BitmapUpdate) {}
        fn on_frame_complete(&mut self, _frame_id: u32) {}
        fn on_close(&mut self) {}
        fn on_unhandled_pdu(&mut self, _pdu: &GfxPdu) {}
    }

    #[test]
    fn state_transitions() {
        let mut client = GraphicsPipelineClient::new(Box::new(TestHandler), None);

        assert_eq!(client.state, ClientState::WaitingForConfirm);
        assert!(!client.is_active());

        let _ = client.handle_pdu(GfxPdu::CapabilitiesConfirm(
            crate::pdu::CapabilitiesConfirmPdu::from_typed(&CapabilitySet::V8 {
                flags: CapabilitiesV8Flags::empty(),
            }),
        ));
        assert_eq!(client.state, ClientState::Active);
        assert!(client.is_active());

        client.close(0);
        assert_eq!(client.state, ClientState::Closed);
        assert!(!client.is_active());
    }

    #[test]
    fn reset_graphics_preserves_surfaces_and_resets_frame_tracking() {
        // MS-RDPEGFX 3.3.5.14 resizes only the Graphics Output Buffer: surfaces have
        // their own delete PDU and survive a reset (the mdrdp patch in
        // handle_reset_graphics documents why). Frame tracking, in contrast, is
        // per-stream state and must reset.
        let mut client = GraphicsPipelineClient::new(Box::new(TestHandler), None);

        let _ = client.handle_pdu(GfxPdu::CreateSurface(crate::pdu::CreateSurfacePdu {
            surface_id: 1,
            width: 100,
            height: 100,
            pixel_format: PixelFormat::XRgb,
        }));
        assert_eq!(client.surfaces.len(), 1);

        // Simulate mid-stream state
        let _ = client.handle_pdu(GfxPdu::StartFrame(crate::pdu::StartFramePdu {
            timestamp: crate::pdu::Timestamp {
                milliseconds: 0,
                seconds: 0,
                minutes: 0,
                hours: 0,
            },
            frame_id: 42,
        }));
        assert!(client.current_frame_id.is_some());
        assert_eq!(client.frames_queued, 1);

        let _ = client.handle_pdu(GfxPdu::ResetGraphics(crate::pdu::ResetGraphicsPdu {
            width: 1920,
            height: 1080,
            monitors: vec![],
        }));

        assert_eq!(client.surfaces.len(), 1, "surfaces survive ResetGraphics");
        assert!(client.current_frame_id.is_none(), "frame_id should be reset");
        assert_eq!(client.frames_queued, 0, "frame queue should be reset");
    }

    // ------------------------------------------------------------------------
    // AVC444 client-level tests: LC dispatch, rect ownership, buffer lifecycle.
    // ------------------------------------------------------------------------

    use std::sync::mpsc::{Receiver, Sender, channel};

    use ironrdp_core::{Encode as _, WriteCursor};

    use crate::decode::{DecodedFrame, DecoderError, DecoderResult};
    use crate::pdu::QuantQuality;

    /// A decoder whose YUV output is scripted: every plane byte is derived from the
    /// first byte of the "H.264" payload, so tests can tell which sub-stream
    /// produced which pixels (Y = tag, U = tag+1, V = tag+2).
    struct StubYuvDecoder {
        width: usize,
        height: usize,
    }

    impl H264Decoder for StubYuvDecoder {
        fn decode(&mut self, _data: &[u8]) -> DecoderResult<DecodedFrame> {
            Err(DecoderError::msg("stub is YUV-only"))
        }

        fn decode_yuv420(&mut self, data: &[u8], out: &mut Yuv420Frame) -> DecoderResult<()> {
            let tag = *data.first().ok_or_else(|| DecoderError::msg("empty payload"))?;
            out.width = self.width;
            out.height = self.height;
            let uv = self.width.div_ceil(2) * self.height.div_ceil(2);
            out.y = vec![tag; self.width * self.height];
            out.u = vec![tag.wrapping_add(1); uv];
            out.v = vec![tag.wrapping_add(2); uv];
            Ok(())
        }

        fn supports_yuv420(&self) -> bool {
            true
        }
    }

    #[derive(Debug)]
    enum Event {
        Update { rect: (u16, u16, u16, u16), data_len: usize },
        Failure(&'static str),
    }

    struct Recorder(Sender<Event>);

    impl GraphicsPipelineHandler for Recorder {
        fn on_bitmap_updated(&mut self, update: &BitmapUpdate) {
            let r = &update.destination_rectangle;
            let _ = self.0.send(Event::Update {
                rect: (r.left, r.top, r.right, r.bottom),
                data_len: update.data.len(),
            });
        }

        fn on_decode_failure(&mut self, _codec_id: Codec1Type, reason: &'static str) {
            let _ = self.0.send(Event::Failure(reason));
        }
    }

    /// A client with a 64x48 surface and a scripted YUV decoder of the given frame size.
    fn avc444_client(frame_w: usize, frame_h: usize) -> (GraphicsPipelineClient, Receiver<Event>) {
        let (tx, rx) = channel();
        let mut client = GraphicsPipelineClient::new(
            Box::new(Recorder(tx)),
            Some(Box::new(StubYuvDecoder {
                width: frame_w,
                height: frame_h,
            })),
        );
        let _ = client.handle_pdu(GfxPdu::CreateSurface(crate::pdu::CreateSurfacePdu {
            surface_id: 1,
            width: 64,
            height: 48,
            pixel_format: PixelFormat::XRgb,
        }));
        (client, rx)
    }

    /// Wire rects are exclusive despite the inclusive typing; build them raw.
    fn wire_rect(left: u16, top: u16, right: u16, bottom: u16) -> InclusiveRectangle {
        InclusiveRectangle {
            left,
            top,
            right,
            bottom,
        }
    }

    fn avc420_sub_stream<'a>(
        rects: Vec<InclusiveRectangle>,
        data: &'a [u8],
    ) -> Avc420BitmapStream<'a> {
        let quants = rects
            .iter()
            .map(|_| QuantQuality {
                quantization_parameter: 22,
                progressive: false,
                quality: 100,
            })
            .collect();
        Avc420BitmapStream {
            rectangles: rects,
            quant_qual_vals: quants,
            data,
        }
    }

    fn deliver_avc444(
        client: &mut GraphicsPipelineClient,
        codec_id: Codec1Type,
        stream: &Avc444BitmapStream<'_>,
    ) {
        let mut bitmap_data = vec![0u8; stream.size()];
        stream
            .encode(&mut WriteCursor::new(&mut bitmap_data))
            .expect("encode avc444 stream");
        client
            .handle_wire_to_surface1(crate::pdu::WireToSurface1Pdu {
                surface_id: 1,
                codec_id,
                pixel_format: PixelFormat::XRgb,
                destination_rectangle: ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: 64,
                    bottom: 48,
                },
                bitmap_data,
            })
            .expect("AVC444 must never error the channel");
    }

    #[test]
    fn avc444_lc0_paints_each_streams_own_rects() {
        let (mut client, rx) = avc444_client(64, 48);

        // Distinct rect lists: luma updates the top-left, chroma the bottom-right.
        let stream = Avc444BitmapStream {
            encoding: Encoding::LUMA_AND_CHROMA,
            stream1: avc420_sub_stream(vec![wire_rect(0, 0, 32, 16)], &[10, 0, 0]),
            stream2: Some(avc420_sub_stream(vec![wire_rect(32, 16, 64, 48)], &[20, 0, 0])),
        };
        deliver_avc444(&mut client, Codec1Type::Avc444v2, &stream);

        let events: Vec<Event> = rx.try_iter().collect();
        let updates: Vec<&Event> = events
            .iter()
            .filter(|e| matches!(e, Event::Update { .. }))
            .collect();
        assert_eq!(updates.len(), 2, "one update per distinct rect: {events:?}");
        assert!(
            matches!(updates[0], Event::Update { rect: (0, 0, 32, 16), data_len } if *data_len == 32 * 16 * 4)
        );
        assert!(
            matches!(updates[1], Event::Update { rect: (32, 16, 64, 48), data_len } if *data_len == 32 * 32 * 4)
        );
        assert!(
            !events.iter().any(|e| matches!(e, Event::Failure(_))),
            "no failures expected: {events:?}"
        );

        // The combined state proves rect ownership: the luma pass wrote only its own
        // rect (Y = tag 10 inside, 0 outside), and the v2 chroma pass wrote odd
        // columns only inside ITS rect (U = aux Y tag 20 there; the luma-replicated
        // U = 11 inside the luma rect).
        let buffer = client.avc444_buffers.get(&1).expect("buffer created");
        let (y, u, _) = buffer.planes();
        assert_eq!(y[0], 10, "luma rect got Y");
        assert_eq!(y[17 * 64 + 33], 0, "chroma-only rect gets no Y");
        assert_eq!(u[1 * 64 + 1], 11, "luma rect: replicated main chroma");
        assert_eq!(u[17 * 64 + 33], 20, "chroma rect: odd column from aux Y plane");
    }

    #[test]
    fn avc444_lc1_updates_luma_only_inside_its_rects() {
        let (mut client, rx) = avc444_client(64, 48);

        // Full-frame LC=0 first: odd-column chroma everywhere comes from aux (tag 20).
        let full = Avc444BitmapStream {
            encoding: Encoding::LUMA_AND_CHROMA,
            stream1: avc420_sub_stream(vec![wire_rect(0, 0, 64, 48)], &[10, 0, 0]),
            stream2: Some(avc420_sub_stream(vec![wire_rect(0, 0, 64, 48)], &[20, 0, 0])),
        };
        deliver_avc444(&mut client, Codec1Type::Avc444v2, &full);

        // LC=1 over the left half only.
        let luma_only = Avc444BitmapStream {
            encoding: Encoding::LUMA,
            stream1: avc420_sub_stream(vec![wire_rect(0, 0, 32, 48)], &[30, 0, 0]),
            stream2: None,
        };
        deliver_avc444(&mut client, Codec1Type::Avc444v2, &luma_only);

        let buffer = client.avc444_buffers.get(&1).expect("buffer");
        let (y, u, _) = buffer.planes();
        assert_eq!(y[5 * 64 + 5], 30, "luma updated inside the rect");
        assert_eq!(y[5 * 64 + 40], 10, "luma preserved outside the rect");
        assert_eq!(
            u[5 * 64 + 5],
            31,
            "inside the rect chroma degrades to the replicated average"
        );
        assert_eq!(
            u[5 * 64 + 41],
            20,
            "outside the rect the full-resolution chroma persists"
        );
        assert!(!rx.try_iter().any(|e| matches!(e, Event::Failure(_))));
    }

    #[test]
    fn avc444_lc2_takes_chroma_from_stream1_and_creates_the_buffer() {
        let (mut client, rx) = avc444_client(64, 48);

        // Chroma-only as the FIRST frame: the buffer must come into being sized from
        // the surface, with luma untouched (black).
        let chroma_only = Avc444BitmapStream {
            encoding: Encoding::CHROMA,
            stream1: avc420_sub_stream(vec![wire_rect(0, 0, 64, 48)], &[40, 0, 0]),
            stream2: None,
        };
        deliver_avc444(&mut client, Codec1Type::Avc444v2, &chroma_only);

        let buffer = client.avc444_buffers.get(&1).expect("buffer created by LC=2");
        assert_eq!((buffer.width(), buffer.height()), (64, 48));
        let (y, u, _) = buffer.planes();
        assert_eq!(y[0], 0, "no luma was delivered");
        assert_eq!(u[3 * 64 + 3], 40, "odd column chroma from the stream1 aux frame");

        let events: Vec<Event> = rx.try_iter().collect();
        assert!(
            matches!(events.as_slice(), [Event::Update { rect: (0, 0, 64, 48), .. }]),
            "one update over the stream1 rects: {events:?}"
        );
    }

    #[test]
    fn avc444_frame_geometry_mismatch_degrades_to_luma_and_is_counted() {
        // Decoded frames at 100x100 do not match the 64x48 surface's packing
        // geometry: the luma pass still applies (bounds-checked), the chroma pass
        // must not run (it would shear), and the skip must be visible.
        let (mut client, rx) = avc444_client(100, 100);

        let stream = Avc444BitmapStream {
            encoding: Encoding::LUMA_AND_CHROMA,
            stream1: avc420_sub_stream(vec![wire_rect(0, 0, 64, 48)], &[10, 0, 0]),
            stream2: Some(avc420_sub_stream(vec![wire_rect(0, 0, 64, 48)], &[20, 0, 0])),
        };
        deliver_avc444(&mut client, Codec1Type::Avc444v2, &stream);

        let buffer = client.avc444_buffers.get(&1).expect("buffer");
        let (y, u, _) = buffer.planes();
        assert_eq!(y[0], 10, "luma still applied");
        assert_eq!(u[1 * 64 + 1], 11, "chroma stayed at the luma-replicated value");
        assert!(
            rx.try_iter()
                .any(|e| matches!(e, Event::Failure("avc444 frame geometry mismatch"))),
            "the degradation must be counted"
        );
    }

    #[test]
    fn avc444_malformed_rects_are_dropped_and_counted() {
        let (mut client, rx) = avc444_client(64, 48);

        // A reversed rect and an out-of-surface rect: both dropped, nothing painted,
        // nothing panics, and the drop is counted.
        let stream = Avc444BitmapStream {
            encoding: Encoding::LUMA,
            stream1: avc420_sub_stream(
                vec![wire_rect(30, 10, 10, 30), wire_rect(0, 0, 65, 48)],
                &[10, 0, 0],
            ),
            stream2: None,
        };
        deliver_avc444(&mut client, Codec1Type::Avc444v2, &stream);

        let events: Vec<Event> = rx.try_iter().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Failure("avc444 malformed region rects"))),
            "{events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(e, Event::Update { .. })),
            "no valid rect, so nothing painted: {events:?}"
        );
    }

    #[test]
    fn avc444_state_dies_with_its_surface_and_on_id_reuse() {
        let (mut client, _rx) = avc444_client(64, 48);
        let stream = Avc444BitmapStream {
            encoding: Encoding::LUMA,
            stream1: avc420_sub_stream(vec![wire_rect(0, 0, 64, 48)], &[10, 0, 0]),
            stream2: None,
        };
        deliver_avc444(&mut client, Codec1Type::Avc444v2, &stream);
        assert!(client.avc444_buffers.contains_key(&1));

        // CreateSurface may reuse the id without a DeleteSurface (resolution
        // change): the old combined state belongs to the old surface.
        let _ = client.handle_pdu(GfxPdu::CreateSurface(crate::pdu::CreateSurfacePdu {
            surface_id: 1,
            width: 32,
            height: 32,
            pixel_format: PixelFormat::XRgb,
        }));
        assert!(
            !client.avc444_buffers.contains_key(&1),
            "id reuse must not inherit the old surface's YUV state"
        );

        deliver_avc444(
            &mut client,
            Codec1Type::Avc444v2,
            &Avc444BitmapStream {
                encoding: Encoding::LUMA,
                stream1: avc420_sub_stream(vec![wire_rect(0, 0, 32, 32)], &[10, 0, 0]),
                stream2: None,
            },
        );
        assert!(client.avc444_buffers.contains_key(&1));

        let _ = client.handle_pdu(GfxPdu::DeleteSurface(crate::pdu::DeleteSurfacePdu { surface_id: 1 }));
        assert!(
            !client.avc444_buffers.contains_key(&1),
            "the buffer dies with the surface"
        );
    }

    #[test]
    fn advertised_capabilities_match_the_decoder() {
        struct RgbaOnly;
        impl H264Decoder for RgbaOnly {
            fn decode(&mut self, _data: &[u8]) -> DecoderResult<DecodedFrame> {
                Err(DecoderError::msg("unused"))
            }
        }

        let handler_caps = || {
            vec![
                CapabilitySet::V10_7 {
                    flags: CapabilitiesV107Flags::SMALL_CACHE,
                },
                CapabilitySet::V8_1 {
                    flags: CapabilitiesV81Flags::AVC420_ENABLED | CapabilitiesV81Flags::SMALL_CACHE,
                },
                CapabilitySet::V8 {
                    flags: CapabilitiesV8Flags::SMALL_CACHE,
                },
            ]
        };

        // No decoder: only the AVC-free V8 set survives.
        let none = advertised_capabilities(handler_caps(), None);
        assert_eq!(none.len(), 1);
        assert!(matches!(none[0], CapabilitySet::V8 { .. }));

        // RGBA-only decoder: the V10.7 set implies AVC444 and is dropped; V8.1
        // (AVC420) survives.
        let rgba: Box<dyn H264Decoder> = Box::new(RgbaOnly);
        let rgba_caps = advertised_capabilities(handler_caps(), Some(rgba.as_ref()));
        assert_eq!(rgba_caps.len(), 2);
        assert!(matches!(rgba_caps[0], CapabilitySet::V8_1 { .. }));

        // YUV-capable decoder: everything the handler asked for.
        let yuv: Box<dyn H264Decoder> = Box::new(StubYuvDecoder { width: 4, height: 4 });
        assert_eq!(advertised_capabilities(handler_caps(), Some(yuv.as_ref())).len(), 3);
    }

    #[test]
    fn crop_decoded_frame_identity() {
        let data = vec![0xFFu8; 4 * 4 * 4];
        let cropped = crop_decoded_frame(&data, 4, 4, 4, 4);
        assert_eq!(cropped.len(), data.len());
    }

    #[test]
    fn crop_decoded_frame_macroblock_alignment() {
        // H.264 encodes 1920x1080 as 1920x1088 (rounded to 16-pixel macroblock boundary)
        let data = vec![0xAAu8; 1920 * 1088 * 4];
        let cropped = crop_decoded_frame(&data, 1920, 1088, 1920, 1080);
        assert_eq!(cropped.len(), 1920 * 1080 * 4);
    }

    #[test]
    fn convert_uncompressed_bgrx_to_rgba() {
        // Wire format: [B, G, R, A] per pixel (0xAARRGGBB little-endian)
        let wire_pixels = vec![
            0x00, 0x80, 0xFF, 0xCC, // B=0, G=128, R=255, A=204
            0x10, 0x20, 0x30, 0x40, // B=16, G=32, R=48, A=64
        ];
        let rgba = convert_uncompressed_to_rgba(&wire_pixels);
        // Expected: [R, G, B, 0xFF] per pixel (alpha forced to opaque)
        assert_eq!(rgba, vec![0xFF, 0x80, 0x00, 0xFF, 0x30, 0x20, 0x10, 0xFF]);
    }
}
