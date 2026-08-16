//! "Bitmap cache" — handoff screen 4, the window that says whether the cache is earning
//! its keep.
//!
//! The screen exists because the obvious number lies: a hit on a 32×32 tile counts the
//! same as a hit on a 448×448 one, so a hit rate flatters a cache that is saving almost
//! no pixels. Everything here is therefore built around bytes, and the grid colours are
//! *relative* measures — a slot is hot compared with the other slots this session, not
//! against some absolute scale nobody can calibrate.
//!
//! The arithmetic that decides a cell's colour lives in free functions above the
//! drawing ([`HeatScale`], [`heat_t`], [`cell_colour`], [`default_selection`]) so it can
//! be tested without a window. A diagnostics window that quietly lies is worse than no
//! diagnostics window at all, and "quietly" is exactly what a mis-scaled colour ramp is.
//!
//! Layout, from the handoff: header 54 · headline band 96 · body 520 (a flexible left
//! pane beside a 300px sidebar). The 30px menu strip above and the 900×700 window around
//! it belong to the host.

use std::time::Instant;

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Layout, Painter, Pos2, Rect, RichText, Sense,
    Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2,
};

use crate::diag::thousands;
use crate::shell::widgets;
use crate::stats::{CacheStats, SlotStat, SlotState, SlotStats};
use crate::ui::theme;

/// What the window wants its host to do after the frame is drawn.
///
/// Returned rather than acted on so the screen stays pure over its inputs: writing a
/// file is the host's business, and a screen that cannot touch the disk cannot surprise
/// anyone by touching it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheAction {
    #[default]
    None,
    WriteMetrics,
}

// --- geometry (handoff values) --------------------------------------------------------

/// Header band, matching the latency window.
const HEADER_H: f32 = 54.0;
/// Headline band: hit rate, pixel share, and the sentence explaining why they differ.
const BAND_H: f32 = 96.0;
/// The headline band's three cells: 210, 230, and whatever is left.
const BAND_HIT_W: f32 = 210.0;
const BAND_PIXELS_W: f32 = 230.0;
/// Side padding shared by the header and the headline band.
const PAD_X: f32 = 24.0;
/// The right sidebar's fixed width.
const SIDEBAR_W: f32 = 300.0;

/// Left pane padding: `18px 20px 20px 24px`.
const LEFT_PAD_TOP: f32 = 18.0;
const LEFT_PAD_RIGHT: f32 = 20.0;
const LEFT_PAD_BOTTOM: f32 = 20.0;
const LEFT_PAD_LEFT: f32 = 24.0;
/// Gap between the left pane's three stacked parts.
const LEFT_GAP: f32 = 14.0;
/// The metric segmented control's outer track.
const TRACK_H: f32 = 30.0;
/// The pinned legend row.
const LEGEND_H: f32 = 16.0;
/// The legend's ramp bar, 210×9 radius 2.
const LEGEND_BAR_W: f32 = 210.0;
const LEGEND_BAR_H: f32 = 9.0;
/// The legend's empty-slot chip, 14×9.
const LEGEND_CHIP_W: f32 = 14.0;

/// Grid: `repeat(27, 18px)` with a 2px gap.
const GRID_COLS: usize = 27;
const CELL: f32 = 18.0;
const CELL_GAP: f32 = 2.0;
/// Pitch from one cell's left edge to the next.
const CELL_PITCH: f32 = CELL + CELL_GAP;
/// The grid's fixed width: 27 cells and 26 gaps.
const GRID_W: f32 = GRID_COLS as f32 * CELL + (GRID_COLS as f32 - 1.0) * CELL_GAP;

/// Sidebar padding and row rhythm.
const SIDE_PAD_X: f32 = 20.0;
const SIDE_PAD_Y: f32 = 16.0;
const SIDE_ROW_H: f32 = 21.0;
const SIDE_GAP: f32 = 10.0;

/// A slot last hit this long ago is fully stale on the Recency ramp.
///
/// The handoff's formula: `1 − min(1, age_ms / 180000)`.
const RECENCY_SPAN_MS: f32 = 180_000.0;
/// The Return ramp tops out at a slot that has paid back its stored bytes twentyfold.
const RETURN_CEILING: f64 = 20.0;

/// The sentence under the two headline figures — the whole reason the screen exists.
const BAND_EXPLANATION: &str = "A hit on a 32×32 tile counts the same as a hit on a 448×448 \
     one, so hit rate flatters the cache. The pixel share is what it actually saved. When \
     these two disagree the cache is not earning its keep.";

// --- the metric -----------------------------------------------------------------------

/// Which measure the grid is colouring by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Served,
    Hits,
    Recency,
    Return,
    Occupancy,
}

impl Metric {
    /// In the order the segmented control shows them.
    pub const ALL: [Metric; 5] = [
        Metric::Served,
        Metric::Hits,
        Metric::Recency,
        Metric::Return,
        Metric::Occupancy,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Metric::Served => "Served",
            Metric::Hits => "Hits",
            Metric::Recency => "Recency",
            Metric::Return => "Return",
            Metric::Occupancy => "Occupancy",
        }
    }

    /// The legend's `(low, high)` labels. They change with the metric, because a ramp
    /// whose ends are unlabelled — or labelled for a different measure — is decoration.
    pub fn legend(self) -> (&'static str, &'static str) {
        match self {
            Metric::Served => ("no bytes served", "carrying the session"),
            Metric::Hits => ("never hit", "hit constantly"),
            Metric::Recency => ("stale", "hit just now"),
            Metric::Return => ("stored, barely used", "paid back ×20"),
            Metric::Occupancy => ("evicted", "live entry"),
        }
    }
}

// --- the maths, kept out of the drawing ------------------------------------------------

/// The session-relative scale every ramp metric is measured against, derived once per
/// frame.
///
/// Recomputed from the snapshot rather than remembered, so a slot that ages out of the
/// top rank stops being drawn as if it were still there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeatScale {
    /// Distinct `bytes_served` values among filled slots, ascending — the rank ladder.
    served_ladder: Vec<u64>,
    /// The busiest slot's hit count, which normalises the Hits ramp.
    max_hits: u32,
}

impl HeatScale {
    pub fn from_slots(slots: &SlotStats) -> Self {
        let mut served_ladder: Vec<u64> = slots
            .iter()
            .filter(|s| s.state != SlotState::Empty)
            .map(|s| s.bytes_served)
            .collect();
        served_ladder.sort_unstable();
        served_ladder.dedup();
        Self {
            served_ladder,
            max_hits: slots.iter().map(|s| s.hits).max().unwrap_or(0),
        }
    }

