//! "Latency and drift" — handoff screen 5, the window this client exists to defend.
//!
//! The whole point of the screen is that the drift figure and the chart agree. They can
//! only agree if the bars and both reference lines are placed with **one** full-scale
//! value, so that scale is computed once, in [`plot_model`], and everything the plot
//! draws reads it from there. An eyeballed line offset would make the chart contradict
//! the headline number, which is the one number a user is being asked to trust.
//!
//! Layout, from the handoff: header 54 · metric row `padding:24px 24px 0` (a drift card
//! beside a 2×2 tile grid) · chart area `flex:1`, `padding:24px`. The 30px menu strip
//! above and the 900×700 window around it belong to the host.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Margin, Painter, Rect, RichText,
    Sense, Stroke, Ui, UiBuilder, pos2, vec2,
};

use crate::diag::{ms1, paint_card, signed_ms1, thousands};
use crate::shell::widgets;
use crate::stats::{BASELINE_SAMPLES, SessionStats, WINDOW};
use crate::ui::theme;

/// What the window wants its host to do after the frame is drawn.
///
/// Returned rather than acted on so the screen stays pure over its inputs: writing a
/// file is the host's business, and a screen that cannot touch the disk cannot surprise
/// anyone by touching it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LatencyAction {
    #[default]
    None,
    WriteMetrics,
    /// The header's Close button: the host should close this window. Drawn in the
    /// window itself because a diagnostics window over a fullscreen session has no
    /// OS titlebar to close it with.
    Close,
}

// --- geometry (handoff values) --------------------------------------------------------

/// Header band, matching the bitmap-cache window.
const HEADER_H: f32 = 54.0;
/// Side padding shared by the header, the metric row and the chart area.
const PAD_X: f32 = 24.0;
/// The metric row's own height, below its 24px top padding.
///
/// Sized so a 2×2 grid of 72px tiles with a 12px gutter fits exactly; the drift card
/// beside it takes the same height.
const METRIC_ROW_H: f32 = 156.0;
/// Gutter between the drift card and the tile grid.
const METRIC_GAP: f32 = 20.0;
/// Gutter between tiles, both axes.
const TILE_GAP: f32 = 12.0;
/// Inner padding of the plot box, all four sides.
const PLOT_PAD: f32 = 16.0;
/// The chart area's own internal gaps, and the footer row it reserves.
const CHART_GAP: f32 = 14.0;
const FOOTER_H: f32 = 30.0;

/// The widest gap between bars. Shrinks before any sample is dropped.
const BAR_GAP: f32 = 2.0;
/// A bar is never narrower than this, so a full window still reads as bars.
const MIN_BAR_W: f32 = 1.0;
/// A sample is never drawn shorter than this fraction of the plot, or a fast session
/// looks like an empty chart.
const MIN_BAR_FRACTION: f32 = 0.03;
/// The plot's full-scale is rounded up to a multiple of this, in microseconds, so the
/// scale does not twitch on every refresh.
const SCALE_STEP_US: f32 = 5_000.0;

// --- the maths, kept out of the drawing ----------------------------------------------

/// The plot's full-scale, in microseconds, derived from the data that will be drawn.
///
/// `display` is exactly the sample slice the bars will use, so the tallest bar cannot
/// overflow the box it is drawn in. p99 gets 20% headroom and the baseline is included
/// because both are drawn as reference lines: a scale that clipped either would hide the
/// comparison the window is for. Rounded up to [`SCALE_STEP_US`] so a single outlier
/// aging out of the window does not visibly rescale the whole chart.
///
/// The handoff's mock (peak 41.3 ms, p99 19.8 ms, baseline 7.8 ms) lands on 45 ms here,
/// which is the full-scale it quotes.
pub fn full_scale_us(display: &[u32], p99_us: u32, baseline_p50_us: u32) -> f32 {
    let peak = display.iter().copied().max().unwrap_or(0) as f32;
    let raw = peak
        .max(p99_us as f32 * 1.2)
        .max(baseline_p50_us as f32)
        .max(0.0);
    (raw / SCALE_STEP_US).ceil().max(1.0) * SCALE_STEP_US
}

