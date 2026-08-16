//! Session statistics: what the connection is actually doing, while it does it.
//!
//! Two of the client's requirements are measurement requirements, not features:
//!
//! * *"Super low latency **that doesn't degrade over time**"* — a number that is good at
//!   connect and bad an hour later is the failure being guarded against, so a single
//!   current reading cannot answer it. [`Latency`] freezes a baseline from the first
//!   samples of the session and reports it alongside the recent window, which makes drift
//!   readable at a glance rather than requiring someone to remember what it used to be.
//! * *"Stats — how effective bitmap cache is"* — [`CacheStats`] counts hits and misses,
//!   but the honest measure of effectiveness is bytes: a hit on a large region is worth
//!   far more than a hit on a small one, so the ratio that matters is bytes served from
//!   the cache against bytes painted overall.
//!
//! Everything here is pure and synchronous. Percentiles are computed exactly, by sorting a
//! copy of a small fixed-capacity window, rather than estimated — at this sample count the
//! cost is irrelevant, and an exact number is one less thing to disbelieve when a
//! measurement looks surprising.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// How many recent samples the rolling window holds.
///
/// Small enough that an exact sort is free, large enough that a p99 means something.
pub const WINDOW: usize = 512;

/// How many samples the frozen baseline is taken from.
///
/// Taken from the start of the session, once, and never updated — that is the whole point.
pub const BASELINE_SAMPLES: usize = 100;

/// A rolling window of microsecond samples with exact percentiles.
#[derive(Debug, Clone, Default)]
pub struct Latency {
    recent: VecDeque<u32>,
    /// Filled once, from the first [`BASELINE_SAMPLES`] samples, then frozen.
    baseline: Option<Percentiles>,
    baseline_acc: Vec<u32>,
    count: u64,
    min: Option<u32>,
    max: u32,
}

/// A percentile summary, in microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Percentiles {
    pub p50: u32,
    pub p95: u32,
    pub p99: u32,
}

impl Latency {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one sample, in microseconds.
    pub fn record(&mut self, micros: u32) {
        self.count += 1;
        self.min = Some(self.min.map_or(micros, |m| m.min(micros)));
        self.max = self.max.max(micros);

        if self.recent.len() == WINDOW {
            self.recent.pop_front();
        }
        self.recent.push_back(micros);

        if self.baseline.is_none() {
            self.baseline_acc.push(micros);
            if self.baseline_acc.len() >= BASELINE_SAMPLES {
                self.baseline = Some(percentiles(&self.baseline_acc));
                // The accumulator has done its job; do not hold the memory for the rest
                // of the session.
                self.baseline_acc = Vec::new();
            }
        }
    }

    /// Percentiles over the recent window. `None` until anything has been recorded.
    pub fn recent(&self) -> Option<Percentiles> {
        if self.recent.is_empty() {
            return None;
        }
        let samples: Vec<u32> = self.recent.iter().copied().collect();
        Some(percentiles(&samples))
    }

    /// The rolling window itself, oldest first.
    ///
    /// The diagnostics chart plots the individual samples, not just their percentiles;
    /// this is a copy so the caller never holds a lock while drawing.
    pub fn recent_samples(&self) -> Vec<u32> {
        self.recent.iter().copied().collect()
    }

    /// The frozen opening reading. `None` until [`BASELINE_SAMPLES`] have arrived.
    pub fn baseline(&self) -> Option<Percentiles> {
        self.baseline
    }

