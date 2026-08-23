//! Annex B access-unit utilities.
//!
//! The live H.264 receiver needs SPS/PPS before it can decode an IDR. Media
//! Foundation may publish those sets out of band in `MF_MT_MPEG_SEQUENCE_HEADER`,
//! but some hardware encoders publish none and send them in-band instead. The server
//! caches either route and prepends the sets to a later bare IDR. The retired HEVC
//! helpers remain below so archived experiment fixtures stay readable.
//!
//! Start-code scanning is safe without any RBSP unescaping: emulation prevention
//! guarantees the three-byte sequence `00 00 01` cannot occur inside a NAL payload.

use std::borrow::Cow;

pub const NAL_TRAIL_R: u8 = 1;
pub const NAL_IDR_W_RADL: u8 = 19;
pub const NAL_IDR_N_LP: u8 = 20;
pub const NAL_CRA: u8 = 21;
pub const NAL_VPS: u8 = 32;
pub const NAL_SPS: u8 = 33;
pub const NAL_PPS: u8 = 34;
pub const NAL_AUD: u8 = 35;
pub const NAL_PREFIX_SEI: u8 = 39;

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

/// H.264's five-bit NAL type. Kept separate from [`nal_type`], which parses the
/// retired HEVC experiment and remains available for archived telemetry tests.
pub fn avc_nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|byte| byte & 0x1f)
}

pub fn avc_has_parameter_sets(au: &[u8]) -> bool {
    let (mut sps, mut pps) = (false, false);
    for nal in nal_units(au) {
        match avc_nal_type(nal) {
            Some(7) => sps = true,
            Some(8) => pps = true,
            _ => {}
        }
    }
    sps && pps
}

pub fn avc_contains_idr(au: &[u8]) -> bool {
    nal_units(au).iter().any(|nal| avc_nal_type(nal) == Some(5))
}

/// H.264 SPS/PPS published by Media Foundation or observed in-band.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvcParameterSets {
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
}

impl AvcParameterSets {
    pub fn from_sequence_header(blob: &[u8]) -> Option<Self> {
        let (mut sps, mut pps) = (None, None);
        for nal in nal_units(blob) {
            match avc_nal_type(nal) {
                Some(7) if sps.is_none() => sps = Some(nal.to_vec()),
                Some(8) if pps.is_none() => pps = Some(nal.to_vec()),
                _ => {}
            }
        }
        Some(Self {
            sps: sps?,
            pps: pps?,
        })
    }

    pub fn to_annex_b(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.sps.len() + self.pps.len() + 8);
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&self.sps);
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&self.pps);
        out
    }
}

pub fn ensure_avc_parameter_sets<'a>(
    au: &'a [u8],
    sets: Option<&AvcParameterSets>,
    keyframe: bool,
) -> (Cow<'a, [u8]>, bool) {
    if !keyframe || avc_has_parameter_sets(au) {
        return (Cow::Borrowed(au), false);
    }
    let Some(sets) = sets else {
        return (Cow::Borrowed(au), false);
    };
    let mut out = sets.to_annex_b();
    out.extend_from_slice(au);
    (Cow::Owned(out), true)
}

/// `nal_unit_type` from HEVC's two-byte NAL header.
///
/// `nuh_temporal_id_plus1 == 0` is forbidden by ISO/IEC 23008-2 and treating such
/// a header as valid would let a corrupt access unit masquerade as an IRAP.
pub fn nal_type(nal: &[u8]) -> Option<u8> {
    let header = nal.get(..2)?;
    (header[1] & 0x07 != 0).then_some((header[0] >> 1) & 0x3F)
}

/// Does this access unit carry a complete VPS/SPS/PPS triplet?
pub fn has_parameter_sets(au: &[u8]) -> bool {
    let (mut vps, mut sps, mut pps) = (false, false, false);
    for nal in nal_units(au) {
        match nal_type(nal) {
            Some(NAL_VPS) => vps = true,
            Some(NAL_SPS) => sps = true,
            Some(NAL_PPS) => pps = true,
            _ => {}
        }
    }
    vps && sps && pps
}

