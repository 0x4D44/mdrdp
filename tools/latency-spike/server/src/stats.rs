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
    /// Which capture source produced this run: `dxgi` (Desktop Duplication) or
    /// `idd` (the driver's shared texture pool). The two have a ~7.5 ms difference
    /// in their present→acquire stage, so a figure that does not name its source
    /// cannot be compared with one that does.
    pub source: &'static str,
    /// Meaningful only when `source` is `dxgi`: the IDD pool is found by name.
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
    /// Idle gap that arms the Increment 3 pixel diff, in milliseconds; 0 means the
    /// diff was disabled for this run. Another telemetry-tuned knob on the same
    /// terms as `rect_max_count`: a measurement compared against another must be
    /// able to say which threshold each ran under.
    pub diff_idle_gap_ms: u32,
    /// How SPS/PPS reach the wire: see `annexb`.
    pub parameter_set_route: &'static str,
    /// Whether the out-of-band `MF_MT_MPEG_SEQUENCE_HEADER` was available as a
    /// fallback source of parameter sets.
    pub sequence_header_available: bool,
}

/// Schema 5: the Increment 3 pixel diff becomes visible — the header gains
/// `diff_idle_gap_ms`, frame rows gain the cumulative `diff_runs`/`diff_hits`/
/// `diff_us_total`, and rects rows gain `from_diff`, which says whether a rect
/// message was metadata-driven or measured against the previous frame.
/// (Schema 4: the header gained `source`, naming which capture path the run used.
/// Schema 3: the header gained `rect_max_count`/`rect_max_bytes`, frame rows
/// gained `dropped_rects`, and `record: "rects"` rows exist at all.)
pub const SCHEMA: u32 = 5;
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
            source: "unknown",
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
            diff_idle_gap_ms: 0,
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
    /// Cumulative count of idle-regime pixel diffs attempted (HLD §6b).
    pub diff_runs: u64,
    /// Of those, how many produced a usable measured delta. `diff_runs - diff_hits`
    /// is the scene-cut cost: a diff that ran, found too much change, and left the
    /// frame on the codec path it was already taking.
    pub diff_hits: u64,
    /// Cumulative microseconds spent inside those diffs. Carried on every frame row
    /// (like `dropped_rects`) so the cost is attributable even when the frame that
    /// paid it produced no rect message.
    pub diff_us_total: u64,
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
    /// Where these rects came from: `false` = the source's own change metadata
    /// satisfied the predicate, `true` = the metadata missed and the Increment 3
    /// pixel diff measured the delta instead. The two arms have different costs and
    /// different trust arguments, so a row that cannot say which one it is cannot be
    /// analysed.
    pub from_diff: bool,
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
        // All three diff counters distinct: a fixture where `diff_runs` equalled
        // `diff_hits` could not tell the two apart if they were swapped, and the
        // difference between them is the whole scene-cut story.
        r.diff_runs = 11;
        r.diff_hits = 6;
        r.diff_us_total = 90_000;
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
        assert_eq!(v["diff_runs"], 11);
        assert_eq!(v["diff_hits"], 6);
        assert_eq!(v["diff_us_total"], 90_000);
        assert_eq!(v["dirty_rect_count"], 4);
        assert_eq!(v["dirty_bytes"], 8192);
        assert_eq!(v["move_rect_count"], 1);
        assert_eq!(v["keyframe_wait_frames"], 17);

        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 22, "unexpected field count: {keys:?}");
    }

    #[test]
    fn a_diff_counter_is_serialised_even_when_zero() {
        // The three diff counters are cumulative, not optional: a run whose diff
        // never fired must still say so, or "the diff was off" and "the diff never
        // triggered" become the same line. (Contrast the `Option` dirty fields
        // below, where absence genuinely means "unknown".)
        let r = FrameRecord::new();
        let v: Value = serde_json::from_str(&to_line(&r)).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj["diff_runs"], 0);
        assert_eq!(obj["diff_hits"], 0);
        assert_eq!(obj["diff_us_total"], 0);
    }

    #[test]
    fn a_rects_line_names_its_stamps_and_which_arm_produced_it() {
        let mut r = RectRecord::new();
        // Distinct values throughout: `rect_count` and `rect_bytes` sharing a number
        // would hide a swap, and so would equal pack stamps.
        r.frame = 12;
        r.rect_count = 3;
        r.rect_bytes = 6144;
        r.pack_start_us = 2000;
        r.pack_end_us = 2100;
        r.send_done_us = 2200;
        r.dropped_rects = 4;
        // The pixel-diff arm. `false` is the metadata arm, and the default, so the
        // fixture sets the value that a forgotten assignment would not produce.
        r.from_diff = true;

        let v: Value = serde_json::from_str(&to_line(&r)).unwrap();
        assert_eq!(v["record"], "rects");
        assert_eq!(v["frame"], 12);
        assert_eq!(v["rect_count"], 3);
        assert_eq!(v["rect_bytes"], 6144);
        assert_eq!(v["pack_start_us"], 2000);
        assert_eq!(v["pack_end_us"], 2100);
        assert_eq!(v["send_done_us"], 2200);
        assert_eq!(v["dropped_rects"], 4);
        assert_eq!(v["from_diff"], true);
        // A fresh record is the metadata arm until something says otherwise.
        assert!(to_line(&RectRecord::new()).contains(r#""from_diff":false"#));

        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 9, "unexpected field count: {keys:?}");
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
    fn the_header_publishes_the_frequency_the_encoder_choice_and_the_source() {
        let mut h = Header::new();
        h.qpc_frequency = 10_000_000;
        h.encoder = "NVIDIA H.264 Encoder MFT".into();
        h.encoder_kind = "async-hardware";
        h.codec_api_applied = vec!["AVLowLatencyMode".into()];
        h.codec_api_refused = vec!["AVEncMPVGOPSize".into()];
        // Which capture path produced the run. Without it an archived file cannot
        // be told from the control arm it will be compared against.
        h.source = "idd";
        // The tuning knobs, all three distinct: a fixture reusing one number could
        // not catch the diff gap being written from the rect predicate's value.
        h.rect_max_count = 32;
        h.rect_max_bytes = 98_304;
        h.diff_idle_gap_ms = 100;
        let v: Value = serde_json::from_str(&to_line(&h)).unwrap();
        assert_eq!(v["record"], "header");
        assert_eq!(v["schema"], SCHEMA);
        assert_eq!(v["wire_version"], WIRE_VERSION);
        assert_eq!(v["qpc_frequency"], 10_000_000);
        assert_eq!(v["encoder"], "NVIDIA H.264 Encoder MFT");
        assert_eq!(v["encoder_kind"], "async-hardware");
        assert_eq!(v["codec_api_applied"][0], "AVLowLatencyMode");
        assert_eq!(v["codec_api_refused"][0], "AVEncMPVGOPSize");
        assert_eq!(v["source"], "idd");
        assert_eq!(v["rect_max_count"], 32);
        assert_eq!(v["rect_max_bytes"], 98_304);
        assert_eq!(v["diff_idle_gap_ms"], 100);

        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 24, "unexpected field count: {keys:?}");
    }
}
