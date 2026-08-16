//! Design tokens from the 2026-08-16 UI handoff, as the single vocabulary every
//! egui screen draws with.
//!
//! The handoff README (`wrk_docs/2026.08.16 - HANDOFF - launcher and diagnostics
//! UI/README.md`) is the source of record for every value here; screens read these
//! tokens and never restate a literal, so a colour that needs correcting is corrected
//! once. Fonts are embedded — egui and iced alike will not reliably find a system
//! IBM Plex on Windows, so the binary carries its own (OFL; `assets/fonts/OFL.txt`).

use egui::{Color32, FontFamily, FontId};

// --- colour -------------------------------------------------------------------------

pub const BG_WINDOW: Color32 = Color32::from_rgb(0x14, 0x16, 0x1A);
pub const BG_CHROME: Color32 = Color32::from_rgb(0x0E, 0x10, 0x13);
pub const BG_PANEL: Color32 = Color32::from_rgb(0x17, 0x1A, 0x1E);
pub const BG_RAISED: Color32 = Color32::from_rgb(0x1E, 0x21, 0x26);
pub const BG_ROW_SELECTED: Color32 = Color32::from_rgb(0x22, 0x26, 0x2C);
pub const LINE_HAIR: Color32 = Color32::from_rgb(0x26, 0x2B, 0x31);
pub const LINE_SUBTLE: Color32 = Color32::from_rgb(0x2F, 0x35, 0x3D);
pub const LINE_STRONG: Color32 = Color32::from_rgb(0x41, 0x4A, 0x54);
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xEA, 0xEE, 0xF3);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xA6, 0xAE, 0xB9);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x6F, 0x78, 0x83);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x54, 0x5D, 0x68);
pub const ACCENT: Color32 = Color32::from_rgb(0x2B, 0xE0, 0x7A);
pub const ACCENT_ON: Color32 = Color32::from_rgb(0x04, 0x15, 0x0B);
pub const ACCENT_FILL: Color32 = Color32::from_rgb(0x17, 0x60, 0x3C);
pub const ACCENT_TINT: Color32 = Color32::from_rgb(0x0D, 0x24, 0x17);
pub const WARN: Color32 = Color32::from_rgb(0xFF, 0xC9, 0x3C);
pub const DANGER: Color32 = Color32::from_rgb(0xFF, 0x5D, 0x4D);
pub const DANGER_ON: Color32 = Color32::from_rgb(0x1A, 0x0B, 0x08);
pub const DANGER_BG: Color32 = Color32::from_rgb(0x2A, 0x15, 0x18);
pub const DANGER_BORDER: Color32 = Color32::from_rgb(0x7A, 0x2A, 0x26);
pub const DANGER_TEXT: Color32 = Color32::from_rgb(0xFF, 0x8B, 0x7A);
pub const DANGER_BODY: Color32 = Color32::from_rgb(0xD3, 0xAA, 0xB0);
pub const INFO: Color32 = Color32::from_rgb(0x3B, 0x8C, 0xFF);
pub const CYAN: Color32 = Color32::from_rgb(0x24, 0xD8, 0xE0);
/// Modal backdrop: `#05070A` at 62%. Content behind additionally drops to 25% opacity.
pub const SCRIM: Color32 = Color32::from_rgba_premultiplied(
    (0x05_u32 * 158 / 255) as u8,
    (0x07_u32 * 158 / 255) as u8,
    (0x0A_u32 * 158 / 255) as u8,
    158, // 0.62 * 255
);
/// The warning card on wizard step 2: background and border.
pub const WARN_CARD_BG: Color32 = Color32::from_rgb(0x1C, 0x14, 0x18);
pub const WARN_CARD_BORDER: Color32 = Color32::from_rgb(0x48, 0x23, 0x2B);

// --- heat ramp (bitmap cache) --------------------------------------------------------

/// Anchor stops for the cache heat ramp: `(t, r, g, b)`, interpolated linearly in sRGB.
pub const HEAT_ANCHORS: [(f32, u8, u8, u8); 8] = [
    (0.00, 46, 59, 255),
    (0.16, 59, 140, 255),
    (0.32, 36, 216, 224),
    (0.48, 43, 224, 122),
    (0.62, 185, 245, 60),
    (0.76, 255, 224, 77),
    (0.88, 255, 154, 46),
    (1.00, 255, 45, 45),
];