fn has_parameter_sets_before_irap(au: &[u8]) -> bool {
    let (mut vps, mut sps, mut pps) = (false, false, false);
    for nal in nal_units(au) {
        match nal_type(nal) {
            Some(NAL_VPS) => vps = true,
            Some(NAL_SPS) => sps = true,
            Some(NAL_PPS) => pps = true,
            Some(kind) if (16..=23).contains(&kind) => return vps && sps && pps,
            _ => {}
        }
    }
    false
}

/// Does this access unit carry an HEVC intra random-access point?
pub fn contains_irap(au: &[u8]) -> bool {
    nal_units(au)
        .iter()
        .any(|n| nal_type(n).is_some_and(|kind| (16..=23).contains(&kind)))
}

/// Fields that must match the stream contract before an epoch may paint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamConfig {
    pub profile_idc: u8,
    pub high_tier: bool,
    pub level_idc: u8,
    pub chroma_format_idc: u8,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamConfigError(pub &'static str);

impl std::fmt::Display for StreamConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HEVC SPS is missing or invalid at {}", self.0)
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn bits(&mut self, count: usize) -> Option<u64> {
        if count > 64 || self.pos.checked_add(count)? > self.data.len() * 8 {
            return None;
        }
        let mut value = 0u64;
        for _ in 0..count {
            let byte = self.data[self.pos / 8];
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            value = (value << 1) | u64::from(bit);
            self.pos += 1;
        }
        Some(value)
    }

    fn flag(&mut self) -> Option<bool> {
        self.bits(1).map(|value| value != 0)
    }

    fn skip(&mut self, count: usize) -> Option<()> {
        self.bits(count).map(|_| ())
    }

    fn ue(&mut self) -> Option<u64> {
        let mut leading_zeroes = 0usize;
        while !self.flag()? {
            leading_zeroes += 1;
            if leading_zeroes > 32 {
                return None;
            }
        }
        if leading_zeroes == 0 {
            return Some(0);
        }
        Some((1u64 << leading_zeroes) - 1 + self.bits(leading_zeroes)?)
    }
}

fn strip_emulation_prevention(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut zeroes = 0usize;
    for &byte in bytes {
        if zeroes >= 2 && byte == 0x03 {
            zeroes = 0;
            continue;
        }
        out.push(byte);
        if byte == 0 {
            zeroes += 1;
        } else {
            zeroes = 0;
        }
    }
    out
}