    /// Where `bytes_served` sits on the rank ladder, in `[0, 1]`.
    ///
    /// Rank-binned deliberately, as the handoff insists: a handful of 448×448 slots
    /// otherwise pin every other filled slot into the top two stops, and the legend then
    /// advertises a scale the view never uses.
    ///
    /// The ladder is of *distinct* values, so slots serving the same number of bytes get
    /// the same colour — ranking them against each other would invent an ordering the
    /// data does not have. A cache whose filled slots all serve the same amount has no
    /// ranking to show: it reads flat, at the low end unless that amount is non-zero and
    /// it is the only value there is.
    pub fn served_t(&self, bytes_served: u64) -> f32 {
        match self.served_ladder.len() {
            0 => 0.0,
            // One value: nothing to rank against. A slot serving nothing is the low end
            // by its own label; a lone slot serving anything at all is carrying the
            // session, because there is nothing else carrying it.
            1 => {
                if bytes_served > 0 {
                    1.0
                } else {
                    0.0
                }
            }
            n => {
                let idx = self.served_ladder.partition_point(|&v| v < bytes_served);
                idx.min(n - 1) as f32 / (n - 1) as f32
            }
        }
    }

    pub fn max_hits(&self) -> u32 {
        self.max_hits
    }
}

/// The ramp position for one slot under one metric, or `None` where no ramp applies —
/// an empty slot, or the Occupancy metric, which is two states rather than a scale.
///
/// `now` comes from the caller so Recency is testable without waiting.
pub fn heat_t(metric: Metric, slot: &SlotStat, scale: &HeatScale, now: Instant) -> Option<f32> {
    if slot.state == SlotState::Empty {
        return None;
    }
    let t = match metric {
        Metric::Served => scale.served_t(slot.bytes_served),
        // log1p, so the busiest slot does not flatten every other one to the cold end.
        Metric::Hits => {
            if scale.max_hits == 0 {
                0.0
            } else {
                (f64::from(slot.hits).ln_1p() / f64::from(scale.max_hits).ln_1p()) as f32
            }
        }
        Metric::Recency => {
            let age_ms = match slot.last_hit {
                Some(at) => now.saturating_duration_since(at).as_secs_f32() * 1000.0,
                // Never hit is as stale as it gets, not as fresh.
                None => f32::INFINITY,
            };
            1.0 - (age_ms / RECENCY_SPAN_MS).min(1.0)
        }
        Metric::Return => {
            if slot.bytes_stored == 0 {
                0.0
            } else {
                let ratio = slot.bytes_served as f64 / slot.bytes_stored as f64;
                (ratio.ln_1p() / RETURN_CEILING.ln_1p()).min(1.0) as f32
            }
        }
        Metric::Occupancy => return None,
    };
    Some(t.clamp(0.0, 1.0))
}

/// The colour one grid cell takes. `slot` is `None` for a position no PDU has ever
/// touched.
///
/// Empty slots are flat [`theme::HEAT_EMPTY`] and never take a ramp colour, so the grid
/// cannot imply a cold-but-filled slot where there is no slot at all.
pub fn cell_colour(
    metric: Metric,
    slot: Option<&SlotStat>,
    scale: &HeatScale,
    now: Instant,
) -> Color32 {
    let Some(slot) = slot else {
        return theme::HEAT_EMPTY;
    };
    if slot.state == SlotState::Empty {
        return theme::HEAT_EMPTY;
    }
    if metric == Metric::Occupancy {
        return if slot.state == SlotState::Live {
            theme::ACCENT
        } else {
            theme::DANGER
        };
    }
    theme::heat(heat_t(metric, slot, scale, now).unwrap_or(0.0))
}

/// The slot the window selects when the user has not chosen one: the highest-served live
/// slot.
///
/// Never an empty or evicted one — the sidebar exists to explain a slot that is
/// currently doing work, and opening on a dead slot wastes the one section that answers
/// "why is this slot hot". Ties keep the lowest slot id, so two refreshes of an idle
/// cache do not swap the selection under the user.
pub fn default_selection(slots: &SlotStats) -> Option<u16> {
    let mut best: Option<&SlotStat> = None;
    for s in slots.iter().filter(|s| s.state == SlotState::Live) {
        if best.is_none_or(|b| s.bytes_served > b.bytes_served) {
            best = Some(s);
        }
    }
    best.map(|s| s.slot)
}

/// How many cells the grid draws.
///
/// [`SlotStats`] only records slots the server has actually touched, and RDP never
/// announces the cache's size, so the highest slot index seen *is* the capacity as far
/// as this session can honestly claim. Positions below it that were never touched are
/// drawn as empty, which is what they are.
pub fn grid_capacity(slots: &SlotStats) -> usize {
    slots
        .iter()
        .map(|s| usize::from(s.slot) + 1)
        .max()
        .unwrap_or(0)
}

/// Live and evicted slot counts, in that order.
pub fn state_counts(slots: &SlotStats) -> (usize, usize) {
    let live = slots.iter().filter(|s| s.state == SlotState::Live).count();
    let evicted = slots
        .iter()
        .filter(|s| s.state == SlotState::Evicted)
        .count();
    (live, evicted)
}

/// The caption right of the metric control: `504 slots · 314 live · 50 evicted`.
pub fn grid_caption(slots: &SlotStats) -> String {
    let (live, evicted) = state_counts(slots);
    format!(
        "{} slots · {live} live · {evicted} evicted",
        grid_capacity(slots)
    )
}

