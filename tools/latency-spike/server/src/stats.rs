//! The per-stage latency budget: QPC arithmetic and the JSONL record shapes.
//!
//! The point of the spike is that no number here is opaque. Every stamp is a raw
//! QPC tick converted once, with the frequency published in the header line so a
//! reader can re-derive ticks if it wants to.

use serde::Serialize;

/// `QueryPerformanceCounter` ticks, converted with the frequency read once at start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QpcClock {
    freq: i64,
}

impl QpcClock {
    /// `None` for a non-positive frequency, which would make every conversion
    /// meaningless (and divide by zero).
    pub fn new(freq: i64) -> Option<Self> {
        (freq > 0).then_some(Self { freq })
    }

    pub fn freq(self) -> i64 {
        self.freq
    }

    /// Ticks to microseconds, truncating.
    ///
    /// The multiply is done in `i128` on purpose. QPC is an absolute count since
    /// boot; at the usual 10 MHz an uptime of about 30 days already makes
    /// `ticks * 1_000_000` exceed `i64::MAX`, and this server is meant to be left
    /// running on a host nobody reboots.
    pub fn micros(self, ticks: i64) -> i64 {
        ((ticks as i128 * 1_000_000) / self.freq as i128) as i64
    }
}

/// First line of every stats file, and the first type-2 message on every connection.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Header {
    pub record: &'static str,
    /// Bumped whenever a field changes meaning, so an archived file stays readable.
    pub schema: u32,
    /// Wire dialect this server speaks. 2 = video rides `MSG_VIDEO_SEQ` (sequence
    /// prefix) and `MSG_RECTS` may appear.
    pub wire_version: u32,
    pub qpc_frequency: i64,
    pub video_port: u16,
    pub input_port: u16,
    pub bitrate_kbps: u32,
    pub gop: u32,
    pub fps: u32,
    pub output_index: usize,
    pub adapter: String,
    pub output: String,
    pub width: u32,
    pub height: u32,
    /// `MFT_FRIENDLY_NAME_Attribute` of the encoder that was actually selected.
    pub encoder: String,
    /// `async-hardware` or `sync-software`.
    pub encoder_kind: &'static str,
    /// Which `ICodecAPI` properties the MFT accepted. An encoder that silently
    /// refuses `AVLowLatencyMode` is the single most likely explanation for a
    /// surprising encode stage, so it must be visible without a debugger.
    pub codec_api_applied: Vec<String>,
    pub codec_api_refused: Vec<String>,
    /// The rect fast-path predicate this server ran with (0/0 when disabled).
    /// Tuning knobs, reported so every archived run names its own thresholds.
    pub rect_max_count: u32,
    pub rect_max_bytes: u64,
    /// How SPS/PPS reach the wire: see `annexb`.
    pub parameter_set_route: &'static str,
    /// Whether the out-of-band `MF_MT_MPEG_SEQUENCE_HEADER` was available as a
    /// fallback source of parameter sets.
    pub sequence_header_available: bool,
}

/// Schema 3: the header gained `rect_max_count`/`rect_max_bytes`, frame rows
/// gained `dropped_rects`, and `record: "rects"` rows exist at all.
pub const SCHEMA: u32 = 3;
pub const WIRE_VERSION: u32 = 2;

impl Header {
    pub fn new() -> Self {
        Self {
            record: "header",
            schema: SCHEMA,
            wire_version: WIRE_VERSION,
            qpc_frequency: 0,
            video_port: 0,
            input_port: 0,
            bitrate_kbps: 0,
            gop: 0,
            fps: 0,
            output_index: 0,
            adapter: String::new(),
            output: String::new(),
            width: 0,
            height: 0,
            encoder: String::new(),
            encoder_kind: "unknown",
            codec_api_applied: Vec::new(),
            codec_api_refused: Vec::new(),
            rect_max_count: 0,
            rect_max_bytes: 0,
            parameter_set_route: "in-band, out-of-band fallback",
            sequence_header_available: false,
        }
    }
}

impl Default for Header {
    fn default() -> Self {
        Self::new()
    }
}