/// Parse the profile/tier/level, pixel format, bit depth, and visible dimensions
/// from the latest HEVC SPS in an Annex B access unit.
pub fn stream_config(stream: &[u8]) -> Result<StreamConfig, StreamConfigError> {
    let sps = nal_units(stream)
        .into_iter()
        .rev()
        .find(|nal| nal_type(nal) == Some(NAL_SPS))
        .ok_or(StreamConfigError("SPS"))?;
    let rbsp = strip_emulation_prevention(sps);
    let payload = rbsp.get(2..).ok_or(StreamConfigError("NAL header"))?;
    let mut bits = BitReader::new(payload);
    let bad = |field| StreamConfigError(field);

    bits.skip(4)
        .ok_or_else(|| bad("sps_video_parameter_set_id"))?;
    let max_sub_layers = bits
        .bits(3)
        .ok_or_else(|| bad("sps_max_sub_layers_minus1"))? as usize;
    bits.skip(1)
        .ok_or_else(|| bad("sps_temporal_id_nesting_flag"))?;

    bits.skip(2).ok_or_else(|| bad("general_profile_space"))?;
    let high_tier = bits.flag().ok_or_else(|| bad("general_tier_flag"))?;
    let profile_idc = bits.bits(5).ok_or_else(|| bad("general_profile_idc"))? as u8;
    bits.skip(32)
        .and_then(|()| bits.skip(48))
        .ok_or_else(|| bad("general_profile constraints"))?;
    let level_idc = bits.bits(8).ok_or_else(|| bad("general_level_idc"))? as u8;

    let mut sub_profile = [false; 8];
    let mut sub_level = [false; 8];
    for layer in 0..max_sub_layers {
        sub_profile[layer] = bits
            .flag()
            .ok_or_else(|| bad("sub_layer_profile_present_flag"))?;
        sub_level[layer] = bits
            .flag()
            .ok_or_else(|| bad("sub_layer_level_present_flag"))?;
    }
    if max_sub_layers > 0 {
        bits.skip((8 - max_sub_layers) * 2)
            .ok_or_else(|| bad("reserved_zero_2bits"))?;
    }
    for layer in 0..max_sub_layers {
        if sub_profile[layer] {
            bits.skip(88).ok_or_else(|| bad("sub_layer profile"))?;
        }
        if sub_level[layer] {
            bits.skip(8).ok_or_else(|| bad("sub_layer level"))?;
        }
    }

    bits.ue().ok_or_else(|| bad("sps_seq_parameter_set_id"))?;
    let chroma_format_idc = bits.ue().ok_or_else(|| bad("chroma_format_idc"))? as u8;
    if chroma_format_idc > 3 {
        return Err(bad("chroma_format_idc range"));
    }
    let separate_colour_plane = if chroma_format_idc == 3 {
        bits.flag()
            .ok_or_else(|| bad("separate_colour_plane_flag"))?
    } else {
        false
    };
    let coded_width = u32::try_from(bits.ue().ok_or_else(|| bad("pic_width_in_luma_samples"))?)
        .map_err(|_| bad("pic_width_in_luma_samples range"))?;
    let coded_height = u32::try_from(bits.ue().ok_or_else(|| bad("pic_height_in_luma_samples"))?)
        .map_err(|_| bad("pic_height_in_luma_samples range"))?;
    let (left, right, top, bottom) = if bits.flag().ok_or_else(|| bad("conformance_window_flag"))? {
        (
            bits.ue().ok_or_else(|| bad("conf_win_left_offset"))?,
            bits.ue().ok_or_else(|| bad("conf_win_right_offset"))?,
            bits.ue().ok_or_else(|| bad("conf_win_top_offset"))?,
            bits.ue().ok_or_else(|| bad("conf_win_bottom_offset"))?,
        )
    } else {
        (0, 0, 0, 0)
    };
    let bit_depth_luma = u8::try_from(
        bits.ue()
            .ok_or_else(|| bad("bit_depth_luma_minus8"))?
            .checked_add(8)
            .ok_or_else(|| bad("bit_depth_luma range"))?,
    )
    .map_err(|_| bad("bit_depth_luma range"))?;
    let bit_depth_chroma = u8::try_from(
        bits.ue()
            .ok_or_else(|| bad("bit_depth_chroma_minus8"))?
            .checked_add(8)
            .ok_or_else(|| bad("bit_depth_chroma range"))?,
    )
    .map_err(|_| bad("bit_depth_chroma range"))?;

    let chroma_array_type = if separate_colour_plane {
        0
    } else {
        chroma_format_idc
    };
    let (crop_x, crop_y) = match chroma_array_type {
        0 => (1u64, 1u64),
        1 => (2, 2),
        2 => (2, 1),
        3 => (1, 1),
        _ => unreachable!("range checked above"),
    };
    let crop_width = (left + right)
        .checked_mul(crop_x)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| bad("conformance width"))?;
    let crop_height = (top + bottom)
        .checked_mul(crop_y)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| bad("conformance height"))?;
    let width = coded_width
        .checked_sub(crop_width)
        .ok_or_else(|| bad("visible width"))?;
    let height = coded_height
        .checked_sub(crop_height)
        .ok_or_else(|| bad("visible height"))?;

    Ok(StreamConfig {
        profile_idc,
        high_tier,
        level_idc,
        chroma_format_idc,
        bit_depth_luma,
        bit_depth_chroma,
        width,
        height,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamContractError {
    pub field: &'static str,
    pub expected: u64,
    pub actual: u64,
}

impl std::fmt::Display for StreamContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HEVC {} mismatch: expected {}, got {}",
            self.field, self.expected, self.actual
        )
    }
}