/// Which codecs filled the cache, by share of the bytes currently stored, largest first.
///
/// Weighted by bytes rather than by slot count for the same reason the headline is:
/// counting slots would let a swarm of tiny tiles outvote the codec that actually
/// carried the pixels. Ties break on the codec name so the bars do not reorder between
/// refreshes.
pub fn codec_shares(slots: &SlotStats) -> Vec<(&'static str, f32)> {
    let mut totals: Vec<(&'static str, u64)> = Vec::new();
    let mut all: u64 = 0;
    for s in slots.iter().filter(|s| s.state != SlotState::Empty) {
        all = all.saturating_add(s.bytes_stored);
        match totals.iter_mut().find(|(name, _)| *name == s.codec) {
            Some((_, bytes)) => *bytes = bytes.saturating_add(s.bytes_stored),
            None => totals.push((s.codec, s.bytes_stored)),
        }
    }
    if all == 0 {
        return Vec::new();
    }
    totals.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    totals
        .into_iter()
        .map(|(name, bytes)| (name, (bytes as f64 / all as f64) as f32))
        .collect()
}

/// The codec name as the handoff writes it.
pub fn codec_label(codec: &str) -> &str {
    match codec {
        "RemoteFxProgressive" => "RFX Progressive",
        other => other,
    }
}

/// Which cell a pointer at `pos` is over, if any.
///
/// The whole grid is one allocation and one hit test rather than 500 widgets: at 1 Hz
/// with several hundred cells the per-widget bookkeeping is the expensive part, and the
/// geometry is a fixed pitch, so the arithmetic is exact. The 2px gaps are *not* part of
/// any cell — a click there selects nothing, which is what the drawn gap implies.
pub fn cell_index_at(origin: Pos2, pos: Pos2, capacity: usize) -> Option<usize> {
    let dx = pos.x - origin.x;
    let dy = pos.y - origin.y;
    if dx < 0.0 || dy < 0.0 {
        return None;
    }
    let col = (dx / CELL_PITCH) as usize;
    let row = (dy / CELL_PITCH) as usize;
    if col >= GRID_COLS {
        return None;
    }
    // Inside the cell itself, not in the gap after it.
    if dx - col as f32 * CELL_PITCH > CELL || dy - row as f32 * CELL_PITCH > CELL {
        return None;
    }
    let index = row * GRID_COLS + col;
    (index < capacity).then_some(index)
}

// --- figures --------------------------------------------------------------------------

/// Bytes as the handoff writes them: `214.0 MiB`, `43 KiB`, `900 B`.
pub fn bytes_str(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if f >= K * K * K {
        format!("{:.1} GiB", f / (K * K * K))
    } else if f >= K * K {
        format!("{:.1} MiB", f / (K * K))
    } else if f >= K {
        format!("{} KiB", (f / K).round() as u64)
    } else {
        format!("{n} B")
    }
}

/// How long ago a slot was last hit: `740ms ago`, `1.4s ago`, `3m ago`, or `never`.
pub fn age_str(last_hit: Option<Instant>, now: Instant) -> String {
    let Some(at) = last_hit else {
        return "never".to_owned();
    };
    let ms = now.saturating_duration_since(at).as_millis();
    if ms < 1_000 {
        format!("{ms}ms ago")
    } else if ms < 60_000 {
        format!("{:.1}s ago", ms as f64 / 1000.0)
    } else {
        format!("{}m ago", (ms as f64 / 60_000.0).round() as u64)
    }
}

/// A slot's payback: bytes served against bytes stored, as `×11.7`.
pub fn return_str(slot: &SlotStat) -> String {
    if slot.bytes_stored == 0 {
        return "—".to_owned();
    }
    format!(
        "×{:.1}",
        slot.bytes_served as f64 / slot.bytes_stored as f64
    )
}

/// The headline hit rate, to one decimal. `—` when nothing has been looked up: a cache
/// nobody asked about is not a cache with a 0% hit rate.
pub fn hit_rate_str(cache: &CacheStats) -> String {
    match cache.hit_rate() {
        Some(r) => format!("{:.1}%", r * 100.0),
        None => "—".to_owned(),
    }
}

/// The honest headline: what share of painted pixels came out of the cache.
pub fn pixel_share_str(cache: &CacheStats) -> String {
    match cache.byte_savings() {
        Some(r) => format!("{}%", (r * 100.0).round() as u64),
        None => "—".to_owned(),
    }
}

/// `2,043 hits / 8 misses`.
pub fn lookup_line(cache: &CacheStats) -> String {
    format!(
        "{} hits / {} misses",
        thousands(cache.hits),
        thousands(cache.misses)
    )
}

/// `214.0 MiB of 349.0 MiB painted`.
pub fn served_line(cache: &CacheStats) -> String {
    format!(
        "{} of {} painted",
        bytes_str(cache.bytes_served),
        bytes_str(cache.bytes_served.saturating_add(cache.bytes_from_wire))
    )
}

// --- the window -----------------------------------------------------------------------

/// The bitmap-cache window's view state: which metric colours the grid, and which slot
/// the sidebar is describing.
///
/// Both survive a refresh — the window polls at 1 Hz, and a selection that reset every
/// second would make the sidebar unreadable.
#[derive(Debug, Clone)]
pub struct CacheWindow {
    metric: Metric,
    selected: Option<u16>,
}

impl Default for CacheWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl CacheWindow {
    pub fn new() -> Self {
        Self {
            metric: Metric::Served,
            selected: None,
        }
    }

    pub fn metric(&self) -> Metric {
        self.metric
    }

    pub fn selected(&self) -> Option<u16> {
        self.selected
    }

    /// One frame. `now` for Recency aging (pass `Instant::now()` from the caller).
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        slots: &SlotStats,
        cache: &CacheStats,
        session: Option<(&str, &str)>,
        now: Instant,
    ) -> CacheAction {
        // Only ever fills an *absent* selection, so a user's choice survives every later
        // refresh — including one where the slot's own numbers changed.
        self.adopt_default_selection(slots);

        let action = header(ui, session);
        headline_band(ui, cache);
        self.body(ui, slots, cache, now);
        action
    }

    /// Take the default selection if, and only if, nothing is selected yet.
    fn adopt_default_selection(&mut self, slots: &SlotStats) {
        if self.selected.is_none() {
            self.selected = default_selection(slots);
        }
    }

    fn body(&mut self, ui: &mut Ui, slots: &SlotStats, cache: &CacheStats, now: Instant) {
        let (rect, _) = ui.allocate_exact_size(
            vec2(ui.available_width(), ui.available_height()),
            Sense::hover(),
        );
        let split = (rect.max.x - SIDEBAR_W).max(rect.min.x);
        let left = Rect::from_min_max(rect.min, pos2(split, rect.max.y));
        let sidebar = Rect::from_min_max(pos2(split, rect.min.y), rect.max);

        self.left_pane(ui, left, slots, now);
        self.sidebar(ui, sidebar, slots, cache, now);
    }

    fn left_pane(&mut self, ui: &mut Ui, pane: Rect, slots: &SlotStats, now: Instant) {
        let content = Rect::from_min_max(
            pos2(pane.min.x + LEFT_PAD_LEFT, pane.min.y + LEFT_PAD_TOP),
            pos2(pane.max.x - LEFT_PAD_RIGHT, pane.max.y - LEFT_PAD_BOTTOM),
        );

        // Metric control, with the grid caption right-aligned beside it.
        let control = Rect::from_min_size(content.min, vec2(content.width(), TRACK_H));
        self.segmented(ui, control);
        ui.painter().text(
            pos2(content.max.x, control.center().y),
            Align2::RIGHT_CENTER,
            grid_caption(slots),
            theme::mono(11.0),
            theme::TEXT_DIM,
        );

        // Legend pinned to the bottom, grid in whatever is left.
        let legend_row =
            Rect::from_min_max(pos2(content.min.x, content.max.y - LEGEND_H), content.max);
        self.legend(ui.painter(), legend_row);

        let region = Rect::from_min_max(
            pos2(content.min.x, control.max.y + LEFT_GAP),
            pos2(content.max.x, legend_row.min.y - LEFT_GAP),
        );
        if region.height() > 0.0 {
            let mut child = ui.new_child(UiBuilder::new().max_rect(region));
            egui::ScrollArea::vertical()
                .id_salt("cache-grid-scroll")
                .max_height(region.height())
                .auto_shrink([false, false])
                .show(&mut child, |ui| {
                    self.grid(ui, slots, now);
                });
        }
    }

    /// The metric segmented control, grouped-track style.
    fn segmented(&mut self, ui: &mut Ui, row: Rect) {
        let font = theme::sans(12.0);
        let widths: Vec<f32> = Metric::ALL
            .iter()
            .map(|m| text_width(ui, m.label(), font.clone()) + 22.0)
            .collect();
        let track_w = widths.iter().sum::<f32>() + 3.0 * (Metric::ALL.len() as f32 - 1.0) + 6.0;
        let track = Rect::from_min_size(row.min, vec2(track_w, TRACK_H));

        let response = ui.interact(track, ui.id().with("cache-metric"), Sense::click());
        let clicked = response.clicked().then(|| response.interact_pointer_pos());

        let painter = ui.painter();
        painter.rect(
            track,
            CornerRadius::same(theme::radius::INPUT),
            theme::BG_CHROME,
            Stroke::new(1.0, theme::LINE_HAIR),
            StrokeKind::Inside,
        );

        let mut x = track.min.x + 3.0;
        for (metric, w) in Metric::ALL.into_iter().zip(widths) {
            let seg = Rect::from_min_size(pos2(x, track.min.y + 3.0), vec2(w, TRACK_H - 6.0));
            let selected = metric == self.metric;
            if selected {
                painter.rect_filled(
                    seg,
                    CornerRadius::same(theme::radius::MENU_ITEM),
                    theme::ACCENT_FILL,
                );
            }
            painter.text(
                seg.center(),
                Align2::CENTER_CENTER,
                metric.label(),
                if selected {
                    theme::sans_medium(12.0)
                } else {
                    theme::sans(12.0)
                },
                if selected {
                    theme::TEXT_PRIMARY
                } else {
                    theme::TEXT_SECONDARY
                },
            );
            // Switching metric re-colours the grid in place; it deliberately leaves the
            // selection alone.
            if let Some(Some(at)) = clicked
                && seg.contains(at)
            {
                self.metric = metric;
            }
            x += w + 3.0;
        }
    }

    /// The heat grid: one 18px cell per slot position, hit-tested arithmetically.
    fn grid(&mut self, ui: &mut Ui, slots: &SlotStats, now: Instant) {
        let capacity = grid_capacity(slots);
        let rows = capacity.div_ceil(GRID_COLS);
        let height = if rows == 0 {
            0.0
        } else {
            rows as f32 * CELL_PITCH - CELL_GAP
        };
        let (rect, response) = ui.allocate_exact_size(vec2(GRID_W, height), Sense::click());
        if capacity == 0 {
            ui.painter().text(
                rect.min,
                Align2::LEFT_TOP,
                "No cache slots touched yet.",
                theme::sans(12.0),
                theme::TEXT_DIM,
            );
            return;
        }

        if response.clicked()
            && let Some(at) = response.interact_pointer_pos()
            && let Some(index) = cell_index_at(rect.min, at, capacity)
        {
            self.selected = Some(index as u16);
        }
        let hovered = response
            .hover_pos()
            .and_then(|at| cell_index_at(rect.min, at, capacity));

        let scale = HeatScale::from_slots(slots);
        let painter = ui.painter();
        for index in 0..capacity {
            let slot = slots.get(index as u16);
            painter.rect_filled(
                cell_rect(rect.min, index),
                CornerRadius::same(theme::radius::CACHE_CELL),
                cell_colour(self.metric, slot, &scale, now),
            );
        }
        // Rings last: they sit outside the cell, in the 2px gutter, so a neighbour drawn
        // afterwards would clip them.
        if let Some(index) = hovered.filter(|i| Some(*i as u16) != self.selected) {
            ring(painter, cell_rect(rect.min, index), 1.0);
        }
        if let Some(selected) = self.selected.map(usize::from).filter(|i| *i < capacity) {
            ring(painter, cell_rect(rect.min, selected), 2.0);
        }
    }

    fn legend(&self, painter: &Painter, row: Rect) {
        let (low, high) = self.metric.legend();
        let cy = row.center().y;
        let low_rect = painter.text(
            pos2(row.min.x, cy),
            Align2::LEFT_CENTER,
            low,
            theme::sans(11.0),
            theme::TEXT_DIM,
        );

        let bar = Rect::from_min_size(
            pos2(low_rect.max.x + 14.0, cy - LEGEND_BAR_H / 2.0),
            vec2(LEGEND_BAR_W, LEGEND_BAR_H),
        );
        legend_bar(painter, bar, self.metric);

        let high_rect = painter.text(
            pos2(bar.max.x + 14.0, cy),
            Align2::LEFT_CENTER,
            high,
            theme::sans(11.0),
            theme::TEXT_DIM,
        );
        let divider_x = high_rect.max.x + 14.0;
        painter.vline(
            divider_x,
            egui::Rangef::new(cy - 7.0, cy + 7.0),
            Stroke::new(1.0, theme::LINE_HAIR),
        );
        let chip = Rect::from_min_size(
            pos2(divider_x + 14.0, cy - LEGEND_BAR_H / 2.0),
            vec2(LEGEND_CHIP_W, LEGEND_BAR_H),
        );
        painter.rect_filled(
            chip,
            CornerRadius::same(theme::radius::CACHE_CELL),
            theme::HEAT_EMPTY,
        );
        painter.text(
            pos2(chip.max.x + 8.0, cy),
            Align2::LEFT_CENTER,
            "empty slot · never filled",
            theme::sans(11.0),
            theme::TEXT_DIM,
        );
    }

    fn sidebar(
        &self,
        ui: &mut Ui,
        rect: Rect,
        slots: &SlotStats,
        cache: &CacheStats,
        now: Instant,
    ) {
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::ZERO, theme::BG_PANEL);
        painter.vline(
            rect.min.x + 0.5,
            rect.y_range(),
            Stroke::new(1.0, theme::LINE_HAIR),
        );

        let left = rect.min.x + SIDE_PAD_X;
        let right = rect.max.x - SIDE_PAD_X;
        let mut y = rect.min.y + SIDE_PAD_Y;

        // 1 — the selected slot.
        let slot = self.selected.and_then(|s| slots.get(s));
        let (state_text, state_colour) = match slot.map(|s| s.state) {
            Some(SlotState::Live) => ("live", theme::ACCENT),
            Some(SlotState::Evicted) => ("evicted", theme::DANGER),
            _ => ("empty", theme::TEXT_DIM),
        };
        let title = match self.selected {
            Some(id) => format!("SLOT {id}"),
            None => "SLOT —".to_owned(),
        };
        y = section_header(
            painter,
            left,
            right,
            y,
            &title,
            Some((state_text, state_colour)),
        );
        let dash = "—".to_owned();
        let rows: [(&str, String, Color32); 7] = [
            (
                "Size",
                slot.map_or_else(|| dash.clone(), |s| format!("{}×{}", s.width, s.height)),
                theme::TEXT_PRIMARY,
            ),
            (
                "Codec in",
                slot.map_or_else(|| dash.clone(), |s| codec_label(s.codec).to_owned()),
                theme::TEXT_PRIMARY,
            ),
            (
                "Hits",
                slot.map_or_else(|| dash.clone(), |s| thousands(u64::from(s.hits))),
                theme::TEXT_PRIMARY,
            ),
            (
                "Served",
                slot.map_or_else(|| dash.clone(), |s| bytes_str(s.bytes_served)),
                theme::TEXT_PRIMARY,
            ),
            (
                "Stored",
                slot.map_or_else(|| dash.clone(), |s| bytes_str(s.bytes_stored)),
                theme::TEXT_PRIMARY,
            ),
            (
                "Return",
                slot.map_or_else(|| dash.clone(), return_str),
                theme::ACCENT,
            ),
            (
                "Last hit",
                slot.map_or_else(|| dash.clone(), |s| age_str(s.last_hit, now)),
                theme::TEXT_PRIMARY,
            ),
        ];
        for (label, value, colour) in rows {
            y = value_row(painter, left, right, y, label, &value, colour);
        }

        y = divider(painter, left, right, y);

        // 2 — session totals. The aggregate counters, not a sum over the grid: a slot's
        // `bytes_stored` is only its current occupant, so summing the grid would
        // under-report everything the cache has churned through.
        y = section_header(painter, left, right, y, "TOTALS", None);
        let (live, _) = state_counts(slots);
        let totals: [(&str, String, Color32); 5] = [
            (
                "Entries",
                format!("{live} / {}", grid_capacity(slots)),
                theme::TEXT_PRIMARY,
            ),
            ("Evictions", thousands(cache.evictions), theme::DANGER),
            ("Served", bytes_str(cache.bytes_served), theme::TEXT_PRIMARY),
            ("Stored", bytes_str(cache.bytes_stored), theme::TEXT_PRIMARY),
            (
                "From wire",
                bytes_str(cache.bytes_from_wire),
                theme::TEXT_PRIMARY,
            ),
        ];
        for (label, value, colour) in totals {
            y = value_row(painter, left, right, y, label, &value, colour);
        }

        y = divider(painter, left, right, y);

        // 3 — which codec filled the cache.
        y = section_header(painter, left, right, y, "CODEC IN", None);
        let shares = codec_shares(slots);
        if shares.is_empty() {
            painter.text(
                pos2(left, y),
                Align2::LEFT_TOP,
                "nothing cached yet",
                theme::sans(12.0),
                theme::TEXT_DIM,
            );
        }
        for (i, (codec, share)) in shares.iter().enumerate() {
            let colour = match i {
                0 => theme::ACCENT,
                1 => theme::CYAN,
                2 => theme::WARN,
                _ => theme::TEXT_MUTED,
            };
            y = codec_bar(painter, left, right, y, codec_label(codec), *share, colour);
        }
    }
}

