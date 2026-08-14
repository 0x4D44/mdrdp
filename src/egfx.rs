//! EGFX graphics-pipeline observation.
//!
//! P2c answers one question the earlier spikes could not: what does `temper` negotiate
//! with **our** offer? P1b measured what it sends FreeRDP, and codec selection is an
//! intersection of client and server capability sets, so that told us the server's
//! behaviour against a different client — not ours.
//!
//! This observes without decoding. It records the capability set the server confirms and
//! the codec ids it then uses, which is what sizes the P3a decode work.
//!
//! Two known risks, both discovered by reading source before writing any of this:
//!
//! 1. `ironrdp-connector` 0.10.0 never sets `RNS_UD_CS_SUPPORT_DYNVC_GFX_PROTOCOL` in the
//!    client core early-capability flags, and exposes no way to set it. FreeRDP does
//!    advertise it. If the server gates the graphics channel on that flag, the channel
//!    never opens and this reports exactly that rather than hanging.
//! 2. `on_wire_to_surface2` — where RFX Progressive arrives — is an empty default
//!    upstream. We can observe those PDUs; decoding them is P3a.

use ironrdp_egfx::client::{BitmapUpdate, GraphicsPipelineHandler, Surface};
use ironrdp_egfx::pdu::{CapabilitiesV107Flags, CapabilitySet, GfxPdu, WireToSurface2Pdu};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// What the graphics pipeline actually did, as opposed to what we hoped it would.
#[derive(Debug, Default, Clone, Serialize)]
pub struct EgfxObservations {
    /// The capability version the server confirmed, if the channel opened at all.
    pub confirmed_capability: Option<String>,
    /// `ResetGraphics` dimensions, if seen.
    pub reset_graphics: Option<(u32, u32)>,
    pub surfaces_created: u32,
    pub frames_completed: u32,
    /// Codec ids seen on surface commands, counted. **This is the P3a answer.**
    pub codec_ids_seen: BTreeMap<String, u32>,
    /// PDUs the upstream client did not handle. ClearCodec lands here — it is the seam
    /// where a decoder would be plugged in.
    pub unhandled_pdus: u32,
}

impl EgfxObservations {
    /// True when the channel opened and the server confirmed capabilities.
    pub fn channel_opened(&self) -> bool {
        self.confirmed_capability.is_some()
    }
}

/// Shared handle so the caller can read observations while the session pumps.
#[derive(Debug, Clone, Default)]
pub struct EgfxProbe(Arc<Mutex<EgfxObservations>>);

impl EgfxProbe {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> EgfxObservations {
        self.0.lock().expect("egfx observations mutex").clone()
    }

    fn with<F: FnOnce(&mut EgfxObservations)>(&self, f: F) {
        f(&mut self.0.lock().expect("egfx observations mutex"));
    }
}

impl GraphicsPipelineHandler for EgfxProbe {
    /// Advertise **V10.7 with AVC explicitly disabled**.
    ///
    /// The upstream default advertises V10.7 with AVC implied, and
    /// `GraphicsPipelineClient` filters every AVC-bearing set when no H.264 decoder is
    /// configured — so a client without one silently falls back to V8, an older pipeline
    /// than the server would otherwise use. Saying "V10.7, and no AVC please" gets the
    /// modern pipeline without needing a decoder we do not have yet.
    fn capabilities(&self) -> Vec<CapabilitySet> {
        vec![CapabilitySet::V10_7 {
            flags: CapabilitiesV107Flags::AVC_DISABLED | CapabilitiesV107Flags::SMALL_CACHE,
        }]
    }

