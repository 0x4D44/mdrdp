//! What the Help menus show: the About facts and the keyboard-shortcut tables.
//!
//! Both processes reach for this module — the launcher draws it as a modal over its
//! list, the session process as a standalone window off its own event loop — so the
//! two can state the version, the licence and the hotkeys only one way. A session
//! window that disagreed with the launcher about which key toggles the overlay would
//! be worse than no Help at all.
//!
//! The shortcut tables are the *claimed* behaviour; the tests below pin them to the
//! code that implements it where a constant can, and `window.rs` owns the rest.

use crate::shell::widgets;
use crate::ui::theme;
use egui::{Frame, Margin, RichText, Stroke, vec2};

/// The rows of the About dialog, under the icon and version line.
///
/// The IronRDP version is checked against `Cargo.toml` by a test in this module: an
/// About box that names last year's protocol stack is a support answer that wastes
/// somebody's afternoon. The values also have to *fit* — the 440px dialog gives them
/// 302px after the label column, and the Vendored row used to run 50px past it.
pub const ABOUT_ROWS: &[(&str, &str)] = &[
    ("Licence", "MIT OR Apache-2.0"),
    ("Protocol", "IronRDP 0.17"),
    ("Vendored", "ironrdp-connector (EGFX needs one flag)"),
];

/// The size the session process opens its Help windows at.
///
/// Aux windows are not resizable, so these have to fit their content — the tests
/// below lay out a real frame at exactly these sizes and fail if anything, the Close
/// button included, lands outside.
pub const ABOUT_WINDOW: [f32; 2] = [440.0, 340.0];
pub const SHORTCUTS_WINDOW: [f32; 2] = [460.0, 260.0];

/// What this platform calls the command modifier. macOS labels it Cmd, everything
/// else Ctrl, and egui's `modifiers.command` already maps to whichever it is — so a
/// shortcut list that hardcoded one would be wrong on the other platform.
pub const COMMAND: &str = if cfg!(target_os = "macos") {
    "Cmd"
} else {
    "Ctrl"
};

/// One keystroke and what it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    pub keys: String,
    pub what: &'static str,
}

fn row(keys: impl Into<String>, what: &'static str) -> Shortcut {
    Shortcut {
        keys: keys.into(),
        what,
    }
}

/// A titled block of shortcuts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutGroup {
    pub title: &'static str,
    pub rows: Vec<Shortcut>,
}