/// The colour band a sample falls in: `accent` below p95, `warn` up to p99, `danger`
/// above it.
///
/// Boundaries are inclusive at the lower end — a sample equal to p95 is drawn `warn` —
/// so the bands cover the range with no value able to fall between two of them.
pub fn bar_colour(sample_us: u32, p95_us: u32, p99_us: u32) -> Color32 {
    if sample_us > p99_us {
        theme::DANGER
    } else if sample_us >= p95_us {
        theme::WARN
    } else {
        theme::ACCENT
    }
}

/// How the bars divide the plot's content width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarLayout {
    /// How many of the most recent samples are drawn.
    pub visible: usize,
    /// Pitch from one bar's left edge to the next.
    pub slot: f32,
    pub bar_w: f32,
    pub gap: f32,
}

/// Fit `count` bars into `content_width`, shrinking the gap before dropping any sample.
///
/// A full 512-sample window in an 820px plot leaves 1.6px per sample, so the handoff's
/// 2px gap cannot survive intact; losing the gap is a cosmetic loss, while dropping
/// samples would silently narrow the window the header claims to be showing. Samples are
/// only dropped when even a 1px gapless bar will not fit, and then the *oldest* go.
pub fn bar_layout(count: usize, content_width: f32) -> BarLayout {
    if count == 0 || content_width < MIN_BAR_W {
        return BarLayout {
            visible: 0,
            slot: 0.0,
            bar_w: 0.0,
            gap: 0.0,
        };
    }
    let capacity = (content_width / MIN_BAR_W).floor() as usize;
    let visible = count.min(capacity.max(1));
    let slot = content_width / visible as f32;
    let gap = (slot - MIN_BAR_W).clamp(0.0, BAR_GAP);
    BarLayout {
        visible,
        slot,
        bar_w: slot - gap,
        gap,
    }
}

/// Bar heights in pixels, one per sample, against `scale_us` and the plot's content box.
///
/// Linear, clamped at the ceiling, with a [`MIN_BAR_FRACTION`] floor so a sample well
/// under the scale is still visible as a bar.
pub fn bar_heights(samples: &[u32], scale_us: f32, content_height: f32) -> Vec<f32> {
    if scale_us <= 0.0 || content_height <= 0.0 {
        return vec![0.0; samples.len()];
    }
    samples
        .iter()
        .map(|&v| {
            let fraction = (v as f32 / scale_us).clamp(MIN_BAR_FRACTION, 1.0);
            fraction * content_height
        })
        .collect()
}

/// Where a reference line sits, as a fraction of the plot's content height from the
/// bottom.
///
/// This is deliberately the same arithmetic the bars use, against the same scale: the
/// handoff's 45 ms full-scale puts its 7.8 ms baseline at 17.3%, and a bar of 7.8 ms
/// must reach exactly that height or the chart contradicts itself.
pub fn reference_fraction(value_us: u32, scale_us: f32) -> f32 {
    if scale_us <= 0.0 {
        return 0.0;
    }
    (value_us as f32 / scale_us).clamp(0.0, 1.0)
}

/// Everything the plot draws, derived once.
///
/// One `scale_us` field, shared by the bars and both reference lines — the structural
/// version of the handoff's "both reference lines must be derived from the same
/// full-scale as the bar heights".
#[derive(Debug, Clone, PartialEq)]
pub struct PlotModel {
    /// The most recent samples that fit, oldest first.
    pub samples: Vec<u32>,
    pub layout: BarLayout,
    pub scale_us: f32,
    pub p95_us: u32,
    pub p99_us: u32,
    /// The frozen baseline median, once there is one.
    pub baseline_p50_us: Option<u32>,
}