// --- drawing helpers -------------------------------------------------------------------

fn header(ui: &mut Ui, session: Option<(&str, &str)>) -> CacheAction {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER_H), Sense::hover());
    let painter = ui.painter();
    painter.hline(
        rect.x_range(),
        rect.max.y - 0.5,
        Stroke::new(1.0, theme::LINE_HAIR),
    );

    let cy = rect.center().y;
    let title = painter.text(
        pos2(rect.min.x + PAD_X, cy),
        Align2::LEFT_CENTER,
        "Bitmap cache",
        theme::sans_semibold(15.0),
        theme::TEXT_PRIMARY,
    );
    if let Some((name, detail)) = session {
        let divider_x = title.max.x + 14.0;
        painter.vline(
            divider_x,
            egui::Rangef::new(cy - 9.0, cy + 9.0),
            Stroke::new(1.0, theme::LINE_SUBTLE),
        );
        let dot_x = divider_x + 14.0;
        painter.circle_filled(pos2(dot_x + 3.0, cy), 3.0, theme::ACCENT);
        let name_rect = painter.text(
            pos2(dot_x + 6.0 + 9.0, cy),
            Align2::LEFT_CENTER,
            name,
            theme::mono(12.0),
            theme::TEXT_PRIMARY,
        );
        painter.text(
            pos2(name_rect.max.x + 9.0, cy),
            Align2::LEFT_CENTER,
            detail,
            theme::mono(12.0),
            theme::TEXT_DIM,
        );
    }

    // The refresh note and the metrics button live in a right-aligned child, so their
    // widths do not have to be guessed.
    let mut action = CacheAction::None;
    let area = Rect::from_min_max(
        pos2(rect.min.x + PAD_X, rect.min.y),
        pos2(rect.max.x - PAD_X, rect.max.y),
    );
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(area)
            .layout(Layout::right_to_left(Align::Center)),
    );
    if widgets::secondary_button(&mut child, "Write metrics JSON", 26.0).clicked() {
        action = CacheAction::WriteMetrics;
    }
    child.add_space(16.0 - child.spacing().item_spacing.x);
    child.label(
        RichText::new("refresh 1s")
            .font(theme::mono(11.0))
            .color(theme::TEXT_DIM),
    );
    action
}

