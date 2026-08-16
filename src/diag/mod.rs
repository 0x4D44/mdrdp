//! The diagnostics windows a session process owns: bitmap cache, latency and drift,
//! channels and codecs (handoff screens 4-6).
//!
//! Each screen is a pure function from one stats snapshot to a drawn frame plus an
//! action enum — no window handling, no polling, no interior state. That keeps the
//! arithmetic every one of these windows exists to show (percentile bands, cache heat,
//! plot scales) unit-testable without a window, which matters because a diagnostics
//! window that quietly lies is worse than no diagnostics window at all.
//!
//! Anything shared between the three lives here; the screens themselves are the
//! submodules. Colours and fonts come from [`crate::ui::theme`] and are never restated
//! as literals.

use crate::ui::theme;
use egui::text::{LayoutJob, TextWrapping};
use egui::{Color32, CornerRadius, FontId, Painter, Pos2, Rangef, Rect, Stroke, StrokeKind, pos2};

pub mod cache;
pub mod channels;
pub mod latency;

/// Paint one line at `at` (left edge, vertically centred), elided with `…` so it never
/// paints past `max_x`. Returns the x where the text ended.
///
/// Every left-flowing string that shares a band with right-aligned content goes through
/// this: a plain `painter.text` has no width and runs straight under its neighbour.
pub fn elided_text(
    painter: &Painter,
    at: Pos2,
    max_x: f32,
    text: &str,
    font: FontId,
    colour: Color32,
) -> f32 {
    let width = max_x - at.x;
    if width <= 0.0 {
        return at.x;
    }
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, colour);
    job.wrap = TextWrapping::truncate_at_width(width);
    let galley = painter.layout_job(job);
    let size = galley.size();
    painter.galley(pos2(at.x, at.y - size.y / 2.0), galley, colour);
    at.x + size.x
}

/// The header's session line — hairline divider, live dot, name, detail — painted after
/// the title and elided so it can never run under the header's right-aligned content at
/// `max_x`. All three diagnostics windows draw exactly this line.
pub fn session_line(
    painter: &Painter,
    cy: f32,
    after_x: f32,
    max_x: f32,
    name: &str,
    detail: &str,
) {
    let divider_x = after_x + 14.0;
    let dot_x = divider_x + 14.0;
    let name_x = dot_x + 6.0 + 9.0;
    if name_x >= max_x {
        return; // No room: a divider and dot with no text reads as a glitch.
    }
    painter.vline(
        divider_x,
        Rangef::new(cy - 9.0, cy + 9.0),
        Stroke::new(1.0, theme::LINE_SUBTLE),
    );
    painter.circle_filled(pos2(dot_x + 3.0, cy), 3.0, theme::ACCENT);
    let name_end = elided_text(
        painter,
        pos2(name_x, cy),
        max_x,
        name,
        theme::mono(12.0),
        theme::TEXT_PRIMARY,
    );
    elided_text(
        painter,
        pos2(name_end + 9.0, cy),
        max_x,
        detail,
        theme::mono(12.0),
        theme::TEXT_DIM,
    );
}

/// Paint a diagnostics card: `bg.chrome` fill, 1px `line.hair` border, 6px radius.
///
/// The drift card, the metric tiles and the plot box are all the same box in the
/// handoff, so they are the same call here.
pub fn paint_card(painter: &Painter, rect: Rect) {
    painter.rect(
        rect,
        CornerRadius::same(theme::radius::WINDOW),
        theme::BG_CHROME,
        Stroke::new(1.0, theme::LINE_HAIR),
        StrokeKind::Inside,
    );
}

/// Microseconds as the one-decimal millisecond figure the handoff shows: `7_800` → `7.8`.
pub fn ms1(micros: u32) -> String {
    format!("{:.1}", f64::from(micros) / 1000.0)
}

/// A signed microsecond delta as a one-decimal millisecond figure: `400` → `+0.4`.
///
/// The sign is always present: a drift reading without one cannot be read at a glance,
/// and "is it getting worse" is the only question this number answers.
pub fn signed_ms1(micros: i64) -> String {
    format!("{:+.1}", micros as f64 / 1000.0)
}

/// A count with thousands separators: `2148` → `2,148`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millisecond_figures_keep_one_decimal() {
        // Distinct values, none derived from another: a swapped argument or a factor of
        // ten cannot pass.
        assert_eq!(ms1(7_800), "7.8");
        assert_eq!(ms1(41_300), "41.3");
        assert_eq!(ms1(0), "0.0");
        assert_eq!(ms1(1_260), "1.3", "rounds rather than truncating");
    }

    #[test]
    fn a_drift_figure_always_carries_its_sign() {
        assert_eq!(signed_ms1(400), "+0.4");
        assert_eq!(signed_ms1(-7_000), "-7.0");
        assert_eq!(signed_ms1(0), "+0.0");
    }

    #[test]
    fn counts_are_grouped_in_threes() {
        assert_eq!(thousands(2_148), "2,148");
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