/// Derive the plot from a stats snapshot and the plot's content width.
pub fn plot_model(stats: &SessionStats, content_width: f32) -> PlotModel {
    let all = stats.latency.recent_samples();
    let layout = bar_layout(all.len(), content_width);
    let samples = all[all.len() - layout.visible..].to_vec();
    let recent = stats.latency.recent().unwrap_or_default();
    let baseline_p50_us = stats.latency.baseline().map(|b| b.p50);
    PlotModel {
        scale_us: full_scale_us(&samples, recent.p99, baseline_p50_us.unwrap_or(0)),
        samples,
        layout,
        p95_us: recent.p95,
        p99_us: recent.p99,
        baseline_p50_us,
    }
}

/// The header's right-hand caption: lifetime sample count and the rolling window size.
pub fn header_right(stats: &SessionStats) -> String {
    format!(
        "n = {} · window {}",
        thousands(stats.latency.count()),
        WINDOW
    )
}

/// The drift card's sub-caption, with the baseline size taken from the constant that
/// governs it rather than restated.
pub fn drift_caption() -> String {
    format!(
        "Median now, less the baseline frozen from the first {BASELINE_SAMPLES} samples. \
         Never re-baselined."
    )
}

/// The line under the plot: the lifetime minimum, and how the percentiles are computed.
pub fn min_caption(stats: &SessionStats) -> String {
    match stats.latency.min() {
        Some(min) => format!("min {} ms · exact percentiles, sorted per read", ms1(min)),
        None => "no samples yet · exact percentiles, sorted per read".to_owned(),
    }
}

// --- drawing --------------------------------------------------------------------------

/// Draw the whole window body from one stats snapshot, without a session label.
///
/// The label is the host's to supply — see [`ui_with_session`].
pub fn ui(ui: &mut Ui, stats: &SessionStats) -> LatencyAction {
    ui_with_session(ui, stats, None)
}

/// [`ui`] with the header's identity chunk filled in.
///
/// `session` is `(name, detail)` exactly as the handoff header reads it — for example
/// `("Temper", "alice@temper:3389 · pid 4821")`. `None` draws the title alone and omits
/// the divider, which is what a host that has not resolved the session yet should pass.
pub fn ui_with_session(
    ui: &mut Ui,
    stats: &SessionStats,
    session: Option<(&str, &str)>,
) -> LatencyAction {
    let header_action = header(ui, stats, session);
    metric_row(ui, stats);
    let chart_action = chart(ui, stats);
    if header_action == LatencyAction::None {
        chart_action
    } else {
        header_action
    }
}

fn header(ui: &mut Ui, stats: &SessionStats, session: Option<(&str, &str)>) -> LatencyAction {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER_H), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.max.y - 0.5,
        Stroke::new(1.0, theme::LINE_HAIR),
    );

    let cy = rect.center().y;
    let title = ui.painter().text(
        pos2(rect.min.x + PAD_X, cy),
        Align2::LEFT_CENTER,
        "Latency and drift",
        theme::sans_semibold(15.0),
        theme::TEXT_PRIMARY,
    );

    // The right-aligned content goes down first, so the session line knows exactly
    // where it must stop instead of painting straight through it.
    let mut action = LatencyAction::None;
    let area = Rect::from_min_max(
        pos2(rect.min.x + PAD_X, rect.min.y),
        pos2(rect.max.x - PAD_X, rect.max.y),
    );
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(area)
            .layout(Layout::right_to_left(Align::Center)),
    );
    if widgets::secondary_button(&mut child, "Close", 26.0).clicked() {
        action = LatencyAction::Close;
    }
    child.add_space(16.0 - child.spacing().item_spacing.x);
    child.label(
        RichText::new(header_right(stats))
            .font(theme::mono(11.0))
            .color(theme::TEXT_DIM),
    );
    let right_edge = child.min_rect().min.x - 14.0;

    if let Some((name, detail)) = session {
        crate::diag::session_line(ui.painter(), cy, title.max.x, right_edge, name, detail);
    }
    action
}

