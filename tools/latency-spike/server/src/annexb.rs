//! Annex B access-unit utilities.
//!
//! The reason this module exists is a hard requirement of the receiver: the mdrdp
//! H.264 decoder (`src/h264.rs`, `decode_yuv420`) errors with "no SPS/PPS seen yet"
//! for any access unit that arrives before a parameter-set pair. Media Foundation's
//! H.264 encoders normally emit SPS/PPS in-band ahead of each IDR, but that is not
//! contractual — the sequence header is *also* published out of band on the output
//! media type as `MF_MT_MPEG_SEQUENCE_HEADER`. So the server keeps the out-of-band
//! copy and prepends it only when a keyframe turns up without one. Which route
//! actually fired is recorded per frame as `param_sets_prepended`.
//!
//! Start-code scanning is safe without any RBSP unescaping: emulation prevention
//! guarantees the three-byte sequence `00 00 01` cannot occur inside a NAL payload.

use std::borrow::Cow;

pub const NAL_NON_IDR: u8 = 1;
pub const NAL_IDR: u8 = 5;
pub const NAL_SEI: u8 = 6;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;
pub const NAL_AUD: u8 = 9;

/// Length of the start code at `buf[i]`, if there is one.
fn start_code_len(buf: &[u8], i: usize) -> Option<usize> {
    let rest = &buf[i..];
    if rest.starts_with(&[0, 0, 0, 1]) {
        Some(4)
    } else if rest.starts_with(&[0, 0, 1]) {
        Some(3)
    } else {
        None
    }
}

/// Split an Annex B access unit into its NAL units, start codes removed.
///
/// Bytes before the first start code are discarded — a well-formed stream has none.
pub fn nal_units(au: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    let mut i = 0usize;
    // Find the first start code.
    while i < au.len() && start_code_len(au, i).is_none() {
        i += 1;
    }
    while i < au.len() {
        let sc = match start_code_len(au, i) {
            Some(n) => n,
            None => break,
        };
        let body_start = i + sc;
        let mut j = body_start;
        while j < au.len() && start_code_len(au, j).is_none() {
            j += 1;
        }
        if body_start < j {
            units.push(&au[body_start..j]);
        }
        i = j;
    }
    units
}

/// `nal_unit_type` from the NAL header byte.
pub fn nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|b| b & 0x1F)
}

/// Does this access unit carry both an SPS and a PPS?
pub fn has_parameter_sets(au: &[u8]) -> bool {
    let (mut sps, mut pps) = (false, false);
    for nal in nal_units(au) {
        match nal_type(nal) {
            Some(NAL_SPS) => sps = true,
            Some(NAL_PPS) => pps = true,
            _ => {}
        }
    }
    sps && pps
}

/// Does this access unit carry an IDR slice?
pub fn contains_idr(au: &[u8]) -> bool {
    nal_units(au).iter().any(|n| nal_type(n) == Some(NAL_IDR))
}

/// The SPS/PPS pair held out of band, ready to be prepended as Annex B.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterSets {
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
}

impl ParameterSets {
    /// Parse an `MF_MT_MPEG_SEQUENCE_HEADER` blob.
    ///
    /// For H.264 that blob is itself an Annex B byte stream holding the SPS and PPS
    /// (and occasionally an SEI, which is dropped). Returns `None` unless both are
    /// present — half a pair is no use to the decoder.
    pub fn from_sequence_header(blob: &[u8]) -> Option<Self> {
        let (mut sps, mut pps) = (None, None);
        for nal in nal_units(blob) {
            match nal_type(nal) {
                Some(NAL_SPS) if sps.is_none() => sps = Some(nal.to_vec()),
                Some(NAL_PPS) if pps.is_none() => pps = Some(nal.to_vec()),
                _ => {}
            }
        }
        Some(Self {
            sps: sps?,
            pps: pps?,
        })
    }

    /// Serialise as `00 00 00 01 <SPS> 00 00 00 01 <PPS>`.
    pub fn to_annex_b(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.sps.len() + self.pps.len() + 8);
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&self.sps);
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&self.pps);
        out
    }
}