#[cfg(any(all(feature = "host", windows), test))]
struct LevelLimit {
    idc: u8,
    max_luma_picture: u64,
    max_luma_rate: u64,
    max_main_bitrate_kbps: u32,
}

// ITU-T H.265 Table A.1/A.2 limits relevant to Rhydra's Main-tier fallback.
// Lower levels cannot carry either supported desktop at the declared 60 fps.
#[cfg(any(all(feature = "host", windows), test))]
const LEVEL_LIMITS: &[LevelLimit] = &[
    LevelLimit {
        idc: 123,
        max_luma_picture: 2_228_224,
        max_luma_rate: 133_693_440,
        max_main_bitrate_kbps: 20_000,
    },
    LevelLimit {
        idc: 150,
        max_luma_picture: 8_912_896,
        max_luma_rate: 267_386_880,
        max_main_bitrate_kbps: 25_000,
    },
    LevelLimit {
        idc: 153,
        max_luma_picture: 8_912_896,
        max_luma_rate: 534_773_760,
        max_main_bitrate_kbps: 40_000,
    },
    LevelLimit {
        idc: 156,
        max_luma_picture: 8_912_896,
        max_luma_rate: 1_069_547_520,
        max_main_bitrate_kbps: 60_000,
    },
    LevelLimit {
        idc: 180,
        max_luma_picture: 35_651_584,
        max_luma_rate: 1_069_547_520,
        max_main_bitrate_kbps: 60_000,
    },
    LevelLimit {
        idc: 183,
        max_luma_picture: 35_651_584,
        max_luma_rate: 2_139_095_040,
        max_main_bitrate_kbps: 120_000,
    },
    LevelLimit {
        idc: 186,
        max_luma_picture: 35_651_584,
        max_luma_rate: 4_278_190_080,
        max_main_bitrate_kbps: 240_000,
    },
];

/// Smallest relevant HEVC Main-tier level that can describe this encoded contract.
#[cfg(any(all(feature = "host", windows), test))]
pub(crate) fn required_level_idc(
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
) -> Result<u8, String> {
    if width == 0 || height == 0 || fps == 0 || bitrate_kbps == 0 {
        return Err("HEVC dimensions, frame rate, and bitrate must be non-zero".to_owned());
    }
    let picture = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| "HEVC picture size overflow".to_owned())?;
    let rate = picture
        .checked_mul(u64::from(fps))
        .ok_or_else(|| "HEVC luma sample rate overflow".to_owned())?;
    LEVEL_LIMITS
        .iter()
        .find(|limit| {
            picture <= limit.max_luma_picture
                && u64::from(width) * u64::from(width) <= limit.max_luma_picture * 8
                && u64::from(height) * u64::from(height) <= limit.max_luma_picture * 8
                && rate <= limit.max_luma_rate
                && bitrate_kbps <= limit.max_main_bitrate_kbps
        })
        .map(|limit| limit.idc)
        .ok_or_else(|| {
            format!(
                "HEVC Main-tier contract {width}x{height}@{fps} at {bitrate_kbps} kbit/s exceeds Level 6.2"
            )
        })
}

/// Enforce the exact stream contract chosen for rhydra tranche 6b.
pub fn validate_config(
    config: StreamConfig,
    expected_width: u32,
    expected_height: u32,
    expected_level_idc: u8,
) -> Result<(), StreamContractError> {
    let fields = [
        ("profile_idc", 1, u64::from(config.profile_idc)),
        // Media Foundation has no HEVC tier control, so the selected level must
        // fit Rhydra's bitrate inside Main tier as well as its picture and rate.
        ("high_tier", 0, u64::from(config.high_tier)),
        (
            "level_idc",
            u64::from(expected_level_idc),
            u64::from(config.level_idc),
        ),
        ("chroma_format_idc", 1, u64::from(config.chroma_format_idc)),
        ("bit_depth_luma", 8, u64::from(config.bit_depth_luma)),
        ("bit_depth_chroma", 8, u64::from(config.bit_depth_chroma)),
        ("width", u64::from(expected_width), u64::from(config.width)),
        (
            "height",
            u64::from(expected_height),
            u64::from(config.height),
        ),
    ];
    for (field, expected, actual) in fields {
        if actual != expected {
            return Err(StreamContractError {
                field,
                expected,
                actual,
            });
        }
    }
    Ok(())
}