fn metric_row(ui: &mut Ui, stats: &SessionStats) {
    let (outer, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), 24.0 + METRIC_ROW_H),
        Sense::hover(),
    );
    let row = Rect::from_min_max(
        pos2(outer.min.x + PAD_X, outer.min.y + 24.0),
        pos2(outer.max.x - PAD_X, outer.max.y),
    );
    let half = (row.width() - METRIC_GAP) / 2.0;
    let painter = ui.painter();

    drift_card(
        painter,
        Rect::from_min_size(row.min, vec2(half, METRIC_ROW_H)),
        stats,
    );

    let grid = Rect::from_min_size(
        pos2(row.min.x + half + METRIC_GAP, row.min.y),
        vec2(half, METRIC_ROW_H),
    );
    let tile_w = (grid.width() - TILE_GAP) / 2.0;
    let tile_h = (grid.height() - TILE_GAP) / 2.0;
    let recent = stats.latency.recent();
    let cells: [(&str, Option<u32>, Color32); 4] = [
        ("p50", recent.map(|p| p.p50), theme::TEXT_PRIMARY),
        ("p95", recent.map(|p| p.p95), theme::TEXT_PRIMARY),
        ("p99", recent.map(|p| p.p99), theme::WARN),
        (
            "max",
            recent.map(|_| stats.latency.max()),
            theme::TEXT_PRIMARY,
        ),
    ];
    for (i, (label, value, colour)) in cells.into_iter().enumerate() {
        let origin = pos2(
            grid.min.x + (i % 2) as f32 * (tile_w + TILE_GAP),
            grid.min.y + (i / 2) as f32 * (tile_h + TILE_GAP),
        );
        tile(
            painter,
            Rect::from_min_size(origin, vec2(tile_w, tile_h)),
            label,
            value,
            colour,
        );
    }
}

fn drift_card(painter: &Painter, rect: Rect, stats: &SessionStats) {
    paint_card(painter, rect);
    let x = rect.min.x + 20.0;
    let label = painter.layout_job(spaced(
        "DRIFT SINCE CONNECT",
        theme::sans_medium(11.0),
        theme::TEXT_MUTED,
        11.0 * 0.08,
    ));
    let label_h = label.size().y;
    painter.galley(pos2(x, rect.min.y + 18.0), label, theme::TEXT_MUTED);

    let value_top = rect.min.y + 18.0 + label_h + 6.0;
    let value_rect = match stats.latency.drift_us() {
        Some(drift) => {
            let value = painter.text(
                pos2(x, value_top),
                Align2::LEFT_TOP,
                signed_ms1(drift),
                theme::mono_medium(38.0),
                theme::ACCENT,
            );
            painter.text(
                pos2(value.max.x + 10.0, value.max.y),
                Align2::LEFT_BOTTOM,
                "ms",
                theme::mono(14.0),
                theme::TEXT_MUTED,
            );
            value
        }
        // Before the baseline freezes there is no drift to report, and a zero would read
        // as a measured one.
        None => painter.text(
            pos2(x, value_top),
            Align2::LEFT_TOP,
            "—",
            theme::mono_medium(38.0),
            theme::TEXT_MUTED,
        ),
    };

    let sub = painter.layout(
        drift_caption(),
        theme::sans(12.0),
        theme::TEXT_DIM,
        rect.width() - 40.0,
    );
    painter.galley(pos2(x, value_rect.max.y + 6.0), sub, theme::TEXT_DIM);
}