/// One frame's worth of the budget. Every `*_us` field is an absolute QPC stamp in
/// microseconds on the server's clock, so differences between adjacent fields are
/// the stage costs and the whole line is one row of the budget.
///
/// Two honest caveats a reader must know:
///
/// * `present_qpc_us` comes from `DXGI_OUTDUPL_FRAME_INFO::LastPresentTime` — when
///   the desktop compositor presented the frame, i.e. *before* we were told about
///   it. `acquire_qpc_us - present_qpc_us` is therefore the duplication pipeline's
///   own latency, not ours.
/// * `convert_end_us` is when `VideoProcessorBlt` *returned*, not when the GPU
///   finished. The colour conversion is submitted, not awaited; its true cost shows
///   up as back-pressure inside the encode stage.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct FrameRecord {
    pub record: &'static str,
    /// Capture sequence number, assigned when the frame was acquired — the same
    /// value the wire carries on `MSG_VIDEO_SEQ`/`MSG_RECTS`, so client and server
    /// rows join exactly. (Schema 1 counted emitted access units instead.)
    pub frame: u64,
    pub present_qpc_us: i64,
    pub acquire_qpc_us: i64,
    pub convert_start_us: i64,
    pub convert_end_us: i64,
    pub encode_submit_us: i64,
    pub encode_out_us: i64,
    pub send_done_us: i64,
    pub au_bytes: usize,
    pub keyframe: bool,
    /// True when this server had to prepend the stored SPS/PPS itself.
    pub param_sets_prepended: bool,
    /// Cumulative count of frames dropped because the send queue was full.
    pub dropped_frames: u64,
    /// Cumulative count of rect messages dropped for the same reason. Carried on
    /// every frame row — not only on `rects` rows — so a socket so far behind that
    /// *no* rect message survives still shows its fast-path losses somewhere.
    pub dropped_rects: u64,
    /// Cumulative count of encoder outputs whose sample timestamp matched no
    /// pending submission — each one is a stamp pairing taken on faith (FIFO).
    pub stamp_mismatches: u64,
    /// Dirty-rect metadata from the duplication, when the frame carried any
    /// (`None` = metadata unavailable, which is NOT the same as zero rects).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty_rect_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_rect_count: Option<u32>,
    /// How many encoded frames a connect-edge keyframe request waited before the
    /// keyframe actually arrived. Present only on the keyframe that answered one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyframe_wait_frames: Option<u32>,
}

impl FrameRecord {
    pub fn new() -> Self {
        Self {
            record: "frame",
            ..Default::default()
        }
    }
}

/// One rect fast-path update: the raw pixels the wire carried ahead of the same
/// frame's access unit. `frame` is the capture sequence number, shared with the
/// frame's own row and the `MSG_RECTS` message.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct RectRecord {
    pub record: &'static str,
    pub frame: u64,
    pub rect_count: u32,
    /// Pixel payload bytes (the wire message adds per-rect headers on top).
    pub rect_bytes: u64,
    /// Immediately before the GPU→CPU readback began.
    pub pack_start_us: i64,
    /// When the packed payload was handed to the sender.
    pub pack_end_us: i64,
    /// When the socket write returned (filled by the sender thread).
    pub send_done_us: i64,
    /// Cumulative rect messages dropped because the send queue was full.
    pub dropped_rects: u64,
}

impl RectRecord {
    pub fn new() -> Self {
        Self {
            record: "rects",
            ..Default::default()
        }
    }
}

/// One injected keystroke.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InputEventRecord {
    pub record: &'static str,
    pub seq: u32,
    pub vk: u16,
    pub kind: &'static str,
    pub recv_qpc_us: i64,
    pub injected_qpc_us: i64,
}

impl InputEventRecord {
    pub fn new(seq: u32, vk: u16, kind: &'static str, recv: i64, injected: i64) -> Self {
        Self {
            record: "input",
            seq,
            vk,
            kind,
            recv_qpc_us: recv,
            injected_qpc_us: injected,
        }
    }
}