    /// How much the median has moved since the baseline, as a signed microsecond delta.
    ///
    /// Positive means slower than the session started. This is the drift number.
    pub fn drift_us(&self) -> Option<i64> {
        let base = self.baseline?;
        let now = self.recent()?;
        Some(i64::from(now.p50) - i64::from(base.p50))
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn min(&self) -> Option<u32> {
        self.min
    }

    pub fn max(&self) -> u32 {
        self.max
    }
}

/// Exact nearest-rank percentiles. `samples` must be non-empty.
fn percentiles(samples: &[u32]) -> Percentiles {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    Percentiles {
        p50: nearest_rank(&sorted, 50),
        p95: nearest_rank(&sorted, 95),
        p99: nearest_rank(&sorted, 99),
    }
}

/// Nearest-rank: the smallest value at or above which `p` percent of samples fall.
fn nearest_rank(sorted: &[u32], p: u32) -> u32 {
    debug_assert!(!sorted.is_empty());
    // ceil(p/100 * n), 1-based, then clamped into the slice.
    let n = sorted.len() as u64;
    let rank = (u64::from(p) * n).div_ceil(100).max(1);
    let idx = (rank - 1).min(n - 1) as usize;
    sorted[idx]
}

/// Offscreen bitmap cache effectiveness.
///
/// A hit means a region was painted from the cache instead of arriving on the wire; the
/// bytes it saved are the point of the cache existing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: u64,
    pub evictions: u64,
    /// Bytes painted from cache entries.
    pub bytes_served: u64,
    /// Bytes written into the cache.
    pub bytes_stored: u64,
    /// Bytes painted from decoded wire data.
    pub bytes_from_wire: u64,
}

impl CacheStats {
    /// Hits as a fraction of cache lookups. `None` when nothing has been looked up.
    pub fn hit_rate(&self) -> Option<f64> {
        let looked_up = self.hits + self.misses;
        if looked_up == 0 {
            return None;
        }
        Some(self.hits as f64 / looked_up as f64)
    }

    /// Bytes served from cache as a fraction of all bytes painted.
    ///
    /// This is the honest effectiveness number: a high hit rate on tiny regions saves
    /// nothing, and this ratio says so where [`hit_rate`](Self::hit_rate) would not.
    pub fn byte_savings(&self) -> Option<f64> {
        let painted = self.bytes_served + self.bytes_from_wire;
        if painted == 0 {
            return None;
        }
        Some(self.bytes_served as f64 / painted as f64)
    }
}

/// What one bitmap-cache slot is currently holding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SlotState {
    /// Never filled, or filled with nothing.
    #[default]
    Empty,
    /// Holds a bitmap the server can still reference.
    Live,
    /// The server dropped it. The counters it earned while live are kept.
    Evicted,
}

impl SlotState {
    /// A stable, payload-free name, for a report or a legend.
    pub fn as_str(self) -> &'static str {
        match self {
            SlotState::Empty => "empty",
            SlotState::Live => "live",
            SlotState::Evicted => "evicted",
        }
    }
}

/// The codec attributed to a slot nothing identifiable has filled yet.
pub const UNKNOWN_CODEC: &str = "unknown";

/// One bitmap-cache slot, as the diagnostics grid needs to draw it.
///
/// The aggregate [`CacheStats`] answers "is the cache working"; this answers "which slots
/// are earning their keep", which is the question a thrashing cache is diagnosed by.
///
/// `bytes_served` is accumulated per hit rather than derived as `hits * bytes_stored`: a
/// slot refilled at a different size would otherwise have its whole history restated at
/// the newest size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotStat {
    pub slot: u16,
    pub width: u16,
    pub height: u16,
    /// The codec reported for the update that filled this slot.
    pub codec: &'static str,
    pub state: SlotState,
    pub hits: u32,
    /// Bytes painted out of this slot, accumulated one stored-size per hit.
    pub bytes_served: u64,
    /// Bytes the current occupant of the slot takes.
    pub bytes_stored: u64,
    pub last_hit: Option<Instant>,
}

impl SlotStat {
    /// A slot that exists but holds nothing.
    pub fn empty(slot: u16) -> Self {
        Self {
            slot,
            width: 0,
            height: 0,
            codec: UNKNOWN_CODEC,
            state: SlotState::Empty,
            hits: 0,
            bytes_served: 0,
            bytes_stored: 0,
            last_hit: None,
        }
    }
}

