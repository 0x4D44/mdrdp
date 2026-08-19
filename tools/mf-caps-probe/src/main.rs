//! mf-caps-probe — what a Windows host's Media Foundation *encoders* will actually
//! accept, measured rather than assumed.
//!
//! The question this answers: for H.264 / HEVC / AV1 / VP9, which chroma formats and
//! bit depths can this box encode in hardware, at 1080p and at 5K? Registry-level
//! enumeration lies in both directions — an MFT can advertise a 4:4:4 input type and
//! still refuse the matching output profile, and it can accept a profile and then
//! emit a bitstream whose SPS says 4:2:0. So the probe runs four escalating tests and
//! prints all four, because the disagreements between them are the interesting part:
//!
//!   1. Host — CPU, OS build, session id, DXGI adapters.
//!   2. Inventory — `MFTEnumEx` per output subtype, hardware pass and all pass, with
//!      each MFT's *registered* input subtypes (the 4:4:4 ones are the tell).
//!   3. Acceptance — `SetOutputType` per (profile, size), then what input types the
//!      encoder offers back for the profile it just accepted.
//!   4. Real encode — 12 synthetic frames through the transform, and the chroma the
//!      **output bitstream** claims, parsed out of the SPS / sequence header.
//!
//! Nothing here panics on a single failure: every attempt records its HRESULT and the
//! probe carries on, because "which one failed and how" is the whole point. Every wait
//! is bounded at 5 s so a wedged driver costs one attempt, not the run.
//!
//! Windows-only; cross-built from macOS with `build.sh`.

// ---------------------------------------------------------------------------
// Portable bit plumbing and bitstream parsers.
//
// These are deliberately outside the `cfg(windows)` block: they are pure logic, so
// `cargo check` on the build host compiles them, which catches most of the fiddly
// parsing errors before anything is copied to the box.
// ---------------------------------------------------------------------------

/// MSB-first bit reader over a byte slice. Every read is bounds-checked and returns
/// `None` past the end — a truncated bitstream must not panic mid-probe.
pub struct BitReader<'a> {
    data: &'a [u8],
    /// Absolute bit position from the start of `data`.
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Read `n` bits (n <= 64), MSB first.
    pub fn u(&mut self, n: usize) -> Option<u64> {
        if n > 64 || self.pos + n > self.data.len() * 8 {
            return None;
        }
        let mut v: u64 = 0;
        for _ in 0..n {
            let byte = self.data[self.pos >> 3];
            let bit = (byte >> (7 - (self.pos & 7))) & 1;
            v = (v << 1) | bit as u64;
            self.pos += 1;
        }
        Some(v)
    }

    pub fn flag(&mut self) -> Option<bool> {
        self.u(1).map(|v| v == 1)
    }

    pub fn skip(&mut self, n: usize) -> Option<()> {
        if self.pos + n > self.data.len() * 8 {
            return None;
        }
        self.pos += n;
        Some(())
    }

    /// Exp-Golomb unsigned (H.264/HEVC `ue(v)`). Bounded at 32 leading zeros so a
    /// misaligned reader cannot spin.
    pub fn ue(&mut self) -> Option<u64> {
        let mut leading = 0usize;
        loop {
            if self.flag()? {
                break;
            }
            leading += 1;
            if leading > 32 {
                return None;
            }
        }
        if leading == 0 {
            return Some(0);
        }
        let rest = self.u(leading)?;
        Some((1u64 << leading) - 1 + rest)
    }

    /// AV1 `uvlc()` — same shape as `ue(v)` but the leading-zero terminator is read
    /// as a separate "done" bit and 32 leading zeros means "unbounded".
    pub fn uvlc(&mut self) -> Option<u64> {
        let mut leading = 0usize;
        loop {
            if self.flag()? {
                break;
            }
            leading += 1;
            if leading >= 32 {
                return Some(u32::MAX as u64);
            }
        }
        let value = self.u(leading)?;
        Some(value + (1u64 << leading) - 1)
    }
}

/// Strip H.264/HEVC emulation-prevention bytes: `00 00 03` → `00 00`.
///
/// Skipping this is the single most common way a hand-written SPS parser reads the
/// wrong `chroma_format_idc`, because a `03` inserted before the field shifts every
/// later bit by eight.
pub fn strip_epb(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0usize;
    for &b in nal {
        if zeros >= 2 && b == 0x03 {
            zeros = 0;
            continue;
        }
        if b == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
        out.push(b);
    }
    out
}

/// Split an Annex-B stream into NAL payloads (start codes removed, EPB still in).
pub fn annexb_nals(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (n, &s) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map(|&e| e - 3).unwrap_or(stream.len());
        // A four-byte start code leaves a trailing zero on the previous NAL.
        while end > s && stream[end - 1] == 0 {
            end -= 1;
        }
        if end > s {
            out.push(&stream[s..end]);
        }
    }
    out
}

/// What a decoder would read out of the bitstream itself.
#[derive(Debug, Default, Clone)]
pub struct ChromaEvidence {
    pub note: String,
    pub profile_idc: Option<u64>,
    pub chroma_format_idc: Option<u64>,
    pub bit_depth: Option<u64>,
    /// Human reading of the numbers above, e.g. "4:4:4 8-bit".
    pub reading: String,
}

fn chroma_word(idc: u64) -> &'static str {
    match idc {
        0 => "monochrome",
        1 => "4:2:0",
        2 => "4:2:2",
        3 => "4:4:4",
        _ => "?",
    }
}

/// H.264 SPS (`nal_unit_type == 7`).
///
/// `chroma_format_idc` is only *present* for the profiles that can carry something
/// other than 4:2:0; for the rest it is inferred as 1. Reporting "absent" as 4:2:0 is
/// correct, and the list of profiles that carry it is the standard's, not a guess.
pub fn parse_h264(stream: &[u8]) -> ChromaEvidence {
    let mut ev = ChromaEvidence::default();
    let Some(sps) = annexb_nals(stream)
        .into_iter()
        .find(|n| !n.is_empty() && (n[0] & 0x1f) == 7)
    else {
        ev.note = "no H.264 SPS NAL (type 7) in the stream".into();
        return ev;
    };
    let rbsp = strip_epb(sps);
    if rbsp.len() < 5 {
        ev.note = "H.264 SPS too short".into();
        return ev;
    }
    let mut r = BitReader::new(&rbsp[1..]); // skip the 1-byte NAL header
    let Some(profile_idc) = r.u(8) else {
        ev.note = "H.264 SPS truncated at profile_idc".into();
        return ev;
    };
    ev.profile_idc = Some(profile_idc);
    if r.skip(8).is_none() || r.skip(8).is_none() {
        // constraint_set flags + level_idc
        ev.note = "H.264 SPS truncated at level_idc".into();
        return ev;
    }
    if r.ue().is_none() {
        ev.note = "H.264 SPS truncated at seq_parameter_set_id".into();
        return ev;
    }
    const HAS_CHROMA_FIELD: [u64; 13] = [
        100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135,
    ];
    if HAS_CHROMA_FIELD.contains(&profile_idc) {
        match r.ue() {
            Some(cf) => {
                ev.chroma_format_idc = Some(cf);
                if cf == 3 {
                    // separate_colour_plane_flag
                    let _ = r.flag();
                }
                let bd_luma = r.ue();
                ev.bit_depth = bd_luma.map(|v| v + 8);
            }
            None => ev.note = "H.264 SPS truncated at chroma_format_idc".into(),
        }
    } else {
        // Not coded for these profiles; the standard's inferred value is 1.
        ev.chroma_format_idc = Some(1);
        ev.bit_depth = Some(8);
        ev.note = "chroma_format_idc not coded for this profile_idc; inferred 1".into();
    }
    ev.reading = format!(
        "profile_idc={} {} {}-bit",
        profile_idc,
        ev.chroma_format_idc.map(chroma_word).unwrap_or("?"),
        ev.bit_depth.map(|d| d.to_string()).unwrap_or("?".into())
    );
    ev
}

/// HEVC SPS (`nal_unit_type == 33`), including the full `profile_tier_level` skip.
pub fn parse_hevc(stream: &[u8]) -> ChromaEvidence {
    let mut ev = ChromaEvidence::default();
    let Some(sps) = annexb_nals(stream)
        .into_iter()
        .find(|n| n.len() >= 2 && ((n[0] >> 1) & 0x3f) == 33)
    else {
        ev.note = "no HEVC SPS NAL (type 33) in the stream".into();
        return ev;
    };
    let rbsp = strip_epb(sps);
    if rbsp.len() < 15 {
        ev.note = "HEVC SPS too short".into();
        return ev;
    }
    let mut r = BitReader::new(&rbsp[2..]); // skip the 2-byte NAL header
    let fail = |ev: &mut ChromaEvidence, at: &str| {
        ev.note = format!("HEVC SPS truncated at {at}");
    };
    if r.skip(4).is_none() {
        fail(&mut ev, "sps_video_parameter_set_id");
        return ev;
    }
    let Some(max_sub_minus1) = r.u(3) else {
        fail(&mut ev, "sps_max_sub_layers_minus1");
        return ev;
    };
    if r.skip(1).is_none() {
        fail(&mut ev, "sps_temporal_id_nesting_flag");
        return ev;
    }
    // profile_tier_level(1, max_sub_minus1). General part: profile_space(2) +
    // tier(1) + profile_idc(5) + 32 compatibility flags + 48 bits of constraint
    // flags + level_idc(8) = 96 bits.
    if r.skip(2).is_none() || r.skip(1).is_none() {
        fail(&mut ev, "general_profile_space");
        return ev;
    }
    let Some(general_profile_idc) = r.u(5) else {
        fail(&mut ev, "general_profile_idc");
        return ev;
    };
    ev.profile_idc = Some(general_profile_idc);
    if r.skip(32).is_none() || r.skip(48).is_none() || r.skip(8).is_none() {
        fail(&mut ev, "profile_tier_level general part");
        return ev;
    }
    let n = max_sub_minus1 as usize;
    let mut sub_profile = [false; 8];
    let mut sub_level = [false; 8];
    for i in 0..n {
        let Some(p) = r.flag() else {
            fail(&mut ev, "sub_layer_profile_present_flag");
            return ev;
        };
        let Some(l) = r.flag() else {
            fail(&mut ev, "sub_layer_level_present_flag");
            return ev;
        };
        sub_profile[i] = p;
        sub_level[i] = l;
    }
    if n > 0 {
        // reserved_zero_2bits padding out to eight sub-layers.
        if r.skip((8 - n) * 2).is_none() {
            fail(&mut ev, "reserved_zero_2bits");
            return ev;
        }
    }
    for i in 0..n {
        if sub_profile[i] && r.skip(88).is_none() {
            fail(&mut ev, "sub_layer profile");
            return ev;
        }
        if sub_level[i] && r.skip(8).is_none() {
            fail(&mut ev, "sub_layer level");
            return ev;
        }
    }
    if r.ue().is_none() {
        fail(&mut ev, "sps_seq_parameter_set_id");
        return ev;
    }
    let Some(cf) = r.ue() else {
        fail(&mut ev, "chroma_format_idc");
        return ev;
    };
    ev.chroma_format_idc = Some(cf);
    if cf == 3 {
        let _ = r.flag(); // separate_colour_plane_flag
    }
    // pic_width/height, conformance window, then the bit depths.
    if r.ue().is_none() || r.ue().is_none() {
        fail(&mut ev, "pic_width_in_luma_samples");
        return ev;
    }
    if let Some(true) = r.flag() {
        // conformance_window_flag
        for _ in 0..4 {
            if r.ue().is_none() {
                fail(&mut ev, "conformance window");
                return ev;
            }
        }
    }
    ev.bit_depth = r.ue().map(|v| v + 8);
    ev.reading = format!(
        "general_profile_idc={} {} {}-bit",
        general_profile_idc,
        chroma_word(cf),
        ev.bit_depth.map(|d| d.to_string()).unwrap_or("?".into())
    );
    ev
}

/// AV1 low-overhead bitstream: find the sequence-header OBU and read `color_config`.
pub fn parse_av1(stream: &[u8]) -> ChromaEvidence {
    let mut ev = ChromaEvidence::default();
    let mut i = 0usize;
    while i < stream.len() {
        let header = stream[i];
        let obu_type = (header >> 3) & 0x0f;
        let extension = (header >> 2) & 1 == 1;
        let has_size = (header >> 1) & 1 == 1;
        let mut p = i + 1;
        if extension {
            p += 1;
        }
        let payload_len = if has_size {
            // leb128
            let mut value: u64 = 0;
            let mut shift = 0;
            let mut consumed = 0;
            loop {
                if p + consumed >= stream.len() || consumed >= 8 {
                    ev.note = "AV1 OBU size leb128 truncated".into();
                    return ev;
                }
                let b = stream[p + consumed];
                value |= ((b & 0x7f) as u64) << shift;
                consumed += 1;
                shift += 7;
                if b & 0x80 == 0 {
                    break;
                }
            }
            p += consumed;
            value as usize
        } else {
            stream.len().saturating_sub(p)
        };
        let end = (p + payload_len).min(stream.len());
        if obu_type == 1 {
            // OBU_SEQUENCE_HEADER
            return parse_av1_seq_header(&stream[p..end]);
        }
        if end <= i {
            break;
        }
        i = end;
    }
    ev.note = "no AV1 OBU_SEQUENCE_HEADER (type 1) found".into();
    ev
}