fn headline_band(ui: &mut Ui, cache: &CacheStats) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), BAND_H), Sense::hover());
    let painter = ui.painter();
    painter.hline(
        rect.x_range(),
        rect.max.y - 0.5,
        Stroke::new(1.0, theme::LINE_HAIR),
    );

    let hit = Rect::from_min_size(rect.min, vec2(BAND_HIT_W, BAND_H));
    let pixels = Rect::from_min_size(pos2(hit.max.x, rect.min.y), vec2(BAND_PIXELS_W, BAND_H));
    for x in [hit.max.x, pixels.max.x] {
        painter.vline(x, rect.y_range(), Stroke::new(1.0, theme::LINE_HAIR));
    }

    band_cell(
        painter,
        hit,
        "HIT RATE",
        &hit_rate_str(cache),
        theme::TEXT_PRIMARY,
        &lookup_line(cache),
    );
    band_cell(
        painter,
        pixels,
        "PIXELS FROM CACHE",
        &pixel_share_str(cache),
        theme::WARN,
        &served_line(cache),
    );

    let text = Rect::from_min_max(pos2(pixels.max.x, rect.min.y), rect.max);
    let galley = painter.layout(
        BAND_EXPLANATION.to_owned(),
        theme::sans(12.0),
        theme::TEXT_MUTED,
        (text.width() - PAD_X * 2.0).max(1.0),
    );
    let top = text.center().y - galley.size().y / 2.0;
    painter.galley(pos2(text.min.x + PAD_X, top), galley, theme::TEXT_MUTED);
}