/// Serialise one record as a JSONL line (no trailing newline).
///
/// Falls back to a minimal error object rather than panicking: losing one stats line
/// must never take the session down.
pub fn to_line<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|e| format!(r#"{{"record":"error","detail":"serialize: {e}"}}"#))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn a_non_positive_frequency_is_refused() {
        assert!(QpcClock::new(0).is_none());
        assert!(QpcClock::new(-1).is_none());
        assert!(QpcClock::new(1).is_some());
    }

    #[test]
    fn ticks_convert_at_the_given_frequency() {
        // 10 MHz is what Windows 10/11 reports on every box measured so far.
        let c = QpcClock::new(10_000_000).unwrap();
        assert_eq!(c.micros(0), 0);
        assert_eq!(c.micros(10), 1);
        assert_eq!(c.micros(10_000_000), 1_000_000);
        // Truncation, not rounding — 9 ticks is 0.9 us.
        assert_eq!(c.micros(9), 0);

        // A different frequency must give a different answer for the same ticks,
        // or the conversion is ignoring its parameter.
        let c2 = QpcClock::new(2_000_000).unwrap();
        assert_eq!(c2.micros(10), 5);
    }

    #[test]
    fn a_long_uptime_does_not_overflow() {
        // 100 days at 10 MHz. In i64 the intermediate `ticks * 1_000_000` wraps
        // negative here; the i128 multiply is what keeps it right.
        let c = QpcClock::new(10_000_000).unwrap();
        let ticks: i64 = 100 * 24 * 3600 * 10_000_000;
        assert!(
            ticks.checked_mul(1_000_000).is_none(),
            "the fixture must actually exceed i64 range, or it proves nothing"
        );
        assert_eq!(c.micros(ticks), 100 * 24 * 3600 * 1_000_000);
    }

    #[test]
    fn a_frame_line_carries_every_stage_under_its_documented_name() {
        let mut r = FrameRecord::new();
        // Distinct ascending values: a fixture where all stamps were equal could
        // not tell `encode_submit_us` from `encode_out_us` if they were swapped.
        r.frame = 7;
        r.present_qpc_us = 1000;
        r.acquire_qpc_us = 1100;
        r.convert_start_us = 1200;
        r.convert_end_us = 1300;
        r.encode_submit_us = 1400;
        r.encode_out_us = 1500;
        r.send_done_us = 1600;
        r.au_bytes = 4242;
        r.keyframe = true;
        r.param_sets_prepended = true;
        r.dropped_frames = 3;
        r.dropped_rects = 5;
        r.stamp_mismatches = 2;
        r.dirty_rect_count = Some(4);
        r.dirty_bytes = Some(8192);
        r.move_rect_count = Some(1);
        r.keyframe_wait_frames = Some(17);

        let v: Value = serde_json::from_str(&to_line(&r)).unwrap();
        assert_eq!(v["record"], "frame");
        assert_eq!(v["frame"], 7);
        assert_eq!(v["present_qpc_us"], 1000);
        assert_eq!(v["acquire_qpc_us"], 1100);
        assert_eq!(v["convert_start_us"], 1200);
        assert_eq!(v["convert_end_us"], 1300);
        assert_eq!(v["encode_submit_us"], 1400);
        assert_eq!(v["encode_out_us"], 1500);
        assert_eq!(v["send_done_us"], 1600);
        assert_eq!(v["au_bytes"], 4242);
        assert_eq!(v["keyframe"], true);
        assert_eq!(v["param_sets_prepended"], true);
        assert_eq!(v["dropped_frames"], 3);
        assert_eq!(v["dropped_rects"], 5);
        assert_eq!(v["stamp_mismatches"], 2);
        assert_eq!(v["dirty_rect_count"], 4);
        assert_eq!(v["dirty_bytes"], 8192);
        assert_eq!(v["move_rect_count"], 1);
        assert_eq!(v["keyframe_wait_frames"], 17);

        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 19, "unexpected field count: {keys:?}");
    }

    #[test]
    fn absent_dirty_metadata_is_omitted_not_zero() {
        // `None` must vanish from the line entirely: a consumer that read a 0 here
        // would conflate "metadata unavailable" with "nothing changed", which is
        // exactly the fast-path predicate hazard the design calls out.
        let r = FrameRecord::new();
        let v: Value = serde_json::from_str(&to_line(&r)).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("dirty_rect_count"));
        assert!(!obj.contains_key("dirty_bytes"));
        assert!(!obj.contains_key("move_rect_count"));
        assert!(!obj.contains_key("keyframe_wait_frames"));
    }

    #[test]
    fn a_line_has_no_embedded_newline() {
        // The whole file is JSONL; a stray newline would split one record in two.
        let mut h = Header::new();
        h.adapter = "Some\nAdapter".into();
        let line = to_line(&h);
        assert!(!line.contains('\n'), "line: {line}");
    }

    #[test]
    fn an_input_line_names_the_kind_and_both_stamps() {
        let r = InputEventRecord::new(9, 0x41, "down", 500, 620);
        let v: Value = serde_json::from_str(&to_line(&r)).unwrap();
        assert_eq!(v["record"], "input");
        assert_eq!(v["seq"], 9);
        assert_eq!(v["vk"], 0x41);
        assert_eq!(v["kind"], "down");
        assert_eq!(v["recv_qpc_us"], 500);
        assert_eq!(v["injected_qpc_us"], 620);
        assert_eq!(v.as_object().unwrap().len(), 6);
    }

    #[test]
    fn the_header_publishes_the_frequency_and_the_encoder_choice() {
        let mut h = Header::new();
        h.qpc_frequency = 10_000_000;
        h.encoder = "NVIDIA H.264 Encoder MFT".into();
        h.encoder_kind = "async-hardware";
        h.codec_api_applied = vec!["AVLowLatencyMode".into()];
        h.codec_api_refused = vec!["AVEncMPVGOPSize".into()];
        let v: Value = serde_json::from_str(&to_line(&h)).unwrap();
        assert_eq!(v["record"], "header");
        assert_eq!(v["schema"], SCHEMA);
        assert_eq!(v["wire_version"], WIRE_VERSION);
        assert_eq!(v["qpc_frequency"], 10_000_000);
        assert_eq!(v["encoder"], "NVIDIA H.264 Encoder MFT");
        assert_eq!(v["encoder_kind"], "async-hardware");
        assert_eq!(v["codec_api_applied"][0], "AVLowLatencyMode");
        assert_eq!(v["codec_api_refused"][0], "AVEncMPVGOPSize");
    }
}