fn parse_av1_seq_header(payload: &[u8]) -> ChromaEvidence {
    let mut ev = ChromaEvidence::default();
    let mut r = BitReader::new(payload);
    let short = |ev: &mut ChromaEvidence, at: &str| {
        ev.note = format!("AV1 sequence header truncated at {at}");
    };
    let Some(seq_profile) = r.u(3) else {
        short(&mut ev, "seq_profile");
        return ev;
    };
    ev.profile_idc = Some(seq_profile);
    let Some(_still_picture) = r.flag() else {
        short(&mut ev, "still_picture");
        return ev;
    };
    let Some(reduced) = r.flag() else {
        short(&mut ev, "reduced_still_picture_header");
        return ev;
    };
    let mut decoder_model_info_present = false;
    let mut buffer_delay_len = 0usize;
    if reduced {
        if r.skip(5).is_none() {
            short(&mut ev, "seq_level_idx[0]");
            return ev;
        }
    } else {
        let Some(timing_info_present) = r.flag() else {
            short(&mut ev, "timing_info_present_flag");
            return ev;
        };
        if timing_info_present {
            if r.skip(32).is_none() || r.skip(32).is_none() {
                short(&mut ev, "timing_info");
                return ev;
            }
            let Some(equal_interval) = r.flag() else {
                short(&mut ev, "equal_picture_interval");
                return ev;
            };
            if equal_interval && r.uvlc().is_none() {
                short(&mut ev, "num_ticks_per_picture_minus_1");
                return ev;
            }
            let Some(dmip) = r.flag() else {
                short(&mut ev, "decoder_model_info_present_flag");
                return ev;
            };
            decoder_model_info_present = dmip;
            if dmip {
                let Some(bdlm1) = r.u(5) else {
                    short(&mut ev, "buffer_delay_length_minus_1");
                    return ev;
                };
                buffer_delay_len = bdlm1 as usize + 1;
                if r.skip(32).is_none() || r.skip(5).is_none() || r.skip(5).is_none() {
                    short(&mut ev, "decoder_model_info");
                    return ev;
                }
            }
        }
        let Some(initial_display_delay_present) = r.flag() else {
            short(&mut ev, "initial_display_delay_present_flag");
            return ev;
        };
        let Some(op_cnt_minus1) = r.u(5) else {
            short(&mut ev, "operating_points_cnt_minus_1");
            return ev;
        };
        for _ in 0..=op_cnt_minus1 {
            if r.skip(12).is_none() {
                short(&mut ev, "operating_point_idc");
                return ev;
            }
            let Some(level) = r.u(5) else {
                short(&mut ev, "seq_level_idx");
                return ev;
            };
            if level > 7 && r.skip(1).is_none() {
                short(&mut ev, "seq_tier");
                return ev;
            }
            if decoder_model_info_present {
                let Some(present_for_op) = r.flag() else {
                    short(&mut ev, "decoder_model_present_for_this_op");
                    return ev;
                };
                if present_for_op
                    && (r.skip(buffer_delay_len).is_none()
                        || r.skip(buffer_delay_len).is_none()
                        || r.skip(1).is_none())
                {
                    short(&mut ev, "operating_parameters_info");
                    return ev;
                }
            }
            if initial_display_delay_present {
                let Some(idd) = r.flag() else {
                    short(&mut ev, "initial_display_delay_present_for_this_op");
                    return ev;
                };
                if idd && r.skip(4).is_none() {
                    short(&mut ev, "initial_display_delay_minus_1");
                    return ev;
                }
            }
        }
    }
    let Some(fw_bits) = r.u(4) else {
        short(&mut ev, "frame_width_bits_minus_1");
        return ev;
    };
    let Some(fh_bits) = r.u(4) else {
        short(&mut ev, "frame_height_bits_minus_1");
        return ev;
    };
    if r.skip(fw_bits as usize + 1).is_none() || r.skip(fh_bits as usize + 1).is_none() {
        short(&mut ev, "max_frame_width/height");
        return ev;
    }
    if !reduced {
        let Some(frame_id_present) = r.flag() else {
            short(&mut ev, "frame_id_numbers_present_flag");
            return ev;
        };
        if frame_id_present && (r.skip(4).is_none() || r.skip(3).is_none()) {
            short(&mut ev, "frame id lengths");
            return ev;
        }
    }
    if r.skip(3).is_none() {
        // use_128x128_superblock, enable_filter_intra, enable_intra_edge_filter
        short(&mut ev, "superblock/intra flags");
        return ev;
    }
    if !reduced {
        if r.skip(4).is_none() {
            // interintra_compound, masked_compound, warped_motion, dual_filter
            short(&mut ev, "compound/motion flags");
            return ev;
        }
        let Some(enable_order_hint) = r.flag() else {
            short(&mut ev, "enable_order_hint");
            return ev;
        };
        if enable_order_hint && r.skip(2).is_none() {
            short(&mut ev, "jnt_comp/ref_frame_mvs");
            return ev;
        }
        let Some(choose_sct) = r.flag() else {
            short(&mut ev, "seq_choose_screen_content_tools");
            return ev;
        };
        let force_sct = if choose_sct {
            2u64
        } else {
            match r.u(1) {
                Some(v) => v,
                None => {
                    short(&mut ev, "seq_force_screen_content_tools");
                    return ev;
                }
            }
        };
        if force_sct > 0 {
            let Some(choose_imv) = r.flag() else {
                short(&mut ev, "seq_choose_integer_mv");
                return ev;
            };
            if !choose_imv && r.skip(1).is_none() {
                short(&mut ev, "seq_force_integer_mv");
                return ev;
            }
        }
        if enable_order_hint && r.skip(3).is_none() {
            short(&mut ev, "order_hint_bits_minus_1");
            return ev;
        }
    }
    if r.skip(3).is_none() {
        // enable_superres, enable_cdef, enable_restoration
        short(&mut ev, "superres/cdef/restoration");
        return ev;
    }

    // color_config()
    let Some(high_bitdepth) = r.flag() else {
        short(&mut ev, "high_bitdepth");
        return ev;
    };
    let bit_depth = if seq_profile == 2 && high_bitdepth {
        match r.flag() {
            Some(twelve) => {
                if twelve {
                    12
                } else {
                    10
                }
            }
            None => {
                short(&mut ev, "twelve_bit");
                return ev;
            }
        }
    } else if seq_profile <= 2 {
        if high_bitdepth {
            10
        } else {
            8
        }
    } else {
        8
    };
    ev.bit_depth = Some(bit_depth);
    let mono = if seq_profile == 1 {
        false
    } else {
        match r.flag() {
            Some(m) => m,
            None => {
                short(&mut ev, "mono_chrome");
                return ev;
            }
        }
    };
    let Some(desc_present) = r.flag() else {
        short(&mut ev, "color_description_present_flag");
        return ev;
    };
    let (mut cp, mut tc, mut mc) = (2u64, 2u64, 2u64); // UNSPECIFIED
    if desc_present {
        cp = match r.u(8) {
            Some(v) => v,
            None => {
                short(&mut ev, "color_primaries");
                return ev;
            }
        };
        tc = match r.u(8) {
            Some(v) => v,
            None => {
                short(&mut ev, "transfer_characteristics");
                return ev;
            }
        };
        mc = match r.u(8) {
            Some(v) => v,
            None => {
                short(&mut ev, "matrix_coefficients");
                return ev;
            }
        };
    }
    let (sub_x, sub_y): (u64, u64);
    if mono {
        // Monochrome is implicitly subsampled 1/1, but nothing reads it — the
        // chroma_format_idc below is the whole answer for this branch.
        let _ = r.flag(); // color_range
        ev.chroma_format_idc = Some(0);
        ev.reading = format!("seq_profile={seq_profile} monochrome {bit_depth}-bit");
        return ev;
    } else if cp == 1 && tc == 13 && mc == 0 {
        // sRGB shortcut: implicitly 4:4:4 full range.
        sub_x = 0;
        sub_y = 0;
    } else {
        let _ = r.flag(); // color_range
        if seq_profile == 0 {
            sub_x = 1;
            sub_y = 1;
        } else if seq_profile == 1 {
            sub_x = 0;
            sub_y = 0;
        } else if bit_depth == 12 {
            sub_x = match r.u(1) {
                Some(v) => v,
                None => {
                    short(&mut ev, "subsampling_x");
                    return ev;
                }
            };
            sub_y = if sub_x == 1 {
                match r.u(1) {
                    Some(v) => v,
                    None => {
                        short(&mut ev, "subsampling_y");
                        return ev;
                    }
                }
            } else {
                0
            };
        } else {
            sub_x = 1;
            sub_y = 0;
        }
    }
    let idc = match (sub_x, sub_y) {
        (1, 1) => 1,
        (1, 0) => 2,
        (0, 0) => 3,
        _ => 9,
    };
    ev.chroma_format_idc = Some(idc);
    ev.reading = format!(
        "seq_profile={seq_profile} subsampling_x={sub_x} subsampling_y={sub_y} => {} {bit_depth}-bit",
        chroma_word(idc)
    );
    ev
}

/// VP9 keyframe uncompressed header.
pub fn parse_vp9(stream: &[u8]) -> ChromaEvidence {
    let mut ev = ChromaEvidence::default();
    let mut r = BitReader::new(stream);
    let short = |ev: &mut ChromaEvidence, at: &str| {
        ev.note = format!("VP9 uncompressed header truncated at {at}");
    };
    let Some(marker) = r.u(2) else {
        short(&mut ev, "frame_marker");
        return ev;
    };
    if marker != 2 {
        ev.note = format!("VP9 frame_marker is {marker}, expected 2 — not a VP9 frame");
        return ev;
    }
    let Some(low) = r.u(1) else {
        short(&mut ev, "profile_low_bit");
        return ev;
    };
    let Some(high) = r.u(1) else {
        short(&mut ev, "profile_high_bit");
        return ev;
    };
    let profile = (high << 1) | low;
    ev.profile_idc = Some(profile);
    if profile == 3 && r.skip(1).is_none() {
        short(&mut ev, "reserved_zero");
        return ev;
    }
    let Some(show_existing) = r.flag() else {
        short(&mut ev, "show_existing_frame");
        return ev;
    };
    if show_existing {
        ev.note = "first VP9 frame is show_existing_frame; no color config".into();
        return ev;
    }
    let Some(frame_type) = r.u(1) else {
        short(&mut ev, "frame_type");
        return ev;
    };
    if r.skip(1).is_none() || r.skip(1).is_none() {
        // show_frame, error_resilient_mode
        short(&mut ev, "show_frame/error_resilient_mode");
        return ev;
    }
    if frame_type != 0 {
        ev.note = "first VP9 frame is not a keyframe; color config only in keyframes".into();
        return ev;
    }
    let Some(sync) = r.u(24) else {
        short(&mut ev, "frame_sync_code");
        return ev;
    };
    if sync != 0x498342 {
        ev.note = format!("VP9 frame_sync_code is 0x{sync:06X}, expected 0x498342");
        return ev;
    }
    let bit_depth = if profile >= 2 {
        match r.u(1) {
            Some(1) => 12,
            Some(_) => 10,
            None => {
                short(&mut ev, "ten_or_twelve_bit");
                return ev;
            }
        }
    } else {
        8
    };
    ev.bit_depth = Some(bit_depth);
    let Some(color_space) = r.u(3) else {
        short(&mut ev, "color_space");
        return ev;
    };
    let (sub_x, sub_y): (u64, u64);
    if color_space != 7 {
        if r.skip(1).is_none() {
            short(&mut ev, "color_range");
            return ev;
        }
        if profile == 1 || profile == 3 {
            sub_x = match r.u(1) {
                Some(v) => v,
                None => {
                    short(&mut ev, "subsampling_x");
                    return ev;
                }
            };
            sub_y = match r.u(1) {
                Some(v) => v,
                None => {
                    short(&mut ev, "subsampling_y");
                    return ev;
                }
            };
            let _ = r.skip(1); // reserved_zero
        } else {
            sub_x = 1;
            sub_y = 1;
        }
    } else {
        // CS_RGB — 4:4:4 by definition.
        sub_x = 0;
        sub_y = 0;
        if profile == 1 || profile == 3 {
            let _ = r.skip(1);
        }
    }
    let idc = match (sub_x, sub_y) {
        (1, 1) => 1,
        (1, 0) => 2,
        (0, 0) => 3,
        _ => 9,
    };
    ev.chroma_format_idc = Some(idc);
    ev.reading = format!(
        "vp9 profile={profile} color_space={color_space} subsampling_x={sub_x} subsampling_y={sub_y} => {} {bit_depth}-bit",
        chroma_word(idc)
    );
    ev
}

/// 16-bytes-per-line hex + ASCII, for the raw dumps the report asks for.
pub fn hex_dump(bytes: &[u8], limit: usize) -> String {
    let take = bytes.len().min(limit);
    let mut out = String::new();
    for (row, chunk) in bytes[..take].chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02X}")).collect();
        out.push_str(&format!("      {:04X}  {}\n", row * 16, hex.join(" ")));
    }
    if out.is_empty() {
        out.push_str("      <empty>\n");
    }
    out
}

fn main() {
    #[cfg(windows)]
    winprobe::run();
    #[cfg(not(windows))]
    {
        eprintln!("mf-caps-probe: Windows-only. Cross-build with build.sh and run it on the host.");
    }
}