fn band_cell(
    painter: &Painter,
    cell: Rect,
    label: &str,
    value: &str,
    value_colour: Color32,
    sub: &str,
) {
    let x = cell.min.x + PAD_X;
    let galley = painter.layout_job(spaced(
        label,
        theme::sans_medium(11.0),
        theme::TEXT_MUTED,
        11.0 * 0.08,
    ));
    let label_bottom = cell.min.y + 18.0 + galley.size().y;
    painter.galley(pos2(x, cell.min.y + 18.0), galley, theme::TEXT_MUTED);
    let value_rect = painter.text(
        pos2(x, label_bottom + 3.0),
        Align2::LEFT_TOP,
        value,
        theme::mono_medium(30.0),
        value_colour,
    );
    painter.text(
        pos2(x, value_rect.max.y + 3.0),
        Align2::LEFT_TOP,
        sub,
        theme::mono(11.0),
        theme::TEXT_DIM,
    );
}

/// One 18px cell's rect, from the grid's origin.
fn cell_rect(origin: Pos2, index: usize) -> Rect {
    let col = index % GRID_COLS;
    let row = index / GRID_COLS;
    Rect::from_min_size(
        pos2(
            origin.x + col as f32 * CELL_PITCH,
            origin.y + row as f32 * CELL_PITCH,
        ),
        vec2(CELL, CELL),
    )
}

/// The hover (1px) or selection (2px) ring, drawn outside the cell like the handoff's
/// `box-shadow: 0 0 0 Npx`.
fn ring(painter: &Painter, cell: Rect, width: f32) {
    painter.rect_stroke(
        cell,
        CornerRadius::same(theme::radius::CACHE_CELL),
        Stroke::new(width, theme::TEXT_PRIMARY),
        StrokeKind::Outside,
    );
}

/// The legend's ramp bar.
///
/// Occupancy is two states, not a scale, so it gets its two colours rather than the
/// ramp — showing a gradient there would advertise a scale the grid never uses, which is
/// the same mistake rank-binning `Served` exists to avoid.
fn legend_bar(painter: &Painter, bar: Rect, metric: Metric) {
    let radius = CornerRadius::same(theme::radius::CACHE_CELL);
    if metric == Metric::Occupancy {
        let mid = bar.center().x;
        painter.rect_filled(
            Rect::from_min_max(bar.min, pos2(mid, bar.max.y)),
            radius,
            theme::DANGER,
        );
        painter.rect_filled(
            Rect::from_min_max(pos2(mid, bar.min.y), bar.max),
            radius,
            theme::ACCENT,
        );
        return;
    }
    // A gradient, one pixel column at a time: egui has no gradient brush, and 210 rects
    // once per frame is not worth a mesh.
    let steps = bar.width().max(1.0) as usize;
    for i in 0..steps {
        let x = bar.min.x + i as f32;
        painter.rect_filled(
            Rect::from_min_max(pos2(x, bar.min.y), pos2(x + 1.0, bar.max.y)),
            CornerRadius::ZERO,
            theme::heat(i as f32 / (steps.saturating_sub(1).max(1)) as f32),
        );
    }
}

/// A sidebar section label, with an optional right-aligned state word. Returns the next y.
fn section_header(
    painter: &Painter,
    left: f32,
    right: f32,
    y: f32,
    label: &str,
    state: Option<(&str, Color32)>,
) -> f32 {
    let galley = painter.layout_job(spaced(
        label,
        theme::mono(11.0),
        theme::TEXT_MUTED,
        11.0 * 0.14,
    ));
    let height = galley.size().y;
    painter.galley(pos2(left, y), galley, theme::TEXT_MUTED);
    if let Some((text, colour)) = state {
        painter.text(
            pos2(right, y + height / 2.0),
            Align2::RIGHT_CENTER,
            text,
            theme::mono(11.0),
            colour,
        );
    }
    y + height + SIDE_GAP
}

/// A label/value row. Returns the next y.
fn value_row(
    painter: &Painter,
    left: f32,
    right: f32,
    y: f32,
    label: &str,
    value: &str,
    colour: Color32,
) -> f32 {
    painter.text(
        pos2(left, y),
        Align2::LEFT_TOP,
        label,
        theme::sans(12.0),
        theme::TEXT_MUTED,
    );
    painter.text(
        pos2(right, y),
        Align2::RIGHT_TOP,
        value,
        theme::mono(12.0),
        colour,
    );
    y + SIDE_ROW_H
}

fn divider(painter: &Painter, left: f32, right: f32, y: f32) -> f32 {
    painter.hline(
        egui::Rangef::new(left, right),
        y + SIDE_GAP - 0.5,
        Stroke::new(1.0, theme::LINE_HAIR),
    );
    y + SIDE_GAP * 2.0
}

/// One codec share bar: name and percentage over a 5px track. Returns the next y.
fn codec_bar(
    painter: &Painter,
    left: f32,
    right: f32,
    y: f32,
    label: &str,
    share: f32,
    colour: Color32,
) -> f32 {
    let text = painter.text(
        pos2(left, y),
        Align2::LEFT_TOP,
        label,
        theme::sans(12.0),
        theme::TEXT_SECONDARY,
    );
    painter.text(
        pos2(right, y),
        Align2::RIGHT_TOP,
        format!("{}%", (share * 100.0).round() as u64),
        theme::mono(11.0),
        theme::TEXT_MUTED,
    );
    let track = Rect::from_min_max(
        pos2(left, text.max.y + 4.0),
        pos2(right, text.max.y + 4.0 + 5.0),
    );
    let radius = CornerRadius::same(3);
    painter.rect_filled(track, radius, theme::BG_RAISED);
    let filled = track.width() * share.clamp(0.0, 1.0);
    if filled > 0.0 {
        painter.rect_filled(
            Rect::from_min_size(track.min, vec2(filled, track.height())),
            radius,
            colour,
        );
    }
    track.max.y + 9.0
}

/// A single run of text with letter spacing, which [`Painter::text`] cannot express.
fn spaced(text: &str, font: FontId, colour: Color32, spacing: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: font,
            color: colour,
            extra_letter_spacing: spacing,
            ..Default::default()
        },
    );
    job
}