fn tile(painter: &Painter, rect: Rect, label: &str, value: Option<u32>, colour: Color32) {
    paint_card(painter, rect);
    let x = rect.min.x + 16.0;
    let label_rect = painter.text(
        pos2(x, rect.min.y + 14.0),
        Align2::LEFT_TOP,
        label,
        theme::sans(11.0),
        theme::TEXT_MUTED,
    );
    let (text, colour) = match value {
        Some(v) => (format!("{} ms", ms1(v)), colour),
        None => ("—".to_owned(), theme::TEXT_MUTED),
    };
    painter.text(
        pos2(x, label_rect.max.y + 3.0),
        Align2::LEFT_TOP,
        text,
        theme::mono(20.0),
        colour,
    );
}

fn chart(ui: &mut Ui, stats: &SessionStats) -> LatencyAction {
    // Everything above the footer is painted; the footer holds a real button and so has
    // to be a real Ui.
    let paint_h = (ui.available_height() - FOOTER_H - CHART_GAP - PAD_X).max(0.0);
    let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width(), paint_h), Sense::hover());
    let inner = Rect::from_min_max(
        pos2(outer.min.x + PAD_X, outer.min.y + PAD_X),
        pos2(outer.max.x - PAD_X, outer.max.y),
    );
    let painter = ui.painter();

    // Section label and legend share one baseline row.
    let label = painter.layout_job(spaced(
        "ROUND TRIP, LAST 512 SAMPLES",
        theme::mono(11.0),
        theme::TEXT_MUTED,
        11.0 * 0.14,
    ));
    let label_h = label.size().y;
    painter.galley(inner.min, label, theme::TEXT_MUTED);

    let model = plot_model(stats, inner.width() - PLOT_PAD * 2.0);
    legend(painter, inner, label_h, &model);

    let plot = Rect::from_min_max(
        pos2(inner.min.x, inner.min.y + label_h + CHART_GAP),
        inner.max,
    );
    paint_card(painter, plot);
    let content = plot.shrink(PLOT_PAD);
    plot_bars(painter, content, &model);

    // Footer: min line left, the metrics-write action right.
    let mut action = LatencyAction::None;
    Frame::new()
        .inner_margin(Margin {
            left: PAD_X as i8,
            right: PAD_X as i8,
            top: CHART_GAP as i8,
            bottom: PAD_X as i8,
        })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(min_caption(stats))
                        .font(theme::mono(11.0))
                        .color(theme::TEXT_DIM),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::secondary_button(ui, "Write metrics JSON", FOOTER_H).clicked() {
                        action = LatencyAction::WriteMetrics;
                    }
                });
            });
        });
    action
}

fn legend(painter: &Painter, inner: Rect, label_h: f32, model: &PlotModel) {
    let cy = inner.min.y + label_h / 2.0;
    let mut right = inner.max.x;
    let entry = |text: String, swatch: Color32, right: &mut f32| {
        let text_rect = painter.text(
            pos2(*right, cy),
            Align2::RIGHT_CENTER,
            text,
            theme::sans(11.0),
            theme::TEXT_DIM,
        );
        let bar = Rect::from_min_size(
            pos2(text_rect.min.x - 7.0 - 14.0, cy - 1.0),
            vec2(14.0, 2.0),
        );
        painter.rect_filled(bar, CornerRadius::ZERO, swatch);
        *right = bar.min.x - 16.0;
    };
    entry("p95".to_owned(), theme::WARN, &mut right);
    let baseline = match model.baseline_p50_us {
        Some(us) => format!("baseline p50 {} ms", ms1(us)),
        None => format!("baseline pending ({BASELINE_SAMPLES} samples)"),
    };
    entry(baseline, theme::INFO, &mut right);
}

