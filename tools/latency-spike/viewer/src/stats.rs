//! The client-side stats file: one JSON object per line.
//!
//! Every `*_us` field is an absolute stamp on **this machine's** clock (see
//! [`crate::clock`]). Differences between fields on one line are the stage costs.
//! Server stamps are on the server's QPC and are never comparable to these — the two
//! clocks are not synchronised, by design — so server lines are passed through in a
//! wrapper that keeps them visibly separate rather than merged into the same shape.
//!
//! Lines are flushed individually. The operator ends a run with Ctrl-C or by closing
//! the window, and a buffered tail lost at that moment is a measurement lost.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::Serialize;

/// Bumped whenever a field changes meaning, so an archived file stays readable.
/// Schema 2: frames gained `seq` — the server's capture sequence number from
/// `MSG_VIDEO_SEQ`, the exact cross-file join key (`null` on a v1 stream).
pub const SCHEMA: u32 = 2;

/// First line of the file: what this run was, and on which clock.
#[derive(Debug, Clone, Serialize)]
pub struct Header {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub schema: u32,
    /// Which clock every `*_us` field below is on.
    pub clock: &'static str,
    pub connect: String,
    pub input: String,
    /// Whether `mdrdp::h264::hardware_decoder()` returned a decoder on this build.
    /// Without one, every access unit is a `decode_error` and the window stays black.
    pub decoder: bool,
}

impl Header {
    pub fn new(clock: &'static str, connect: String, input: String, decoder: bool) -> Self {
        Self {
            kind: "client-header",
            schema: SCHEMA,
            clock,
            connect,
            input,
            decoder,
        }
    }
}

/// The client-side stage stamps of one frame, carried from the decode thread to the
/// window thread so `present_done_us` can close the line where it is actually taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameStamps {
    /// Arrival ordinal on this client (counts every video message seen).
    pub frame: u64,
    /// The server's capture sequence number, when the stream carried one.
    pub seq: Option<u64>,
    /// When the `read` that completed this message returned.
    pub recv_done_us: u64,
    /// Immediately before `H264Decoder::decode`.
    pub decode_in_us: u64,
    /// Immediately after it returned.
    pub decode_out_us: u64,
    pub au_bytes: usize,
    pub keyframe: bool,
    pub width: u32,
    pub height: u32,
}

/// One frame, end to end on the client.
///
/// `present_done_us` is `null` exactly when `dropped` is true: the decode thread had a
/// newer frame before the window thread ever presented this one. Dropped frames are
/// recorded rather than discarded — a file that silently omits them would overstate
/// how well the pipeline kept up.
#[derive(Debug, Clone, Serialize)]
pub struct FrameRecord {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub frame: u64,
    /// `null` only on a v1 stream that sent bare `MSG_VIDEO`.
    pub seq: Option<u64>,
    pub recv_done_us: u64,
    pub decode_in_us: u64,
    pub decode_out_us: u64,
    pub present_done_us: Option<u64>,
    pub au_bytes: usize,
    pub keyframe: bool,
    pub width: u32,
    pub height: u32,
    pub dropped: bool,
}

impl FrameRecord {
    pub fn new(stamps: &FrameStamps, present_done_us: Option<u64>) -> Self {
        Self {
            kind: "frame",
            frame: stamps.frame,
            seq: stamps.seq,
            recv_done_us: stamps.recv_done_us,
            decode_in_us: stamps.decode_in_us,
            decode_out_us: stamps.decode_out_us,
            present_done_us,
            au_bytes: stamps.au_bytes,
            keyframe: stamps.keyframe,
            width: stamps.width,
            height: stamps.height,
            dropped: present_done_us.is_none(),
        }
    }
}

/// An access unit the decoder refused.
///
/// Expected at the head of a stream — the server's first frames can reach us before
/// the parameter sets — and self-healing at the next keyframe. A run of these that
/// never stops is the real signal, which is why every one is recorded.
#[derive(Debug, Clone, Serialize)]
pub struct DecodeErrorRecord {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub frame: u64,
    pub recv_done_us: u64,
    pub au_bytes: usize,
    pub keyframe: bool,
    pub detail: String,
}

impl DecodeErrorRecord {
    pub fn new(
        frame: u64,
        recv_done_us: u64,
        au_bytes: usize,
        keyframe: bool,
        detail: String,
    ) -> Self {
        Self {
            kind: "decode_error",
            frame,
            recv_done_us,
            au_bytes,
            keyframe,
            detail,
        }
    }
}

/// One keystroke this viewer put on the input channel.
///
/// `seq` is the record's own sequence number, echoed by the server's `input` stats
/// line, which is what lets a keystroke be followed across the two files despite the
/// two clocks never being comparable.
#[derive(Debug, Clone, Serialize)]
pub struct InputRecord {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub seq: u32,
    pub vk: u16,
    pub transition: &'static str,
    pub sent_us: u64,
}

impl InputRecord {
    pub fn new(seq: u32, vk: u16, transition: &'static str, sent_us: u64) -> Self {
        Self {
            kind: "input",
            seq,
            vk,
            transition,
            sent_us,
        }
    }
}

/// Serialise one record as a JSONL line (no trailing newline).
///
/// Falls back to a minimal error object rather than panicking: losing one stats line
/// must never take the viewer down.
pub fn to_line<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|e| format!(r#"{{"type":"error","detail":"serialize: {e}"}}"#))
}