fn text_width(ui: &Ui, text: &str, font: FontId) -> f32 {
    ui.fonts_mut(|f| {
        f.layout_no_wrap(text.to_owned(), font, Color32::WHITE)
            .size()
            .x
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Three slots with nothing in common: distinct ids, sizes, codecs and byte counts,
    /// so a swapped field or a mixed-up slot cannot pass a test by coincidence.
    ///
    /// Returns the slots and the `now` to read them against — 150 s after the first hit,
    /// which makes the three ages 150 s, 90 s and 9 s. Everything is built by *adding* to
    /// one `Instant`: subtracting from `Instant::now()` panics on a freshly booted host.
    fn fixture() -> (SlotStats, Instant) {
        let base = Instant::now();
        let mut slots = SlotStats::new();
        slots.fill(3, 64, 32, "ClearCodec", 1234);
        slots.fill(9, 128, 96, "RemoteFxProgressive", 5678);
        slots.fill(17, 448, 256, "Uncompressed", 9012);
        // Distinct hit counts and distinct ages.
        slots.hit_at(3, 1, base);
        slots.hit_at(9, 4, base + Duration::from_secs(60));
        slots.hit_at(17, 9, base + Duration::from_secs(141));
        (slots, base + Duration::from_secs(150))
    }

    #[test]
    fn served_is_ranked_across_the_ramp_not_scaled_by_value() {
        // 1234 / 22712 / 81108 bytes served: wildly different magnitudes, but rank
        // binning must spread them evenly at 0, 0.5 and 1 — that is the whole reason
        // the handoff bins this metric.
        let (slots, _) = fixture();
        let scale = HeatScale::from_slots(&slots);
        assert_eq!(scale.served_t(1234), 0.0);
        assert_eq!(scale.served_t(5678 * 4), 0.5);
        assert_eq!(scale.served_t(9012 * 9), 1.0);

        // Equal values must share a rank: ranking them against each other would invent
        // an ordering the data does not have.
        let mut flat = SlotStats::new();
        flat.fill(1, 8, 8, "ClearCodec", 100);
        flat.fill(2, 8, 8, "ClearCodec", 100);
        flat.fill(3, 8, 8, "ClearCodec", 700);
        flat.hit(1, 1);
        flat.hit(2, 1);
        flat.hit(3, 1);
        let flat_scale = HeatScale::from_slots(&flat);
        assert_eq!(flat_scale.served_t(100), 0.0);
        assert_eq!(flat_scale.served_t(700), 1.0);

        // A cache where nothing has been served has no ranking to show, and must not
        // paint a slot as "carrying the session".
        let mut cold = SlotStats::new();
        cold.fill(4, 8, 8, "ClearCodec", 64);
        assert_eq!(HeatScale::from_slots(&cold).served_t(0), 0.0);
    }

    #[test]
    fn each_metric_computes_the_value_the_handoff_specifies() {
        let (slots, now) = fixture();
        let scale = HeatScale::from_slots(&slots);
        let slot3 = *slots.get(3).unwrap();
        let slot9 = *slots.get(9).unwrap();
        let slot17 = *slots.get(17).unwrap();

        // Hits: log1p(hits) / log1p(max hits), max being slot 17's nine.
        let expected = (1.0_f64.ln_1p() / 9.0_f64.ln_1p()) as f32;
        assert!(
            (heat_t(Metric::Hits, &slot3, &scale, now).unwrap() - expected).abs() < 1e-6,
            "log1p(1)/log1p(9)"
        );
        assert_eq!(heat_t(Metric::Hits, &slot17, &scale, now), Some(1.0));

        // Recency: 1 - min(1, age_ms/180000). Slot 9 was hit 90 s ago — exactly half.
        assert!((heat_t(Metric::Recency, &slot9, &scale, now).unwrap() - 0.5).abs() < 1e-3);
        // And 150 s ago is 1 - 150/180.
        assert!((heat_t(Metric::Recency, &slot3, &scale, now).unwrap() - (1.0 / 6.0)).abs() < 1e-3);
        // Past the span it floors rather than going negative.
        let stale = now + Duration::from_secs(600);
        assert_eq!(heat_t(Metric::Recency, &slot17, &scale, stale), Some(0.0));

        // Return: min(1, log1p(served/stored) / log1p(20)). Slot 9 served 4x its size.
        let expected = (4.0_f64.ln_1p() / 20.0_f64.ln_1p()) as f32;
        assert!((heat_t(Metric::Return, &slot9, &scale, now).unwrap() - expected).abs() < 1e-6);
        // Twentyfold payback is the ceiling, and beyond it stays there.
        let mut rich = SlotStats::new();
        rich.fill(5, 16, 16, "ClearCodec", 10);
        rich.hit(5, 40);
        let rich_slot = *rich.get(5).unwrap();
        let rich_scale = HeatScale::from_slots(&rich);
        assert_eq!(
            heat_t(Metric::Return, &rich_slot, &rich_scale, now),
            Some(1.0)
        );

        // Occupancy has no ramp at all: two states, two colours.
        assert_eq!(heat_t(Metric::Occupancy, &slot17, &scale, now), None);
    }

    #[test]
    fn empty_slots_are_flat_and_occupancy_reads_state_not_heat() {
        let (mut slots, base) = fixture();
        slots.evict(9);
        let scale = HeatScale::from_slots(&slots);
        let live = *slots.get(17).unwrap();
        let evicted = *slots.get(9).unwrap();

        assert_eq!(
            cell_colour(Metric::Occupancy, Some(&live), &scale, base),
            theme::ACCENT
        );
        assert_eq!(
            cell_colour(Metric::Occupancy, Some(&evicted), &scale, base),
            theme::DANGER
        );
        // A position no PDU ever touched, and an explicitly empty record: both flat.
        assert_eq!(
            cell_colour(Metric::Served, None, &scale, base),
            theme::HEAT_EMPTY
        );
        let never = SlotStat::empty(21);
        assert_eq!(
            cell_colour(Metric::Served, Some(&never), &scale, base),
            theme::HEAT_EMPTY
        );
        assert_eq!(
            cell_colour(Metric::Occupancy, Some(&never), &scale, base),
            theme::HEAT_EMPTY,
            "an empty slot is not an evicted one"
        );
        // A filled slot does take a ramp colour, or the assertions above prove nothing.
        assert_ne!(
            cell_colour(Metric::Served, Some(&live), &scale, base),
            theme::HEAT_EMPTY
        );
    }

    #[test]
    fn the_default_selection_is_the_busiest_live_slot() {
        let (mut slots, _) = fixture();
        // Slot 17 serves 81_108 bytes, the most of the three.
        assert_eq!(default_selection(&slots), Some(17));

        // Evicting it must move the default on: the sidebar exists to explain a slot
        // that is still doing work.
        slots.evict(17);
        assert_eq!(default_selection(&slots), Some(9));

        // With nothing served at all it still lands on a live slot, never an empty one,
        // and takes the lowest id so two refreshes agree.
        let mut idle = SlotStats::new();
        idle.fill(6, 32, 32, "ClearCodec", 500);
        idle.fill(2, 32, 32, "ClearCodec", 500);
        assert_eq!(default_selection(&idle), Some(2));

        // No live slot: no default rather than a misleading one.
        let mut dead = SlotStats::new();
        dead.fill(4, 32, 32, "ClearCodec", 500);
        dead.evict(4);
        assert_eq!(default_selection(&dead), None);
        assert_eq!(default_selection(&SlotStats::new()), None);
    }

    #[test]
    fn a_selection_survives_a_refresh_that_changed_the_slots() {
        let (mut slots, _) = fixture();
        let mut window = CacheWindow::new();
        window.adopt_default_selection(&slots);
        assert_eq!(
            window.selected(),
            Some(17),
            "opens on the busiest live slot"
        );

        // The user picks a quieter slot.
        window.selected = Some(3);

        // A refresh a second later: slot 3 is now the busiest, slot 17 is evicted, and
        // the default would be a different slot. The user's choice must still stand.
        slots.hit(3, 500);
        slots.evict(17);
        assert_eq!(default_selection(&slots), Some(3));
        window.adopt_default_selection(&slots);
        assert_eq!(window.selected(), Some(3));

        // And a selection the default would never make survives too.
        window.selected = Some(9);
        window.adopt_default_selection(&slots);
        assert_eq!(window.selected(), Some(9), "a refresh must not re-select");
    }

    #[test]
    fn the_legend_labels_change_with_the_metric() {
        // Verbatim from the handoff's metric table; these are the only words that tell
        // the user what the colours mean.
        assert_eq!(
            Metric::Served.legend(),
            ("no bytes served", "carrying the session")
        );
        assert_eq!(Metric::Hits.legend(), ("never hit", "hit constantly"));
        assert_eq!(Metric::Recency.legend(), ("stale", "hit just now"));
        assert_eq!(
            Metric::Return.legend(),
            ("stored, barely used", "paid back ×20")
        );
        assert_eq!(Metric::Occupancy.legend(), ("evicted", "live entry"));
    }

    #[test]
    fn the_grid_covers_every_slot_position_up_to_the_highest_seen() {
        let (slots, _) = fixture();
        // Highest id is 17, so the grid draws positions 0..=17 — the untouched ones as
        // empty cells, because that is what they are.
        assert_eq!(grid_capacity(&slots), 18);
        assert_eq!(slots.len(), 3, "only three were ever touched");
        assert_eq!(grid_capacity(&SlotStats::new()), 0);
    }

    #[test]
    fn the_grid_caption_counts_what_the_grid_draws() {
        let (mut slots, _) = fixture();
        slots.evict(9);
        assert_eq!(grid_caption(&slots), "18 slots · 2 live · 1 evicted");
        assert_eq!(state_counts(&slots), (2, 1));
    }

    #[test]
    fn a_click_lands_on_the_cell_under_it_and_the_gaps_select_nothing() {
        let origin = pos2(100.0, 50.0);
        // First cell.
        assert_eq!(cell_index_at(origin, pos2(101.0, 51.0), 60), Some(0));
        // Third column, second row: 2 * 20 = 40 across, 20 down.
        assert_eq!(cell_index_at(origin, pos2(141.0, 71.0), 60), Some(29));
        // The last column, and one pixel past it — the grid is 27 wide.
        assert_eq!(
            cell_index_at(origin, pos2(100.0 + 26.0 * 20.0, 51.0), 60),
            Some(26)
        );
        assert_eq!(
            cell_index_at(origin, pos2(100.0 + 27.0 * 20.0, 51.0), 60),
            None
        );
        // The 2px gutter belongs to no cell.
        assert_eq!(cell_index_at(origin, pos2(119.0, 51.0), 60), None);
        assert_eq!(cell_index_at(origin, pos2(101.0, 69.0), 60), None);
        // Above and left of the grid, and past the last slot.
        assert_eq!(cell_index_at(origin, pos2(99.0, 51.0), 60), None);
        assert_eq!(cell_index_at(origin, pos2(101.0, 49.0), 60), None);
        assert_eq!(cell_index_at(origin, pos2(101.0, 51.0), 0), None);

        // The drawn rect and the hit test must agree, or a click lands on a neighbour.
        for index in [0_usize, 26, 27, 53] {
            let r = cell_rect(origin, index);
            assert_eq!(cell_index_at(origin, r.center(), 60), Some(index));
        }
    }

    #[test]
    fn codec_shares_weigh_bytes_and_stay_in_a_stable_order() {
        let (mut slots, _) = fixture();
        // Stored: ClearCodec 1234, RFX 5678, Uncompressed 9012 — 15_924 in all.
        let shares = codec_shares(&slots);
        assert_eq!(shares.len(), 3);
        assert_eq!(shares[0].0, "Uncompressed");
        assert_eq!(shares[1].0, "RemoteFxProgressive");
        assert_eq!(shares[2].0, "ClearCodec");
        assert!((shares[0].1 - 9012.0 / 15_924.0).abs() < 1e-6);
        assert!((shares.iter().map(|s| s.1).sum::<f32>() - 1.0).abs() < 1e-5);

        // An evicted slot still shows what flowed through the cache.
        slots.evict(17);
        assert_eq!(codec_shares(&slots)[0].0, "Uncompressed");

        assert!(codec_shares(&SlotStats::new()).is_empty());
        assert_eq!(codec_label("RemoteFxProgressive"), "RFX Progressive");
        assert_eq!(codec_label("ClearCodec"), "ClearCodec");
    }

    #[test]
    fn the_headline_figures_read_off_the_aggregate_counters() {
        let cache = CacheStats {
            hits: 2043,
            misses: 8,
            entries: 314,
            evictions: 50,
            bytes_served: 224_395_264,
            bytes_stored: 33_554_432,
            bytes_from_wire: 141_557_760,
        };
        assert_eq!(hit_rate_str(&cache), "99.6%");
        assert_eq!(lookup_line(&cache), "2,043 hits / 8 misses");
        assert_eq!(pixel_share_str(&cache), "61%");
        assert_eq!(served_line(&cache), "214.0 MiB of 349.0 MiB painted");

        // Nothing looked up is unknown, not zero.
        let empty = CacheStats::default();
        assert_eq!(hit_rate_str(&empty), "—");
        assert_eq!(pixel_share_str(&empty), "—");
    }

    #[test]
    fn slot_figures_read_the_way_the_sidebar_shows_them() {
        assert_eq!(bytes_str(900), "900 B");
        assert_eq!(bytes_str(44_032), "43 KiB");
        assert_eq!(bytes_str(224_395_264), "214.0 MiB");
        assert_eq!(bytes_str(3 * 1024 * 1024 * 1024), "3.0 GiB");

        let hit = Instant::now();
        assert_eq!(age_str(None, hit), "never");
        assert_eq!(
            age_str(Some(hit), hit + Duration::from_millis(740)),
            "740ms ago"
        );
        assert_eq!(
            age_str(Some(hit), hit + Duration::from_millis(1400)),
            "1.4s ago"
        );
        assert_eq!(age_str(Some(hit), hit + Duration::from_secs(200)), "3m ago");

        let mut slots = SlotStats::new();
        slots.fill(7, 448, 448, "ClearCodec", 1000);
        slots.hit(7, 12);
        let slot = *slots.get(7).unwrap();
        assert_eq!(return_str(&slot), "×12.0");
        assert_eq!(return_str(&SlotStat::empty(8)), "—");
    }
}