fn plot_bars(painter: &Painter, content: Rect, model: &PlotModel) {
    let heights = bar_heights(&model.samples, model.scale_us, content.height());
    for (i, (&sample, &h)) in model.samples.iter().zip(heights.iter()).enumerate() {
        let x = content.min.x + i as f32 * model.layout.slot;
        let bar = Rect::from_min_max(
            pos2(x, content.max.y - h),
            pos2(x + model.layout.bar_w, content.max.y),
        );
        painter.rect_filled(
            bar,
            CornerRadius::same(1),
            bar_colour(sample, model.p95_us, model.p99_us),
        );
    }
    // Reference lines last, so they read over the bars — and from the same scale, which
    // is the whole point of the chart.
    if let Some(baseline) = model.baseline_p50_us {
        reference_line(painter, content, baseline, model.scale_us, theme::INFO, 0.7);
    }
    if model.p95_us > 0 {
        reference_line(
            painter,
            content,
            model.p95_us,
            model.scale_us,
            theme::WARN,
            0.5,
        );
    }
}

fn reference_line(
    painter: &Painter,
    content: Rect,
    value_us: u32,
    scale_us: f32,
    colour: Color32,
    opacity: f32,
) {
    let y = content.max.y - reference_fraction(value_us, scale_us) * content.height();
    painter.hline(
        content.x_range(),
        y,
        Stroke::new(1.0, colour.gamma_multiply(opacity)),
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The handoff mock's numbers, in microseconds: p95 14.1 ms, p99 19.8 ms,
    /// baseline p50 7.8 ms, peak sample 41.3 ms.
    const MOCK_P95: u32 = 14_100;
    const MOCK_P99: u32 = 19_800;
    const MOCK_BASELINE: u32 = 7_800;
    const MOCK_PEAK: u32 = 41_300;

    fn stats_with(samples: &[u32]) -> SessionStats {
        let mut s = SessionStats::new();
        for &v in samples {
            s.latency.record(v);
        }
        s
    }

    #[test]
    fn a_bar_takes_its_colour_from_the_percentile_band() {
        // Four distinct values, one per band plus both boundaries.
        assert_eq!(bar_colour(8_200, MOCK_P95, MOCK_P99), theme::ACCENT);
        assert_eq!(bar_colour(16_400, MOCK_P95, MOCK_P99), theme::WARN);
        assert_eq!(bar_colour(MOCK_PEAK, MOCK_P95, MOCK_P99), theme::DANGER);
        assert_eq!(
            bar_colour(MOCK_P95, MOCK_P95, MOCK_P99),
            theme::WARN,
            "the band is inclusive at its lower edge"
        );
        assert_eq!(
            bar_colour(MOCK_P99, MOCK_P95, MOCK_P99),
            theme::WARN,
            "danger starts above p99, not at it"
        );
    }

    #[test]
    fn the_full_scale_reproduces_the_handoffs_45_ms() {
        // Peak 41.3 ms rounds up to the next 5 ms step, exactly as the mock quotes.
        let scale = full_scale_us(&[8_200, 16_400, MOCK_PEAK], MOCK_P99, MOCK_BASELINE);
        assert_eq!(scale, 45_000.0);
    }

    #[test]
    fn the_full_scale_covers_the_tail_and_the_baseline_even_without_a_tall_sample() {
        // No sample above 3 ms, but a p99 of 19.8 ms: 19.8 * 1.2 = 23.76 -> 25 ms.
        assert_eq!(full_scale_us(&[2_100, 3_000], MOCK_P99, 1_000), 25_000.0);
        // A baseline above everything else still has to be drawable.
        assert_eq!(full_scale_us(&[2_100], 3_000, 31_000), 35_000.0);
        // Nothing at all is still a usable scale, not a zero divide.
        assert_eq!(full_scale_us(&[], 0, 0), 5_000.0);
    }

    #[test]
    fn the_reference_lines_use_the_same_scale_as_the_bars() {
        // The handoff's worked example: 45 ms full-scale puts 7.8 ms at 17.3% and
        // 14.1 ms at 31.3%.
        let scale = 45_000.0;
        assert!((reference_fraction(MOCK_BASELINE, scale) - 0.173_333).abs() < 1e-5);
        assert!((reference_fraction(MOCK_P95, scale) - 0.313_333).abs() < 1e-5);

        // And a bar of the baseline value reaches exactly the baseline line: same
        // arithmetic, same scale, or the chart contradicts its own headline.
        let content_h = 300.0;
        let bar = bar_heights(&[MOCK_BASELINE], scale, content_h)[0];
        assert_eq!(bar, reference_fraction(MOCK_BASELINE, scale) * content_h);
    }

    #[test]
    fn bar_heights_clamp_at_the_ceiling_and_keep_a_visible_floor() {
        let heights = bar_heights(&[90_000, 100, 22_500], 45_000.0, 400.0);
        assert_eq!(heights[0], 400.0, "a sample past the scale fills the box");
        assert_eq!(heights[1], 12.0, "0.1 ms still draws a 3% bar");
        assert_eq!(heights[2], 200.0, "half-scale is half the box");
    }

    #[test]
    fn bar_layout_shrinks_the_gap_before_dropping_a_sample() {
        // The mock's 96 bars in an 820px plot keep the full 2px gap.
        let roomy = bar_layout(96, 820.0);
        assert_eq!(roomy.visible, 96);
        assert_eq!(roomy.gap, 2.0);
        assert!((roomy.bar_w - (820.0 / 96.0 - 2.0)).abs() < 1e-4);

        // A full 512-sample window in the same plot cannot afford the gap, but keeps
        // every sample.
        let full = bar_layout(512, 820.0);
        assert_eq!(full.visible, 512, "the window is not silently narrowed");
        assert!(full.gap < 2.0 && full.gap > 0.0, "gap {}", full.gap);
        assert_eq!(full.bar_w, 1.0);

        // Narrower than one pixel per sample: the oldest samples go.
        let cramped = bar_layout(512, 300.0);
        assert_eq!(cramped.visible, 300);
        assert_eq!(cramped.gap, 0.0);

        assert_eq!(bar_layout(0, 820.0).visible, 0);
    }

    #[test]
    fn the_plot_model_drops_the_oldest_samples_and_scales_to_what_it_kept() {
        // 400 samples of 2 ms then 20 of 30 ms: a 20-wide plot must keep the recent
        // tail, not the opening.
        let mut samples = vec![2_000_u32; 400];
        samples.extend(std::iter::repeat_n(30_000_u32, 20));
        let stats = stats_with(&samples);

        let model = plot_model(&stats, 20.0);
        assert_eq!(model.layout.visible, 20);
        assert_eq!(model.samples, vec![30_000_u32; 20]);
        // p99 is 30 ms here, so the 20% tail headroom (36 ms) sets the scale, rounded up
        // to the next 5 ms step.
        assert_eq!(model.scale_us, 40_000.0);
        assert_eq!(model.baseline_p50_us, Some(2_000), "frozen at the opening");
    }

    #[test]
    fn the_header_counts_every_sample_and_names_the_window() {
        let stats = stats_with(&vec![3_300_u32; 1_001]);
        assert_eq!(header_right(&stats), "n = 1,001 · window 512");
    }

    #[test]
    fn the_captions_read_off_the_data_rather_than_inventing_zeroes() {
        let empty = SessionStats::new();
        assert_eq!(
            min_caption(&empty),
            "no samples yet · exact percentiles, sorted per read"
        );
        let stats = stats_with(&[5_100, 9_400, 41_300]);
        assert_eq!(
            min_caption(&stats),
            "min 5.1 ms · exact percentiles, sorted per read"
        );
        assert_eq!(
            drift_caption(),
            "Median now, less the baseline frozen from the first 100 samples. \
             Never re-baselined."
        );
    }

    #[test]
    fn an_empty_session_still_produces_a_drawable_plot() {
        let model = plot_model(&SessionStats::new(), 820.0);
        assert!(model.samples.is_empty());
        assert_eq!(model.baseline_p50_us, None);
        assert!(
            model.scale_us > 0.0,
            "no zero divide waiting in the drawing"
        );
    }
}