/// Every cache slot this session has touched, keyed by slot id.
///
/// Ordered, so the grid draws in slot order without the caller sorting, and so two
/// snapshots taken a second apart put the same slot in the same place.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlotStats {
    slots: BTreeMap<u16, SlotStat>,
}

impl SlotStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// A `SurfaceToCache` filled (or refilled) the slot.
    ///
    /// Size, codec and stored bytes describe the new occupant; the hit history the slot
    /// already earned is deliberately kept, because the interesting question about a
    /// repeatedly refilled slot is how much it has served over the session.
    pub fn fill(
        &mut self,
        slot: u16,
        width: u16,
        height: u16,
        codec: &'static str,
        bytes_stored: u64,
    ) {
        let entry = self
            .slots
            .entry(slot)
            .or_insert_with(|| SlotStat::empty(slot));
        entry.width = width;
        entry.height = height;
        entry.codec = codec;
        entry.state = SlotState::Live;
        entry.bytes_stored = bytes_stored;
    }

    /// A `CacheToSurface` painted this slot into `times` destinations.
    ///
    /// One hit per destination, matching [`CacheStats`]: a single PDU that stamps a cached
    /// tile in twelve places saved twelve regions' worth of wire traffic.
    ///
    /// A slot we never saw filled is ignored rather than invented: without a fill there is
    /// no size to serve, so a record for it would carry zero bytes and misreport the grid.
    pub fn hit(&mut self, slot: u16, times: u32) {
        self.hit_at(slot, times, Instant::now());
    }

    /// [`hit`](Self::hit) with the clock supplied, so age-based behaviour is testable.
    pub fn hit_at(&mut self, slot: u16, times: u32, at: Instant) {
        if times == 0 {
            return;
        }
        let Some(entry) = self.slots.get_mut(&slot) else {
            return;
        };
        entry.hits = entry.hits.saturating_add(times);
        entry.bytes_served = entry
            .bytes_served
            .saturating_add(entry.bytes_stored.saturating_mul(u64::from(times)));
        entry.last_hit = Some(at);
    }

    /// An `EvictCacheEntry` dropped the slot.
    ///
    /// The counters stay: what a slot served before it was evicted is exactly the evidence
    /// that says whether the eviction cost anything.
    pub fn evict(&mut self, slot: u16) {
        if let Some(entry) = self.slots.get_mut(&slot) {
            entry.state = SlotState::Evicted;
        }
    }

    pub fn get(&self, slot: u16) -> Option<&SlotStat> {
        self.slots.get(&slot)
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Every touched slot, in slot order.
    pub fn iter(&self) -> impl Iterator<Item = &SlotStat> {
        self.slots.values()
    }
}

/// A cloneable handle on the live [`SlotStats`].
///
/// The graphics thread writes on every cache PDU; the diagnostics UI polls
/// [`snapshot`](Self::snapshot) at 1 Hz from another thread. Same idiom, and same
/// poisoned-lock recovery, as [`StatsHandle`] and [`crate::gfx::GfxStatsHandle`].
#[derive(Debug, Clone, Default)]
pub struct SlotStatsHandle(Arc<Mutex<SlotStats>>);