pub fn validate_stream(
    stream: &[u8],
    expected_width: u32,
    expected_height: u32,
    expected_level_idc: u8,
) -> Result<StreamConfig, String> {
    let config = stream_config(stream).map_err(|error| error.to_string())?;
    validate_config(config, expected_width, expected_height, expected_level_idc)
        .map_err(|error| error.to_string())?;
    Ok(config)
}

/// The VPS/SPS/PPS triplet held out of band, ready to be prepended as Annex B.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterSets {
    pub vps: Vec<u8>,
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
}

impl ParameterSets {
    /// Parse an `MF_MT_MPEG_SEQUENCE_HEADER` blob.
    ///
    /// For HEVC that blob is Annex B VPS/SPS/PPS. Returns `None` unless all three
    /// are present — an incomplete configuration is no use to the decoder.
    pub fn from_sequence_header(blob: &[u8]) -> Option<Self> {
        let (mut vps, mut sps, mut pps) = (None, None, None);
        for nal in nal_units(blob) {
            match nal_type(nal) {
                Some(NAL_VPS) if vps.is_none() => vps = Some(nal.to_vec()),
                Some(NAL_SPS) if sps.is_none() => sps = Some(nal.to_vec()),
                Some(NAL_PPS) if pps.is_none() => pps = Some(nal.to_vec()),
                _ => {}
            }
        }
        Some(Self {
            vps: vps?,
            sps: sps?,
            pps: pps?,
        })
    }

    /// Serialise as Annex B VPS, SPS, then PPS.
    pub fn to_annex_b(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.vps.len() + self.sps.len() + self.pps.len() + 12);
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&self.vps);
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&self.sps);
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&self.pps);
        out
    }
}

/// A bare IRAP cannot be decoded safely and must never reach the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingParameterSets;