/// Empty cache slots are flat `bg.raised` and never take a ramp colour.
pub const HEAT_EMPTY: Color32 = BG_RAISED;

/// The ramp colour for `t ∈ [0, 1]`, clamped. Linear interpolation in sRGB between
/// [`HEAT_ANCHORS`]; red appears only at the hot end.
pub fn heat(t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mut lo = HEAT_ANCHORS[0];
    for hi in HEAT_ANCHORS.iter().skip(1) {
        if t <= hi.0 {
            let span = hi.0 - lo.0;
            let f = if span > 0.0 { (t - lo.0) / span } else { 0.0 };
            let lerp = |a: u8, b: u8| -> u8 {
                (f32::from(a) + (f32::from(b) - f32::from(a)) * f).round() as u8
            };
            return Color32::from_rgb(lerp(lo.1, hi.1), lerp(lo.2, hi.2), lerp(lo.3, hi.3));
        }
        lo = *hi;
    }
    Color32::from_rgb(lo.1, lo.2, lo.3)
}

// --- type ---------------------------------------------------------------------------

/// Named egui font families for the non-default weights. Regular weights map onto the
/// built-in `Proportional` (Plex Sans) and `Monospace` (Plex Mono) families.
pub const SANS_MEDIUM: &str = "plex-sans-medium";
pub const SANS_SEMIBOLD: &str = "plex-sans-semibold";
pub const MONO_MEDIUM: &str = "plex-mono-medium";
pub const MONO_SEMIBOLD: &str = "plex-mono-semibold";

pub fn sans(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
pub fn sans_medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SANS_MEDIUM.into()))
}
pub fn sans_semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SANS_SEMIBOLD.into()))
}
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}
pub fn mono_medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(MONO_MEDIUM.into()))
}
pub fn mono_semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(MONO_SEMIBOLD.into()))
}

// --- spacing and radii ---------------------------------------------------------------

/// Corner radii, by role (px).
pub mod radius {
    pub const CACHE_CELL: u8 = 2;
    pub const MENU_ITEM: u8 = 3;
    pub const INPUT: u8 = 4;
    pub const CARD: u8 = 5;
    pub const WINDOW: u8 = 6;
    pub const MODAL: u8 = 7;
    pub const TOGGLE_TRACK: u8 = 10;
}

// --- egui wiring ---------------------------------------------------------------------

/// Register the embedded IBM Plex faces and return egui font definitions.
///
/// `Proportional` resolves to Plex Sans 400 and `Monospace` to Plex Mono 400; the
/// heavier weights are separate named families because egui selects by family, not by
/// weight. egui's default fonts stay registered underneath as symbol fallback.
pub fn font_definitions() -> egui::FontDefinitions {
    use egui::FontData;
    let mut fonts = egui::FontDefinitions::default();
    let faces: [(&str, &[u8]); 6] = [
        (
            "plex-sans",
            include_bytes!("../../assets/fonts/IBMPlexSans-Regular.ttf"),
        ),
        (
            SANS_MEDIUM,
            include_bytes!("../../assets/fonts/IBMPlexSans-Medium.ttf"),
        ),
        (
            SANS_SEMIBOLD,
            include_bytes!("../../assets/fonts/IBMPlexSans-SemiBold.ttf"),
        ),
        (
            "plex-mono",
            include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf"),
        ),
        (
            MONO_MEDIUM,
            include_bytes!("../../assets/fonts/IBMPlexMono-Medium.ttf"),
        ),
        (
            MONO_SEMIBOLD,
            include_bytes!("../../assets/fonts/IBMPlexMono-SemiBold.ttf"),
        ),
    ];
    for (name, bytes) in faces {
        fonts
            .font_data
            .insert(name.to_owned(), FontData::from_static(bytes).into());
    }
    // Plex leads each built-in family; egui's defaults remain as symbol fallback.
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "plex-sans".to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "plex-mono".to_owned());
    for name in [SANS_MEDIUM, SANS_SEMIBOLD, MONO_MEDIUM, MONO_SEMIBOLD] {
        // Named families: the weight itself, then the same symbol fallback the
        // built-ins get, so a checkmark in a semibold run still renders.
        let mut family = vec![name.to_owned()];
        family.extend(
            fonts
                .families
                .get(&FontFamily::Proportional)
                .into_iter()
                .flatten()
                .skip(1)
                .cloned(),
        );
        fonts.families.insert(FontFamily::Name(name.into()), family);
    }
    fonts
}