/// `0.1.39 · macos aarch64` — the version line under the app name.
pub fn version_line() -> String {
    format!(
        "{} · {} {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// The session window's shortcuts, as `window.rs` implements them.
///
/// Only two keys are claimed locally; the closing note is the part users actually
/// need, because "why did my Ctrl+Alt+Del not arrive" is the question this window
/// exists to pre-empt.
pub fn session_shortcuts() -> Vec<ShortcutGroup> {
    vec![
        ShortcutGroup {
            title: "SESSION",
            rows: vec![
                row("Ctrl+Alt+Enter", "Toggle fullscreen"),
                row("Ctrl+Alt+S", "Show or hide the stats overlay"),
            ],
        },
        ShortcutGroup {
            title: "DIAGNOSTICS AND HELP WINDOWS",
            rows: vec![row(
                format!("Esc  or  {COMMAND}+W"),
                "Close the window in front",
            )],
        },
    ]
}

/// The launcher's shortcuts, as `shell::LauncherApp::handle_keys` implements them.
pub fn launcher_shortcuts() -> Vec<ShortcutGroup> {
    vec![
        ShortcutGroup {
            title: "FAVOURITES",
            rows: vec![
                row("Up  /  Down", "Move the selection"),
                row("Enter", "Connect to the selected favourite"),
                row("N", "New connection"),
            ],
        },
        ShortcutGroup {
            title: "DIALOGS",
            rows: vec![row("Esc", "Close the dialog in front")],
        },
    ]
}

/// The About content: icon, name, version line, fact rows.
///
/// Body only — the caller supplies the frame, because the launcher draws this inside
/// a modal and the session process inside a window of its own.
pub fn about_body(ui: &mut egui::Ui, icon: Option<&egui::TextureHandle>) {
    ui.vertical_centered(|ui| {
        if let Some(icon) = icon {
            ui.add(egui::Image::new(icon).fit_to_exact_size(vec2(72.0, 72.0)));
        }
        ui.label(
            RichText::new("mdrdp")
                .font(theme::sans_semibold(18.0))
                .color(theme::TEXT_PRIMARY),
        );
        ui.label(
            RichText::new(version_line())
                .font(theme::mono(12.0))
                .color(theme::TEXT_MUTED),
        );
    });
    ui.add_space(6.0);
    for (label, value) in ABOUT_ROWS {
        ui.horizontal(|ui| {
            ui.add_sized(
                [90.0, 18.0],
                egui::Label::new(
                    RichText::new(*label)
                        .font(theme::sans(12.0))
                        .color(theme::TEXT_MUTED),
                ),
            );
            ui.label(
                RichText::new(*value)
                    .font(theme::mono(12.0))
                    .color(theme::TEXT_SECONDARY),
            );
        });
    }
}

/// The shortcuts content: one heading plus key/meaning rows per group.
pub fn shortcuts_body(ui: &mut egui::Ui, groups: &[ShortcutGroup]) {
    for (i, group) in groups.iter().enumerate() {
        if i > 0 {
            ui.add_space(4.0);
        }
        ui.label(
            RichText::new(group.title)
                .font(theme::sans_medium(11.0))
                .color(theme::TEXT_DIM),
        );
        for shortcut in &group.rows {
            ui.horizontal(|ui| {
                ui.add_sized(
                    [150.0, 18.0],
                    egui::Label::new(
                        RichText::new(&shortcut.keys)
                            .font(theme::mono(12.0))
                            .color(theme::TEXT_PRIMARY),
                    ),
                );
                ui.label(
                    RichText::new(shortcut.what)
                        .font(theme::sans(12.0))
                        .color(theme::TEXT_SECONDARY),
                );
            });
        }
    }
    ui.add_space(2.0);
    ui.label(
        RichText::new("Every other key goes to the remote desktop.")
            .font(theme::sans(12.0))
            .color(theme::TEXT_MUTED),
    );
}

/// The session process's Help ▸ About window. `true` once it should close.
///
/// `icon` caches the texture for this window's egui context. It is the *window's*
/// cache, not the process's: each aux window owns a separate GL context and texture
/// manager, so a handle outliving its window would draw from a dead one.
pub fn about_window(ui: &mut egui::Ui, icon: &mut Option<egui::TextureHandle>) -> bool {
    if icon.is_none() {
        *icon = icon_texture(ui.ctx());
    }
    let icon = icon.clone();
    window_shell(ui, |ui| about_body(ui, icon.as_ref()))
}

/// The session process's Help ▸ Keyboard shortcuts window. `true` once it should close.
pub fn shortcuts_window(ui: &mut egui::Ui, groups: &[ShortcutGroup]) -> bool {
    window_shell(ui, |ui| shortcuts_body(ui, groups))
}

/// A standalone Help window: §7 body margins over `bg.window`, then the chrome strip
/// with the Close button. The same shape as the end-of-session dialogs, which is the
/// point — these windows sit in the same process and must not look like guests.
fn window_shell(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui)) -> bool {
    let mut close = false;
    Frame::new()
        .fill(theme::BG_WINDOW)
        .inner_margin(Margin::same(0))
        .show(ui, |ui| {
            Frame::new()
                .inner_margin(Margin {
                    left: 24,
                    right: 24,
                    top: 22,
                    bottom: 18,
                })
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 10.0;
                    body(ui);
                });
            Frame::new()
                .fill(theme::BG_CHROME)
                .inner_margin(Margin {
                    left: 24,
                    right: 24,
                    top: 14,
                    bottom: 14,
                })
                .show(ui, |ui| {
                    ui.painter().hline(
                        ui.max_rect().x_range().expand(24.0),
                        ui.max_rect().min.y - 14.0,
                        Stroke::new(1.0, theme::LINE_HAIR),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close |= widgets::primary_button(ui, "Close", 34.0).clicked();
                    });
                });
        });
    close
}

/// Decode the embedded app icon. `None` on decode failure — cosmetic, never fatal.
pub fn app_icon() -> Option<egui::IconData> {
    let bytes: &[u8] = include_bytes!("../../assets/icon/macos/icon-256.png");
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some(egui::IconData {
        rgba: buf,
        width: info.width,
        height: info.height,
    })
}