/// Wrap a server stats line for the client's file, keeping its bytes intact.
///
/// The server's own lines are already JSON and use `record` as their discriminator,
/// where ours use `type`; merging them into one namespace would make a reader guess.
/// So a well-formed line is nested verbatim under `line` — the original bytes are
/// concatenated, not re-serialised, so nothing is reordered or reformatted — and
/// anything that is not valid JSON is escaped into `text` instead of being dropped.
pub fn server_line(payload: &[u8]) -> String {
    match std::str::from_utf8(payload) {
        Ok(text) if serde_json::from_str::<serde_json::Value>(text).is_ok() => {
            format!(r#"{{"type":"server","line":{text}}}"#)
        }
        Ok(text) => to_line(&serde_json::json!({"type": "server_raw", "text": text})),
        Err(_) => to_line(&serde_json::json!({
            "type": "server_raw",
            "text": null,
            "bytes": payload.len(),
        })),
    }
}

/// The stats file, or a sink that discards everything when `--out` was not given.
#[derive(Debug)]
pub struct StatsLog {
    out: Mutex<Option<BufWriter<File>>>,
    /// A write failure is reported once. A per-frame warning would bury the run's own
    /// output under thousands of identical lines.
    warned: AtomicBool,
}

impl StatsLog {
    pub fn create(path: Option<&str>) -> std::io::Result<Self> {
        let out = match path {
            Some(p) => Some(BufWriter::new(File::create(p)?)),
            None => None,
        };
        Ok(Self {
            out: Mutex::new(out),
            warned: AtomicBool::new(false),
        })
    }

    /// A log that writes nowhere — for tests and for a run without `--out`.
    pub fn discarding() -> Self {
        Self {
            out: Mutex::new(None),
            warned: AtomicBool::new(false),
        }
    }

    pub fn record<T: Serialize>(&self, value: &T) {
        self.write_line(&to_line(value));
    }

    pub fn write_line(&self, line: &str) {
        let mut guard = self
            .out
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(file) = guard.as_mut() else {
            return;
        };
        // Flushed per line: see the module docs.
        let result = writeln!(file, "{line}").and_then(|()| file.flush());
        if let Err(e) = result {
            if !self.warned.swap(true, Ordering::Relaxed) {
                eprintln!("stats: write failed, further failures are silent: {e}");
            }
        }
    }

    pub fn flush(&self) {
        let mut guard = self
            .out
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(file) = guard.as_mut() {
            let _ = file.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn parse(line: &str) -> Value {
        serde_json::from_str(line).expect("a stats line must be valid JSON")
    }

    #[test]
    fn a_presented_frame_carries_every_stage_and_is_not_dropped() {
        // Every stamp a different value: a fixture where they agreed could not catch
        // two fields being written from the same variable.
        let stamps = FrameStamps {
            frame: 7,
            seq: Some(42),
            recv_done_us: 1_000,
            decode_in_us: 1_100,
            decode_out_us: 3_400,
            au_bytes: 4_321,
            keyframe: true,
            width: 1920,
            height: 1080,
        };
        let v = parse(&to_line(&FrameRecord::new(&stamps, Some(5_000))));
        assert_eq!(v["type"], "frame");
        assert_eq!(v["frame"], 7);
        assert_eq!(
            v["seq"], 42,
            "seq must be the server's number, not the ordinal"
        );
        assert_eq!(v["recv_done_us"], 1_000);
        assert_eq!(v["decode_in_us"], 1_100);
        assert_eq!(v["decode_out_us"], 3_400);
        assert_eq!(v["present_done_us"], 5_000);
        assert_eq!(v["au_bytes"], 4_321);
        assert_eq!(v["keyframe"], true);
        assert_eq!(v["width"], 1920);
        assert_eq!(v["height"], 1080);
        assert_eq!(v["dropped"], false);
    }

    #[test]
    fn a_frame_with_no_present_stamp_is_marked_dropped() {
        let stamps = FrameStamps {
            frame: 8,
            seq: None,
            recv_done_us: 1,
            decode_in_us: 2,
            decode_out_us: 3,
            au_bytes: 4,
            keyframe: false,
            width: 640,
            height: 360,
        };
        let v = parse(&to_line(&FrameRecord::new(&stamps, None)));
        assert_eq!(v["present_done_us"], Value::Null);
        assert_eq!(v["dropped"], true);
        assert_eq!(v["seq"], Value::Null, "a v1 stream has no seq to invent");
    }

    #[test]
    fn a_server_line_is_nested_byte_for_byte() {
        // Field order and spacing preserved: the wrapper concatenates, it does not
        // re-serialise. A round trip through serde_json would reorder nothing here but
        // would drop the spacing, which is how this test tells the two apart.
        let raw = br#"{"record":"frame", "frame":1,"au_bytes":900}"#;
        let wrapped = server_line(raw);
        assert_eq!(
            wrapped,
            r#"{"type":"server","line":{"record":"frame", "frame":1,"au_bytes":900}}"#
        );
        let v = parse(&wrapped);
        assert_eq!(v["type"], "server");
        assert_eq!(v["line"]["record"], "frame");
        assert_eq!(v["line"]["au_bytes"], 900);
    }

    #[test]
    fn a_server_line_that_is_not_json_is_escaped_rather_than_dropped() {
        let wrapped = server_line(b"input: SendInput injected 0 of 1 events");
        let v = parse(&wrapped);
        assert_eq!(v["type"], "server_raw");
        assert_eq!(v["text"], "input: SendInput injected 0 of 1 events");
    }

    #[test]
    fn a_server_line_that_is_not_utf8_still_produces_a_valid_line() {
        let v = parse(&server_line(&[0xFF, 0xFE, 0x00]));
        assert_eq!(v["type"], "server_raw");
        assert_eq!(v["bytes"], 3);
    }

    #[test]
    fn the_discarding_log_accepts_writes_without_a_file() {
        let log = StatsLog::discarding();
        log.write_line("anything");
        log.flush();
    }
}