/// Guarantee the decoder sees parameter sets at every IRAP.
///
/// Borrows unchanged when the encoder already emitted them in band (the common case
/// on every MF encoder measured so far), which keeps the steady-state path free of a
/// copy. Non-IRAPs are never touched: repeating VPS/SPS/PPS on a trailing picture costs bytes
/// and buys nothing.
pub fn ensure_parameter_sets<'a>(
    au: &'a [u8],
    sets: Option<&ParameterSets>,
    irap: bool,
) -> Result<(Cow<'a, [u8]>, bool), MissingParameterSets> {
    if !irap || has_parameter_sets_before_irap(au) {
        return Ok((Cow::Borrowed(au), false));
    }
    let sets = sets.ok_or(MissingParameterSets)?;
    let mut out = sets.to_annex_b();
    out.extend_from_slice(au);
    Ok((Cow::Owned(out), true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avc_parameter_sets_are_recognised_and_prepended_to_a_bare_idr() {
        let sps = [0, 0, 0, 1, 0x67, 0x11];
        let pps = [0, 0, 1, 0x68, 0x22];
        let idr = [0, 0, 0, 1, 0x65, 0x33];
        let sets =
            AvcParameterSets::from_sequence_header(&[sps.as_slice(), pps.as_slice()].concat())
                .expect("complete SPS/PPS");
        assert!(avc_contains_idr(&idr));
        let (ready, prepended) = ensure_avc_parameter_sets(&idr, Some(&sets), true);
        assert!(prepended);
        assert!(avc_has_parameter_sets(&ready));
    }

    fn nal4(nal_type: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 1, nal_type << 1, 0x01];
        v.extend_from_slice(body);
        v
    }

    fn nal3(nal_type: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![0, 0, 1, nal_type << 1, 0x01];
        v.extend_from_slice(body);
        v
    }

    fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.iter().flatten().copied().collect()
    }

    #[test]
    fn nal_units_splits_on_both_start_code_lengths() {
        let au = cat(&[
            nal4(NAL_SPS, &[0x11, 0x12]),
            nal3(NAL_PPS, &[0x21]),
            nal4(NAL_IDR_W_RADL, &[0x31, 0x32, 0x33]),
        ]);
        let units = nal_units(&au);
        assert_eq!(units.len(), 3);
        assert_eq!(units[0], &[NAL_SPS << 1, 0x01, 0x11, 0x12]);
        assert_eq!(units[1], &[NAL_PPS << 1, 0x01, 0x21]);
        assert_eq!(units[2], &[NAL_IDR_W_RADL << 1, 0x01, 0x31, 0x32, 0x33]);
    }

    #[test]
    fn hevc_nal_type_uses_the_two_byte_header_and_rejects_temporal_id_zero() {
        assert_eq!(nal_type(&[NAL_IDR_W_RADL << 1, 0x01]), Some(NAL_IDR_W_RADL));
        assert_eq!(nal_type(&[NAL_SPS << 1, 0x01]), Some(NAL_SPS));
        assert_eq!(nal_type(&[NAL_SPS << 1]), None, "two bytes are required");
        assert_eq!(
            nal_type(&[NAL_SPS << 1, 0x00]),
            None,
            "temporal_id_plus1 zero is forbidden"
        );
        assert_eq!(nal_type(&[]), None);
    }

    #[test]
    fn parameter_sets_are_detected_only_when_all_three_are_present() {
        let all = cat(&[
            nal4(NAL_VPS, &[0]),
            nal4(NAL_SPS, &[1]),
            nal4(NAL_PPS, &[2]),
            nal4(NAL_IDR_W_RADL, &[3]),
        ]);
        let sps_pps = cat(&[nal4(NAL_SPS, &[1]), nal4(NAL_PPS, &[2])]);
        let vps_sps = cat(&[nal4(NAL_VPS, &[0]), nal4(NAL_SPS, &[1])]);
        let neither = cat(&[nal4(NAL_TRAIL_R, &[3])]);
        assert!(has_parameter_sets(&all));
        assert!(!has_parameter_sets(&sps_pps));
        assert!(!has_parameter_sets(&vps_sps));
        assert!(!has_parameter_sets(&neither));
    }

    #[test]
    fn contains_irap_recognises_both_idrs_and_cra_but_not_wrappers() {
        for kind in [NAL_IDR_W_RADL, NAL_IDR_N_LP, NAL_CRA] {
            let with = cat(&[
                nal4(NAL_AUD, &[0xF0]),
                nal4(NAL_PREFIX_SEI, &[0x01]),
                nal4(kind, &[0x02]),
            ]);
            assert!(contains_irap(&with), "kind {kind}");
        }
        let without = cat(&[nal4(NAL_AUD, &[0xF0]), nal4(NAL_TRAIL_R, &[0x02])]);
        assert!(!contains_irap(&without));
    }

    #[test]
    fn a_sequence_header_blob_yields_the_triplet() {
        let blob = cat(&[
            nal4(NAL_VPS, &[0x0C]),
            nal4(NAL_SPS, &[0x64, 0x00, 0x1F]),
            nal4(NAL_PPS, &[0xEE, 0x3C]),
        ]);
        let sets = ParameterSets::from_sequence_header(&blob).expect("all present");
        assert_eq!(sets.vps, vec![NAL_VPS << 1, 0x01, 0x0C]);
        assert_eq!(sets.sps, vec![NAL_SPS << 1, 0x01, 0x64, 0x00, 0x1F]);
        assert_eq!(sets.pps, vec![NAL_PPS << 1, 0x01, 0xEE, 0x3C]);
    }

    #[test]
    fn a_sequence_header_missing_any_set_is_refused() {
        let blob = cat(&[nal4(NAL_SPS, &[0x64]), nal4(NAL_PPS, &[0x01])]);
        assert_eq!(ParameterSets::from_sequence_header(&blob), None);
    }

    #[test]
    fn to_annex_b_round_trips_through_the_parser() {
        let sets = ParameterSets {
            vps: vec![NAL_VPS << 1, 0x01, 0x99],
            sps: vec![NAL_SPS << 1, 0x01, 0xAA, 0xBB],
            pps: vec![NAL_PPS << 1, 0x01, 0xCC],
        };
        let parsed = ParameterSets::from_sequence_header(&sets.to_annex_b()).unwrap();
        assert_eq!(parsed, sets);
    }

    #[test]
    fn an_inband_irap_is_left_alone_without_a_copy() {
        let sets = ParameterSets {
            vps: nal4(NAL_VPS, &[0x99])[4..].to_vec(),
            sps: nal4(NAL_SPS, &[0xAA])[4..].to_vec(),
            pps: nal4(NAL_PPS, &[0xBB])[4..].to_vec(),
        };
        let au = cat(&[
            nal4(NAL_VPS, &[0x99]),
            nal4(NAL_SPS, &[0xAA]),
            nal4(NAL_PPS, &[0xBB]),
            nal4(NAL_IDR_W_RADL, &[0x01]),
        ]);
        let (out, prepended) = ensure_parameter_sets(&au, Some(&sets), true).unwrap();
        assert!(!prepended);
        assert!(matches!(out, Cow::Borrowed(_)), "no copy taken");
        assert_eq!(out.as_ref(), au.as_slice());
    }

    #[test]
    fn a_bare_irap_gets_the_stored_sets_in_front() {
        let sets = ParameterSets {
            vps: nal4(NAL_VPS, &[0x99])[4..].to_vec(),
            sps: nal4(NAL_SPS, &[0xAA])[4..].to_vec(),
            pps: nal4(NAL_PPS, &[0xBB])[4..].to_vec(),
        };
        let au = nal4(NAL_CRA, &[0x01, 0x02]);
        let (out, prepended) = ensure_parameter_sets(&au, Some(&sets), true).unwrap();
        assert!(prepended);
        assert!(has_parameter_sets(&out));
        let units = nal_units(&out);
        assert_eq!(
            units
                .iter()
                .map(|n| nal_type(n).unwrap())
                .collect::<Vec<_>>(),
            vec![NAL_VPS, NAL_SPS, NAL_PPS, NAL_CRA]
        );
        assert_eq!(units[3], &[NAL_CRA << 1, 0x01, 0x01, 0x02]);
    }

    #[test]
    fn parameter_sets_after_the_irap_are_moved_in_front() {
        let sets = ParameterSets {
            vps: nal4(NAL_VPS, &[0x99])[4..].to_vec(),
            sps: nal4(NAL_SPS, &[0xAA])[4..].to_vec(),
            pps: nal4(NAL_PPS, &[0xBB])[4..].to_vec(),
        };
        let malformed = cat(&[
            nal4(NAL_CRA, &[0x01]),
            nal4(NAL_VPS, &[0x99]),
            nal4(NAL_SPS, &[0xAA]),
            nal4(NAL_PPS, &[0xBB]),
        ]);
        let (out, prepended) = ensure_parameter_sets(&malformed, Some(&sets), true).unwrap();
        assert!(prepended);
        let kinds: Vec<_> = nal_units(&out)
            .iter()
            .map(|nal| nal_type(nal).unwrap())
            .collect();
        assert_eq!(&kinds[..4], &[NAL_VPS, NAL_SPS, NAL_PPS, NAL_CRA]);
    }

    #[test]
    fn a_non_irap_is_never_padded() {
        let sets = ParameterSets {
            vps: nal4(NAL_VPS, &[0x99])[4..].to_vec(),
            sps: nal4(NAL_SPS, &[0xAA])[4..].to_vec(),
            pps: nal4(NAL_PPS, &[0xBB])[4..].to_vec(),
        };
        let au = nal4(NAL_TRAIL_R, &[0x01]);
        let (out, prepended) = ensure_parameter_sets(&au, Some(&sets), false).unwrap();
        assert!(!prepended);
        assert_eq!(out.as_ref(), au.as_slice());
    }

    #[test]
    fn a_bare_irap_without_cached_vps_sps_pps_is_refused() {
        let au = nal4(NAL_CRA, &[0xAA]);
        assert_eq!(
            ensure_parameter_sets(&au, None, true),
            Err(MissingParameterSets)
        );
    }

    #[test]
    fn parses_every_negotiated_field_from_a_real_main_high_tier_level_4_1_sps() {
        // libx265 HEVC Main, high tier, level 4.1, 1920x1080, 4:2:0 8-bit.
        // Distinct dimensions and fields make a bit-offset error visible.
        let stream = [
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x21, 0x60, 0x00, 0x00,
            0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x7b, 0x95, 0x98, 0x09,
            0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0x01, 0x21, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90,
            0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x7b, 0xa0, 0x03, 0xc0, 0x80, 0x10, 0xe5,
            0x96, 0x56, 0x69, 0x24, 0xca, 0xf0, 0x16, 0x80, 0x80, 0x00, 0x00, 0x03, 0x00, 0x80,
            0x00, 0x00, 0x1e, 0x04, 0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xc1, 0x72, 0xb4, 0x62,
            0x40,
        ];
        assert_eq!(
            stream_config(&stream),
            Ok(StreamConfig {
                profile_idc: 1,
                high_tier: true,
                level_idc: 123,
                chroma_format_idc: 1,
                bit_depth_luma: 8,
                bit_depth_chroma: 8,
                width: 1920,
                height: 1080,
            })
        );
    }

    #[test]
    fn stream_contract_accepts_main_tier_and_rejects_every_wrong_field() {
        let expected = StreamConfig {
            profile_idc: 1,
            high_tier: false,
            level_idc: 123,
            chroma_format_idc: 1,
            bit_depth_luma: 8,
            bit_depth_chroma: 8,
            width: 1920,
            height: 1080,
        };
        assert_eq!(validate_config(expected, 1920, 1080, 123), Ok(()));

        let wrong = [
            StreamConfig {
                profile_idc: 2,
                ..expected
            },
            StreamConfig {
                high_tier: true,
                ..expected
            },
            StreamConfig {
                level_idc: 120,
                ..expected
            },
            StreamConfig {
                chroma_format_idc: 2,
                ..expected
            },
            StreamConfig {
                bit_depth_luma: 10,
                ..expected
            },
            StreamConfig {
                bit_depth_chroma: 10,
                ..expected
            },
            StreamConfig {
                width: 1918,
                ..expected
            },
            StreamConfig {
                height: 1078,
                ..expected
            },
        ];
        for config in wrong {
            assert!(
                validate_config(config, 1920, 1080, 123).is_err(),
                "accepted {config:?}"
            );
        }
    }

    #[test]
    fn hevc_level_is_selected_from_picture_rate_and_main_tier_bitrate() {
        assert_eq!(required_level_idc(2560, 1440, 60, 20_000), Ok(150));
        assert_eq!(required_level_idc(5120, 2880, 60, 20_000), Ok(180));
        assert_eq!(required_level_idc(2560, 1440, 60, 30_000), Ok(153));
        assert_eq!(required_level_idc(5120, 2880, 120, 20_000), Ok(183));
    }

    #[test]
    fn hevc_level_refuses_a_contract_beyond_level_6_2() {
        assert!(required_level_idc(0, 2880, 60, 20_000).is_err());
        assert!(required_level_idc(5120, 2880, 300, 20_000).is_err());
        assert!(required_level_idc(5120, 2880, 60, 300_000).is_err());
    }
}