    /// Record which codec each surface command used — the P3a question.
    ///
    /// Only the codec id is read. It is a small protocol enum; the PDU itself carries
    /// bitmap data, so the whole PDU is never formatted.
    /// RFX Progressive arrives here, NOT via `on_unhandled_pdu`.
    ///
    /// `ironrdp-egfx` dispatches `WireToSurface2` to this dedicated callback and returns,
    /// so a `WireToSurface2` arm inside `on_unhandled_pdu` is dead code. An earlier
    /// version of this file had exactly that, which is why a previous measurement
    /// reported "ClearCodec only" — the progressive PDUs were arriving and being silently
    /// dropped by the empty upstream default.
    fn on_wire_to_surface2(&mut self, pdu: &WireToSurface2Pdu) {
        let codec = format!("WireToSurface2/{:?}", pdu.codec_id);
        self.with(|o| *o.codec_ids_seen.entry(codec).or_insert(0) += 1);
    }

    fn on_unhandled_pdu(&mut self, pdu: &GfxPdu) {
        match pdu {
            GfxPdu::WireToSurface1(p) => {
                let codec = format!("{:?}", p.codec_id);
                self.with(|o| *o.codec_ids_seen.entry(codec).or_insert(0) += 1);
            }
            GfxPdu::WireToSurface2(p) => {
                let codec = format!("WireToSurface2/{:?}", p.codec_id);
                self.with(|o| *o.codec_ids_seen.entry(codec).or_insert(0) += 1);
            }
            _ => self.with(|o| o.unhandled_pdus += 1),
        }
    }

    fn on_capabilities_confirmed(&mut self, caps: &CapabilitySet) {
        // The capability set's Debug is a fixed enum rendering, not payload.
        let rendered = format!("{caps:?}");
        self.with(|o| o.confirmed_capability = Some(rendered));
    }

    fn on_reset_graphics(&mut self, width: u32, height: u32) {
        self.with(|o| o.reset_graphics = Some((width, height)));
    }

    fn on_surface_created(&mut self, _surface: &Surface) {
        self.with(|o| o.surfaces_created += 1);
    }

    fn on_bitmap_updated(&mut self, _update: &BitmapUpdate) {
        // Deliberately does not read the bitmap. This is session content.
    }

    fn on_frame_complete(&mut self, _frame_id: u32) {
        self.with(|o| o.frames_completed += 1);
    }
}

impl EgfxProbe {
    /// Record a codec id observed on a surface command.
    ///
    /// Codec ids are protocol constants, never payload.
    pub fn note_codec(&self, codec: &str) {
        self.with(|o| *o.codec_ids_seen.entry(codec.to_owned()).or_insert(0) += 1);
    }

    pub fn note_unhandled(&self) {
        self.with(|o| o.unhandled_pdus += 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_with_no_confirmation_reports_the_channel_never_opened() {
        // The failure this must report clearly rather than hang on: the server declining
        // to open the graphics channel because we never advertised the DYNVC_GFX flag.
        let probe = EgfxProbe::new();
        let obs = probe.snapshot();
        assert!(!obs.channel_opened());
        assert!(obs.confirmed_capability.is_none());
        assert_eq!(obs.surfaces_created, 0);
    }

    #[test]
    fn codec_ids_are_counted_per_kind() {
        let probe = EgfxProbe::new();
        probe.note_codec("CLEARCODEC");
        probe.note_codec("CLEARCODEC");
        probe.note_codec("CAPROGRESSIVE");

        let obs = probe.snapshot();
        assert_eq!(obs.codec_ids_seen.get("CLEARCODEC"), Some(&2));
        assert_eq!(obs.codec_ids_seen.get("CAPROGRESSIVE"), Some(&1));
    }

    #[test]
    fn observations_are_shared_across_clones() {
        // The handler is moved into the graphics client; the caller keeps a clone and
        // must still see what the handler recorded.
        let probe = EgfxProbe::new();
        let handler_side = probe.clone();
        handler_side.note_unhandled();
        handler_side.note_unhandled();
        assert_eq!(probe.snapshot().unhandled_pdus, 2);
    }

    #[test]
    fn snapshot_does_not_alias_later_mutation() {
        let probe = EgfxProbe::new();
        let before = probe.snapshot();
        probe.note_codec("CLEARCODEC");
        assert!(before.codec_ids_seen.is_empty(), "snapshot must be a copy");
        assert_eq!(probe.snapshot().codec_ids_seen.len(), 1);
    }
}