/// The app icon as a texture in `ctx`. `None` on decode failure.
pub fn icon_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let icon = app_icon()?;
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [icon.width as usize, icon.height as usize],
        &icon.rgba,
    );
    Some(ctx.load_texture("about-icon", image, egui::TextureOptions::LINEAR))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest's declared `ironrdp` version, e.g. `0.17.0`.
    fn manifest_ironrdp_version() -> String {
        let manifest = include_str!("../../Cargo.toml");
        let line = manifest
            .lines()
            .find(|l| l.starts_with("ironrdp = "))
            .expect("Cargo.toml declares ironrdp");
        let (_, after) = line.split_once("version = \"").expect("a version field");
        let (version, _) = after.split_once('"').expect("a closed version string");
        version.to_owned()
    }

    #[test]
    fn the_about_box_names_the_protocol_stack_we_actually_build_against() {
        let declared = manifest_ironrdp_version();
        let (major_minor, _) = declared.rsplit_once('.').expect("major.minor.patch");
        let expected = format!("IronRDP {major_minor}");
        let shown = ABOUT_ROWS
            .iter()
            .find(|(label, _)| *label == "Protocol")
            .expect("an About row names the protocol")
            .1;
        assert_eq!(
            shown, expected,
            "About says {shown:?} but Cargo.toml declares ironrdp {declared}"
        );
    }

    #[test]
    fn the_version_line_leads_with_this_build_s_version() {
        let line = version_line();
        assert!(
            line.starts_with(env!("CARGO_PKG_VERSION")),
            "version line {line:?} does not lead with the crate version"
        );
        assert!(
            line.contains(std::env::consts::ARCH),
            "{line:?} names no arch"
        );
    }

    #[test]
    fn the_command_modifier_is_labelled_the_way_this_platform_labels_it() {
        // Not derived from the same `cfg!` the constant uses: a swapped arm fails here.
        let expected = if std::env::consts::OS == "macos" {
            "Cmd"
        } else {
            "Ctrl"
        };
        assert_eq!(COMMAND, expected);
    }

    #[test]
    fn every_shortcut_group_carries_rows_and_no_key_is_listed_twice() {
        for groups in [session_shortcuts(), launcher_shortcuts()] {
            assert!(!groups.is_empty(), "a surface with no shortcut groups");
            let mut seen: Vec<String> = Vec::new();
            for group in &groups {
                assert!(
                    !group.rows.is_empty(),
                    "group {:?} would draw a heading over nothing",
                    group.title
                );
                for shortcut in &group.rows {
                    assert!(!shortcut.keys.trim().is_empty(), "a row with no keystroke");
                    assert!(!shortcut.what.trim().is_empty(), "a row with no meaning");
                    assert!(
                        !seen.contains(&shortcut.keys),
                        "{:?} is listed twice on one surface",
                        shortcut.keys
                    );
                    seen.push(shortcut.keys.clone());
                }
            }
        }
    }

    #[test]
    fn the_session_list_names_the_two_hotkeys_the_window_actually_claims() {
        // `window.rs` intercepts exactly these two and forwards everything else; a
        // Help window that omitted one would send the user hunting for a menu.
        let listed: Vec<String> = session_shortcuts()
            .iter()
            .flat_map(|g| g.rows.iter().map(|s| s.keys.clone()))
            .collect();
        assert!(listed.contains(&"Ctrl+Alt+S".to_owned()), "{listed:?}");
        assert!(listed.contains(&"Ctrl+Alt+Enter".to_owned()), "{listed:?}");
    }

    /// Lay out one real frame of a Help window at the size it actually opens at, and
    /// return every text it drew with the rect the text occupies.
    ///
    /// The bodies cannot be asserted pixel by pixel, but they can be *run*: this lays
    /// out and tessellates the whole window, which is what catches content that
    /// overflows a window nobody can resize.
    fn frame(
        size: [f32; 2],
        mut body: impl FnMut(&mut egui::Ui) -> bool,
    ) -> Vec<(String, egui::Rect)> {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                vec2(size[0], size[1]),
            )),
            ..Default::default()
        };
        // Seeded `true` — the value a body must not return with nothing clicked — so a
        // closure that never runs cannot pass the caller's assert.
        let mut close = true;
        let output = ctx.run_ui(input, |ui| {
            close = body(ui);
        });
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            if let egui::epaint::Shape::Text(t) = &clipped.shape {
                let size = t.galley.size();
                let min_x = match t.galley.job.halign {
                    egui::Align::LEFT => t.pos.x,
                    egui::Align::Center => t.pos.x - size.x / 2.0,
                    egui::Align::RIGHT => t.pos.x - size.x,
                };
                texts.push((
                    t.galley.text().to_owned(),
                    egui::Rect::from_min_size(egui::pos2(min_x, t.pos.y), size),
                ));
            }
        }
        // Consumed before the asserts: FullOutput's destructor panics on unapplied
        // deltas, which would turn a plain assert failure into a SIGABRT.
        output.drop_without_applying_deltas();
        assert!(!close, "an untouched Help window asked to close");
        texts
    }

    /// A non-resizable window has to be sized to its content in *both* directions:
    /// too small clips the Close button off the bottom, too large strands the chrome
    /// strip in mid-air with bare background below it.
    fn assert_fits(texts: &[(String, egui::Rect)], size: [f32; 2]) {
        let window = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(size[0], size[1]));
        for (text, rect) in texts {
            assert!(
                window.contains_rect(*rect),
                "{text:?} at {rect:?} falls outside the {size:?} window"
            );
        }
        let lowest = texts
            .iter()
            .map(|(_, r)| r.bottom())
            .fold(f32::MIN, f32::max);
        let slack = window.bottom() - lowest;
        assert!(
            slack <= 30.0,
            "{slack:.1}px of dead space below the last text in a {size:?} window"
        );
    }

    /// Every drawn text lies within `width`. For the launcher's modal, which grows to
    /// fit its content vertically but is pinned to a fixed width.
    fn assert_fits_width(texts: &[(String, egui::Rect)], width: f32) {
        for (text, rect) in texts {
            assert!(
                rect.right() <= width,
                "{text:?} runs to {:.1}px in a {width}px dialog",
                rect.right()
            );
        }
    }

    #[test]
    fn the_about_window_fits_the_size_it_opens_at() {
        // The real path, icon included: `about_window` loads the texture from the
        // context, and the 72px icon is what pushes everything below it down.
        let texts = frame(ABOUT_WINDOW, |ui| {
            let mut icon = None;
            about_window(ui, &mut icon)
        });
        assert_fits(&texts, ABOUT_WINDOW);
        let drawn: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        assert!(drawn.contains(&"mdrdp"), "{drawn:?}");
        assert!(drawn.contains(&"Close"), "{drawn:?}");
        assert!(drawn.contains(&"IronRDP 0.17"), "{drawn:?}");
        assert!(
            drawn
                .iter()
                .any(|t| t.starts_with(env!("CARGO_PKG_VERSION"))),
            "no version line in {drawn:?}"
        );
    }

    #[test]
    fn the_shortcuts_window_fits_the_size_it_opens_at() {
        let groups = session_shortcuts();
        let texts = frame(SHORTCUTS_WINDOW, |ui| shortcuts_window(ui, &groups));
        assert_fits(&texts, SHORTCUTS_WINDOW);
        assert_every_row_is_drawn(&texts, &groups);
    }

    /// The launcher draws the same table in a modal that grows to fit its height, so
    /// only the width can betray it — and its list is the longer one. The figure is
    /// the content width `shell::dialogs::shortcuts` leaves: 460 less its 24px margins.
    #[test]
    fn the_launcher_shortcut_table_fits_the_dialog_width() {
        const CONTENT_WIDTH: f32 = 460.0 - 24.0 - 24.0;
        let groups = launcher_shortcuts();
        let texts = frame([CONTENT_WIDTH, 600.0], |ui| {
            shortcuts_body(ui, &groups);
            false
        });
        assert_fits_width(&texts, CONTENT_WIDTH);
        assert_every_row_is_drawn(&texts, &groups);
    }

    fn assert_every_row_is_drawn(texts: &[(String, egui::Rect)], groups: &[ShortcutGroup]) {
        let drawn: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        for group in groups {
            assert!(
                drawn.contains(&group.title),
                "{:?} in {drawn:?}",
                group.title
            );
            for shortcut in &group.rows {
                assert!(
                    drawn.contains(&shortcut.keys.as_str()),
                    "{:?} missing from {drawn:?}",
                    shortcut.keys
                );
                assert!(
                    drawn.contains(&shortcut.what),
                    "{:?} in {drawn:?}",
                    shortcut.what
                );
            }
        }
    }

    #[test]
    fn the_embedded_icon_decodes() {
        let icon = app_icon().expect("the bundled 256px icon decodes");
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }
}