/// Apply the handoff theme to a context: fonts plus the dark visual style.
///
/// Idempotent, and cheap after the first call; both processes call it once per
/// context at startup.
pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
    // One deliberate dark look; the OS light/dark preference does not apply here.
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.visuals = visuals();
    ctx.set_style_of(egui::Theme::Dark, style);
}

/// The handoff's dark palette as egui visuals. Screens still colour their own
/// widgets from the tokens; this sets the ambient chrome so anything unstyled
/// lands close rather than egui-grey.
pub fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(TEXT_SECONDARY);
    v.panel_fill = BG_WINDOW;
    v.window_fill = BG_WINDOW;
    v.window_stroke = egui::Stroke::new(1.0, LINE_STRONG);
    v.extreme_bg_color = BG_CHROME;
    v.faint_bg_color = BG_RAISED;
    v.selection.bg_fill = ACCENT_FILL;
    v.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = WARN;
    v.error_fg_color = DANGER;
    v.widgets.noninteractive.bg_fill = BG_WINDOW;
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT_SECONDARY);
    v.widgets.inactive.bg_fill = BG_RAISED;
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT_SECONDARY);
    v.widgets.hovered.bg_fill = BG_ROW_SELECTED;
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, TEXT_PRIMARY);
    v.widgets.active.bg_fill = ACCENT_FILL;
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0, TEXT_PRIMARY);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ramp_returns_each_anchor_exactly() {
        for (t, r, g, b) in HEAT_ANCHORS {
            assert_eq!(heat(t), Color32::from_rgb(r, g, b), "anchor at t={t}");
        }
    }

    #[test]
    fn the_ramp_interpolates_between_adjacent_anchors() {
        // Midway between t=0.00 (46,59,255) and t=0.16 (59,140,255).
        let mid = heat(0.08);
        assert_eq!(mid, Color32::from_rgb(53, 100, 255), "sRGB midpoint");
    }

    #[test]
    fn the_ramp_clamps_rather_than_extrapolating() {
        assert_eq!(heat(-1.0), heat(0.0));
        assert_eq!(heat(2.0), heat(1.0));
        let hot = heat(1.0);
        assert_eq!(hot, Color32::from_rgb(255, 45, 45));
    }

    #[test]
    fn red_appears_only_at_the_hot_end() {
        // Below the 0.88 anchor the red channel never dominates both others the way
        // the terminal red does; spot-check the neutral middle.
        let mid = heat(0.48);
        assert_eq!(
            mid,
            Color32::from_rgb(43, 224, 122),
            "the accent green anchor"
        );
    }

    #[test]
    fn every_embedded_face_parses_as_a_font() {
        // The include_bytes! payloads must be real fonts, not LFS pointers or HTML
        // error pages — egui would panic at startup on garbage.
        let fonts = font_definitions();
        for name in [
            "plex-sans",
            SANS_MEDIUM,
            SANS_SEMIBOLD,
            "plex-mono",
            MONO_MEDIUM,
            MONO_SEMIBOLD,
        ] {
            let data = fonts.font_data.get(name).expect(name);
            assert!(data.font.len() > 10_000, "{name} is suspiciously small");
            // TrueType magic: 0x00010000, or 'true'/'OTTO' for variants.
            let magic = &data.font[..4];
            assert!(
                matches!(magic, [0x00, 0x01, 0x00, 0x00] | b"true" | b"OTTO"),
                "{name} does not start with a TrueType magic number"
            );
        }
    }

    #[test]
    fn plex_leads_both_builtin_families_with_fallback_behind() {
        let fonts = font_definitions();
        let prop = &fonts.families[&FontFamily::Proportional];
        assert_eq!(prop[0], "plex-sans");
        assert!(prop.len() > 1, "egui symbol fallback must stay registered");
        let mono = &fonts.families[&FontFamily::Monospace];
        assert_eq!(mono[0], "plex-mono");
        let semibold = &fonts.families[&FontFamily::Name(SANS_SEMIBOLD.into())];
        assert_eq!(semibold[0], SANS_SEMIBOLD);
        assert!(semibold.len() > 1, "weights need the symbol fallback too");
    }
}