// ---------------------------------------------------------------------------
// The Windows half.
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod winprobe {
    use super::{hex_dump, parse_av1, parse_h264, parse_hevc, parse_vp9, ChromaEvidence};
    use std::ffi::c_void;
    use std::time::{Duration, Instant};
    use windows::core::{Interface, GUID, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{HMODULE, VARIANT_FALSE, VARIANT_TRUE};
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Resource,
        ID3D11Texture2D, D3D11_BIND_FLAG, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
        D3D11_CPU_ACCESS_WRITE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
        D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_WRITE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
    };
    // D3D12CreateDevice lives here; every D3D12 *video* encode type lives under
    // Win32::Media::MediaFoundation in this crate, and comes in with the glob below.
    use windows::Win32::Graphics::Direct3D12::{D3D12CreateDevice, ID3D12Device};
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT, DXGI_FORMAT_AYUV, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12,
        DXGI_FORMAT_P010, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_Y210, DXGI_FORMAT_Y410,
        DXGI_FORMAT_Y416, DXGI_FORMAT_YUY2, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
    };
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1};
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    };
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_UI4};

    /// Every wait in the probe is bounded by this. A wedged driver costs one attempt.
    const WAIT_LIMIT: Duration = Duration::from_secs(5);
    /// Frames per encode attempt. Small on purpose: this measures *cold* behaviour and
    /// whether the pipe works at all, not steady-state throughput.
    const FRAMES: usize = 12;
    const BITRATE_BPS: u32 = 20_000_000;
    const FPS: u32 = 60;
    const GOP: u32 = 4;
    /// Hard ceiling on real-encode attempts so a box with many encoders cannot turn
    /// this into an unbounded run.
    const MAX_ENCODE_ATTEMPTS: usize = 64;

    // -- small formatting helpers ------------------------------------------------

    fn guid_str(g: &GUID) -> String {
        let d = g.data4;
        format!(
            "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
            g.data1, g.data2, g.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
        )
    }

    /// The Media Foundation "format GUID" base: `XXXXXXXX-0000-0010-8000-00AA00389B71`.
    /// Everything from NV12 to Y416 is that base with a FourCC or a D3DFMT number in
    /// `data1`, so decoding it generically beats a hand-kept name table — an unknown
    /// subtype still prints as readable characters instead of a raw GUID.
    fn is_mf_format_base(g: &GUID) -> bool {
        g.data2 == 0
            && g.data3 == 0x0010
            && g.data4 == [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71]
    }

    /// A FourCC-based MF subtype GUID: `XXXXXXXX-0000-0010-8000-00AA00389B71`.
    const fn fourcc_guid(cc: &[u8; 4]) -> GUID {
        GUID {
            data1: u32::from_le_bytes([cc[0], cc[1], cc[2], cc[3]]),
            data2: 0,
            data3: 0x0010,
            data4: [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
        }
    }

    /// Planar 4:4:4 and 4:2:2 8-bit. Windows has no `MFVideoFormat_` constant for
    /// either, but the Microsoft HEVC store encoder offers both — and `I444` is the
    /// only 4:4:4 input it will take, so missing it means missing the answer.
    const MFVIDEOFORMAT_I444: GUID = fourcc_guid(b"I444");
    const MFVIDEOFORMAT_I422: GUID = fourcc_guid(b"I422");

    /// Named exceptions that do not follow the FourCC base, plus the codec subtypes
    /// whose FourCC is not self-explanatory.
    fn named_subtype(g: &GUID) -> Option<&'static str> {
        let table: &[(GUID, &str)] = &[
            (MFVideoFormat_NV12, "NV12"),
            (MFVideoFormat_P010, "P010"),
            (MFVideoFormat_P016, "P016"),
            (MFVideoFormat_YUY2, "YUY2"),
            (MFVideoFormat_Y210, "Y210 (4:2:2 10-bit)"),
            (MFVideoFormat_Y216, "Y216 (4:2:2 16-bit)"),
            (MFVideoFormat_AYUV, "AYUV (4:4:4 8-bit)"),
            (MFVideoFormat_Y410, "Y410 (4:4:4 10-bit)"),
            (MFVideoFormat_Y416, "Y416 (4:4:4 16-bit)"),
            (MFVideoFormat_ARGB32, "ARGB32 (4:4:4 RGB)"),
            (MFVideoFormat_RGB32, "RGB32 (4:4:4 RGB)"),
            (MFVideoFormat_RGB24, "RGB24 (4:4:4 RGB)"),
            (MFVIDEOFORMAT_I444, "I444 (planar 4:4:4 8-bit)"),
            (MFVIDEOFORMAT_I422, "I422 (planar 4:2:2 8-bit)"),
            (MFVideoFormat_IYUV, "IYUV"),
            (MFVideoFormat_I420, "I420"),
            (MFVideoFormat_YV12, "YV12"),
            (MFVideoFormat_NV11, "NV11"),
            (MFVideoFormat_UYVY, "UYVY"),
            (MFVideoFormat_v210, "v210 (4:2:2 10-bit)"),
            (MFVideoFormat_L8, "L8 (monochrome)"),
            (MFVideoFormat_L16, "L16 (monochrome 16)"),
            (MFVideoFormat_H264, "H264"),
            (MFVideoFormat_HEVC, "HEVC"),
            (MFVideoFormat_HEVC_ES, "HEVC_ES"),
            (MFVideoFormat_AV1, "AV1"),
            (MFVideoFormat_VP90, "VP90"),
            (MFVideoFormat_VP80, "VP80"),
        ];
        table.iter().find(|(k, _)| *k == *g).map(|(_, v)| *v)
    }

    fn subtype_name(g: &GUID) -> String {
        if let Some(n) = named_subtype(g) {
            return n.to_owned();
        }
        if is_mf_format_base(g) {
            let bytes = g.data1.to_le_bytes();
            if bytes.iter().all(|&b| (0x20..=0x7e).contains(&b)) {
                return format!("'{}'", String::from_utf8_lossy(&bytes));
            }
            return format!("D3DFMT#{} {}", g.data1, guid_str(g));
        }
        guid_str(g)
    }

    /// Mark the formats that can carry 4:4:4 — the whole reason this probe exists.
    fn is_444_input(g: &GUID) -> bool {
        *g == MFVideoFormat_AYUV
            || *g == MFVideoFormat_Y410
            || *g == MFVideoFormat_Y416
            || *g == MFVideoFormat_ARGB32
            || *g == MFVideoFormat_RGB32
            || *g == MFVideoFormat_RGB24
            || *g == MFVIDEOFORMAT_I444
    }

    fn hr_str(e: &windows::core::Error) -> String {
        format!("0x{:08X} ({})", e.code().0 as u32, e.message())
    }

    fn wide_to_string(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    fn variant_u32(value: u32) -> VARIANT {
        let mut v = VARIANT::default();
        // SAFETY: writing the discriminant and matching union arm of a zeroed VARIANT.
        unsafe {
            (*v.Anonymous.Anonymous).vt = VT_UI4;
            (*v.Anonymous.Anonymous).Anonymous.ulVal = value;
        }
        v
    }

    fn variant_bool(value: bool) -> VARIANT {
        let mut v = VARIANT::default();
        // SAFETY: as above; VT_BOOL selects `boolVal`.
        unsafe {
            (*v.Anonymous.Anonymous).vt = VT_BOOL;
            (*v.Anonymous.Anonymous).Anonymous.boolVal =
                if value { VARIANT_TRUE } else { VARIANT_FALSE };
        }
        v
    }

    fn pack_ratio(high: u32, low: u32) -> u64 {
        ((high as u64) << 32) | low as u64
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    // -- section 1: host ---------------------------------------------------------

    fn cpu_brand() -> String {
        // CPUID leaves 0x80000002..0x80000004 hold the 48-byte brand string.
        let mut bytes = Vec::with_capacity(48);
        for leaf in 0x8000_0002u32..=0x8000_0004 {
            // The leaves are the documented brand-string leaves, supported by every
            // CPU this binary can run on.
            let r = std::arch::x86_64::__cpuid(leaf);
            for reg in [r.eax, r.ebx, r.ecx, r.edx] {
                bytes.extend_from_slice(&reg.to_le_bytes());
            }
        }
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).trim().to_owned()
    }

    fn reg_sz(subkey: &str, value: &str) -> Option<String> {
        let sk = wide(subkey);
        let vn = wide(value);
        let mut buf = [0u16; 512];
        let mut cb = (buf.len() * 2) as u32;
        // SAFETY: both wide strings are NUL-terminated locals; `buf`/`cb` describe the
        // same buffer, which is what RegGetValueW requires.
        let rc = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(sk.as_ptr()),
                PCWSTR(vn.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut c_void),
                Some(&mut cb),
            )
        };
        if rc.is_ok() {
            Some(wide_to_string(&buf))
        } else {
            None
        }
    }

    fn reg_dword(subkey: &str, value: &str) -> Option<u32> {
        let sk = wide(subkey);
        let vn = wide(value);
        let mut out = 0u32;
        let mut cb = 4u32;
        // SAFETY: as above; a DWORD read writes exactly four bytes into `out`.
        let rc = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(sk.as_ptr()),
                PCWSTR(vn.as_ptr()),
                RRF_RT_REG_DWORD,
                None,
                Some(&mut out as *mut u32 as *mut c_void),
                Some(&mut cb),
            )
        };
        if rc.is_ok() {
            Some(out)
        } else {
            None
        }
    }

    fn print_host() -> Option<(ID3D11Device, ID3D11DeviceContext)> {
        println!("=== 1. HOST ===");
        println!("cpu_brand              : {}", cpu_brand());
        const CV: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
        println!(
            "windows_product        : {}",
            reg_sz(CV, "ProductName").unwrap_or_else(|| "<unreadable>".into())
        );
        println!(
            "windows_display_version: {}",
            reg_sz(CV, "DisplayVersion").unwrap_or_else(|| "<unreadable>".into())
        );
        println!(
            "windows_build          : {}.{}",
            reg_sz(CV, "CurrentBuild").unwrap_or_else(|| "?".into()),
            reg_dword(CV, "UBR")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".into())
        );
        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        let mut session = 0u32;
        // SAFETY: `session` is a live out-parameter.
        let session_str = match unsafe { ProcessIdToSessionId(pid, &mut session) } {
            Ok(()) => session.to_string(),
            Err(e) => format!("<failed: {}>", hr_str(&e)),
        };
        println!("process_id             : {pid}");
        println!("session_id             : {session_str}");
        println!(
            "  (session 0 is the non-interactive SSH/service session; the console user is \
             normally session 1 or 2. Hardware MFT visibility can differ between them.)"
        );

        // DXGI adapters, and a D3D11 device on the first hardware one for the MFTs
        // that will not talk without a device manager.
        let mut device: Option<(ID3D11Device, ID3D11DeviceContext)> = None;
        // SAFETY: the factory interface is inferred from the binding.
        match unsafe { CreateDXGIFactory1::<IDXGIFactory1>() } {
            Ok(factory) => {
                let mut i = 0u32;
                loop {
                    // SAFETY: `factory` is live; EnumAdapters1 errors past the last index.
                    let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(i) } {
                        Ok(a) => a,
                        Err(_) => break,
                    };
                    // SAFETY: `adapter` is live; GetDesc1 returns the struct by value.
                    if let Ok(desc) = unsafe { adapter.GetDesc1() } {
                        println!(
                            "adapter[{i}]             : \"{}\" vendor=0x{:04X} device=0x{:04X} \
                             subsys=0x{:08X} rev={} vram_dedicated={} MiB shared={} MiB flags=0x{:X}",
                            wide_to_string(&desc.Description),
                            desc.VendorId,
                            desc.DeviceId,
                            desc.SubSysId,
                            desc.Revision,
                            desc.DedicatedVideoMemory / (1024 * 1024),
                            desc.SharedSystemMemory / (1024 * 1024),
                            desc.Flags
                        );
                        if device.is_none() && desc.Flags & 2 == 0 {
                            // DXGI_ADAPTER_FLAG_SOFTWARE == 2; skip WARP.
                            device = create_device(&adapter);
                        }
                    }
                    i += 1;
                }
                if i == 0 {
                    println!("adapter[]              : <none enumerated>");
                }
            }
            Err(e) => println!("dxgi_factory           : FAILED {}", hr_str(&e)),
        }
        println!(
            "d3d11_device           : {}",
            if device.is_some() {
                "created (available as an IMFDXGIDeviceManager for D3D-aware MFTs)"
            } else {
                "NOT created — D3D-aware MFTs will be probed without a device manager"
            }
        );

        // SAFETY: balanced by MFShutdown at the end of `run`.
        match unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) } {
            Ok(()) => println!("mf_startup             : ok"),
            Err(e) => println!("mf_startup             : FAILED {}", hr_str(&e)),
        }
        println!();
        device
    }

    fn create_device(adapter: &IDXGIAdapter1) -> Option<(ID3D11Device, ID3D11DeviceContext)> {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let levels = [D3D_FEATURE_LEVEL_11_0];
        // SAFETY: out-parameters are live Options; D3D_DRIVER_TYPE_UNKNOWN is
        // mandatory when an adapter is supplied.
        let hr = unsafe {
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        };
        if hr.is_err() {
            return None;
        }
        let device = device?;
        let context = context?;
        // A hardware MFT runs its own threads against this device; without
        // multithread protection the symptom is an intermittent hang, not an error.
        if let Ok(mt) = device.cast::<ID3D11Multithread>() {
            // SAFETY: `mt` is a live interface on our own device.
            let _ = unsafe { mt.SetMultithreadProtected(true) };
        }
        Some((device, context))
    }

    fn device_manager(device: &ID3D11Device) -> Option<IMFDXGIDeviceManager> {
        let mut token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        // SAFETY: both out-parameters are live locals.
        unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) }.ok()?;
        let manager = manager?;
        // SAFETY: `device` is live; `token` is the only value this manager accepts.
        unsafe { manager.ResetDevice(device, token) }.ok()?;
        Some(manager)
    }

    // -- section 2: inventory ----------------------------------------------------

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Codec {
        H264,
        Hevc,
        Av1,
        Vp9,
    }

    impl Codec {
        fn name(self) -> &'static str {
            match self {
                Codec::H264 => "H.264",
                Codec::Hevc => "HEVC",
                Codec::Av1 => "AV1",
                Codec::Vp9 => "VP9",
            }
        }
        fn subtype(self) -> GUID {
            match self {
                Codec::H264 => MFVideoFormat_H264,
                Codec::Hevc => MFVideoFormat_HEVC,
                Codec::Av1 => MFVideoFormat_AV1,
                Codec::Vp9 => MFVideoFormat_VP90,
            }
        }
        /// H.264 carries the profile in `MF_MT_MPEG2_PROFILE` only; the newer codecs
        /// want `MF_MT_VIDEO_PROFILE` as well, and encoders disagree about which.
        fn also_video_profile(self) -> bool {
            !matches!(self, Codec::H264)
        }
        fn parse(self, stream: &[u8]) -> ChromaEvidence {
            match self {
                Codec::H264 => parse_h264(stream),
                Codec::Hevc => parse_hevc(stream),
                Codec::Av1 => parse_av1(stream),
                Codec::Vp9 => parse_vp9(stream),
            }
        }
    }

    const CODECS: [Codec; 4] = [Codec::H264, Codec::Hevc, Codec::Av1, Codec::Vp9];

    struct Entry {
        codec: Codec,
        /// Position in the `ALL|SORTANDFILTER` enumeration for this codec. An
        /// `IMFActivate` is single-use — `ShutdownObject` poisons it, and every later
        /// `ActivateObject` on the same one returns `MF_E_INVALIDREQUEST` — so each
        /// attempt re-enumerates and picks this ordinal out again. Several MFTs
        /// (the DX12 and store-extension encoders) publish an all-zero CLSID, which
        /// is why identity here is ordinal + name, never CLSID alone.
        ordinal: usize,
        name: String,
        clsid: GUID,
        hardware: bool,
        vendor_id: Option<String>,
        hardware_url: Option<String>,
        input_subtypes: Vec<GUID>,
    }

    /// Enumeration flags used for every re-enumeration, so an ordinal stays stable.
    fn all_flags() -> MFT_ENUM_FLAG {
        MFT_ENUM_FLAG(MFT_ENUM_FLAG_ALL.0 | MFT_ENUM_FLAG_SORTANDFILTER.0)
    }

    /// A never-yet-activated `IMFActivate` for `entry`.
    fn fresh_activate(entry: &Entry) -> Result<IMFActivate, String> {
        let list = enumerate(entry.codec, all_flags())
            .map_err(|e| format!("re-enumeration failed {e}"))?;
        if let Some(a) = list.get(entry.ordinal) {
            let attrs: IMFAttributes = a.cast().map_err(|e| hr_str(&e))?;
            let name = allocated_string(&attrs, &MFT_FRIENDLY_NAME_Attribute).unwrap_or_default();
            if name == entry.name {
                return Ok(a.clone());
            }
        }
        // The registry order moved under us; fall back to the first name match.
        for a in &list {
            let Ok(attrs) = a.cast::<IMFAttributes>() else {
                continue;
            };
            if allocated_string(&attrs, &MFT_FRIENDLY_NAME_Attribute).as_deref()
                == Some(entry.name.as_str())
            {
                return Ok(a.clone());
            }
        }
        Err(format!("\"{}\" vanished from the enumeration", entry.name))
    }

    fn allocated_string(attrs: &IMFAttributes, key: &GUID) -> Option<String> {
        let mut ptr = PWSTR::null();
        let mut len = 0u32;
        // SAFETY: both out-parameters are live locals.
        if unsafe { attrs.GetAllocatedString(key, &mut ptr, &mut len) }.is_err() || ptr.is_null() {
            return None;
        }
        // SAFETY: MF allocated a NUL-terminated wide string with CoTaskMemAlloc;
        // `to_string` copies before the free below.
        let s = unsafe { ptr.to_string() }.ok();
        // SAFETY: the string came from CoTaskMemAlloc inside GetAllocatedString.
        unsafe { CoTaskMemFree(Some(ptr.as_ptr() as *const c_void)) };
        s
    }

    /// Decode `MFT_INPUT_TYPES_Attributes` — a blob of `MFT_REGISTER_TYPE_INFO`
    /// (two GUIDs, 32 bytes each).
    fn registered_input_subtypes(attrs: &IMFAttributes) -> Vec<GUID> {
        // SAFETY: `attrs` is live; a missing attribute is an error, not a write.
        let Ok(size) = (unsafe { attrs.GetBlobSize(&MFT_INPUT_TYPES_Attributes) }) else {
            return Vec::new();
        };
        if size == 0 {
            return Vec::new();
        }
        let mut blob = vec![0u8; size as usize];
        // SAFETY: `blob` is exactly `size` bytes, which is what GetBlobSize reported.
        if unsafe { attrs.GetBlob(&MFT_INPUT_TYPES_Attributes, &mut blob, None) }.is_err() {
            return Vec::new();
        }
        let stride = std::mem::size_of::<MFT_REGISTER_TYPE_INFO>();
        let count = blob.len() / stride;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            // SAFETY: the blob is at least `count * stride` bytes and
            // MFT_REGISTER_TYPE_INFO is two plain GUIDs, so a copy out of an
            // unaligned offset via read_unaligned is sound.
            let info: MFT_REGISTER_TYPE_INFO = unsafe {
                std::ptr::read_unaligned(blob.as_ptr().add(i * stride) as *const MFT_REGISTER_TYPE_INFO)
            };
            out.push(info.guidSubtype);
        }
        out
    }

    fn enumerate(codec: Codec, flags: MFT_ENUM_FLAG) -> Result<Vec<IMFActivate>, String> {
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: codec.subtype(),
        };
        let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: the type-info local outlives the call; `array`/`count` are live
        // out-parameters. MF allocates the array with CoTaskMemAlloc. Input type is
        // None so the enumeration is not filtered by pixel format.
        let hr = unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                flags,
                None,
                Some(&output),
                &mut array,
                &mut count,
            )
        };
        if let Err(e) = hr {
            return Err(hr_str(&e));
        }
        if array.is_null() || count == 0 {
            if !array.is_null() {
                // SAFETY: the array came from CoTaskMemAlloc inside MFTEnumEx.
                unsafe { CoTaskMemFree(Some(array as *const c_void)) };
            }
            return Ok(Vec::new());
        }
        // SAFETY: MF wrote `count` interface pointers into `array`.
        let slice = unsafe { std::slice::from_raw_parts_mut(array, count as usize) };
        // `take` moves each pointer out without an extra AddRef, so the free below
        // cannot double-release anything.
        let v: Vec<IMFActivate> = slice.iter_mut().filter_map(Option::take).collect();
        // SAFETY: as above.
        unsafe { CoTaskMemFree(Some(array as *const c_void)) };
        Ok(v)
    }

    fn describe(activate: &IMFActivate, codec: Codec, ordinal: usize) -> Entry {
        let attrs: IMFAttributes = activate.cast().expect("IMFActivate is an IMFAttributes");
        let name = allocated_string(&attrs, &MFT_FRIENDLY_NAME_Attribute)
            .unwrap_or_else(|| "<unnamed MFT>".to_owned());
        // SAFETY: `attrs` is live; a missing attribute is an error value.
        let clsid = unsafe { attrs.GetGUID(&MFT_TRANSFORM_CLSID_Attribute) }.unwrap_or_default();
        let hardware_url = allocated_string(&attrs, &MFT_ENUM_HARDWARE_URL_Attribute);
        let vendor_id = allocated_string(&attrs, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute);
        Entry {
            codec,
            ordinal,
            name,
            clsid,
            hardware: hardware_url.is_some(),
            vendor_id,
            hardware_url,
            input_subtypes: registered_input_subtypes(&attrs),
        }
    }

    fn print_entry(prefix: &str, e: &Entry) {
        println!("{prefix}{}", e.name);
        println!("{prefix}  clsid            : {}", guid_str(&e.clsid));
        println!(
            "{prefix}  hardware         : {} (MFT_ENUM_HARDWARE_URL_Attribute {})",
            if e.hardware { "YES" } else { "no" },
            e.hardware_url
                .as_deref()
                .map(|u| format!("= \"{u}\""))
                .unwrap_or_else(|| "absent".into())
        );
        println!(
            "{prefix}  vendor_id        : {}",
            e.vendor_id.as_deref().unwrap_or("<absent>")
        );
        if e.input_subtypes.is_empty() {
            println!("{prefix}  registered inputs: <none published>");
        } else {
            let names: Vec<String> = e
                .input_subtypes
                .iter()
                .map(|g| {
                    let n = subtype_name(g);
                    if is_444_input(g) {
                        format!("**{n}**")
                    } else {
                        n
                    }
                })
                .collect();
            println!("{prefix}  registered inputs: {}", names.join(", "));
            let four44: Vec<String> = e
                .input_subtypes
                .iter()
                .filter(|g| is_444_input(g))
                .map(subtype_name)
                .collect();
            println!(
                "{prefix}  4:4:4-capable in : {}",
                if four44.is_empty() {
                    "NONE".to_owned()
                } else {
                    four44.join(", ")
                }
            );
        }
    }

    /// Section 2. Returns the deduplicated candidate list section 3 will drive.
    fn inventory() -> Vec<Entry> {
        println!("=== 2. ENCODER MFT INVENTORY ===");
        println!("(inputs marked **like this** can carry 4:4:4)");
        let hw_flags = MFT_ENUM_FLAG(
            MFT_ENUM_FLAG_HARDWARE.0
                | MFT_ENUM_FLAG_ASYNCMFT.0
                | MFT_ENUM_FLAG_SYNCMFT.0
                | MFT_ENUM_FLAG_SORTANDFILTER.0,
        );
        // Candidates come from the ALL pass only, because that is the enumeration
        // `fresh_activate` re-runs and the ordinal must index into it. Every hardware
        // MFT appears in both passes, so nothing is lost.
        let mut candidates: Vec<Entry> = Vec::new();
        for codec in CODECS {
            println!();
            println!("-- output subtype {} {} --", codec.name(), guid_str(&codec.subtype()));
            for (label, flags, collect) in [
                ("HARDWARE|ASYNCMFT|SYNCMFT|SORTANDFILTER", hw_flags, false),
                ("ALL|SORTANDFILTER", all_flags(), true),
            ] {
                let _ = flags;
                match enumerate(codec, flags) {
                    Err(e) => println!("  [{label}] MFTEnumEx FAILED {e}"),
                    Ok(list) if list.is_empty() => println!("  [{label}] none"),
                    Ok(list) => {
                        println!("  [{label}] {} MFT(s)", list.len());
                        for (ordinal, a) in list.iter().enumerate() {
                            let e = describe(a, codec, ordinal);
                            print_entry("    - ", &e);
                            if !collect {
                                continue;
                            }
                            // Identity is (name, clsid): the DX12 and store-extension
                            // encoders all publish an all-zero CLSID, so a CLSID-only
                            // key silently drops every one after the first.
                            let dup = candidates
                                .iter()
                                .any(|c| c.codec == codec && c.clsid == e.clsid && c.name == e.name);
                            if !dup {
                                candidates.push(e);
                            }
                        }
                    }
                }
            }
        }
        println!();
        candidates
    }

    // -- profiles ----------------------------------------------------------------

    #[derive(Clone, Copy)]
    struct Profile {
        codec: Codec,
        /// The `codecapi.h` symbol, so a reader can cross-check the number.
        symbol: &'static str,
        value: u32,
        /// What the profile *means*, for the summary matrix.
        chroma: &'static str,
        depth: u32,
    }

    /// Every value below was read out of `~/.xwin/sdk/include/um/codecapi.h`. The
    /// symbol is printed next to the number so a wrong constant is visible rather
    /// than silently mis-labelling a result.
    const PROFILES: &[Profile] = &[
        // eAVEncH264VProfile (codecapi.h:1217)
        Profile { codec: Codec::H264, symbol: "eAVEncH264VProfile_Main",   value: 77,  chroma: "4:2:0", depth: 8 },
        Profile { codec: Codec::H264, symbol: "eAVEncH264VProfile_High",   value: 100, chroma: "4:2:0", depth: 8 },
        Profile { codec: Codec::H264, symbol: "eAVEncH264VProfile_High10", value: 110, chroma: "4:2:0", depth: 10 },
        Profile { codec: Codec::H264, symbol: "eAVEncH264VProfile_422",    value: 122, chroma: "4:2:2", depth: 10 },
        Profile { codec: Codec::H264, symbol: "eAVEncH264VProfile_444",    value: 244, chroma: "4:4:4", depth: 8 },
        // eAVEncH265VProfile (codecapi.h:1242). NOTE: MF has no Main_422_8.
        Profile { codec: Codec::Hevc, symbol: "eAVEncH265VProfile_Main_420_8",  value: 1, chroma: "4:2:0", depth: 8 },
        Profile { codec: Codec::Hevc, symbol: "eAVEncH265VProfile_Main_420_10", value: 2, chroma: "4:2:0", depth: 10 },
        Profile { codec: Codec::Hevc, symbol: "eAVEncH265VProfile_Main_420_12", value: 3, chroma: "4:2:0", depth: 12 },
        Profile { codec: Codec::Hevc, symbol: "eAVEncH265VProfile_Main_422_10", value: 4, chroma: "4:2:2", depth: 10 },
        Profile { codec: Codec::Hevc, symbol: "eAVEncH265VProfile_Main_422_12", value: 5, chroma: "4:2:2", depth: 12 },
        Profile { codec: Codec::Hevc, symbol: "eAVEncH265VProfile_Main_444_8",  value: 6, chroma: "4:4:4", depth: 8 },
        Profile { codec: Codec::Hevc, symbol: "eAVEncH265VProfile_Main_444_10", value: 7, chroma: "4:4:4", depth: 10 },
        Profile { codec: Codec::Hevc, symbol: "eAVEncH265VProfile_Main_444_12", value: 8, chroma: "4:4:4", depth: 12 },
        // eAVEncAV1VProfile (codecapi.h:1277) — sequential from 0.
        Profile { codec: Codec::Av1, symbol: "eAVEncAV1VProfile_Main_420_8",         value: 1,  chroma: "4:2:0", depth: 8 },
        Profile { codec: Codec::Av1, symbol: "eAVEncAV1VProfile_Main_420_10",        value: 2,  chroma: "4:2:0", depth: 10 },
        Profile { codec: Codec::Av1, symbol: "eAVEncAV1VProfile_High_444_8",         value: 5,  chroma: "4:4:4", depth: 8 },
        Profile { codec: Codec::Av1, symbol: "eAVEncAV1VProfile_High_444_10",        value: 6,  chroma: "4:4:4", depth: 10 },
        Profile { codec: Codec::Av1, symbol: "eAVEncAV1VProfile_Professional_422_8", value: 10, chroma: "4:2:2", depth: 8 },
        Profile { codec: Codec::Av1, symbol: "eAVEncAV1VProfile_Professional_422_10", value: 11, chroma: "4:2:2", depth: 10 },
        // eAVEncVP9VProfile (codecapi.h:1269). MF publishes no 4:4:4 symbol, so 4 and
        // 5 are unsymbolised experiments — if one is accepted, only the bitstream can
        // say what it actually selected.
        Profile { codec: Codec::Vp9, symbol: "eAVEncVP9VProfile_unknown", value: 0, chroma: "?",     depth: 0 },
        Profile { codec: Codec::Vp9, symbol: "eAVEncVP9VProfile_420_8",   value: 1, chroma: "4:2:0", depth: 8 },
        Profile { codec: Codec::Vp9, symbol: "eAVEncVP9VProfile_420_10",  value: 2, chroma: "4:2:0", depth: 10 },
        Profile { codec: Codec::Vp9, symbol: "eAVEncVP9VProfile_420_12",  value: 3, chroma: "4:2:0", depth: 12 },
        Profile { codec: Codec::Vp9, symbol: "<UNSYMBOLISED raw 4>",      value: 4, chroma: "?",     depth: 0 },
        Profile { codec: Codec::Vp9, symbol: "<UNSYMBOLISED raw 5>",      value: 5, chroma: "?",     depth: 0 },
    ];

    const SIZES: [(u32, u32); 2] = [(1920, 1080), (5120, 2880)];

    // -- media type construction -------------------------------------------------

    fn make_output_type(
        codec: Codec,
        profile: u32,
        w: u32,
        h: u32,
    ) -> Result<IMFMediaType, windows::core::Error> {
        // SAFETY: no arguments; every setter below runs on the live type.
        let t = unsafe { MFCreateMediaType() }?;
        unsafe {
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            t.SetGUID(&MF_MT_SUBTYPE, &codec.subtype())?;
            t.SetUINT64(&MF_MT_FRAME_SIZE, pack_ratio(w, h))?;
            t.SetUINT64(&MF_MT_FRAME_RATE, pack_ratio(FPS, 1))?;
            t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_ratio(1, 1))?;
            t.SetUINT32(&MF_MT_AVG_BITRATE, BITRATE_BPS)?;
            t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            t.SetUINT32(&MF_MT_MPEG2_PROFILE, profile)?;
            if codec.also_video_profile() {
                t.SetUINT32(&MF_MT_VIDEO_PROFILE, profile)?;
            }
        }
        Ok(t)
    }

    fn make_input_type(
        subtype: &GUID,
        w: u32,
        h: u32,
    ) -> Result<IMFMediaType, windows::core::Error> {
        // SAFETY: as above.
        let t = unsafe { MFCreateMediaType() }?;
        unsafe {
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
            t.SetUINT64(&MF_MT_FRAME_SIZE, pack_ratio(w, h))?;
            t.SetUINT64(&MF_MT_FRAME_RATE, pack_ratio(FPS, 1))?;
            t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_ratio(1, 1))?;
            t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            t.SetUINT32(&MF_MT_DEFAULT_STRIDE, stride_bytes(subtype, w))?;
        }
        Ok(t)
    }

    /// Bytes per row for the formats the probe feeds. Unknown formats fall back to
    /// 4 bytes per pixel, and the attempt says so rather than pretending.
    fn stride_bytes(subtype: &GUID, w: u32) -> u32 {
        if *subtype == MFVideoFormat_NV12
            || *subtype == MFVideoFormat_IYUV
            || *subtype == MFVideoFormat_I420
            || *subtype == MFVideoFormat_YV12
            || *subtype == MFVIDEOFORMAT_I444
            || *subtype == MFVIDEOFORMAT_I422
        {
            w
        } else if *subtype == MFVideoFormat_P010
            || *subtype == MFVideoFormat_P016
            || *subtype == MFVideoFormat_YUY2
            || *subtype == MFVideoFormat_UYVY
        {
            w * 2
        } else if *subtype == MFVideoFormat_RGB24 {
            w * 3
        } else if *subtype == MFVideoFormat_Y416 || *subtype == MFVideoFormat_Y216 {
            w * 8
        } else {
            // AYUV, Y410, Y210, ARGB32, RGB32 and anything unrecognised.
            w * 4
        }
    }

    /// Total bytes for one frame in `subtype`.
    fn frame_bytes(subtype: &GUID, w: u32, h: u32) -> usize {
        let stride = stride_bytes(subtype, w) as usize;
        let h = h as usize;
        if *subtype == MFVideoFormat_NV12
            || *subtype == MFVideoFormat_IYUV
            || *subtype == MFVideoFormat_I420
            || *subtype == MFVideoFormat_YV12
            || *subtype == MFVideoFormat_P010
            || *subtype == MFVideoFormat_P016
        {
            // Planar 4:2:0: luma plane plus a half-height chroma plane.
            stride * h * 3 / 2
        } else if *subtype == MFVIDEOFORMAT_I444 {
            // Three full-size planes.
            stride * h * 3
        } else if *subtype == MFVIDEOFORMAT_I422 {
            // Luma plus two half-width planes.
            stride * h * 2
        } else {
            stride * h
        }
    }

    /// Rows the CPU must fill for one frame, counting every plane. Needed separately
    /// from `frame_bytes` because a mapped D3D staging texture has a row pitch that
    /// is usually larger than the packed stride, so the copy must go row by row.
    fn frame_rows(subtype: &GUID, w: u32, h: u32) -> usize {
        let stride = stride_bytes(subtype, w).max(1) as usize;
        frame_bytes(subtype, w, h) / stride
    }

    /// A smooth moving gradient. Content does not change what the SPS says about
    /// chroma, so this only needs to be cheap, valid, and not a flat field (a flat
    /// field lets some encoders skip work and emit a suspiciously tiny stream).
    fn fill_pattern(buf: &mut [u8], stride: usize, frame: usize) {
        for (i, b) in buf.iter_mut().enumerate() {
            let x = if stride > 0 { i % stride } else { i };
            let y = if stride > 0 { i / stride } else { 0 };
            *b = (((x / 3) + (y / 3) + frame * 5) & 0xff) as u8;
        }
    }

    fn make_sample(
        subtype: &GUID,
        w: u32,
        h: u32,
        frame: usize,
    ) -> Result<IMFSample, windows::core::Error> {
        let size = frame_bytes(subtype, w, h);
        let stride = stride_bytes(subtype, w) as usize;
        // SAFETY: a plain allocation request.
        let buffer = unsafe { MFCreateMemoryBuffer(size as u32) }?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut max = 0u32;
        // SAFETY: `ptr`/`max` are live out-parameters; Unlock below is paired.
        unsafe { buffer.Lock(&mut ptr, Some(&mut max), None) }?;
        if !ptr.is_null() {
            // SAFETY: MF guarantees `ptr` is valid for `max` bytes until Unlock.
            let slice = unsafe { std::slice::from_raw_parts_mut(ptr, max as usize) };
            fill_pattern(slice, stride, frame);
        }
        // SAFETY: balances the Lock above.
        unsafe { buffer.Unlock() }?;
        // SAFETY: the buffer is live and `size` is what it was allocated with.
        unsafe { buffer.SetCurrentLength(size as u32) }?;
        // SAFETY: no arguments; the buffer is live.
        let sample = unsafe { MFCreateSample() }?;
        let duration = 10_000_000i64 / FPS as i64;
        unsafe {
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(frame as i64 * duration)?;
            sample.SetSampleDuration(duration)?;
        }
        Ok(sample)
    }

    /// The DXGI format an MF subtype maps to, for the texture route. `None` means the
    /// probe cannot build a GPU sample for that format and will say so.
    fn dxgi_format(subtype: &GUID) -> Option<DXGI_FORMAT> {
        Some(if *subtype == MFVideoFormat_NV12 {
            DXGI_FORMAT_NV12
        } else if *subtype == MFVideoFormat_P010 {
            DXGI_FORMAT_P010
        } else if *subtype == MFVideoFormat_AYUV {
            DXGI_FORMAT_AYUV
        } else if *subtype == MFVideoFormat_Y410 {
            DXGI_FORMAT_Y410
        } else if *subtype == MFVideoFormat_Y416 {
            DXGI_FORMAT_Y416
        } else if *subtype == MFVideoFormat_Y210 {
            DXGI_FORMAT_Y210
        } else if *subtype == MFVideoFormat_YUY2 {
            DXGI_FORMAT_YUY2
        } else if *subtype == MFVideoFormat_ARGB32 || *subtype == MFVideoFormat_RGB32 {
            DXGI_FORMAT_B8G8R8A8_UNORM
        } else {
            return None;
        })
    }

    fn texture_desc(format: DXGI_FORMAT, w: u32, h: u32) -> D3D11_TEXTURE2D_DESC {
        D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            ..Default::default()
        }
    }

    /// A GPU-resident input sample: fill a staging texture on the CPU, copy it to a
    /// default-usage texture, and wrap that as an `IMFSample`.
    ///
    /// This is the route a D3D12/D3D11-backed hardware encoder needs. Feeding one a
    /// system-memory buffer looks like it works — `ProcessInput` returns S_OK — and
    /// then the transform simply never asks for another frame.
    fn make_texture_sample(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        subtype: &GUID,
        w: u32,
        h: u32,
        frame: usize,
    ) -> Result<IMFSample, String> {
        let Some(format) = dxgi_format(subtype) else {
            return Err(format!(
                "no DXGI format for input subtype {}",
                subtype_name(subtype)
            ));
        };
        let stride = stride_bytes(subtype, w) as usize;
        let rows = frame_rows(subtype, w, h);

        let mut staging_desc = texture_desc(format, w, h);
        staging_desc.Usage = D3D11_USAGE_STAGING;
        staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_WRITE.0 as u32;
        let mut staging: Option<ID3D11Texture2D> = None;
        // SAFETY: the desc is a live local and `staging` is a live out-parameter.
        unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) }
            .map_err(|e| format!("CreateTexture2D(staging, {format:?}) {}", hr_str(&e)))?;
        let staging = staging.ok_or("CreateTexture2D returned no staging texture")?;

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        let staging_res: ID3D11Resource = staging.cast().map_err(|e| hr_str(&e))?;
        // SAFETY: subresource 0 is the only one; `mapped` is a live out-parameter and
        // Unmap below is paired with this call.
        unsafe { context.Map(&staging_res, 0, D3D11_MAP_WRITE, 0, Some(&mut mapped)) }
            .map_err(|e| format!("Map(staging) {}", hr_str(&e)))?;
        if !mapped.pData.is_null() {
            let pitch = mapped.RowPitch as usize;
            let mut row_buf = vec![0u8; stride];
            for row in 0..rows {
                fill_pattern_row(&mut row_buf, row, frame);
                // SAFETY: D3D guarantees `pitch * rows` bytes are mapped and writable;
                // each row write stays inside its own pitch-sized slot.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        row_buf.as_ptr(),
                        (mapped.pData as *mut u8).add(row * pitch),
                        stride.min(pitch),
                    );
                }
            }
        }
        // SAFETY: balances the Map above.
        unsafe { context.Unmap(&staging_res, 0) };

        let mut gpu_desc = texture_desc(format, w, h);
        gpu_desc.Usage = D3D11_USAGE_DEFAULT;
        // Encoders want a texture they can sample; a few formats refuse
        // RENDER_TARGET, so this walks down to bare-minimum bind flags.
        let mut gpu: Option<ID3D11Texture2D> = None;
        let mut last: Option<String> = None;
        for bind in [
            D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0,
            D3D11_BIND_SHADER_RESOURCE.0,
            D3D11_BIND_FLAG(0).0,
        ] {
            gpu_desc.BindFlags = bind as u32;
            gpu = None;
            // SAFETY: as above.
            match unsafe { device.CreateTexture2D(&gpu_desc, None, Some(&mut gpu)) } {
                Ok(()) if gpu.is_some() => break,
                Ok(()) => {}
                Err(e) => last = Some(hr_str(&e)),
            }
        }
        let gpu = gpu.ok_or_else(|| {
            format!(
                "CreateTexture2D(default, {format:?}) failed for every bind-flag combination: {}",
                last.unwrap_or_else(|| "no error reported".into())
            )
        })?;
        let gpu_res: ID3D11Resource = gpu.cast().map_err(|e| hr_str(&e))?;
        // SAFETY: both resources are live and share a description apart from usage.
        unsafe { context.CopyResource(&gpu_res, &staging_res) };

        // SAFETY: `gpu` is live; subresource 0 is the only one it has.
        let buffer = unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &gpu, 0, false) }
            .map_err(|e| format!("MFCreateDXGISurfaceBuffer {}", hr_str(&e)))?;
        // A DXGI surface buffer starts with a current length of zero, and an encoder
        // that trusts the length sees an empty frame.
        if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
            // SAFETY: `two_d` is a live view of the same buffer.
            if let Ok(length) = unsafe { two_d.GetContiguousLength() } {
                // SAFETY: as above.
                let _ = unsafe { buffer.SetCurrentLength(length) };
            }
        }
        // SAFETY: no arguments; the buffer is live.
        let sample = unsafe { MFCreateSample() }.map_err(|e| hr_str(&e))?;
        let duration = 10_000_000i64 / FPS as i64;
        // SAFETY: sample and buffer are live.
        unsafe {
            sample
                .AddBuffer(&buffer)
                .map_err(|e| format!("AddBuffer {}", hr_str(&e)))?;
            let _ = sample.SetSampleTime(frame as i64 * duration);
            let _ = sample.SetSampleDuration(duration);
        }
        Ok(sample)
    }

    /// The same gradient `fill_pattern` writes, for one row at a time.
    fn fill_pattern_row(row_buf: &mut [u8], row: usize, frame: usize) {
        for (x, b) in row_buf.iter_mut().enumerate() {
            *b = (((x / 3) + (row / 3) + frame * 5) & 0xff) as u8;
        }
    }

    // -- section 3: profile acceptance ------------------------------------------

    /// One (encoder, profile, size) acceptance result.
    struct Acceptance {
        entry_index: usize,
        profile: Profile,
        w: u32,
        h: u32,
        accepted: bool,
        /// What `GetInputAvailableType` offered once the profile was accepted.
        offered_inputs: Vec<GUID>,
    }

    /// Activate one MFT and get it as far as "ready to have types set".
    ///
    /// `use_manager` is separated out because it is the one setting that changes the
    /// answer on some drivers, and step 4 needs to be able to retry without it.
    fn activate_and_unlock(
        entry: &Entry,
        manager: Option<&IMFDXGIDeviceManager>,
        use_manager: bool,
    ) -> Result<(IMFActivate, IMFTransform, bool, bool), String> {
        // A never-used activation object every time; see `Entry::ordinal`.
        let activate = fresh_activate(entry)?;
        // SAFETY: `activate` is live.
        let transform: IMFTransform = unsafe { activate.ActivateObject() }
            .map_err(|e| format!("ActivateObject failed {}", hr_str(&e)))?;
        // SAFETY: the transform is live.
        let attrs = unsafe { transform.GetAttributes() }.ok();
        let is_async = attrs
            .as_ref()
            // SAFETY: `attrs` is live; a missing attribute is an error value.
            .and_then(|a| unsafe { a.GetUINT32(&MF_TRANSFORM_ASYNC) }.ok())
            .unwrap_or(0)
            == 1;
        if is_async {
            let Some(a) = attrs.as_ref() else {
                return Err("async MFT with no attribute store".into());
            };
            // SAFETY: `a` is live. Mandatory: an async MFT refuses every other call
            // until it is unlocked.
            if let Err(e) = unsafe { a.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) } {
                return Err(format!("MF_TRANSFORM_ASYNC_UNLOCK failed {}", hr_str(&e)));
            }
        }
        let d3d_aware = attrs
            .as_ref()
            // SAFETY: as above.
            .and_then(|a| unsafe { a.GetUINT32(&MF_SA_D3D11_AWARE) }.ok())
            .unwrap_or(0)
            != 0;
        let mut manager_set = false;
        if d3d_aware && use_manager {
            if let Some(m) = manager {
                let raw = m.as_raw() as usize;
                // SAFETY: the manager outlives this transform, which is what
                // MFT_MESSAGE_SET_D3D_MANAGER requires of the pointer.
                if unsafe { transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, raw) }.is_ok() {
                    manager_set = true;
                }
            }
        }
        Ok((activate, transform, is_async, manager_set))
    }

    fn offered_input_types(transform: &IMFTransform) -> (Vec<GUID>, Option<String>) {
        let mut out = Vec::new();
        let mut note = None;
        for i in 0..64u32 {
            // SAFETY: the transform is live; MF_E_NO_MORE_TYPES ends the walk.
            match unsafe { transform.GetInputAvailableType(0, i) } {
                Ok(t) => {
                    // SAFETY: `t` is live.
                    if let Ok(g) = unsafe { t.GetGUID(&MF_MT_SUBTYPE) } {
                        out.push(g);
                    }
                }
                Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
                Err(e) => {
                    note = Some(format!("GetInputAvailableType({i}) -> {}", hr_str(&e)));
                    break;
                }
            }
        }
        (out, note)
    }

    fn acceptance_pass(
        entries: &[Entry],
        drive: &[usize],
        manager: Option<&IMFDXGIDeviceManager>,
    ) -> Vec<Acceptance> {
        println!("=== 3. PROFILE ACCEPTANCE (SetOutputType) ===");
        let mut results = Vec::new();
        for &idx in drive {
            let entry = &entries[idx];
            println!();
            println!(
                "-- {} [{}] {} --",
                entry.name,
                if entry.hardware { "hardware" } else { "software" },
                entry.codec.name()
            );
            for profile in PROFILES.iter().filter(|p| p.codec == entry.codec) {
                for (w, h) in SIZES {
                    let (accepted, detail, offered) =
                        try_profile(entry, *profile, w, h, manager);
                    println!(
                        "   {:>4}x{:<4}  {:<40} value={:<3} {:<5} {:>2}-bit  {}",
                        w,
                        h,
                        profile.symbol,
                        profile.value,
                        profile.chroma,
                        profile.depth,
                        detail
                    );
                    if accepted && !offered.is_empty() {
                        let names: Vec<String> = offered
                            .iter()
                            .map(|g| {
                                let n = subtype_name(g);
                                if is_444_input(g) {
                                    format!("**{n}**")
                                } else {
                                    n
                                }
                            })
                            .collect();
                        println!("                 offered inputs: {}", names.join(", "));
                    }
                    results.push(Acceptance {
                        entry_index: idx,
                        profile: *profile,
                        w,
                        h,
                        accepted,
                        offered_inputs: offered,
                    });
                }
            }
        }
        println!();
        results
    }

    fn try_profile(
        entry: &Entry,
        profile: Profile,
        w: u32,
        h: u32,
        manager: Option<&IMFDXGIDeviceManager>,
    ) -> (bool, String, Vec<GUID>) {
        let (activate, transform, _is_async, manager_set) =
            match activate_and_unlock(entry, manager, true) {
                Ok(v) => v,
                Err(e) => return (false, format!("FAILED at activation: {e}"), Vec::new()),
            };
        let result = (|| {
            let t = match make_output_type(entry.codec, profile.value, w, h) {
                Ok(t) => t,
                Err(e) => return (false, format!("FAILED building type {}", hr_str(&e)), Vec::new()),
            };
            // SAFETY: both interfaces are live.
            match unsafe { transform.SetOutputType(0, &t, 0) } {
                Ok(()) => {
                    let (offered, note) = offered_input_types(&transform);
                    let mut d = format!(
                        "ACCEPTED (S_OK){}",
                        if manager_set { " [d3d manager set]" } else { "" }
                    );
                    if let Some(n) = note {
                        d.push_str(&format!(" [{n}]"));
                    }
                    (true, d, offered)
                }
                Err(e) => (false, format!("refused {}", hr_str(&e)), Vec::new()),
            }
        })();
        // SAFETY: the activate object owns the transform. This poisons `activate` for
        // any further ActivateObject, which is fine — it is discarded here and the
        // next attempt re-enumerates a fresh one.
        let _ = unsafe { activate.ShutdownObject() };
        drop(transform);
        result
    }

    // -- section 4: real encode --------------------------------------------------

    struct EncodeResult {
        frames_in: usize,
        frames_out: usize,
        total_bytes: usize,
        mean_ms: f64,
        max_ms: f64,
        first_sample: Vec<u8>,
        stream: Vec<u8>,
        sequence_header: Option<Vec<u8>>,
        route: String,
        error: Option<String>,
    }

    struct OutBlob {
        bytes: Vec<u8>,
        latency_ms: f64,
    }

    /// One `ProcessOutput`. `Ok(None)` means "nothing right now".
    fn process_output(
        transform: &IMFTransform,
        provides_samples: bool,
        output_size: u32,
        submit_times: &[Instant],
        duration_hns: i64,
        collected: &mut Vec<OutBlob>,
    ) -> Result<bool, String> {
        let mut sample_in: Option<IMFSample> = None;
        if !provides_samples {
            // SAFETY: a plain allocation, then a fresh sample that owns it.
            let buffer = unsafe { MFCreateMemoryBuffer(output_size.max(1 << 20)) }
                .map_err(|e| format!("MFCreateMemoryBuffer {}", hr_str(&e)))?;
            let sample = unsafe { MFCreateSample() }
                .map_err(|e| format!("MFCreateSample {}", hr_str(&e)))?;
            unsafe { sample.AddBuffer(&buffer) }
                .map_err(|e| format!("AddBuffer {}", hr_str(&e)))?;
            sample_in = Some(sample);
        }
        let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(sample_in),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        // SAFETY: `buffers` and `status` are live locals for the call.
        let outcome = unsafe { transform.ProcessOutput(0, &mut buffers, &mut status) };
        let now = Instant::now();
        // Reclaim whatever the struct holds — our sample on the sync path, the MFT's
        // on the async path. Either way it is ours to release.
        let produced = std::mem::ManuallyDrop::into_inner(std::mem::replace(
            &mut buffers[0].pSample,
            std::mem::ManuallyDrop::new(None),
        ));
        let events = std::mem::ManuallyDrop::into_inner(std::mem::replace(
            &mut buffers[0].pEvents,
            std::mem::ManuallyDrop::new(None),
        ));
        drop(events);

        match outcome {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(false),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // SAFETY: the transform is live.
                let new_type = unsafe { transform.GetOutputAvailableType(0, 0) }
                    .map_err(|e| format!("stream change, GetOutputAvailableType {}", hr_str(&e)))?;
                unsafe { transform.SetOutputType(0, &new_type, 0) }
                    .map_err(|e| format!("stream change, SetOutputType {}", hr_str(&e)))?;
                return Ok(false);
            }
            Err(e) => return Err(format!("ProcessOutput {}", hr_str(&e))),
        }
        let Some(sample) = produced else {
            return Ok(false);
        };
        // Pair the output back to its submission by sample time — the encoder is
        // required to preserve it. Index order is the fallback.
        // SAFETY: `sample` is live.
        let idx = unsafe { sample.GetSampleTime() }
            .ok()
            .filter(|_| duration_hns > 0)
            .map(|t| (t / duration_hns) as usize)
            .filter(|i| *i < submit_times.len())
            .unwrap_or_else(|| collected.len().min(submit_times.len().saturating_sub(1)));
        let latency_ms = submit_times
            .get(idx)
            .map(|s| now.duration_since(*s).as_secs_f64() * 1000.0)
            .unwrap_or(f64::NAN);

        // SAFETY: the sample is live; Lock/Unlock are paired below.
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }
            .map_err(|e| format!("ConvertToContiguousBuffer {}", hr_str(&e)))?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut current = 0u32;
        unsafe { buffer.Lock(&mut ptr, None, Some(&mut current)) }
            .map_err(|e| format!("Lock {}", hr_str(&e)))?;
        let mut bytes = Vec::new();
        if !ptr.is_null() && current > 0 {
            // SAFETY: MF guarantees `ptr` is valid for `current` bytes until Unlock.
            bytes.extend_from_slice(unsafe { std::slice::from_raw_parts(ptr, current as usize) });
        }
        // SAFETY: balances the Lock above; must run even if the copy was empty.
        let _ = unsafe { buffer.Unlock() };
        collected.push(OutBlob { bytes, latency_ms });
        Ok(true)
    }

    fn read_sequence_header(transform: &IMFTransform) -> Option<Vec<u8>> {
        // SAFETY: the transform is live.
        let t = unsafe { transform.GetOutputCurrentType(0) }.ok()?;
        // SAFETY: `t` is live; a missing attribute is an error, not a write.
        let size = unsafe { t.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) }.ok()?;
        if size == 0 {
            return None;
        }
        let mut blob = vec![0u8; size as usize];
        // SAFETY: `blob` is exactly `size` bytes.
        unsafe { t.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None) }.ok()?;
        Some(blob)
    }

    /// One full encode attempt on one route. `use_manager == false` is the retry for
    /// a driver that refuses system-memory input once a D3D manager is attached.
    #[allow(clippy::too_many_arguments)]
    fn encode_once(
        entry: &Entry,
        profile: Profile,
        input_subtype: &GUID,
        w: u32,
        h: u32,
        manager: Option<&IMFDXGIDeviceManager>,
        use_manager: bool,
        gpu: Option<(&ID3D11Device, &ID3D11DeviceContext)>,
    ) -> EncodeResult {
        let mut r = EncodeResult {
            frames_in: 0,
            frames_out: 0,
            total_bytes: 0,
            mean_ms: f64::NAN,
            max_ms: f64::NAN,
            first_sample: Vec::new(),
            stream: Vec::new(),
            sequence_header: None,
            route: String::new(),
            error: None,
        };
        let (activate, transform, is_async, manager_set) =
            match activate_and_unlock(entry, manager, use_manager) {
                Ok(v) => v,
                Err(e) => {
                    r.error = Some(format!("activation: {e}"));
                    return r;
                }
            };
        r.route = format!(
            "{}, {} d3d manager, {} samples",
            if is_async { "async" } else { "sync" },
            if manager_set { "with" } else { "without" },
            if gpu.is_some() {
                "D3D11 texture"
            } else {
                "system-memory"
            }
        );

        let outcome = (|| -> Result<(), String> {
            // ICodecAPI low-latency settings must precede the output type on several
            // encoders, so they go first even though a refusal is not fatal.
            let codec_api = transform.cast::<ICodecAPI>().ok();
            if let Some(api) = codec_api.as_ref() {
                // SAFETY: both pointers are live locals; refusals are expected and
                // deliberately ignored — this is a capability probe, not a pipeline.
                unsafe {
                    let _ = api.SetValue(&CODECAPI_AVLowLatencyMode, &variant_bool(true));
                    let _ = api.SetValue(
                        &CODECAPI_AVEncCommonRateControlMode,
                        &variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32),
                    );
                    let _ = api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(BITRATE_BPS));
                }
            }
            let out_type = make_output_type(entry.codec, profile.value, w, h)
                .map_err(|e| format!("building output type {}", hr_str(&e)))?;
            // SAFETY: both interfaces are live.
            unsafe { transform.SetOutputType(0, &out_type, 0) }
                .map_err(|e| format!("SetOutputType {}", hr_str(&e)))?;
            let in_type = make_input_type(input_subtype, w, h)
                .map_err(|e| format!("building input type {}", hr_str(&e)))?;
            // SAFETY: as above.
            unsafe { transform.SetInputType(0, &in_type, 0) }
                .map_err(|e| format!("SetInputType({}) {}", subtype_name(input_subtype), hr_str(&e)))?;
            if let Some(api) = codec_api.as_ref() {
                // SAFETY: as above; several encoders only honour these once the types
                // are locked in.
                unsafe {
                    let _ = api.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &variant_u32(0));
                    let _ = api.SetValue(&CODECAPI_AVEncMPVGOPSize, &variant_u32(GOP));
                }
            }
            // SAFETY: the transform is live.
            let info = unsafe { transform.GetOutputStreamInfo(0) }
                .map_err(|e| format!("GetOutputStreamInfo {}", hr_str(&e)))?;
            let provides_samples =
                info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
            r.sequence_header = read_sequence_header(&transform);

            // SAFETY: the transform is live.
            unsafe {
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                    .map_err(|e| format!("BEGIN_STREAMING {}", hr_str(&e)))?;
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                    .map_err(|e| format!("START_OF_STREAM {}", hr_str(&e)))?;
            }

            let duration_hns = 10_000_000i64 / FPS as i64;
            let events = if is_async {
                Some(
                    transform
                        .cast::<IMFMediaEventGenerator>()
                        .map_err(|e| format!("async MFT has no event generator {}", hr_str(&e)))?,
                )
            } else {
                None
            };
            let mut submit_times: Vec<Instant> = Vec::with_capacity(FRAMES);
            let mut collected: Vec<OutBlob> = Vec::new();

            for frame in 0..FRAMES {
                let sample = match gpu {
                    Some((device, context)) => {
                        make_texture_sample(device, context, input_subtype, w, h, frame)
                            .map_err(|e| format!("building texture sample: {e}"))?
                    }
                    None => make_sample(input_subtype, w, h, frame)
                        .map_err(|e| format!("building sample {}", hr_str(&e)))?,
                };
                if let Some(ev) = events.as_ref() {
                    // Wait, bounded, for one METransformNeedInput credit, servicing
                    // METransformHaveOutput while we wait — an encoder with both work
                    // to give and room to take must not deadlock.
                    let deadline = Instant::now() + WAIT_LIMIT;
                    let mut credited = false;
                    while Instant::now() < deadline {
                        // SAFETY: `ev` is live; NO_WAIT makes this a poll.
                        match unsafe { ev.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                            Ok(event) => {
                                // SAFETY: `event` is live.
                                let kind = MF_EVENT_TYPE(
                                    unsafe { event.GetType() }.unwrap_or(0) as i32,
                                );
                                if kind == METransformNeedInput {
                                    credited = true;
                                    break;
                                } else if kind == METransformHaveOutput {
                                    process_output(
                                        &transform,
                                        provides_samples,
                                        info.cbSize,
                                        &submit_times,
                                        duration_hns,
                                        &mut collected,
                                    )?;
                                }
                            }
                            Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                                std::thread::sleep(Duration::from_millis(1))
                            }
                            Err(e) => return Err(format!("GetEvent {}", hr_str(&e))),
                        }
                    }
                    if !credited {
                        return Err(format!(
                            "timed out after {:?} waiting for METransformNeedInput at frame {frame}",
                            WAIT_LIMIT
                        ));
                    }
                }
                let t0 = Instant::now();
                // SAFETY: transform and sample are live.
                unsafe { transform.ProcessInput(0, &sample, 0) }
                    .map_err(|e| format!("ProcessInput frame {frame} {}", hr_str(&e)))?;
                submit_times.push(t0);
                r.frames_in += 1;
                if events.is_none() {
                    // A sync MFT is drained by polling until it needs more input.
                    let deadline = Instant::now() + WAIT_LIMIT;
                    while Instant::now() < deadline {
                        if !process_output(
                            &transform,
                            provides_samples,
                            info.cbSize,
                            &submit_times,
                            duration_hns,
                            &mut collected,
                        )? {
                            break;
                        }
                    }
                }
            }

            // Drain.
            // SAFETY: the transform is live.
            unsafe {
                let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
                let _ = transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
            }
            let deadline = Instant::now() + WAIT_LIMIT;
            if let Some(ev) = events.as_ref() {
                while Instant::now() < deadline {
                    // SAFETY: `ev` is live.
                    match unsafe { ev.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                        Ok(event) => {
                            // SAFETY: `event` is live.
                            let kind =
                                MF_EVENT_TYPE(unsafe { event.GetType() }.unwrap_or(0) as i32);
                            if kind == METransformDrainComplete {
                                break;
                            }
                            if kind == METransformHaveOutput {
                                process_output(
                                    &transform,
                                    provides_samples,
                                    info.cbSize,
                                    &submit_times,
                                    duration_hns,
                                    &mut collected,
                                )?;
                            }
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(1)),
                    }
                }
            } else {
                while Instant::now() < deadline {
                    if !process_output(
                        &transform,
                        provides_samples,
                        info.cbSize,
                        &submit_times,
                        duration_hns,
                        &mut collected,
                    )? {
                        break;
                    }
                }
            }
            // SAFETY: the transform is live; a failure here is unactionable.
            unsafe {
                let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            }
            // The sequence header is only published once the encoder has settled on
            // an output type, so re-read it after the run if it was absent before.
            if r.sequence_header.is_none() {
                r.sequence_header = read_sequence_header(&transform);
            }

            r.frames_out = collected.len();
            r.total_bytes = collected.iter().map(|c| c.bytes.len()).sum();
            let lats: Vec<f64> = collected
                .iter()
                .map(|c| c.latency_ms)
                .filter(|v| v.is_finite())
                .collect();
            if !lats.is_empty() {
                r.mean_ms = lats.iter().sum::<f64>() / lats.len() as f64;
                r.max_ms = lats.iter().cloned().fold(f64::MIN, f64::max);
            }
            if let Some(first) = collected.first() {
                r.first_sample = first.bytes.clone();
            }
            for c in &collected {
                r.stream.extend_from_slice(&c.bytes);
            }
            Ok(())
        })();

        if let Err(e) = outcome {
            r.error = Some(e);
        }
        // SAFETY: releases the transform. `activate` is discarded with it; the next
        // attempt re-enumerates.
        let _ = unsafe { activate.ShutdownObject() };
        drop(transform);
        r
    }

    /// Pick the input format that best matches what the profile is meant to carry,
    /// preferring something the encoder actually offered.
    fn choose_input(profile: Profile, offered: &[GUID], registered: &[GUID]) -> Option<GUID> {
        let wanted: &[GUID] = match (profile.chroma, profile.depth) {
            ("4:4:4", 8) => &[
                MFVideoFormat_AYUV,
                MFVIDEOFORMAT_I444,
                MFVideoFormat_ARGB32,
                MFVideoFormat_RGB32,
            ],
            ("4:4:4", _) => &[MFVideoFormat_Y410, MFVideoFormat_Y416, MFVideoFormat_AYUV],
            ("4:2:2", 8) => &[MFVideoFormat_YUY2, MFVIDEOFORMAT_I422, MFVideoFormat_UYVY],
            ("4:2:2", _) => &[MFVideoFormat_Y210, MFVideoFormat_YUY2],
            (_, 8) => &[MFVideoFormat_NV12, MFVideoFormat_IYUV, MFVideoFormat_YV12],
            (_, 0) => &[MFVideoFormat_NV12],
            (_, _) => &[MFVideoFormat_P010, MFVideoFormat_P016, MFVideoFormat_NV12],
        };
        for w in wanted {
            if offered.contains(w) {
                return Some(*w);
            }
        }
        for w in wanted {
            if registered.contains(w) {
                return Some(*w);
            }
        }
        // Nothing preferred is available; take the first thing the encoder offered
        // rather than skipping the attempt, and let the caller print what was used.
        offered.first().or_else(|| registered.first()).copied()
    }

    struct Row {
        /// Rows are per ENCODER, not per codec. Merging them hides the finding this
        /// probe exists to surface: one encoder emitting real 4:4:4 while another,
        /// on the same box and the same profile value, silently clamps to 4:2:0.
        encoder: String,
        hardware: bool,
        codec: Codec,
        chroma: &'static str,
        depth: u32,
        symbol: &'static str,
        accepted_1080: bool,
        accepted_5k: bool,
        encoded_1080: bool,
        encoded_5k: bool,
        bitstream: String,
    }

    fn encode_pass(
        entries: &[Entry],
        acceptances: &[Acceptance],
        manager: Option<&IMFDXGIDeviceManager>,
        gpu: Option<(&ID3D11Device, &ID3D11DeviceContext)>,
        rows: &mut Vec<Row>,
    ) {
        println!("=== 4. REAL ENCODE ({FRAMES} synthetic frames, cold, n={FRAMES}) ===");
        println!(
            "(latency is submit->output per frame from a cold start; CBR {} Mbit, GOP {GOP}, \
             low-latency requested)",
            BITRATE_BPS / 1_000_000
        );
        let mut attempts = 0usize;
        for acc in acceptances.iter().filter(|a| a.accepted) {
            let entry = &entries[acc.entry_index];
            // 5K attempts are limited to H.264/HEVC, which is where the ceiling
            // question actually bites.
            if acc.w > 1920 && !matches!(entry.codec, Codec::H264 | Codec::Hevc) {
                continue;
            }
            if attempts >= MAX_ENCODE_ATTEMPTS {
                println!("   (attempt cap {MAX_ENCODE_ATTEMPTS} reached; remaining combos skipped)");
                break;
            }
            let Some(input) = choose_input(acc.profile, &acc.offered_inputs, &entry.input_subtypes)
            else {
                println!();
                println!(
                    "-- {} {} {} {}x{} : SKIPPED, no usable input subtype",
                    entry.name, entry.codec.name(), acc.profile.symbol, acc.w, acc.h
                );
                continue;
            };
            attempts += 1;
            println!();
            println!(
                "-- {} | {} {} (value={}) | {}x{} | input {} --",
                entry.name,
                entry.codec.name(),
                acc.profile.symbol,
                acc.profile.value,
                acc.w,
                acc.h,
                subtype_name(&input)
            );
            // Three routes, cheapest first. B exists because a D3D12-backed encoder
            // takes a system-memory sample without complaint and then never asks for
            // another; C because some drivers refuse system memory only once a
            // device manager is attached.
            let routes: [(&str, bool, bool); 3] = [
                ("A", true, false),  // manager (if aware), system-memory samples
                ("B", true, true),   // manager, D3D11 texture samples
                ("C", false, false), // no manager, system-memory samples
            ];
            let mut res: Option<EncodeResult> = None;
            for (label, use_manager, use_textures) in routes {
                if use_textures && gpu.is_none() {
                    println!("   route {label} -> skipped, no D3D11 device on this host");
                    continue;
                }
                let attempt = encode_once(
                    entry,
                    acc.profile,
                    &input,
                    acc.w,
                    acc.h,
                    manager,
                    use_manager,
                    if use_textures { gpu } else { None },
                );
                if attempt.error.is_none() && attempt.frames_out > 0 {
                    println!("   route {label} ({}) -> OK", attempt.route);
                    res = Some(attempt);
                    break;
                }
                println!(
                    "   route {label} ({}) -> FAILED: {}",
                    attempt.route,
                    attempt
                        .error
                        .clone()
                        .unwrap_or_else(|| "no output samples".into())
                );
            }
            let Some(res) = res else {
                record_row(rows, entry, acc, false, String::new());
                continue;
            };
            println!("   route            : {}", res.route);
            println!(
                "   frames_in/out    : {}/{}   total_bytes={}   mean_latency={:.2} ms  max={:.2} ms",
                res.frames_in, res.frames_out, res.total_bytes, res.mean_ms, res.max_ms
            );
            let ev: ChromaEvidence = entry.codec.parse(&res.stream);
            println!(
                "   bitstream says   : {}{}",
                if ev.reading.is_empty() {
                    "<unparsed>".to_owned()
                } else {
                    ev.reading.clone()
                },
                if ev.note.is_empty() {
                    String::new()
                } else {
                    format!("   [note: {}]", ev.note)
                }
            );
            println!(
                "   parsed fields    : profile_idc={:?} chroma_format_idc={:?} bit_depth={:?}",
                ev.profile_idc, ev.chroma_format_idc, ev.bit_depth
            );
            println!("   first sample, first 48 bytes:");
            print!("{}", hex_dump(&res.first_sample, 48));
            match res.sequence_header.as_ref() {
                Some(blob) => {
                    println!("   MF_MT_MPEG_SEQUENCE_HEADER ({} bytes):", blob.len());
                    print!("{}", hex_dump(blob, 64));
                }
                None => println!("   MF_MT_MPEG_SEQUENCE_HEADER: <not published>"),
            }
            let reading = if ev.reading.is_empty() {
                ev.note.clone()
            } else {
                ev.reading.clone()
            };
            record_row(rows, entry, acc, true, reading);
        }
        if attempts == 0 {
            println!("   (no accepted profile produced an encode attempt)");
        }
        println!();
    }

    fn record_row(
        rows: &mut Vec<Row>,
        entry: &Entry,
        acc: &Acceptance,
        encoded: bool,
        bitstream: String,
    ) {
        let is_5k = acc.w > 1920;
        if let Some(row) = rows
            .iter_mut()
            .find(|r| r.encoder == entry.name && r.codec == entry.codec && r.symbol == acc.profile.symbol)
        {
            if is_5k {
                row.encoded_5k |= encoded;
            } else {
                row.encoded_1080 |= encoded;
            }
            if !bitstream.is_empty() && (row.bitstream.is_empty() || !is_5k) {
                row.bitstream = bitstream;
            }
            return;
        }
        rows.push(Row {
            encoder: entry.name.clone(),
            hardware: entry.hardware,
            codec: entry.codec,
            chroma: acc.profile.chroma,
            depth: acc.profile.depth,
            symbol: acc.profile.symbol,
            accepted_1080: false,
            accepted_5k: false,
            encoded_1080: encoded && !is_5k,
            encoded_5k: encoded && is_5k,
            bitstream,
        });
    }

    // -- section 5: summary ------------------------------------------------------

    fn summary(entries: &[Entry], acceptances: &[Acceptance], rows: &mut Vec<Row>) {
        // Fold acceptance into the rows first, so a profile that was accepted but
        // never encoded still gets a line.
        for acc in acceptances {
            let entry = &entries[acc.entry_index];
            let is_5k = acc.w > 1920;
            if let Some(row) = rows.iter_mut().find(|r| {
                r.encoder == entry.name && r.codec == entry.codec && r.symbol == acc.profile.symbol
            }) {
                if acc.accepted {
                    if is_5k {
                        row.accepted_5k = true;
                    } else {
                        row.accepted_1080 = true;
                    }
                }
            } else {
                rows.push(Row {
                    encoder: entry.name.clone(),
                    hardware: entry.hardware,
                    codec: entry.codec,
                    chroma: acc.profile.chroma,
                    depth: acc.profile.depth,
                    symbol: acc.profile.symbol,
                    accepted_1080: acc.accepted && !is_5k,
                    accepted_5k: acc.accepted && is_5k,
                    encoded_1080: false,
                    encoded_5k: false,
                    bitstream: String::new(),
                });
            }
        }

        println!("=== 5. SUMMARY MATRIX ===");
        println!(
            "{:<44} {:<6} {:<6} {:<6} {:<38} {:<8} {:<18} {:<16} {}",
            "encoder", "codec", "chroma", "depth", "profile symbol", "hw enc", "accepted 1080/5K", "encoded 1080/5K", "bitstream says"
        );
        println!("{}", "-".repeat(210));
        for row in rows.iter() {
            println!(
                "{:<44} {:<6} {:<6} {:<6} {:<38} {:<8} {:<18} {:<16} {}",
                row.encoder,
                row.codec.name(),
                row.chroma,
                if row.depth == 0 {
                    "?".to_owned()
                } else {
                    row.depth.to_string()
                },
                row.symbol,
                if row.hardware { "yes" } else { "no" },
                format!(
                    "{} / {}",
                    yn(row.accepted_1080),
                    yn(row.accepted_5k)
                ),
                format!("{} / {}", yn(row.encoded_1080), yn(row.encoded_5k)),
                if row.bitstream.is_empty() {
                    "-".to_owned()
                } else {
                    row.bitstream.clone()
                }
            );
        }
        println!();
    }

    fn yn(v: bool) -> &'static str {
        if v {
            "yes"
        } else {
            "no"
        }
    }

    // -- section 6: D3D12 Video Encode capabilities ------------------------------
    //
    // Sections 2-4 ask what the *MFTs* will do. This section asks the API those MFTs
    // wrap. It closes the hole section 4 leaves: the Microsoft DX12 encoder MFTs
    // accept 4:4:4 output types and then never encode a frame under any route the
    // probe can drive, so their acceptance proves nothing on its own. `CheckFeatureSupport`
    // answers the same question without an encode session — and it also distinguishes
    // "no AV1 MFT is registered" from "this silicon cannot encode AV1", which the MFT
    // enumeration alone cannot.
    //
    // Every value below is read from `~/.xwin/sdk/include/um/d3d12video.h`, cited by
    // line, and printed with its symbolic name so a wrong constant is visible rather
    // than silently mislabelling a cell.

    /// D3D12_VIDEO_ENCODER_CODEC (d3d12video.h:6942).
    const D3D12_CODECS: [(D3D12_VIDEO_ENCODER_CODEC, &str); 3] = [
        (D3D12_VIDEO_ENCODER_CODEC_H264, "H264"),
        (D3D12_VIDEO_ENCODER_CODEC_HEVC, "HEVC"),
        (D3D12_VIDEO_ENCODER_CODEC_AV1, "AV1"),
    ];

    /// D3D12_VIDEO_ENCODER_PROFILE_H264 (d3d12video.h:6957).
    const D3D12_H264_PROFILES: [(i32, &str); 3] = [
        (0, "H264_MAIN"),
        (1, "H264_HIGH"),
        (2, "H264_HIGH_10"),
    ];

    /// D3D12_VIDEO_ENCODER_PROFILE_HEVC (d3d12video.h:6965) — the full enum.
    const D3D12_HEVC_PROFILES: [(i32, &str); 9] = [
        (0, "HEVC_MAIN"),
        (1, "HEVC_MAIN10"),
        (2, "HEVC_MAIN12"),
        (3, "HEVC_MAIN10_422"),
        (4, "HEVC_MAIN12_422"),
        (5, "HEVC_MAIN_444"),
        (6, "HEVC_MAIN10_444"),
        (7, "HEVC_MAIN12_444"),
        (8, "HEVC_MAIN16_444"),
    ];

    /// D3D12_VIDEO_ENCODER_AV1_PROFILE (d3d12video.h:6342).
    const D3D12_AV1_PROFILES: [(i32, &str); 3] = [
        (0, "AV1_MAIN"),
        (1, "AV1_HIGH"),
        (2, "AV1_PROFESSIONAL"),
    ];

    fn d3d12_profiles(codec: D3D12_VIDEO_ENCODER_CODEC) -> &'static [(i32, &'static str)] {
        if codec == D3D12_VIDEO_ENCODER_CODEC_H264 {
            &D3D12_H264_PROFILES
        } else if codec == D3D12_VIDEO_ENCODER_CODEC_HEVC {
            &D3D12_HEVC_PROFILES
        } else {
            &D3D12_AV1_PROFILES
        }
    }

    /// The input formats worth asking about. The 4:4:4 ones against `HEVC_MAIN_444`
    /// are the decisive cells.
    const D3D12_INPUT_FORMATS: [(DXGI_FORMAT, &str); 9] = [
        (DXGI_FORMAT_NV12, "NV12"),
        (DXGI_FORMAT_P010, "P010"),
        (DXGI_FORMAT_YUY2, "YUY2"),
        (DXGI_FORMAT_Y210, "Y210"),
        (DXGI_FORMAT_AYUV, "AYUV*"),
        (DXGI_FORMAT_Y410, "Y410*"),
        (DXGI_FORMAT_Y416, "Y416*"),
        (DXGI_FORMAT_R8G8B8A8_UNORM, "RGBA8*"),
        (DXGI_FORMAT_B8G8R8A8_UNORM, "BGRA8*"),
    ];

    /// Build a `D3D12_VIDEO_ENCODER_PROFILE_DESC` pointing at `slot`.
    ///
    /// The union holds a *pointer* to the profile value and `DataSize` is the size of
    /// the pointee, not of the union. Getting that backwards is the usual way these
    /// calls return `E_INVALIDARG` for a perfectly supported profile.
    fn profile_desc(
        codec: D3D12_VIDEO_ENCODER_CODEC,
        slot: &mut i32,
    ) -> D3D12_VIDEO_ENCODER_PROFILE_DESC {
        let mut desc = D3D12_VIDEO_ENCODER_PROFILE_DESC {
            DataSize: std::mem::size_of::<i32>() as u32,
            ..Default::default()
        };
        let p = slot as *mut i32;
        // All three union arms are pointer-sized and point at the same live `i32`
        // slot; the profile enums are all `#[repr(transparent)] i32`.
        if codec == D3D12_VIDEO_ENCODER_CODEC_H264 {
            desc.Anonymous.pH264Profile = p as *mut D3D12_VIDEO_ENCODER_PROFILE_H264;
        } else if codec == D3D12_VIDEO_ENCODER_CODEC_HEVC {
            desc.Anonymous.pHEVCProfile = p as *mut D3D12_VIDEO_ENCODER_PROFILE_HEVC;
        } else {
            desc.Anonymous.pAV1Profile = p as *mut D3D12_VIDEO_ENCODER_AV1_PROFILE;
        }
        desc
    }

    /// Storage a driver can write a level into, sized per codec. H.264 levels are a
    /// bare enum; HEVC and AV1 carry a level *and* a tier.
    #[derive(Default, Clone, Copy)]
    struct LevelSlot {
        h264: i32,
        hevc: [i32; 2],
        av1: [i32; 2],
    }

    fn level_setting(
        codec: D3D12_VIDEO_ENCODER_CODEC,
        slot: &mut LevelSlot,
    ) -> D3D12_VIDEO_ENCODER_LEVEL_SETTING {
        let mut setting = D3D12_VIDEO_ENCODER_LEVEL_SETTING::default();
        // Each arm points at the matching live field of `slot`, and DataSize is the
        // size of that pointee — not of the union.
        if codec == D3D12_VIDEO_ENCODER_CODEC_H264 {
            setting.DataSize = std::mem::size_of::<i32>() as u32;
            setting.Anonymous.pH264LevelSetting =
                &mut slot.h264 as *mut i32 as *mut D3D12_VIDEO_ENCODER_LEVELS_H264;
        } else if codec == D3D12_VIDEO_ENCODER_CODEC_HEVC {
            setting.DataSize =
                std::mem::size_of::<D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC>() as u32;
            setting.Anonymous.pHEVCLevelSetting =
                slot.hevc.as_mut_ptr() as *mut D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC;
        } else {
            setting.DataSize =
                std::mem::size_of::<D3D12_VIDEO_ENCODER_AV1_LEVEL_TIER_CONSTRAINTS>() as u32;
            setting.Anonymous.pAV1LevelSetting =
                slot.av1.as_mut_ptr() as *mut D3D12_VIDEO_ENCODER_AV1_LEVEL_TIER_CONSTRAINTS;
        }
        setting
    }

    fn level_text(codec: D3D12_VIDEO_ENCODER_CODEC, slot: &LevelSlot) -> String {
        if codec == D3D12_VIDEO_ENCODER_CODEC_H264 {
            format!("Level={}", slot.h264)
        } else if codec == D3D12_VIDEO_ENCODER_CODEC_HEVC {
            format!("Level={} Tier={}", slot.hevc[0], slot.hevc[1])
        } else {
            format!("Level={} Tier={}", slot.av1[0], slot.av1[1])
        }
    }

    fn support_flag_names(flags: i32) -> String {
        const NAMES: [(i32, &str); 16] = [
            (1, "GENERAL_SUPPORT_OK"),
            (2, "RATE_CONTROL_RECONFIGURATION_AVAILABLE"),
            (4, "RESOLUTION_RECONFIGURATION_AVAILABLE"),
            (8, "RATE_CONTROL_VBV_SIZE_CONFIG_AVAILABLE"),
            (16, "RATE_CONTROL_FRAME_ANALYSIS_AVAILABLE"),
            (32, "RECONSTRUCTED_FRAMES_REQUIRE_TEXTURE_ARRAYS"),
            (64, "RATE_CONTROL_DELTA_QP_AVAILABLE"),
            (128, "SUBREGION_LAYOUT_RECONFIGURATION_AVAILABLE"),
            (256, "RATE_CONTROL_ADJUSTABLE_QP_RANGE_AVAILABLE"),
            (512, "RATE_CONTROL_INITIAL_QP_AVAILABLE"),
            (1024, "RATE_CONTROL_MAX_FRAME_SIZE_AVAILABLE"),
            (2048, "SEQUENCE_GOP_RECONFIGURATION_AVAILABLE"),
            (4096, "MOTION_ESTIMATION_PRECISION_MODE_LIMIT_AVAILABLE"),
            (8192, "RATE_CONTROL_EXTENSION1_SUPPORT"),
            (16384, "RATE_CONTROL_QUALITY_VS_SPEED_AVAILABLE"),
            (32768, "READABLE_RECONSTRUCTED_PICTURE_LAYOUT_AVAILABLE"),
        ];
        decode_flags(flags, &NAMES)
    }

    fn validation_flag_names(flags: i32) -> String {
        const NAMES: [(i32, &str); 11] = [
            (1, "CODEC_NOT_SUPPORTED"),
            (8, "INPUT_FORMAT_NOT_SUPPORTED"),
            (16, "CODEC_CONFIGURATION_NOT_SUPPORTED"),
            (32, "RATE_CONTROL_MODE_NOT_SUPPORTED"),
            (64, "RATE_CONTROL_CONFIGURATION_NOT_SUPPORTED"),
            (128, "INTRA_REFRESH_MODE_NOT_SUPPORTED"),
            (256, "SUBREGION_LAYOUT_MODE_NOT_SUPPORTED"),
            (512, "RESOLUTION_NOT_SUPPORTED_IN_LIST"),
            (2048, "GOP_STRUCTURE_NOT_SUPPORTED"),
            (4096, "SUBREGION_LAYOUT_DATA_NOT_SUPPORTED"),
            (0, "NONE"),
        ];
        decode_flags(flags, &NAMES)
    }

    fn decode_flags(flags: i32, names: &[(i32, &str)]) -> String {
        if flags == 0 {
            return "NONE".to_owned();
        }
        let mut out: Vec<&str> = Vec::new();
        let mut seen = 0i32;
        for (bit, name) in names {
            if *bit != 0 && flags & bit != 0 {
                out.push(name);
                seen |= bit;
            }
        }
        let unknown = flags & !seen;
        if unknown != 0 {
            return format!("{} | <unknown bits 0x{unknown:X}>", out.join(" | "));
        }
        out.join(" | ")
    }

    /// One `(codec, profile, input format, resolution)` cell for test (e).
    struct SupportProbe {
        codec: D3D12_VIDEO_ENCODER_CODEC,
        codec_name: &'static str,
        profile: i32,
        profile_name: &'static str,
        format: DXGI_FORMAT,
        format_name: &'static str,
    }

    /// Ask the driver which HEVC coding-unit / transform-unit sizes it accepts,
    /// instead of guessing.
    ///
    /// A hand-picked HEVC configuration comes back `CODEC_CONFIGURATION_NOT_SUPPORTED`
    /// on this silicon, and that flag then describes *our guess*, not the chroma
    /// format we are asking about — it masks the answer. `CODEC_CONFIGURATION_SUPPORT`
    /// hands back the legal values, so feeding those in removes the guess entirely.
    fn hevc_codec_config(
        video: &ID3D12VideoDevice,
        profile: i32,
    ) -> (D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC, String) {
        // The fallback if the query fails: the most ordinary configuration there is.
        let mut cfg = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC {
            ConfigurationFlags: Default::default(),
            MinLumaCodingUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_CUSIZE_16x16,
            MaxLumaCodingUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_CUSIZE_64x64,
            MinLumaTransformUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_TUSIZE_4x4,
            MaxLumaTransformUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_TUSIZE_32x32,
            max_transform_hierarchy_depth_inter: 3,
            max_transform_hierarchy_depth_intra: 3,
        };
        // Two struct revisions exist. HEVC1 is the larger, later one, and 4:4:4 HEVC
        // arrived with that revision — which is why the plain HEVC struct returns
        // E_INVALIDARG for exactly the 4:4:4 profiles. Ask with HEVC1 first.
        // SAFETY: both are plain C PODs of integers and `#[repr(transparent)]`
        // newtypes, so an all-zero bit pattern is a valid value of either.
        let mut limits1: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_HEVC1 =
            unsafe { std::mem::zeroed() };
        let mut limits0: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_HEVC =
            unsafe { std::mem::zeroed() };

        for revision in [1u8, 0u8] {
            let mut profile_slot = profile;
            let mut support = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT::default();
            if revision == 1 {
                support.DataSize = std::mem::size_of::<
                    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_HEVC1,
                >() as u32;
                support.Anonymous.pHEVCSupport1 = &mut limits1;
            } else {
                support.DataSize = std::mem::size_of::<
                    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_HEVC,
                >() as u32;
                support.Anonymous.pHEVCSupport = &mut limits0;
            }
            let mut data = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT {
                NodeIndex: 0,
                Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
                Profile: profile_desc(D3D12_VIDEO_ENCODER_CODEC_HEVC, &mut profile_slot),
                IsSupported: false.into(),
                CodecSupportLimits: support,
            };
            // SAFETY: `data` and everything it points at are live locals for the call.
            let hr = unsafe {
                video.CheckFeatureSupport(
                    D3D12_FEATURE_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT,
                    &mut data as *mut _ as *mut c_void,
                    std::mem::size_of::<
                        D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT,
                    >() as u32,
                )
            };
            let label = if revision == 1 { "HEVC1" } else { "HEVC" };
            match hr {
                Ok(()) if data.IsSupported.as_bool() => {
                    // The CODEC_CONFIGURATION union has only the one HEVC arm, so the
                    // values from either revision go into the same config struct.
                    let (min_cu, max_cu, min_tu, max_tu, d_inter, d_intra, flags) =
                        if revision == 1 {
                            (
                                limits1.MinLumaCodingUnitSize,
                                limits1.MaxLumaCodingUnitSize,
                                limits1.MinLumaTransformUnitSize,
                                limits1.MaxLumaTransformUnitSize,
                                limits1.max_transform_hierarchy_depth_inter,
                                limits1.max_transform_hierarchy_depth_intra,
                                limits1.SupportFlags.0,
                            )
                        } else {
                            (
                                limits0.MinLumaCodingUnitSize,
                                limits0.MaxLumaCodingUnitSize,
                                limits0.MinLumaTransformUnitSize,
                                limits0.MaxLumaTransformUnitSize,
                                limits0.max_transform_hierarchy_depth_inter,
                                limits0.max_transform_hierarchy_depth_intra,
                                limits0.SupportFlags.0,
                            )
                        };
                    cfg.MinLumaCodingUnitSize = min_cu;
                    cfg.MaxLumaCodingUnitSize = max_cu;
                    cfg.MinLumaTransformUnitSize = min_tu;
                    cfg.MaxLumaTransformUnitSize = max_tu;
                    cfg.max_transform_hierarchy_depth_inter = d_inter;
                    cfg.max_transform_hierarchy_depth_intra = d_intra;
                    return (
                        cfg,
                        format!(
                            "{label} driver cfg: CU {}..{} TU {}..{} depth {}/{} flags=0x{:X}",
                            min_cu.0, max_cu.0, min_tu.0, max_tu.0, d_inter, d_intra, flags
                        ),
                    );
                }
                Ok(()) if revision == 0 => {
                    return (cfg, format!("{label} reports IsSupported=false; using defaults"))
                }
                Err(e) if revision == 0 => {
                    return (
                        cfg,
                        format!("{label} query failed {}; using defaults", hr_str(&e)),
                    )
                }
                // revision 1 failed or reported unsupported — fall through and retry
                // with the older struct.
                _ => {}
            }
        }
        (cfg, "no CODEC_CONFIGURATION_SUPPORT revision answered; using defaults".into())
    }

    /// Test (e): the full `D3D12_FEATURE_VIDEO_ENCODER_SUPPORT` query.
    ///
    /// Every field must be filled with something the driver considers valid or the
    /// answer is a validation failure about the *config*, not about the chroma format
    /// we are actually asking about. The HEVC configuration is therefore read back
    /// from the driver first; the GOP and rate control are the most ordinary values
    /// that exist.
    fn d3d12_encoder_support(
        video: &ID3D12VideoDevice,
        p: &SupportProbe,
        w: u32,
        h: u32,
    ) -> String {
        let mut profile_slot = p.profile;
        let mut min_level = LevelSlot::default();
        let mut max_level = LevelSlot::default();

        // Minimal-but-valid codec configuration per codec.
        let mut h264_cfg = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264::default();
        let (mut hevc_cfg, hevc_cfg_note) = hevc_codec_config(video, p.profile);
        let mut av1_cfg = D3D12_VIDEO_ENCODER_AV1_CODEC_CONFIGURATION {
            FeatureFlags: Default::default(),
            OrderHintBitsMinus1: 7,
        };
        let mut codec_config = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION::default();
        // Each arm points at the live local for that codec; DataSize is the pointee.
        if p.codec == D3D12_VIDEO_ENCODER_CODEC_H264 {
            codec_config.DataSize =
                std::mem::size_of::<D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264>() as u32;
            codec_config.Anonymous.pH264Config = &mut h264_cfg;
        } else if p.codec == D3D12_VIDEO_ENCODER_CODEC_HEVC {
            codec_config.DataSize =
                std::mem::size_of::<D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC>() as u32;
            codec_config.Anonymous.pHEVCConfig = &mut hevc_cfg;
        } else {
            codec_config.DataSize =
                std::mem::size_of::<D3D12_VIDEO_ENCODER_AV1_CODEC_CONFIGURATION>() as u32;
            codec_config.Anonymous.pAV1Config = &mut av1_cfg;
        }

        // GOP: one IDR every 30 frames, no B-pictures.
        let mut h264_gop = D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264 {
            GOPLength: 30,
            PPicturePeriod: 1,
            pic_order_cnt_type: 2,
            log2_max_frame_num_minus4: 0,
            log2_max_pic_order_cnt_lsb_minus4: 0,
        };
        let mut hevc_gop = D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_HEVC {
            GOPLength: 30,
            PPicturePeriod: 1,
            log2_max_pic_order_cnt_lsb_minus4: 0,
        };
        let mut av1_gop = D3D12_VIDEO_ENCODER_AV1_SEQUENCE_STRUCTURE {
            IntraDistance: 30,
            InterFramePeriod: 1,
        };
        let mut gop = D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE::default();
        // As above.
        if p.codec == D3D12_VIDEO_ENCODER_CODEC_H264 {
            gop.DataSize =
                std::mem::size_of::<D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264>() as u32;
            gop.Anonymous.pH264GroupOfPictures = &mut h264_gop;
        } else if p.codec == D3D12_VIDEO_ENCODER_CODEC_HEVC {
            gop.DataSize =
                std::mem::size_of::<D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_HEVC>() as u32;
            gop.Anonymous.pHEVCGroupOfPictures = &mut hevc_gop;
        } else {
            gop.DataSize =
                std::mem::size_of::<D3D12_VIDEO_ENCODER_AV1_SEQUENCE_STRUCTURE>() as u32;
            gop.Anonymous.pAV1SequenceStructure = &mut av1_gop;
        }

        // CQP is the one rate-control mode every encoder implements, so a refusal
        // here is about the format, not about the bitrate model.
        let cqp = D3D12_VIDEO_ENCODER_RATE_CONTROL_CQP {
            ConstantQP_FullIntracodedFrame: 30,
            ConstantQP_InterPredictedFrame_PrevRefOnly: 30,
            ConstantQP_InterPredictedFrame_BiDirectionalRef: 30,
        };
        let mut rate_control = D3D12_VIDEO_ENCODER_RATE_CONTROL {
            Mode: D3D12_VIDEO_ENCODER_RATE_CONTROL_MODE_CQP,
            Flags: Default::default(),
            ConfigParams: D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS {
                DataSize: std::mem::size_of::<D3D12_VIDEO_ENCODER_RATE_CONTROL_CQP>() as u32,
                ..Default::default()
            },
            TargetFrameRate: DXGI_RATIONAL {
                Numerator: FPS,
                Denominator: 1,
            },
        };
        // `cqp` outlives the CheckFeatureSupport call below.
        rate_control.ConfigParams.Anonymous.pConfiguration_CQP = &cqp;

        let resolutions = [D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
            Width: w,
            Height: h,
        }];
        let mut limits = D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOLUTION_SUPPORT_LIMITS::default();

        let mut data = D3D12_FEATURE_DATA_VIDEO_ENCODER_SUPPORT {
            NodeIndex: 0,
            Codec: p.codec,
            InputFormat: p.format,
            CodecConfiguration: codec_config,
            CodecGopSequence: gop,
            RateControl: rate_control,
            IntraRefresh: D3D12_VIDEO_ENCODER_INTRA_REFRESH_MODE_NONE,
            SubregionFrameEncoding: D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE_FULL_FRAME,
            ResolutionsListCount: 1,
            pResolutionList: resolutions.as_ptr(),
            MaxReferenceFramesInDPB: 1,
            // Outputs.
            ValidationFlags: Default::default(),
            SupportFlags: Default::default(),
            SuggestedProfile: profile_desc(p.codec, &mut profile_slot),
            SuggestedLevel: level_setting(p.codec, &mut max_level),
            pResolutionDependentSupport: &mut limits,
        };
        let _ = &mut min_level;

        // SAFETY: `data` and everything it points at are live locals for the call;
        // the size is the struct's own size, as the API requires.
        let hr = unsafe {
            video.CheckFeatureSupport(
                D3D12_FEATURE_VIDEO_ENCODER_SUPPORT,
                &mut data as *mut _ as *mut c_void,
                std::mem::size_of::<D3D12_FEATURE_DATA_VIDEO_ENCODER_SUPPORT>() as u32,
            )
        };
        if let Err(e) = hr {
            return format!("CheckFeatureSupport FAILED {}", hr_str(&e));
        }
        let sf = data.SupportFlags.0;
        let vf = data.ValidationFlags.0;
        let mut out = format!(
            "SupportFlags=0x{sf:08X} [{}]  ValidationFlags=0x{vf:08X} [{}]  SuggestedLevel({}) MaxSubregions={} SubregionBlockPixels={}",
            support_flag_names(sf),
            validation_flag_names(vf),
            level_text(p.codec, &max_level),
            limits.MaxSubregionsNumber,
            limits.SubregionBlockPixelsSize
        );
        if p.codec == D3D12_VIDEO_ENCODER_CODEC_HEVC {
            out.push_str(&format!("\n           [{hevc_cfg_note}]"));
        }
        out
    }

    fn d3d12_video_caps() {
        println!("=== 6. D3D12 VIDEO ENCODE CAPABILITIES (CheckFeatureSupport only, no encoding) ===");
        println!(
            "(input formats marked * can carry 4:4:4. This section queries the API the \
             Microsoft DX12 encoder MFTs wrap, so it answers what section 4 could not drive.)"
        );

        // SAFETY: the factory interface is inferred from the binding.
        let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
            Ok(f) => f,
            Err(e) => {
                println!("  CreateDXGIFactory1 FAILED {}", hr_str(&e));
                println!();
                return;
            }
        };
        let mut index = 0u32;
        let mut probed = 0usize;
        loop {
            // SAFETY: `factory` is live; EnumAdapters1 errors past the last index.
            let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(index) } {
                Ok(a) => a,
                Err(_) => break,
            };
            index += 1;
            // SAFETY: `adapter` is live.
            let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
                continue;
            };
            let name = wide_to_string(&desc.Description);
            if desc.Flags & 2 != 0 {
                // DXGI_ADAPTER_FLAG_SOFTWARE — WARP has no encode silicon to report.
                println!();
                println!("-- adapter[{}] \"{name}\": WARP, skipped --", index - 1);
                continue;
            }
            println!();
            println!(
                "-- adapter[{}] \"{name}\" vendor=0x{:04X} device=0x{:04X} --",
                index - 1,
                desc.VendorId,
                desc.DeviceId
            );
            probed += 1;
            d3d12_adapter_caps(&adapter);
            }
        if probed == 0 {
            println!("  no non-WARP adapter enumerated");
        }
        println!();
    }

    fn d3d12_adapter_caps(adapter: &IDXGIAdapter1) {
        let mut device: Option<ID3D12Device> = None;
        // SAFETY: `adapter` is live; `device` is a live out-parameter.
        if let Err(e) = unsafe {
            D3D12CreateDevice(adapter, D3D_FEATURE_LEVEL_11_0, &mut device)
        } {
            println!("   D3D12CreateDevice FAILED {}", hr_str(&e));
            return;
        }
        let Some(device) = device else {
            println!("   D3D12CreateDevice returned no device");
            return;
        };
        // Prefer ID3D12VideoDevice3 and say which one we got — the caps a driver
        // reports can differ by interface generation, so the reader needs to know.
        let video3 = device.cast::<ID3D12VideoDevice3>().ok();
        let video: ID3D12VideoDevice = match device.cast::<ID3D12VideoDevice>() {
            Ok(v) => v,
            Err(e) => {
                println!("   no ID3D12VideoDevice on this device: {}", hr_str(&e));
                return;
            }
        };
        println!(
            "   video device interface : {}",
            if video3.is_some() {
                "ID3D12VideoDevice3"
            } else {
                "ID3D12VideoDevice (ID3D12VideoDevice3 NOT available)"
            }
        );

        // (a) codec support.
        println!("   (a) D3D12_FEATURE_VIDEO_ENCODER_CODEC");
        let mut supported: Vec<(D3D12_VIDEO_ENCODER_CODEC, &str)> = Vec::new();
        for (codec, name) in D3D12_CODECS {
            let mut data = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC {
                NodeIndex: 0,
                Codec: codec,
                IsSupported: false.into(),
            };
            // SAFETY: `data` is a live local of exactly the size passed.
            let hr = unsafe {
                video.CheckFeatureSupport(
                    D3D12_FEATURE_VIDEO_ENCODER_CODEC,
                    &mut data as *mut _ as *mut c_void,
                    std::mem::size_of::<D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC>() as u32,
                )
            };
            match hr {
                Ok(()) => {
                    let ok = data.IsSupported.as_bool();
                    println!("       {name:<5} IsSupported={}", if ok { "YES" } else { "no" });
                    if ok {
                        supported.push((codec, name));
                    }
                }
                Err(e) => println!("       {name:<5} FAILED {}", hr_str(&e)),
            }
        }
        if supported.is_empty() {
            println!("       no codec supported; (b)-(e) skipped for this adapter");
            return;
        }

        // (b) profile / level.
        println!("   (b) D3D12_FEATURE_VIDEO_ENCODER_PROFILE_LEVEL");
        let mut supported_profiles: Vec<(D3D12_VIDEO_ENCODER_CODEC, &str, i32, &str)> = Vec::new();
        for (codec, codec_name) in &supported {
            for (value, profile_name) in d3d12_profiles(*codec) {
                let mut profile_slot = *value;
                let mut min_slot = LevelSlot::default();
                let mut max_slot = LevelSlot::default();
                let mut data = D3D12_FEATURE_DATA_VIDEO_ENCODER_PROFILE_LEVEL {
                    NodeIndex: 0,
                    Codec: *codec,
                    Profile: profile_desc(*codec, &mut profile_slot),
                    IsSupported: false.into(),
                    MinSupportedLevel: level_setting(*codec, &mut min_slot),
                    MaxSupportedLevel: level_setting(*codec, &mut max_slot),
                };
                // SAFETY: `data` and the slots it points at are live locals.
                let hr = unsafe {
                    video.CheckFeatureSupport(
                        D3D12_FEATURE_VIDEO_ENCODER_PROFILE_LEVEL,
                        &mut data as *mut _ as *mut c_void,
                        std::mem::size_of::<D3D12_FEATURE_DATA_VIDEO_ENCODER_PROFILE_LEVEL>()
                            as u32,
                    )
                };
                match hr {
                    Ok(()) => {
                        let ok = data.IsSupported.as_bool();
                        println!(
                            "       {codec_name:<5} {profile_name:<16} (value={value}) IsSupported={:<3}  Min[{}]  Max[{}]",
                            if ok { "YES" } else { "no" },
                            level_text(*codec, &min_slot),
                            level_text(*codec, &max_slot)
                        );
                        if ok {
                            supported_profiles.push((*codec, codec_name, *value, profile_name));
                        }
                    }
                    Err(e) => println!(
                        "       {codec_name:<5} {profile_name:<16} (value={value}) FAILED {}",
                        hr_str(&e)
                    ),
                }
            }
        }

        // (c) input format matrix — the decisive cells.
        println!("   (c) D3D12_FEATURE_VIDEO_ENCODER_INPUT_FORMAT");
        print!("       {:<22}", "profile \\ format");
        for (_, fname) in D3D12_INPUT_FORMATS {
            print!(" {fname:>7}");
        }
        println!();
        for (codec, codec_name, value, profile_name) in &supported_profiles {
            print!("       {:<22}", format!("{codec_name} {profile_name}"));
            for (format, _) in D3D12_INPUT_FORMATS {
                let mut profile_slot = *value;
                let mut data = D3D12_FEATURE_DATA_VIDEO_ENCODER_INPUT_FORMAT {
                    NodeIndex: 0,
                    Codec: *codec,
                    Profile: profile_desc(*codec, &mut profile_slot),
                    Format: format,
                    IsSupported: false.into(),
                };
                // SAFETY: `data` is a live local of exactly the size passed.
                let hr = unsafe {
                    video.CheckFeatureSupport(
                        D3D12_FEATURE_VIDEO_ENCODER_INPUT_FORMAT,
                        &mut data as *mut _ as *mut c_void,
                        std::mem::size_of::<D3D12_FEATURE_DATA_VIDEO_ENCODER_INPUT_FORMAT>() as u32,
                    )
                };
                let cell = match hr {
                    Ok(()) => {
                        if data.IsSupported.as_bool() {
                            "YES"
                        } else {
                            "-"
                        }
                    }
                    Err(_) => "ERR",
                };
                print!(" {cell:>7}");
            }
            println!();
        }

        // (d) output resolution.
        println!("   (d) D3D12_FEATURE_VIDEO_ENCODER_OUTPUT_RESOLUTION_RATIOS_COUNT / _OUTPUT_RESOLUTION");
        for (codec, codec_name) in &supported {
            let mut count = D3D12_FEATURE_DATA_VIDEO_ENCODER_OUTPUT_RESOLUTION_RATIOS_COUNT {
                NodeIndex: 0,
                Codec: *codec,
                ResolutionRatiosCount: 0,
            };
            // SAFETY: `count` is a live local of exactly the size passed.
            let hr = unsafe {
                video.CheckFeatureSupport(
                    D3D12_FEATURE_VIDEO_ENCODER_OUTPUT_RESOLUTION_RATIOS_COUNT,
                    &mut count as *mut _ as *mut c_void,
                    std::mem::size_of::<
                        D3D12_FEATURE_DATA_VIDEO_ENCODER_OUTPUT_RESOLUTION_RATIOS_COUNT,
                    >() as u32,
                )
            };
            if let Err(e) = hr {
                println!("       {codec_name:<5} RATIOS_COUNT FAILED {}", hr_str(&e));
                continue;
            }
            let n = count.ResolutionRatiosCount as usize;
            // The driver writes exactly `n` ratio entries. When n is 0 the pointer
            // must be NULL — passing a valid pointer with a zero count is what makes
            // this call return E_INVALIDARG on Intel, which reads like "no resolution
            // info available" and is really "you filled the struct wrong".
            let mut ratios =
                vec![D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_RATIO_DESC::default(); n];
            let mut data = D3D12_FEATURE_DATA_VIDEO_ENCODER_OUTPUT_RESOLUTION {
                NodeIndex: 0,
                Codec: *codec,
                ResolutionRatiosCount: count.ResolutionRatiosCount,
                IsSupported: false.into(),
                MinResolutionSupported: Default::default(),
                MaxResolutionSupported: Default::default(),
                ResolutionWidthMultipleRequirement: 0,
                ResolutionHeightMultipleRequirement: 0,
                pResolutionRatios: if n == 0 {
                    std::ptr::null_mut()
                } else {
                    ratios.as_mut_ptr()
                },
            };
            // SAFETY: as above; `ratios` is sized from the count the driver just gave.
            let hr = unsafe {
                video.CheckFeatureSupport(
                    D3D12_FEATURE_VIDEO_ENCODER_OUTPUT_RESOLUTION,
                    &mut data as *mut _ as *mut c_void,
                    std::mem::size_of::<D3D12_FEATURE_DATA_VIDEO_ENCODER_OUTPUT_RESOLUTION>()
                        as u32,
                )
            };
            match hr {
                Ok(()) => {
                    let max = data.MaxResolutionSupported;
                    let fits_5k = max.Width >= 5120 && max.Height >= 2880;
                    println!(
                        "       {codec_name:<5} IsSupported={:<3} ratios={} min={}x{} max={}x{} \
                         multiple=w{}/h{}  5120x2880 fits: {}",
                        if data.IsSupported.as_bool() { "YES" } else { "no" },
                        count.ResolutionRatiosCount,
                        data.MinResolutionSupported.Width,
                        data.MinResolutionSupported.Height,
                        max.Width,
                        max.Height,
                        data.ResolutionWidthMultipleRequirement,
                        data.ResolutionHeightMultipleRequirement,
                        if fits_5k { "YES" } else { "NO" }
                    );
                }
                Err(e) => println!(
                    "       {codec_name:<5} ratios={} OUTPUT_RESOLUTION FAILED {}",
                    count.ResolutionRatiosCount,
                    hr_str(&e)
                ),
            }
        }

        // (e) the full support query for the combos we actually care about.
        println!("   (e) D3D12_FEATURE_VIDEO_ENCODER_SUPPORT");
        println!(
            "       NOTE: a row whose ValidationFlags say CODEC_CONFIGURATION_NOT_SUPPORTED is \
             INCONCLUSIVE, not negative —"
        );
        println!(
            "       the driver is rejecting the codec configuration this probe supplied, so the \
             row says nothing about the"
        );
        println!(
            "       chroma format. Read (c) for that. A row reading GENERAL_SUPPORT_OK with \
             ValidationFlags NONE is a real yes."
        );
        let wanted: [(&str, &str, DXGI_FORMAT, &str); 5] = [
            ("HEVC", "HEVC_MAIN", DXGI_FORMAT_NV12, "NV12"),
            ("HEVC", "HEVC_MAIN_444", DXGI_FORMAT_AYUV, "AYUV*"),
            ("HEVC", "HEVC_MAIN10_444", DXGI_FORMAT_Y410, "Y410*"),
            ("H264", "H264_HIGH", DXGI_FORMAT_NV12, "NV12"),
            ("AV1", "AV1_MAIN", DXGI_FORMAT_NV12, "NV12"),
        ];
        for (codec_name, profile_name, format, format_name) in wanted {
            let Some((codec, _, value, _)) = supported_profiles
                .iter()
                .find(|(_, c, _, p)| *c == codec_name && *p == profile_name)
                .copied()
            else {
                println!(
                    "       {codec_name:<5} {profile_name:<16} + {format_name:<6} : SKIPPED, profile not supported on this adapter"
                );
                continue;
            };
            let probe = SupportProbe {
                codec,
                codec_name,
                profile: value,
                profile_name,
                format,
                format_name,
            };
            for (w, h) in SIZES {
                println!(
                    "       {:<5} {:<16} + {:<6} @ {w}x{h}",
                    probe.codec_name, probe.profile_name, probe.format_name
                );
                println!("           {}", d3d12_encoder_support(&video, &probe, w, h));
            }
        }
    }

    // -- entry point -------------------------------------------------------------

    pub fn run() {
        // SAFETY: one CoInitializeEx per process, on the thread that drives MF.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

        println!("mf-caps-probe {} — Media Foundation encoder capability probe", env!("CARGO_PKG_VERSION"));
        println!();

        let device = print_host();
        let manager = device.as_ref().map(|(d, _)| d).and_then(device_manager);
        let gpu = device.as_ref().map(|(d, c)| (d, c));

        let entries = inventory();

        // Drive every hardware MFT, plus the Microsoft software H.264/HEVC encoders
        // for contrast — a software "yes" next to a hardware "no" is the shape that
        // tells you the limitation is the GPU, not Windows.
        let mut drive: Vec<usize> = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            let software_reference = !e.hardware && matches!(e.codec, Codec::H264 | Codec::Hevc);
            if e.hardware || software_reference {
                drive.push(i);
            }
        }
        if drive.is_empty() {
            println!("(no encoder MFTs to drive — sections 3 and 4 will be empty)");
        } else {
            println!("driving {} MFT(s) in sections 3 and 4:", drive.len());
            for &i in &drive {
                let e = &entries[i];
                println!(
                    "  [{}] {} ({}, ordinal {} in the {} ALL enumeration)",
                    i,
                    e.name,
                    if e.hardware { "hardware" } else { "software" },
                    e.ordinal,
                    e.codec.name()
                );
            }
            println!();
        }

        let acceptances = acceptance_pass(&entries, &drive, manager.as_ref());
        let mut rows: Vec<Row> = Vec::new();
        encode_pass(&entries, &acceptances, manager.as_ref(), gpu, &mut rows);
        summary(&entries, &acceptances, &mut rows);
        d3d12_video_caps();

        println!("probe complete.");
        // SAFETY: balances the MFStartup in `print_host`.
        let _ = unsafe { MFShutdown() };
    }
}