impl SlotStatsHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// A point-in-time copy. Never aliases later mutation.
    pub fn snapshot(&self) -> SlotStats {
        self.lock().clone()
    }

    pub fn update<F: FnOnce(&mut SlotStats)>(&self, f: F) {
        f(&mut self.lock());
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SlotStats> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Everything worth showing about a running session.
#[derive(Debug, Clone, Default)]
pub struct SessionStats {
    pub latency: Latency,
    /// Client decode-and-paint work per painted frame: the summed PDU `process`
    /// time between one generation bump and the next. CPU cost, not wall time —
    /// gaps waiting for the network are deliberately excluded.
    pub decode: Latency,
    /// Painted-into-the-store → presented-by-the-window, per frame. This is the
    /// cross-thread handoff: the wake, the redraw, the scale/convert pass and the
    /// present call. The input round trip in `latency` stops at the paint, so
    /// this segment is exactly what that number hides.
    pub present: Latency,
    /// The generation whose paint is still waiting for its present, and when it
    /// painted. One slot: a newer paint overwrites an unpresented older one, so
    /// the measurement always tracks the freshest content.
    pending_present: Option<(u64, Instant)>,
    pub cache: CacheStats,
    pub frames: u64,
    pub bytes_in: u64,
    pub decode_errors: u64,
    pub undecoded_regions: u64,
    /// Surface codec identifiers and the number of updates observed for each.
    pub codecs: BTreeMap<String, u64>,
    /// Bytes painted per codec, keyed like `codecs`. The title bar diffs this
    /// between refreshes to name the codec currently carrying the picture.
    pub codec_painted: BTreeMap<String, u64>,
}

impl SessionStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// The session thread painted `generation` into the store just now.
    pub fn mark_painted(&mut self, generation: u64) {
        self.pending_present = Some((generation, Instant::now()));
    }

    /// The window put `generation` on screen; close the pending handoff if this
    /// present covers it. A newer generation covers an older pending one — the
    /// screen now shows content at least that new.
    pub fn mark_presented(&mut self, generation: u64) {
        if let Some((painted, at)) = self.pending_present
            && generation >= painted
        {
            let micros = u32::try_from(at.elapsed().as_micros()).unwrap_or(u32::MAX);
            self.present.record(micros);
            self.pending_present = None;
        }
    }

    /// The overlay text, one line per row.
    ///
    /// Kept here rather than in the renderer so the wording is testable without a window.
    pub fn overlay_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();

        match self.latency.recent() {
            Some(p) => {
                lines.push(format!(
                    "latency  p50 {:.1}ms  p95 {:.1}ms  p99 {:.1}ms  max {:.1}ms",
                    ms(p.p50),
                    ms(p.p95),
                    ms(p.p99),
                    ms(self.latency.max())
                ));
                match self.latency.drift_us() {
                    Some(d) if d > 0 => lines.push(format!(
                        "         drift +{:.1}ms since connect  (n={})",
                        d as f64 / 1000.0,
                        self.latency.count()
                    )),
                    Some(d) => lines.push(format!(
                        "         drift {:.1}ms since connect  (n={})",
                        d as f64 / 1000.0,
                        self.latency.count()
                    )),
                    None => lines.push(format!(
                        "         baseline pending  (n={}/{})",
                        self.latency.count(),
                        BASELINE_SAMPLES
                    )),
                }
            }
            None => lines.push("latency  no samples yet".to_string()),
        }

        // The client-side split of that round trip: decode work and the
        // paint→present handoff. What is left over lives on the server and the wire.
        if let (Some(d), Some(p)) = (self.decode.recent(), self.present.recent()) {
            lines.push(format!(
                "pipeline decode p50 {:.1}ms p95 {:.1}ms  present p50 {:.1}ms p95 {:.1}ms",
                ms(d.p50),
                ms(d.p95),
                ms(p.p50),
                ms(p.p95)
            ));
        }

        let hit = match self.cache.hit_rate() {
            Some(r) => format!("{:.0}%", r * 100.0),
            None => "n/a".to_string(),
        };
        let saved = match self.cache.byte_savings() {
            Some(r) => format!("{:.0}%", r * 100.0),
            None => "n/a".to_string(),
        };
        lines.push(format!(
            "cache    {hit} hit  {saved} of pixels  ({} hits, {} misses, {} entries)",
            self.cache.hits, self.cache.misses, self.cache.entries
        ));
        lines.push(format!(
            "frames   {}  in {}",
            self.frames,
            bytes_human(self.bytes_in)
        ));
        let codecs = if self.codecs.is_empty() {
            "no surface updates yet".to_owned()
        } else {
            self.codecs
                .iter()
                .map(|(name, count)| format!("{}:{count}", display_codec_name(name)))
                .collect::<Vec<_>>()
                .join("  ")
        };
        lines.push(format!("codec    {codecs}"));

        // Only shown when non-zero: a permanent line of zeroes trains the eye to skip it,
        // and these two are exactly the numbers that must not be skipped.
        if self.decode_errors > 0 || self.undecoded_regions > 0 {
            lines.push(format!(
                "STALE    {} decode errors  {} undecoded regions",
                self.decode_errors, self.undecoded_regions
            ));
        }
        lines
    }
}