/// Guarantee the decoder sees parameter sets at every keyframe.
///
/// Borrows unchanged when the encoder already emitted them in band (the common case
/// on every MF encoder measured so far), which keeps the steady-state path free of a
/// copy. Non-keyframes are never touched: repeating SPS/PPS on a P-frame costs bytes
/// and buys nothing.
pub fn ensure_parameter_sets<'a>(
    au: &'a [u8],
    sets: Option<&ParameterSets>,
    keyframe: bool,
) -> (Cow<'a, [u8]>, bool) {
    if !keyframe || has_parameter_sets(au) {
        return (Cow::Borrowed(au), false);
    }
    let Some(sets) = sets else {
        // Nothing to prepend. The frame still goes out; the receiver will refuse it
        // and the stats line records that we could not help.
        return (Cow::Borrowed(au), false);
    };
    let mut out = sets.to_annex_b();
    out.extend_from_slice(au);
    (Cow::Owned(out), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one Annex B NAL with a distinguishable body, 4-byte start code.
    fn nal4(nal_type: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 1, nal_type & 0x1F];
        v.extend_from_slice(body);
        v
    }

    /// The same, with a 3-byte start code.
    fn nal3(nal_type: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![0, 0, 1, nal_type & 0x1F];
        v.extend_from_slice(body);
        v
    }

    fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.iter().flatten().copied().collect()
    }

    #[test]
    fn nal_units_splits_on_both_start_code_lengths() {
        // Distinct bodies per NAL: a uniform fixture could not detect a mis-sliced
        // boundary.
        let au = cat(&[
            nal4(NAL_SPS, &[0x11, 0x12]),
            nal3(NAL_PPS, &[0x21]),
            nal4(NAL_IDR, &[0x31, 0x32, 0x33]),
        ]);
        let units = nal_units(&au);
        assert_eq!(units.len(), 3);
        assert_eq!(units[0], &[NAL_SPS, 0x11, 0x12]);
        assert_eq!(units[1], &[NAL_PPS, 0x21]);
        assert_eq!(units[2], &[NAL_IDR, 0x31, 0x32, 0x33]);
    }

    #[test]
    fn nal_type_reads_the_low_five_bits() {
        // 0x65 is the real-world IDR header: nal_ref_idc=3, type=5.
        assert_eq!(nal_type(&[0x65, 0x88]), Some(NAL_IDR));
        assert_eq!(nal_type(&[0x67]), Some(NAL_SPS));
        assert_eq!(nal_type(&[0x68]), Some(NAL_PPS));
        assert_eq!(nal_type(&[]), None);
    }

    #[test]
    fn parameter_sets_are_detected_only_when_both_are_present() {
        let both = cat(&[
            nal4(NAL_SPS, &[1]),
            nal4(NAL_PPS, &[2]),
            nal4(NAL_IDR, &[3]),
        ]);
        let sps_only = cat(&[nal4(NAL_SPS, &[1]), nal4(NAL_IDR, &[3])]);
        let pps_only = cat(&[nal4(NAL_PPS, &[2]), nal4(NAL_IDR, &[3])]);
        let neither = cat(&[nal4(NAL_NON_IDR, &[3])]);
        assert!(has_parameter_sets(&both));
        assert!(!has_parameter_sets(&sps_only));
        assert!(!has_parameter_sets(&pps_only));
        assert!(!has_parameter_sets(&neither));
    }

    #[test]
    fn contains_idr_ignores_an_sei_or_aud_wrapper() {
        let with = cat(&[
            nal4(NAL_AUD, &[0xF0]),
            nal4(NAL_SEI, &[0x01]),
            nal4(NAL_IDR, &[0x02]),
        ]);
        let without = cat(&[nal4(NAL_AUD, &[0xF0]), nal4(NAL_NON_IDR, &[0x02])]);
        assert!(contains_idr(&with));
        assert!(!contains_idr(&without));
    }

    #[test]
    fn a_sequence_header_blob_yields_the_pair() {
        let blob = cat(&[
            nal4(NAL_SPS, &[0x64, 0x00, 0x1F]),
            nal4(NAL_PPS, &[0xEE, 0x3C]),
        ]);
        let sets = ParameterSets::from_sequence_header(&blob).expect("both present");
        assert_eq!(sets.sps, vec![NAL_SPS, 0x64, 0x00, 0x1F]);
        assert_eq!(sets.pps, vec![NAL_PPS, 0xEE, 0x3C]);
    }

    #[test]
    fn a_sequence_header_missing_the_pps_is_refused() {
        let blob = nal4(NAL_SPS, &[0x64]);
        assert_eq!(ParameterSets::from_sequence_header(&blob), None);
    }

    #[test]
    fn to_annex_b_round_trips_through_the_parser() {
        let sets = ParameterSets {
            sps: vec![NAL_SPS, 0xAA, 0xBB],
            pps: vec![NAL_PPS, 0xCC],
        };
        let parsed = ParameterSets::from_sequence_header(&sets.to_annex_b()).unwrap();
        assert_eq!(parsed, sets);
    }

    #[test]
    fn an_inband_keyframe_is_left_alone() {
        let sets = ParameterSets {
            sps: vec![NAL_SPS, 0xAA],
            pps: vec![NAL_PPS, 0xBB],
        };
        let au = cat(&[
            nal4(NAL_SPS, &[0xAA]),
            nal4(NAL_PPS, &[0xBB]),
            nal4(NAL_IDR, &[0x01]),
        ]);
        let (out, prepended) = ensure_parameter_sets(&au, Some(&sets), true);
        assert!(!prepended);
        assert!(matches!(out, Cow::Borrowed(_)), "no copy taken");
        assert_eq!(out.as_ref(), au.as_slice());
    }

    #[test]
    fn a_bare_keyframe_gets_the_stored_sets_in_front() {
        let sets = ParameterSets {
            sps: vec![NAL_SPS, 0xAA],
            pps: vec![NAL_PPS, 0xBB],
        };
        let au = nal4(NAL_IDR, &[0x01, 0x02]);
        let (out, prepended) = ensure_parameter_sets(&au, Some(&sets), true);
        assert!(prepended);
        assert!(has_parameter_sets(&out));
        let units = nal_units(&out);
        // Order matters: SPS, then PPS, then the slice — the decoder builds its
        // session from the pair it has seen *before* the slice.
        assert_eq!(
            units
                .iter()
                .map(|n| nal_type(n).unwrap())
                .collect::<Vec<_>>(),
            vec![NAL_SPS, NAL_PPS, NAL_IDR]
        );
        assert_eq!(units[2], &[NAL_IDR, 0x01, 0x02]);
    }

    #[test]
    fn a_non_keyframe_is_never_padded() {
        let sets = ParameterSets {
            sps: vec![NAL_SPS, 0xAA],
            pps: vec![NAL_PPS, 0xBB],
        };
        let au = nal4(NAL_NON_IDR, &[0x01]);
        let (out, prepended) = ensure_parameter_sets(&au, Some(&sets), false);
        assert!(!prepended);
        assert_eq!(out.as_ref(), au.as_slice());
    }

    #[test]
    fn a_bare_keyframe_with_no_stored_sets_passes_through_unflagged() {
        let au = nal4(NAL_IDR, &[0x01]);
        let (out, prepended) = ensure_parameter_sets(&au, None, true);
        assert!(!prepended);
        assert_eq!(out.as_ref(), au.as_slice());
    }
}
