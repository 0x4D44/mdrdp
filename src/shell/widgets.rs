//! Shared egui widgets drawn to the handoff's shared-chrome spec.
//!
//! Every screen builds its buttons and captions from here so the chrome cannot drift
//! between screens; sizes come in as parameters only where the handoff itself varies
//! them (38px buttons on screens, 34px in dialogs, 32px in the list header).

use crate::ui::theme;
use egui::{Color32, CornerRadius, Response, RichText, Sense, Stroke, Ui, vec2};

/// A filled `accent` primary button: 13px/600 `accent.on`, radius 4.
pub fn primary_button(ui: &mut Ui, label: &str, height: f32) -> Response {
    solid_button(ui, label, height, theme::ACCENT, theme::ACCENT_ON)
}

/// A `danger`-filled destructive button.
pub fn danger_button(ui: &mut Ui, label: &str, height: f32) -> Response {
    solid_button(ui, label, height, theme::DANGER, theme::DANGER_ON)
}

fn solid_button(ui: &mut Ui, label: &str, height: f32, fill: Color32, on: Color32) -> Response {
    let text = RichText::new(label)
        .font(theme::sans_semibold(13.0))
        .color(on);
    let pad = vec2(18.0, 0.0);
    let galley_width = text_width(ui, label, theme::sans_semibold(13.0));
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley_width + pad.x * 2.0, height), Sense::click());
    let fill = if response.hovered() {
        fill.gamma_multiply(0.9)
    } else {
        fill
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(theme::radius::INPUT), fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        theme::sans_semibold(13.0),
        on,
    );
    let _ = text;
    response
}

/// A bordered transparent secondary button: 1px `line.strong`, 13px `text.secondary`.
pub fn secondary_button(ui: &mut Ui, label: &str, height: f32) -> Response {
    let galley_width = text_width(ui, label, theme::sans(13.0));
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley_width + 14.0 * 2.0, height), Sense::click());
    let stroke_color = if response.hovered() {
        theme::TEXT_MUTED
    } else {
        theme::LINE_STRONG
    };
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(theme::radius::INPUT),
        Stroke::new(1.0, stroke_color),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        theme::sans(13.0),
        theme::TEXT_SECONDARY,
    );
    response
}

fn text_width(ui: &Ui, label: &str, font: egui::FontId) -> f32 {
    ui.fonts_mut(|f| {
        f.layout_no_wrap(label.to_owned(), font, Color32::WHITE)
            .size()
            .x
    })
}

/// A 6px status dot.
pub fn status_dot(ui: &mut Ui, colour: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(6.0, 6.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.0, colour);
}