fn display_codec_name(name: &str) -> &str {
    match name.strip_prefix("WireToSurface2/").unwrap_or(name) {
        "RemoteFxProgressive" => "RFX Progressive",
        other => other,
    }
}

/// A cloneable handle on the live [`SessionStats`].
///
/// The session thread writes; the window thread reads to draw the overlay. Mirrors
/// [`crate::gfx::GfxStatsHandle`], including its treatment of a poisoned lock: a panic
/// while counting must not take down a working session, so the counters are recovered
/// rather than propagated.
#[derive(Debug, Clone, Default)]
pub struct StatsHandle(Arc<Mutex<SessionStats>>);

impl StatsHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// A point-in-time copy. Never aliases later mutation.
    pub fn snapshot(&self) -> SessionStats {
        self.lock().clone()
    }

    pub fn update<F: FnOnce(&mut SessionStats)>(&self, f: F) {
        f(&mut self.lock());
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SessionStats> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn ms(micros: u32) -> f64 {
    f64::from(micros) / 1000.0
}

fn bytes_human(n: u64) -> String {
    const K: u64 = 1024;
    match n {
        n if n < K => format!("{n}B"),
        n if n < K * K => format!("{:.1}KiB", n as f64 / K as f64),
        n if n < K * K * K => format!("{:.1}MiB", n as f64 / (K * K) as f64),
        n => format!("{:.2}GiB", n as f64 / (K * K * K) as f64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_matches_hand_computed_values() {
        // 1..=100 sorted. Hand-computed, not derived from the code under test:
        // p50 -> rank ceil(0.50*100)=50 -> value 50. p95 -> rank 95 -> 95. p99 -> 99.
        let sorted: Vec<u32> = (1..=100).collect();
        assert_eq!(nearest_rank(&sorted, 50), 50);
        assert_eq!(nearest_rank(&sorted, 95), 95);
        assert_eq!(nearest_rank(&sorted, 99), 99);
    }

    #[test]
    fn a_single_sample_is_every_percentile() {
        let sorted = [7];
        assert_eq!(nearest_rank(&sorted, 50), 7);
        assert_eq!(nearest_rank(&sorted, 99), 7);
    }

    #[test]
    fn percentiles_do_not_depend_on_input_order() {
        let ascending: Vec<u32> = (1..=100).collect();
        let mut descending = ascending.clone();
        descending.reverse();
        assert_eq!(percentiles(&ascending), percentiles(&descending));
    }

    #[test]
    fn the_window_keeps_only_the_most_recent_samples() {
        let mut l = Latency::new();
        // Fill the window with a slow value, then completely replace it with a fast one.
        for _ in 0..WINDOW {
            l.record(10_000);
        }
        assert_eq!(l.recent().unwrap().p50, 10_000);
        for _ in 0..WINDOW {
            l.record(1_000);
        }
        assert_eq!(
            l.recent().unwrap().p50,
            1_000,
            "the old samples must have aged out"
        );
        assert_eq!(
            l.count(),
            (WINDOW * 2) as u64,
            "but the lifetime count is all of them"
        );
        assert_eq!(l.min(), Some(1_000));
        assert_eq!(l.max(), 10_000, "lifetime max survives the window");
    }

    #[test]
    fn the_baseline_freezes_and_drift_shows_degradation() {
        let mut l = Latency::new();
        // A fast opening: every baseline sample is 1ms.
        for _ in 0..BASELINE_SAMPLES {
            l.record(1_000);
        }
        let base = l.baseline().expect("baseline should be frozen by now");
        assert_eq!(base.p50, 1_000);

        // The session degrades to 5ms and stays there long enough to fill the window.
        for _ in 0..WINDOW {
            l.record(5_000);
        }
        assert_eq!(
            l.baseline().unwrap().p50,
            1_000,
            "the baseline must NOT follow the recent window — that is the whole point"
        );
        assert_eq!(l.recent().unwrap().p50, 5_000);
        assert_eq!(l.drift_us(), Some(4_000), "4ms slower than at connect");
    }

    #[test]
    fn drift_is_negative_when_the_session_speeds_up() {
        let mut l = Latency::new();
        for _ in 0..BASELINE_SAMPLES {
            l.record(9_000);
        }
        for _ in 0..WINDOW {
            l.record(2_000);
        }
        assert_eq!(l.drift_us(), Some(-7_000));
    }

    #[test]
    fn there_is_no_baseline_or_drift_before_enough_samples() {
        let mut l = Latency::new();
        for _ in 0..(BASELINE_SAMPLES - 1) {
            l.record(1_000);
        }
        assert_eq!(l.baseline(), None);
        assert_eq!(l.drift_us(), None);
        assert!(l.recent().is_some(), "but recent readings are available");
    }

    #[test]
    fn an_empty_latency_reports_nothing_rather_than_zero() {
        let l = Latency::new();
        assert_eq!(l.recent(), None);
        assert_eq!(l.min(), None);
        assert_eq!(l.count(), 0);
    }

    #[test]
    fn hit_rate_and_byte_savings_answer_different_questions() {
        // Nine hits on tiny regions, one miss that carried almost all the pixels: a high
        // hit rate that saved very little. The two numbers must disagree here, or the
        // byte measure is not adding anything.
        let c = CacheStats {
            hits: 9,
            misses: 1,
            bytes_served: 900,
            bytes_from_wire: 99_100,
            ..Default::default()
        };
        assert_eq!(c.hit_rate(), Some(0.9));
        let savings = c.byte_savings().unwrap();
        assert!(
            savings < 0.01,
            "90% of lookups hit but under 1% of pixels were saved; got {savings}"
        );
    }

    #[test]
    fn an_untouched_cache_reports_no_rate_rather_than_zero_percent() {
        let c = CacheStats::default();
        assert_eq!(c.hit_rate(), None, "0/0 is unknown, not 0%");
        assert_eq!(c.byte_savings(), None);
    }

    #[test]
    fn a_present_closes_the_matching_paint_handoff() {
        let mut s = SessionStats::new();
        s.mark_painted(7);
        s.mark_presented(7);
        assert_eq!(s.present.count(), 1);
        // The slot is consumed: re-presenting the same content (overlay redraws,
        // resizes) must not mint extra samples.
        s.mark_presented(7);
        assert_eq!(s.present.count(), 1);
    }

    #[test]
    fn a_present_of_newer_content_covers_an_older_pending_paint() {
        let mut s = SessionStats::new();
        s.mark_painted(3);
        s.mark_presented(9);
        assert_eq!(s.present.count(), 1, "the screen shows gen 9 ≥ pending 3");
    }

    #[test]
    fn a_present_of_older_content_leaves_the_pending_paint_open() {
        let mut s = SessionStats::new();
        s.mark_painted(5);
        s.mark_presented(4);
        assert_eq!(
            s.present.count(),
            0,
            "gen 4 on screen does not show paint 5"
        );
        s.mark_presented(5);
        assert_eq!(s.present.count(), 1);
    }

    #[test]
    fn a_present_with_no_pending_paint_records_nothing() {
        let mut s = SessionStats::new();
        s.mark_presented(1);
        assert_eq!(s.present.count(), 0);
    }

    #[test]
    fn the_overlay_names_the_pipeline_split_once_both_segments_have_samples() {
        let mut s = SessionStats::new();
        assert!(
            !s.overlay_lines().iter().any(|l| l.contains("pipeline")),
            "no pipeline line before any samples"
        );
        s.decode.record(2_000);
        s.present.record(8_000);
        let lines = s.overlay_lines();
        let pipeline = lines
            .iter()
            .find(|l| l.contains("pipeline"))
            .expect("pipeline line appears");
        assert!(pipeline.contains("decode p50 2.0ms"), "{pipeline}");
        assert!(pipeline.contains("present p50 8.0ms"), "{pipeline}");
    }

    #[test]
    fn a_perfect_cache_reports_one() {
        let c = CacheStats {
            hits: 4,
            misses: 0,
            bytes_served: 500,
            bytes_from_wire: 0,
            ..Default::default()
        };
        assert_eq!(c.hit_rate(), Some(1.0));
        assert_eq!(c.byte_savings(), Some(1.0));
    }

    #[test]
    fn the_overlay_names_drift_and_cache_effectiveness() {
        let mut s = SessionStats::new();
        for _ in 0..BASELINE_SAMPLES {
            s.latency.record(1_000);
        }
        for _ in 0..WINDOW {
            s.latency.record(3_500);
        }
        s.cache = CacheStats {
            hits: 3,
            misses: 1,
            entries: 2,
            bytes_served: 750,
            bytes_from_wire: 250,
            ..Default::default()
        };
        s.frames = 42;
        s.bytes_in = 2 * 1024 * 1024;
        s.codecs.insert("ClearCodec".to_owned(), 17);
        s.codecs
            .insert("WireToSurface2/RemoteFxProgressive".to_owned(), 3);

        let text = s.overlay_lines().join("\n");
        assert!(text.contains("p50 3.5ms"), "got:\n{text}");
        assert!(text.contains("max 3.5ms"), "got:\n{text}");
        assert!(text.contains("drift +2.5ms"), "got:\n{text}");
        assert!(text.contains("75% hit"), "got:\n{text}");
        assert!(text.contains("75% of pixels"), "got:\n{text}");
        assert!(text.contains("2.0MiB"), "got:\n{text}");
        assert!(text.contains("ClearCodec:17"), "got:\n{text}");
        assert!(text.contains("RFX Progressive:3"), "got:\n{text}");
    }

    #[test]
    fn the_overlay_stays_quiet_about_staleness_until_there_is_some() {
        let mut s = SessionStats::new();
        assert!(!s.overlay_lines().join("\n").contains("STALE"));
        s.undecoded_regions = 1;
        assert!(s.overlay_lines().join("\n").contains("STALE"));
    }

    #[test]
    fn the_overlay_survives_a_session_with_no_data_at_all() {
        let s = SessionStats::new();
        let lines = s.overlay_lines();
        assert!(!lines.is_empty());
        assert!(lines[0].contains("no samples"));
        // No panic, no NaN, no "0%" implying a measured zero.
        let text = lines.join("\n");
        assert!(!text.contains("NaN"), "got:\n{text}");
        assert!(text.contains("codec    no surface updates yet"));
    }

    #[test]
    fn filling_then_hitting_a_slot_accumulates_stored_bytes_per_hit() {
        // Every field a different value, so a swapped width/height or a hits/bytes mix-up
        // cannot pass.
        let mut slots = SlotStats::new();
        slots.fill(9, 448, 256, "ClearCodec", 1234);

        let filled = *slots.get(9).expect("the slot was just filled");
        assert_eq!(filled.slot, 9);
        assert_eq!(filled.width, 448);
        assert_eq!(filled.height, 256);
        assert_eq!(filled.codec, "ClearCodec");
        assert_eq!(filled.state, SlotState::Live);
        assert_eq!(filled.hits, 0);
        assert_eq!(filled.bytes_served, 0, "a fill serves nothing by itself");
        assert_eq!(filled.bytes_stored, 1234);
        assert_eq!(filled.last_hit, None);

        // One PDU stamping the tile in three places, then another stamping it once.
        slots.hit(9, 3);
        slots.hit(9, 1);

        let hit = *slots.get(9).expect("slot 9");
        assert_eq!(hit.hits, 4, "one hit per destination painted");
        assert_eq!(
            hit.bytes_served,
            1234 * 4,
            "each hit serves the stored size again"
        );
        assert_eq!(hit.bytes_stored, 1234, "the occupant did not change size");
        assert!(hit.last_hit.is_some());
    }

    #[test]
    fn an_evicted_slot_keeps_the_counters_it_earned() {
        // What a slot served before the server dropped it is the evidence that says
        // whether the eviction cost anything, so eviction must not zero it.
        let mut slots = SlotStats::new();
        slots.fill(9, 448, 256, "RemoteFxProgressive", 1234);
        slots.hit(9, 2);
        slots.evict(9);

        let evicted = *slots.get(9).expect("an evicted slot is still reported");
        assert_eq!(evicted.state, SlotState::Evicted);
        assert_eq!(evicted.hits, 2);
        assert_eq!(evicted.bytes_served, 2468);
        assert_eq!(evicted.bytes_stored, 1234);
        assert_eq!(evicted.width, 448);
        assert_eq!(evicted.height, 256);
        assert_eq!(evicted.codec, "RemoteFxProgressive");
    }

    #[test]
    fn a_refilled_slot_serves_at_its_new_size_without_restating_its_history() {
        // bytes_served is accumulated, not `hits * bytes_stored`. Deriving it would
        // restate the two 1234-byte hits at the new 40-byte size and report 160.
        let mut slots = SlotStats::new();
        slots.fill(9, 448, 256, "ClearCodec", 1234);
        slots.hit(9, 2);
        slots.fill(9, 10, 4, "Uncompressed", 40);
        slots.hit(9, 2);

        let s = *slots.get(9).expect("slot 9");
        assert_eq!(s.hits, 4);
        assert_eq!(s.bytes_served, 1234 * 2 + 40 * 2);
        assert_eq!(s.bytes_stored, 40, "only the current occupant's size");
        assert_eq!(s.width, 10);
        assert_eq!(s.height, 4);
        assert_eq!(s.codec, "Uncompressed");
        assert_eq!(s.state, SlotState::Live, "a refill makes a slot live again");
    }

    #[test]
    fn a_slot_that_was_never_filled_is_not_invented_by_a_hit_or_an_evict() {
        let mut slots = SlotStats::new();
        slots.hit(9, 5);
        slots.evict(11);
        assert!(slots.is_empty(), "no size to serve means no record to keep");
        assert_eq!(slots.get(9), None);
    }

    #[test]
    fn slots_are_reported_in_slot_order() {
        let mut slots = SlotStats::new();
        slots.fill(11, 448, 256, "ClearCodec", 1234);
        slots.fill(2, 64, 32, "Uncompressed", 40);
        let order: Vec<u16> = slots.iter().map(|s| s.slot).collect();
        assert_eq!(order, vec![2, 11]);
        assert_eq!(slots.len(), 2);
    }

    #[test]
    fn a_snapshot_does_not_alias_later_mutation() {
        let handle = SlotStatsHandle::new();
        handle.update(|s| s.fill(9, 448, 256, "ClearCodec", 1234));
        let before = handle.snapshot();
        handle.update(|s| s.hit(9, 7));
        assert_eq!(before.get(9).unwrap().hits, 0, "the copy must be frozen");
        assert_eq!(handle.snapshot().get(9).unwrap().hits, 7);
    }

    #[test]
    fn byte_sizes_read_the_way_a_human_expects() {
        assert_eq!(bytes_human(512), "512B");
        assert_eq!(bytes_human(1024), "1.0KiB");
        assert_eq!(bytes_human(1024 * 1024), "1.0MiB");
        assert_eq!(bytes_human(3 * 1024 * 1024 * 1024), "3.00GiB");
    }
}
