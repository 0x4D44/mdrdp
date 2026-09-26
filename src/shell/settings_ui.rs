//! The Settings modal — screen 3 of the 2026-08-16 UI handoff.
//!
//! A 720×580 modal over the connections list, seven panes down a 186px sidebar. Every
//! visual value is the handoff README's ("3. Settings (modal over the list)" plus the
//! shared-chrome specs), read through [`crate::ui::theme`] tokens.
//!
//! **The modal edits a working copy.** [`SettingsModal::new`] clones the caller's
//! [`Settings`]; nothing leaves this module until Save, which hands back the edited
//! clone. Cancel and `×` throw the clone away, so a half-made edit never reaches disk.
//!
//! The Certificate-trust pane is the exception, and deliberately so: the spec gives its
//! rows a per-row `Forget` with no Save semantics, so a forget writes `known_hosts`
//! immediately. Per the handoff's destructive-action rule it takes a second press, and
//! the confirmation names the file it is about to rewrite.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::settings::{ClipboardDirection, Settings, StageLogLevel, WindowMode};
use crate::shell::widgets;
use crate::trust::KnownHosts;
use crate::ui::theme;
use egui::{
    Align, Align2, Color32, CornerRadius, Frame, Id, Layout, Rect, Response, RichText, Sense,
    Stroke, StrokeKind, TextEdit, Ui, UiBuilder, Vec2, pos2, vec2,
};

// --- geometry (handoff §3) -----------------------------------------------------------

const MODAL_W: f32 = 720.0;
const MODAL_H: f32 = 580.0;
const TITLE_H: f32 = 52.0;
const FOOTER_H: f32 = 64.0;
const SIDEBAR_W: f32 = 186.0;
const PANE_PAD_X: f32 = 26.0;
const PANE_PAD_Y: f32 = 24.0;
const ROW_GAP: f32 = 22.0;
const LABEL_W: f32 = 150.0;
const INPUT_H: f32 = 34.0;

/// Clamp ranges for the free-text numeric fields. A field that will not parse keeps the
/// value it had; one that parses out of range is pulled to the nearest end rather than
/// silently reverting, so a typo of one digit does not look like the edit was ignored.
const DIM_MIN: u16 = 320;
const IMAGE_BYTES_MIN: u64 = 1024;
const IMAGE_BYTES_MAX: u64 = 1024 * 1024 * 1024;
const TIMEOUT_SECS_MIN: u64 = 1;
const TIMEOUT_SECS_MAX: u64 = 600;

/// What the modal wants the launcher to do after this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsOutcome {
    /// Still open; nothing decided this frame.
    Open,
    /// Cancel, `×` or `Esc` — discard the working copy.
    Cancelled,
    /// Save — the edited settings, ready to persist.
    Saved(Settings),
}

/// The seven sidebar panes, in sidebar order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Defaults,
    Graphics,
    Keyboard,
    Audio,
    Clipboard,
    Diagnostics,
    Certificates,
}

impl Pane {
    pub const ALL: [Pane; 7] = [
        Pane::Defaults,
        Pane::Graphics,
        Pane::Keyboard,
        Pane::Audio,
        Pane::Clipboard,
        Pane::Diagnostics,
        Pane::Certificates,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Pane::Defaults => "Defaults",
            Pane::Graphics => "Graphics",
            Pane::Keyboard => "Keyboard",
            Pane::Audio => "Audio",
            Pane::Clipboard => "Clipboard",
            Pane::Diagnostics => "Diagnostics",
            Pane::Certificates => "Certificate trust",
        }
    }
}

/// One pinned host, as the Certificate-trust pane shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pin {
    /// `host:port`, the store's own key.
    host: String,
    /// `9f:2c:…:b7` — head and tail of the SHA-256, as the spec's row copy reads.
    short: String,
    /// `14 Aug`. The store keeps no per-pin date, so this is the file's modification
    /// time and every row therefore carries the same one; better an honest
    /// last-touched date than an invented per-row one.
    date: Option<String>,
}

/// The Settings modal: a working copy of the settings plus the modal's own view state.
pub struct SettingsModal {
    working: Settings,
    pane: Pane,
    // Free-text buffers. Committed into the working copy on Save so a mid-edit
    // half-typed number never lands in the settings themselves.
    username: String,
    port: String,
    width: String,
    height: String,
    device_named: bool,
    device: String,
    max_image: String,
    timeout: String,
    metrics_dir: String,
    // Certificate trust.
    known_hosts_path: Option<PathBuf>,
    pins: Vec<Pin>,
    pins_error: Option<String>,
    /// The host whose `Forget` has been pressed once and is awaiting its second press.
    confirm_forget: Option<String>,
}

impl SettingsModal {
    /// Open the modal over a clone of `current`. `known_hosts_path` feeds the
    /// Certificate-trust pane; `None` leaves it empty with a note.
    pub fn new(current: Settings, known_hosts_path: Option<PathBuf>) -> Self {
        let mut modal = SettingsModal {
            username: current.defaults.username.clone().unwrap_or_default(),
            port: current.defaults.port.to_string(),
            width: current.defaults.width.to_string(),
            height: current.defaults.height.to_string(),
            device_named: current.audio.device != "default",
            device: if current.audio.device == "default" {
                String::new()
            } else {
                current.audio.device.clone()
            },
            max_image: format_bytes(current.clipboard.max_image_bytes),
            timeout: format_secs(current.clipboard.timeout_secs),
            metrics_dir: current.diagnostics.metrics_dir.clone(),
            working: current,
            pane: Pane::Defaults,
            known_hosts_path,
            pins: Vec::new(),
            pins_error: None,
            confirm_forget: None,
        };
        modal.reload_pins();
        modal
    }

    /// Which pane is showing.
    pub fn pane(&self) -> Pane {
        self.pane
    }

    /// Draw the scrim and the 720×580 modal, and route its input.
    pub fn ui(&mut self, ctx: &egui::Context, favourites_path: &Path) -> SettingsOutcome {
        // Scrim: swallow clicks so the list underneath is inert while Settings is up.
        let screen = ctx.content_rect();
        egui::Area::new(Id::new("settings-scrim"))
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.painter().rect_filled(screen, 0.0, theme::SCRIM);
                ui.allocate_rect(screen, Sense::click());
            });

        let mut outcome = SettingsOutcome::Open;
        egui::Area::new(Id::new("settings-modal"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(MODAL_W);
                Frame::new()
                    .fill(theme::BG_WINDOW)
                    .stroke(Stroke::new(1.0, theme::LINE_STRONG))
                    .corner_radius(CornerRadius::same(theme::radius::MODAL))
                    .show(ui, |ui| {
                        ui.set_width(MODAL_W);
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        if self.title_row(ui) {
                            outcome = SettingsOutcome::Cancelled;
                        }
                        self.body(ui, favourites_path);
                        match self.footer(ui) {
                            FooterAction::None => {}
                            FooterAction::Cancel => outcome = SettingsOutcome::Cancelled,
                            FooterAction::Save => {
                                outcome = SettingsOutcome::Saved(self.commit());
                            }
                        }
                    });
            });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            return SettingsOutcome::Cancelled;
        }
        outcome
    }

    /// Apply the text buffers to the working copy and hand it back.
    ///
    /// Toggles, checkboxes and segments edit the working copy in place as they are
    /// clicked; only the free-text fields need parsing, and they are parsed here so an
    /// unparseable field keeps its previous value instead of zeroing one.
    fn commit(&mut self) -> Settings {
        let previous = self.working.clone();
        let s = &mut self.working;
        s.defaults.username = match self.username.trim() {
            "" => None,
            name => Some(name.to_owned()),
        };
        s.defaults.port = parse_u16(&self.port, previous.defaults.port, 1, u16::MAX);
        s.defaults.width = parse_u16(&self.width, previous.defaults.width, DIM_MIN, u16::MAX);
        s.defaults.height = parse_u16(&self.height, previous.defaults.height, DIM_MIN, u16::MAX);
        s.audio.device = if self.device_named {
            match self.device.trim() {
                "" => "default".to_owned(),
                name => name.to_owned(),
            }
        } else {
            "default".to_owned()
        };
        s.clipboard.max_image_bytes =
            parse_bytes(&self.max_image, previous.clipboard.max_image_bytes);
        s.clipboard.timeout_secs = parse_secs(&self.timeout, previous.clipboard.timeout_secs);
        s.diagnostics.metrics_dir = self.metrics_dir.trim().to_owned();
        s.clone()
    }

    // --- chrome -----------------------------------------------------------------------

    /// The 52px title row. Returns true if `×` was pressed.
    fn title_row(&mut self, ui: &mut Ui) -> bool {
        let (rect, _) = ui.allocate_exact_size(vec2(MODAL_W, TITLE_H), Sense::hover());
        ui.painter().text(
            pos2(rect.min.x + PANE_PAD_X, rect.center().y),
            Align2::LEFT_CENTER,
            "Settings",
            theme::sans_semibold(16.0),
            theme::TEXT_PRIMARY,
        );
        let close = Rect::from_center_size(
            pos2(rect.max.x - PANE_PAD_X + 2.0, rect.center().y),
            vec2(24.0, 24.0),
        );
        let response = ui.interact(close, Id::new("settings-close"), Sense::click());
        ui.painter().text(
            close.center(),
            Align2::CENTER_CENTER,
            "×",
            theme::sans(18.0),
            if response.hovered() {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_MUTED
            },
        );
        ui.painter().hline(
            rect.x_range(),
            rect.max.y - 0.5,
            Stroke::new(1.0, theme::LINE_HAIR),
        );
        response.clicked()
    }

    fn body(&mut self, ui: &mut Ui, favourites_path: &Path) {
        let height = MODAL_H - TITLE_H - FOOTER_H;
        let (rect, _) = ui.allocate_exact_size(vec2(MODAL_W, height), Sense::hover());
        let sidebar = Rect::from_min_size(rect.min, vec2(SIDEBAR_W, height));
        let pane = Rect::from_min_max(pos2(rect.min.x + SIDEBAR_W, rect.min.y), rect.max);
        self.sidebar(ui, sidebar, favourites_path);

        let mut pane_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(pane.shrink2(vec2(PANE_PAD_X, PANE_PAD_Y)))
                .layout(Layout::top_down(Align::Min)),
        );
        pane_ui.spacing_mut().item_spacing.y = ROW_GAP;
        egui::ScrollArea::vertical()
            .id_salt(("settings-pane", self.pane.label()))
            .auto_shrink([false, false])
            .show(&mut pane_ui, |ui| match self.pane {
                Pane::Defaults => self.defaults_pane(ui),
                Pane::Graphics => self.graphics_pane(ui),
                Pane::Keyboard => self.keyboard_pane(ui),
                Pane::Audio => self.audio_pane(ui),
                Pane::Clipboard => self.clipboard_pane(ui),
                Pane::Diagnostics => self.diagnostics_pane(ui),
                Pane::Certificates => self.certificates_pane(ui),
            });
    }

    fn sidebar(&mut self, ui: &mut Ui, rect: Rect, favourites_path: &Path) {
        ui.painter().rect_filled(rect, 0.0, theme::BG_PANEL);
        ui.painter().vline(
            rect.max.x - 0.5,
            rect.y_range(),
            Stroke::new(1.0, theme::LINE_HAIR),
        );

        // Items: 32px, radius 4, 13px; selected on accent.fill.
        let mut y = rect.min.y + 12.0;
        for pane in Pane::ALL {
            let item =
                Rect::from_min_size(pos2(rect.min.x + 10.0, y), vec2(SIDEBAR_W - 20.0, 32.0));
            let response = ui.interact(
                item,
                Id::new(("settings-pane-item", pane.label())),
                Sense::click(),
            );
            if response.clicked() {
                self.pane = pane;
                self.confirm_forget = None;
            }
            let selected = self.pane == pane;
            if selected {
                ui.painter().rect_filled(
                    item,
                    CornerRadius::same(theme::radius::INPUT),
                    theme::ACCENT_FILL,
                );
            } else if response.hovered() {
                ui.painter().rect_filled(
                    item,
                    CornerRadius::same(theme::radius::INPUT),
                    theme::BG_ROW_SELECTED,
                );
            }
            ui.painter().text(
                pos2(item.min.x + 11.0, item.center().y),
                Align2::LEFT_CENTER,
                pane.label(),
                theme::sans(13.0),
                if selected {
                    theme::TEXT_PRIMARY
                } else {
                    theme::TEXT_SECONDARY
                },
            );
            y = item.max.y + 2.0;
        }

        // Bottom: the favourites file, its directory, and a Reveal action.
        let dir = favourites_path
            .parent()
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        let name = favourites_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "favourites.toml".to_owned());
        let foot = Rect::from_min_max(
            pos2(rect.min.x + 10.0, rect.max.y - 92.0),
            pos2(rect.max.x - 10.0, rect.max.y - 12.0),
        );
        let mut foot_ui = ui.new_child(UiBuilder::new().max_rect(foot));
        foot_ui.spacing_mut().item_spacing.y = 4.0;
        foot_ui.label(
            RichText::new(name)
                .font(theme::mono(10.0))
                .color(theme::TEXT_MUTED)
                .line_height(Some(15.0)),
        );
        foot_ui.label(
            RichText::new(dir)
                .font(theme::mono(10.0))
                .color(theme::TEXT_DIM)
                .line_height(Some(15.0)),
        );
        let reveal = foot_ui.add(
            egui::Label::new(
                RichText::new("Reveal")
                    .font(theme::sans(11.0))
                    .color(theme::ACCENT),
            )
            .sense(Sense::click()),
        );
        if reveal.clicked() {
            reveal_directory(favourites_path);
        }
    }

    /// The 64px footer: the apply note left, Cancel and Save right.
    fn footer(&mut self, ui: &mut Ui) -> FooterAction {
        let (rect, _) = ui.allocate_exact_size(vec2(MODAL_W, FOOTER_H), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, theme::BG_CHROME);
        ui.painter().hline(
            rect.x_range(),
            rect.min.y + 0.5,
            Stroke::new(1.0, theme::LINE_HAIR),
        );
        ui.painter().text(
            pos2(rect.min.x + PANE_PAD_X, rect.center().y),
            Align2::LEFT_CENTER,
            "Changes apply to new sessions.",
            theme::sans(12.0),
            theme::TEXT_MUTED,
        );
        let buttons = Rect::from_min_max(
            pos2(rect.max.x - 300.0, rect.center().y - 19.0),
            pos2(rect.max.x - PANE_PAD_X, rect.center().y + 19.0),
        );
        let mut action = FooterAction::None;
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(buttons)
                .layout(Layout::right_to_left(Align::Center)),
        );
        child.spacing_mut().item_spacing.x = 10.0;
        if widgets::primary_button(&mut child, "Save", 38.0).clicked() {
            action = FooterAction::Save;
        }
        if widgets::secondary_button(&mut child, "Cancel", 38.0).clicked() {
            action = FooterAction::Cancel;
        }
        action
    }

    // --- panes ------------------------------------------------------------------------

    fn defaults_pane(&mut self, ui: &mut Ui) {
        row(ui, "Username", INPUT_H, |ui| {
            text_input(ui, "settings-username", &mut self.username, 240.0, true);
        });
        row(ui, "Port", INPUT_H, |ui| {
            text_input(ui, "settings-port", &mut self.port, 110.0, true);
        });
        row(ui, "Session window", INPUT_H, |ui| {
            segmented(
                ui,
                "settings-window",
                &mut self.working.defaults.window,
                &[
                    (WindowMode::Fullscreen, "Fullscreen"),
                    (WindowMode::Explicit, "Explicit"),
                ],
            );
        });
        // Explicit size dims while Fullscreen is selected — the value is derived then.
        let explicit = self.working.defaults.window == WindowMode::Explicit;
        row(ui, "Explicit size", INPUT_H, |ui| {
            text_input(ui, "settings-width", &mut self.width, 88.0, explicit);
            ui.label(
                RichText::new("×")
                    .font(theme::mono(13.0))
                    .color(if explicit {
                        theme::TEXT_MUTED
                    } else {
                        theme::TEXT_DIM
                    }),
            );
            text_input(ui, "settings-height", &mut self.height, 88.0, explicit);
        });
        divider(ui);
        section_label(ui, "ON CONNECT");
        toggle_row(
            ui,
            &mut self.working.defaults.keep_launcher_open,
            "Keep the launcher open",
            Some("Each session runs in its own process, so several can run at once."),
        );
        toggle_row(
            ui,
            &mut self.working.defaults.reconnect_last,
            "Reconnect the last session on launch",
            None,
        );
    }

    fn graphics_pane(&mut self, ui: &mut Ui) {
        checkbox_row(
            ui,
            &mut self.working.graphics.clear_codec,
            "ClearCodec",
            None,
        );
        checkbox_row(
            ui,
            &mut self.working.graphics.rfx_progressive,
            "RFX Progressive",
            None,
        );
        checkbox_row(
            ui,
            &mut self.working.graphics.allow_uncompressed,
            "Allow uncompressed fallback",
            None,
        );
        divider(ui);
        toggle_row(
            ui,
            &mut self.working.graphics.dynamic_resolution,
            "Dynamic resolution on window resize",
            None,
        );
        toggle_row(
            ui,
            &mut self.working.graphics.integer_fullscreen_fit,
            "Integer scaling on 5K+ displays",
            Some(
                "Fullscreen past the H.264 ceiling uses half resolution at an exact \
                 2x instead of a fractional stretch",
            ),
        );
    }

    fn keyboard_pane(&mut self, ui: &mut Ui) {
        toggle_row(
            ui,
            &mut self.working.keyboard.mac_keyboard_mode,
            "Mac keyboard mode",
            Some(
                "Printable keys use the Mac layout; shortcuts, navigation and editing keys \
                 keep their positional behavior.",
            ),
        );
        note(
            ui,
            "Changes apply to new sessions. The default keeps positional scancodes.",
        );
    }

    fn audio_pane(&mut self, ui: &mut Ui) {
        toggle_row(ui, &mut self.working.audio.playback, "Playback", None);
        toggle_row(
            ui,
            &mut self.working.audio.microphone,
            "Microphone",
            Some("When enabled, the remote host can request local microphone audio"),
        );
        row(ui, "Output device", INPUT_H, |ui| {
            segmented(
                ui,
                "settings-audio-device",
                &mut self.device_named,
                &[(false, "Default"), (true, "Named")],
            );
        });
        if self.device_named {
            row(ui, "Device name", INPUT_H, |ui| {
                text_input(ui, "settings-device", &mut self.device, 240.0, true);
            });
        }
        note(
            ui,
            "Default follows the system output device; a named device is matched by name \
             when the session starts.",
        );
    }

    fn clipboard_pane(&mut self, ui: &mut Ui) {
        row(ui, "Direction", INPUT_H, |ui| {
            // The three the spec names; a stored `from_remote` keeps its own segment so
            // opening Settings can never silently rewrite a direction nobody touched.
            let mut options: Vec<(ClipboardDirection, &str)> = vec![
                (ClipboardDirection::Both, "Both ways"),
                (ClipboardDirection::ToRemote, "To remote"),
            ];
            if self.working.clipboard.direction == ClipboardDirection::FromRemote {
                options.push((ClipboardDirection::FromRemote, "From remote"));
            }
            options.push((ClipboardDirection::Off, "Off"));
            segmented(
                ui,
                "settings-clipboard-direction",
                &mut self.working.clipboard.direction,
                &options,
            );
        });
        row(ui, "Max image", INPUT_H, |ui| {
            text_input(ui, "settings-max-image", &mut self.max_image, 96.0, true);
        });
        row(ui, "Transfer timeout", INPUT_H, |ui| {
            text_input(ui, "settings-timeout", &mut self.timeout, 96.0, true);
        });
        note(
            ui,
            "A transfer that times out is abandoned so later transfers still work.",
        );
    }

    fn diagnostics_pane(&mut self, ui: &mut Ui) {
        toggle_row(
            ui,
            &mut self.working.diagnostics.overlay_on_connect,
            "Show the stats overlay on connect",
            None,
        );
        row(ui, "Metrics JSON directory", INPUT_H, |ui| {
            text_input(
                ui,
                "settings-metrics-dir",
                &mut self.metrics_dir,
                260.0,
                true,
            );
        });
        row(ui, "Stage log", INPUT_H, |ui| {
            segmented(
                ui,
                "settings-stage-log",
                &mut self.working.diagnostics.stage_log,
                &[
                    (StageLogLevel::Off, "Off"),
                    (StageLogLevel::Stages, "Stages"),
                    (StageLogLevel::Verbose, "Verbose"),
                ],
            );
        });
        note(
            ui,
            "Reports carry no host, account, credential, path, clipboard or pixel data.",
        );
    }

    fn certificates_pane(&mut self, ui: &mut Ui) {
        if let Some(error) = &self.pins_error {
            ui.label(
                RichText::new(error)
                    .font(theme::sans(12.0))
                    .color(theme::DANGER)
                    .line_height(Some(19.0)),
            );
        }
        if self.pins.is_empty() && self.pins_error.is_none() {
            ui.label(
                RichText::new("No certificates pinned yet.")
                    .font(theme::sans(13.0))
                    .color(theme::TEXT_MUTED),
            );
        }

        let store_path = self
            .known_hosts_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "known_hosts".to_owned());
        let mut forget: Option<String> = None;
        let pins = self.pins.clone();
        let confirming = self.confirm_forget.clone();
        ui.spacing_mut().item_spacing.y = 0.0;
        for pin in &pins {
            let armed = confirming.as_deref() == Some(pin.host.as_str());
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::hover());
            ui.painter().text(
                pos2(rect.min.x, rect.min.y + 12.0),
                Align2::LEFT_TOP,
                &pin.host,
                theme::mono(12.0),
                theme::TEXT_PRIMARY,
            );
            let sub = match &pin.date {
                Some(date) => format!("SHA-256 {} · pinned {date}", pin.short),
                None => format!("SHA-256 {}", pin.short),
            };
            ui.painter().text(
                pos2(rect.min.x, rect.min.y + 31.0),
                Align2::LEFT_TOP,
                sub,
                theme::mono(11.0),
                theme::TEXT_DIM,
            );

            let label = if armed {
                "Forget — press again"
            } else {
                "Forget"
            };
            let width = text_width(ui, label, theme::sans(12.0)) + 4.0;
            let hit = Rect::from_min_size(
                pos2(rect.max.x - width, rect.center().y - 11.0),
                vec2(width, 22.0),
            );
            let response =
                ui.interact(hit, Id::new(("settings-forget", &pin.host)), Sense::click());
            ui.painter().text(
                pos2(rect.max.x, rect.center().y),
                Align2::RIGHT_CENTER,
                label,
                theme::sans(12.0),
                theme::DANGER,
            );
            if response.clicked() {
                forget = Some(pin.host.clone());
            }
            if armed {
                ui.painter().text(
                    pos2(rect.min.x, rect.max.y - 10.0),
                    Align2::LEFT_BOTTOM,
                    format!("Press again to delete this pin from {store_path}"),
                    theme::sans(11.0),
                    theme::DANGER_BODY,
                );
            }
            ui.painter().hline(
                rect.x_range(),
                rect.max.y - 0.5,
                Stroke::new(1.0, theme::LINE_HAIR),
            );
        }
        ui.spacing_mut().item_spacing.y = ROW_GAP;
        if let Some(host) = forget {
            self.press_forget(&host);
        }

        ui.add_space(ROW_GAP);
        note(
            ui,
            "Trust on first use. A changed fingerprint stops the connection and asks.",
        );
    }

    // --- certificate trust ------------------------------------------------------------

    /// First press arms the confirmation; the second rewrites `known_hosts`.
    ///
    /// The store is re-read immediately before the write, so a pin another process
    /// added while Settings sat open is not thrown away by our copy of the list.
    fn press_forget(&mut self, host: &str) {
        if self.confirm_forget.as_deref() != Some(host) {
            self.confirm_forget = Some(host.to_owned());
            return;
        }
        self.confirm_forget = None;
        let Some(path) = self.known_hosts_path.clone() else {
            return;
        };
        match KnownHosts::load(&path) {
            Ok(mut store) => {
                if store.forget(host)
                    && let Err(e) = store.save(&path)
                {
                    self.pins_error = Some(format!("could not rewrite the pin store: {e}"));
                }
            }
            Err(e) => self.pins_error = Some(format!("could not read the pin store: {e}")),
        }
        self.reload_pins();
    }

    fn reload_pins(&mut self) {
        self.pins.clear();
        let Some(path) = self.known_hosts_path.clone() else {
            return;
        };
        let date = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(date_label);
        match KnownHosts::load(&path) {
            Ok(store) => {
                self.pins_error = None;
                self.pins = store
                    .pins()
                    .map(|(host, fp)| Pin {
                        host: host.to_owned(),
                        short: short_fingerprint(&fp.to_hex()),
                        date: date.clone(),
                    })
                    .collect();
            }
            Err(e) => self.pins_error = Some(format!("could not read the pin store: {e}")),
        }
    }
}

enum FooterAction {
    None,
    Cancel,
    Save,
}

// --- shared widgets, drawn to the handoff's shared-chrome spec -------------------------

/// A pane row: an uppercase 150px field label, then its control.
fn row(ui: &mut Ui, label: &str, height: f32, control: impl FnOnce(&mut Ui)) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let label_rect = Rect::from_min_size(
        pos2(rect.min.x, rect.center().y - 8.0),
        vec2(LABEL_W - 12.0, 16.0),
    );
    let mut label_ui = ui.new_child(UiBuilder::new().max_rect(label_rect));
    label_ui.label(
        RichText::new(label.to_uppercase())
            .font(theme::sans_medium(11.0))
            .color(theme::TEXT_MUTED)
            .extra_letter_spacing(11.0 * 0.09),
    );
    let control_rect = Rect::from_min_max(pos2(rect.min.x + LABEL_W, rect.min.y), rect.max);
    let mut control_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(control_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    control_ui.spacing_mut().item_spacing.x = 8.0;
    control(&mut control_ui);
}

/// A `line.hair` divider between the structural halves of a pane.
fn divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, theme::LINE_HAIR),
    );
}

/// A Plex Mono 11px uppercase section label with the spec's 0.14em tracking.
fn section_label(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(theme::mono(11.0))
            .color(theme::TEXT_DIM)
            .extra_letter_spacing(11.0 * 0.14),
    );
}

/// A pane's explanatory note: 12px/20px `text.muted` prose.
fn note(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(theme::sans(12.0))
            .color(theme::TEXT_MUTED)
            .line_height(Some(20.0)),
    );
}

/// A 34×19 toggle: radius 10 track, 15px knob with a 2px inset.
fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(34.0, 19.0), Sense::click());
    if response.clicked() {
        *on = !*on;
    }
    let (track, knob) = if *on {
        (theme::ACCENT, theme::ACCENT_ON)
    } else {
        (theme::LINE_SUBTLE, theme::TEXT_MUTED)
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(theme::radius::TOGGLE_TRACK), track);
    let knob_x = if *on {
        rect.max.x - 2.0 - 7.5
    } else {
        rect.min.x + 2.0 + 7.5
    };
    ui.painter()
        .circle_filled(pos2(knob_x, rect.center().y), 7.5, knob);
    response
}

/// A toggle with its label (and optional sub-copy) to the right.
fn toggle_row(ui: &mut Ui, on: &mut bool, label: &str, sub: Option<&str>) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        toggle(ui, on);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.label(
                RichText::new(label)
                    .font(theme::sans(13.0))
                    .color(theme::TEXT_PRIMARY),
            );
            if let Some(sub) = sub {
                ui.label(
                    RichText::new(sub)
                        .font(theme::sans(12.0))
                        .color(theme::TEXT_MUTED)
                        .line_height(Some(19.0)),
                );
            }
        });
    });
}

/// A 16px checkbox: `accent` fill with an `accent.on` check, or a `line.strong` outline.
fn checkbox(ui: &mut Ui, checked: &mut bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
    if response.clicked() {
        *checked = !*checked;
    }
    if *checked {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(theme::radius::MENU_ITEM),
            theme::ACCENT,
        );
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "✓",
            theme::sans_semibold(11.0),
            theme::ACCENT_ON,
        );
    } else {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(theme::radius::MENU_ITEM),
            Stroke::new(1.0, theme::LINE_STRONG),
            StrokeKind::Inside,
        );
    }
    response
}

fn checkbox_row(ui: &mut Ui, checked: &mut bool, label: &str, sub: Option<&str>) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        checkbox(ui, checked);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.label(
                RichText::new(label)
                    .font(theme::sans(13.0))
                    .color(theme::TEXT_PRIMARY),
            );
            if let Some(sub) = sub {
                ui.label(
                    RichText::new(sub)
                        .font(theme::sans(12.0))
                        .color(theme::TEXT_MUTED)
                        .line_height(Some(19.0)),
                );
            }
        });
    });
}

/// A grouped-track segmented control: `bg.chrome` track, 1px `line.hair`, radius 4,
/// 3px padding, segments `padding:5px 11px` at radius 3 and 12px.
fn segmented<T: PartialEq + Copy>(ui: &mut Ui, id: &str, value: &mut T, options: &[(T, &str)]) {
    const SEG_PAD_X: f32 = 11.0;
    const SEG_H: f32 = 22.0;
    const TRACK_PAD: f32 = 3.0;
    let widths: Vec<f32> = options
        .iter()
        .map(|(_, label)| text_width(ui, label, theme::sans(12.0)) + SEG_PAD_X * 2.0)
        .collect();
    let total: f32 = widths.iter().sum::<f32>() + TRACK_PAD * 2.0;
    let (track, _) = ui.allocate_exact_size(vec2(total, SEG_H + TRACK_PAD * 2.0), Sense::hover());
    ui.painter().rect(
        track,
        CornerRadius::same(theme::radius::INPUT),
        theme::BG_CHROME,
        Stroke::new(1.0, theme::LINE_HAIR),
        StrokeKind::Inside,
    );
    let mut x = track.min.x + TRACK_PAD;
    for (i, ((option, label), width)) in options.iter().zip(&widths).enumerate() {
        let seg = Rect::from_min_size(pos2(x, track.min.y + TRACK_PAD), vec2(*width, SEG_H));
        let response = ui.interact(seg, Id::new((id, i)), Sense::click());
        if response.clicked() {
            *value = *option;
        }
        let selected = *value == *option;
        if selected {
            ui.painter().rect_filled(
                seg,
                CornerRadius::same(theme::radius::MENU_ITEM),
                theme::ACCENT_FILL,
            );
        }
        ui.painter().text(
            seg.center(),
            Align2::CENTER_CENTER,
            label,
            theme::sans(12.0),
            if selected {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_MUTED
            },
        );
        x = seg.max.x;
    }
}

/// A 34px input: `bg.chrome`, radius 4, Plex Mono 13px. Disabled inputs render
/// `text.dim` on a `line.hair` border, as the spec's derived/disabled state.
fn text_input(ui: &mut Ui, id: &str, value: &mut String, width: f32, enabled: bool) -> Response {
    let (rect, _) = ui.allocate_exact_size(vec2(width, INPUT_H), Sense::hover());
    let id = Id::new(id);
    let focused = enabled && ui.ctx().memory(|m| m.has_focus(id));
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(theme::radius::INPUT),
        theme::BG_CHROME,
    );
    let font = theme::mono(13.0);
    let row_h = ui.fonts_mut(|f| f.row_height(&font));
    let inner = Rect::from_min_size(
        pos2(rect.min.x + 11.0, (rect.center().y - row_h / 2.0).round()),
        vec2((rect.width() - 22.0).max(0.0), row_h),
    );
    let edit = TextEdit::singleline(value)
        .id(id)
        .font(font)
        .margin(egui::Margin::ZERO)
        .frame(Frame::NONE)
        .text_color(if enabled {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_DIM
        })
        .desired_width(inner.width());
    let response = ui.add_enabled_ui(enabled, |ui| ui.place(inner, edit)).inner;
    let border = if focused {
        theme::ACCENT
    } else if enabled {
        theme::LINE_SUBTLE
    } else {
        theme::LINE_HAIR
    };
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(theme::radius::INPUT),
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    response
}

fn text_width(ui: &Ui, text: &str, font: egui::FontId) -> f32 {
    ui.fonts_mut(|f| {
        f.layout_no_wrap(text.to_owned(), font, Color32::WHITE)
            .size()
            .x
    })
}

/// Open the directory holding `path` in the platform file manager.
///
/// Deliberately a private copy of the launcher's one-liner rather than a shared import:
/// this module owns one sidebar affordance, and coupling it to the shell module's
/// internals to save four lines would be the worse trade.
fn reveal_directory(path: &Path) {
    let dir = path.parent().unwrap_or(path);
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(dir).spawn();
    #[cfg(target_os = "windows")]
    let result = Command::new("explorer").arg(dir).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = Command::new("xdg-open").arg(dir).spawn();
    if let Err(e) = result {
        eprintln!("could not reveal {}: {e}", dir.display());
    }
}

// --- field parsing and formatting ------------------------------------------------------

/// Parse a numeric field, clamping into `min..=max`; unparseable text keeps `fallback`.
fn parse_u16(text: &str, fallback: u16, min: u16, max: u16) -> u16 {
    match text.trim().parse::<u64>() {
        Ok(v) => v.clamp(u64::from(min), u64::from(max)) as u16,
        Err(_) => fallback,
    }
}

/// Parse the Max-image field: a plain byte count, or a `KiB`/`MiB` figure as displayed.
fn parse_bytes(text: &str, fallback: u64) -> u64 {
    let lowered = text.trim().to_ascii_lowercase();
    let (number, multiplier) = if let Some(rest) = lowered.strip_suffix("mib") {
        (rest, 1024 * 1024)
    } else if let Some(rest) = lowered.strip_suffix("kib") {
        (rest, 1024)
    } else if let Some(rest) = lowered.strip_suffix("mb") {
        (rest, 1024 * 1024)
    } else if let Some(rest) = lowered.strip_suffix("kb") {
        (rest, 1024)
    } else if let Some(rest) = lowered.strip_suffix('b') {
        (rest, 1)
    } else {
        (lowered.as_str(), 1)
    };
    let Ok(value) = number.trim().parse::<f64>() else {
        return fallback;
    };
    if !value.is_finite() || value <= 0.0 {
        return fallback;
    }
    let bytes = (value * f64::from(multiplier)).round();
    (bytes as u64).clamp(IMAGE_BYTES_MIN, IMAGE_BYTES_MAX)
}

/// `1 MiB` / `512 KiB` / `900 B` — exact units only, so the field round-trips.
fn format_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else if bytes >= KIB && bytes.is_multiple_of(KIB) {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Parse the Transfer-timeout field, accepting the `5 s` form it is displayed in.
fn parse_secs(text: &str, fallback: u64) -> u64 {
    let lowered = text.trim().to_ascii_lowercase();
    let number = lowered.strip_suffix('s').unwrap_or(&lowered);
    match number.trim().parse::<u64>() {
        Ok(v) => v.clamp(TIMEOUT_SECS_MIN, TIMEOUT_SECS_MAX),
        Err(_) => fallback,
    }
}

fn format_secs(secs: u64) -> String {
    format!("{secs} s")
}

/// `9f:2c:…:b7` — the head and tail of a SHA-256 hex digest, as the row copy reads.
fn short_fingerprint(hex: &str) -> String {
    if hex.len() < 6 {
        return hex.to_owned();
    }
    format!("{}:{}:…:{}", &hex[0..2], &hex[2..4], &hex[hex.len() - 2..])
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `14 Aug` for a UTC instant.
fn date_label(t: SystemTime) -> Option<String> {
    let secs = t.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let (_, month, day) = civil_from_days((secs / 86_400) as i64);
    Some(format!("{day} {}", MONTHS[(month - 1) as usize]))
}

/// Days-since-epoch to `(year, month, day)` — Howard Hinnant's civil_from_days.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::Fingerprint;

    const FP_A: &str = "9f2c0000000000000000000000000000000000000000000000000000000000b7";
    const FP_B: &str = "0011000000000000000000000000000000000000000000000000000000000022";

    fn tmpdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mdrdp-settings-ui-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// Settings with a distinct value in every table, so a commit that copies one
    /// field into another cannot pass.
    fn edited() -> Settings {
        let mut s = Settings::default();
        s.defaults.username = Some("ano".to_owned());
        s.defaults.port = 3391;
        s.defaults.window = WindowMode::Explicit;
        s.defaults.width = 1600;
        s.defaults.height = 1000;
        s.defaults.keep_launcher_open = false;
        s.defaults.reconnect_last = true;
        s.graphics.clear_codec = false;
        s.graphics.rfx_progressive = false;
        s.graphics.allow_uncompressed = true;
        s.graphics.dynamic_resolution = false;
        s.keyboard.mac_keyboard_mode = true;
        s.audio.playback = false;
        s.audio.microphone = true;
        s.audio.device = "USB Audio".to_owned();
        s.clipboard.direction = ClipboardDirection::ToRemote;
        s.clipboard.max_image_bytes = 2 * 1024 * 1024;
        s.clipboard.timeout_secs = 9;
        s.diagnostics.overlay_on_connect = true;
        s.diagnostics.metrics_dir = "/tmp/runs".to_owned();
        s.diagnostics.stage_log = StageLogLevel::Verbose;
        s
    }

    /// Make every edit the modal can make, in the buffers and the working copy alike.
    fn apply_edits(m: &mut SettingsModal, target: &Settings) {
        m.username = target.defaults.username.clone().unwrap_or_default();
        m.port = target.defaults.port.to_string();
        m.width = target.defaults.width.to_string();
        m.height = target.defaults.height.to_string();
        m.device_named = target.audio.device != "default";
        m.device = target.audio.device.clone();
        m.max_image = format_bytes(target.clipboard.max_image_bytes);
        m.timeout = format_secs(target.clipboard.timeout_secs);
        m.metrics_dir = target.diagnostics.metrics_dir.clone();
        m.working.defaults.window = target.defaults.window;
        m.working.defaults.keep_launcher_open = target.defaults.keep_launcher_open;
        m.working.defaults.reconnect_last = target.defaults.reconnect_last;
        m.working.graphics = target.graphics.clone();
        m.working.keyboard = target.keyboard.clone();
        m.working.audio.playback = target.audio.playback;
        m.working.audio.microphone = target.audio.microphone;
        m.working.clipboard.direction = target.clipboard.direction;
        m.working.diagnostics.overlay_on_connect = target.diagnostics.overlay_on_connect;
        m.working.diagnostics.stage_log = target.diagnostics.stage_log;
    }

    #[test]
    fn selecting_a_pane_changes_nothing_but_the_pane() {
        let original = Settings::default();
        let mut m = SettingsModal::new(original.clone(), None);
        assert_eq!(m.pane(), Pane::Defaults);
        for pane in Pane::ALL {
            m.pane = pane;
            assert_eq!(m.pane(), pane);
        }
        assert_eq!(m.working, original, "pane selection must not edit settings");
    }

    #[test]
    fn the_working_copy_is_isolated_until_save() {
        let original = Settings::default();
        let target = edited();
        let mut m = SettingsModal::new(original.clone(), None);
        apply_edits(&mut m, &target);

        // Cancel: the caller's settings are untouched, because nothing was handed back.
        assert_eq!(original, Settings::default(), "the caller's copy is intact");

        let saved = m.commit();
        assert_eq!(saved, target, "Save returns the edited copy");
        // Every table actually changed — a commit that dropped a table would still
        // equal `target` in the others.
        assert_ne!(saved.defaults, original.defaults);
        assert_ne!(saved.graphics, original.graphics);
        assert_ne!(saved.audio, original.audio);
        assert_ne!(saved.clipboard, original.clipboard);
        assert_ne!(saved.diagnostics, original.diagnostics);
    }

    #[test]
    fn numeric_fields_clamp_and_fall_back() {
        assert_eq!(parse_u16("3391", 3389, 1, u16::MAX), 3391);
        assert_eq!(
            parse_u16("99999", 3389, 1, u16::MAX),
            u16::MAX,
            "clamped up"
        );
        assert_eq!(parse_u16("0", 3389, 1, u16::MAX), 1, "clamped down");
        assert_eq!(parse_u16("", 3389, 1, u16::MAX), 3389, "kept on nonsense");
        assert_eq!(parse_u16("八", 3389, 1, u16::MAX), 3389);
        assert_eq!(parse_u16("100", 1920, DIM_MIN, u16::MAX), DIM_MIN);
    }

    #[test]
    fn a_nonsense_number_keeps_the_previous_value_through_commit() {
        let mut m = SettingsModal::new(Settings::default(), None);
        m.port = "not a port".to_owned();
        m.width = "0".to_owned();
        let saved = m.commit();
        assert_eq!(saved.defaults.port, 3389, "unparseable text keeps the port");
        assert_eq!(
            saved.defaults.width, DIM_MIN,
            "an out-of-range width clamps"
        );
    }

    #[test]
    fn byte_and_second_fields_round_trip_their_display_form() {
        assert_eq!(format_bytes(1_048_576), "1 MiB");
        assert_eq!(format_bytes(524_288), "512 KiB");
        assert_eq!(format_bytes(900), "900 B");
        assert_eq!(parse_bytes("1 MiB", 7), 1_048_576);
        assert_eq!(parse_bytes("512 KiB", 7), 524_288);
        assert_eq!(parse_bytes("1048576", 7), 1_048_576);
        assert_eq!(
            parse_bytes("0", 7),
            7,
            "zero is nonsense, keep the old value"
        );
        assert_eq!(parse_bytes("junk", 7), 7);
        assert_eq!(parse_bytes("64 B", 7), IMAGE_BYTES_MIN, "clamped up");
        assert_eq!(parse_bytes("9 MiB", 7), 9 * 1024 * 1024);

        assert_eq!(format_secs(5), "5 s");
        assert_eq!(parse_secs("5 s", 3), 5);
        assert_eq!(parse_secs("9", 3), 9);
        assert_eq!(parse_secs("0 s", 3), TIMEOUT_SECS_MIN);
        assert_eq!(parse_secs("100000 s", 3), TIMEOUT_SECS_MAX);
        assert_eq!(parse_secs("soon", 3), 3);
    }

    #[test]
    fn a_named_device_survives_commit_and_default_clears_it() {
        let mut m = SettingsModal::new(Settings::default(), None);
        m.device_named = true;
        m.device = "USB Audio".to_owned();
        assert_eq!(m.commit().audio.device, "USB Audio");

        let mut back = SettingsModal::new(m.commit(), None);
        assert!(back.device_named, "a named device reopens as Named");
        back.device_named = false;
        assert_eq!(back.commit().audio.device, "default");
    }

    #[test]
    fn the_fingerprint_row_shows_head_and_tail() {
        assert_eq!(short_fingerprint(FP_A), "9f:2c:…:b7");
        assert_eq!(short_fingerprint("abcd"), "abcd", "too short to abbreviate");
    }

    #[test]
    fn the_date_label_is_the_civil_date_of_the_instant() {
        let at = |secs: u64| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        assert_eq!(date_label(at(0)).as_deref(), Some("1 Jan"));
        // 2000-02-29T00:00:00Z — the leap day the naive 365-day arithmetic misses.
        assert_eq!(date_label(at(951_782_400)).as_deref(), Some("29 Feb"));
        // 2026-08-14T12:00:00Z.
        assert_eq!(date_label(at(1_786_708_800)).as_deref(), Some("14 Aug"));
    }

    #[test]
    fn forgetting_a_pin_needs_a_second_press() {
        let path = tmpdir().join("known_hosts_forget");
        let _ = std::fs::remove_file(&path);
        let mut store = KnownHosts::default();
        store.insert("temper:3389", Fingerprint::from_hex(FP_A).unwrap());
        store.insert("quench:3389", Fingerprint::from_hex(FP_B).unwrap());
        store.save(&path).expect("save");

        let mut m = SettingsModal::new(Settings::default(), Some(path.clone()));
        assert_eq!(m.pins.len(), 2, "both pins listed");
        assert_eq!(m.pins[0].host, "quench:3389", "sorted by host");
        assert_eq!(m.pins[1].short, "9f:2c:…:b7");

        // First press only arms the confirmation — the file must be untouched.
        m.press_forget("temper:3389");
        assert_eq!(m.confirm_forget.as_deref(), Some("temper:3389"));
        assert!(
            KnownHosts::load(&path)
                .unwrap()
                .get("temper:3389")
                .is_some(),
            "one press must not delete a pin"
        );

        // Second press writes it through.
        m.press_forget("temper:3389");
        assert_eq!(m.confirm_forget, None, "the confirmation is spent");
        let reloaded = KnownHosts::load(&path).expect("reload");
        assert!(reloaded.get("temper:3389").is_none(), "pin removed");
        assert!(reloaded.get("quench:3389").is_some(), "sibling pin kept");
        assert_eq!(m.pins.len(), 1, "the list refreshed from disk");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn arming_one_row_does_not_arm_another() {
        let path = tmpdir().join("known_hosts_arm");
        let _ = std::fs::remove_file(&path);
        let mut store = KnownHosts::default();
        store.insert("temper:3389", Fingerprint::from_hex(FP_A).unwrap());
        store.insert("quench:3389", Fingerprint::from_hex(FP_B).unwrap());
        store.save(&path).expect("save");

        let mut m = SettingsModal::new(Settings::default(), Some(path.clone()));
        m.press_forget("temper:3389");
        m.press_forget("quench:3389"); // arms the second, does not fire the first
        assert_eq!(m.confirm_forget.as_deref(), Some("quench:3389"));
        assert_eq!(
            KnownHosts::load(&path).unwrap().pins().count(),
            2,
            "switching rows must not delete anything"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Run one real egui frame of the modal on the pane it is showing.
    ///
    /// `ui()` cannot be asserted pixel by pixel, but it can be *run*: this lays out and
    /// tessellates the whole modal, which is what catches a bad rect, an id clash, or a
    /// widget placed outside its parent.
    fn frame(m: &mut SettingsModal, path: &Path) -> SettingsOutcome {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 700.0))),
            ..Default::default()
        };
        let mut outcome = SettingsOutcome::Open;
        let output = ctx.run_ui(input, |ui| {
            outcome = m.ui(ui.ctx(), path);
        });
        output.drop_without_applying_deltas();
        outcome
    }

    #[test]
    fn a_real_frame_draws_every_pane() {
        let path = tmpdir().join("known_hosts_draw");
        let _ = std::fs::remove_file(&path);
        let mut store = KnownHosts::default();
        store.insert("temper:3389", Fingerprint::from_hex(FP_A).unwrap());
        store.save(&path).expect("save");

        let favourites = tmpdir().join("favourites.toml");
        let mut m = SettingsModal::new(edited(), Some(path.clone()));
        // A clipboard direction with no segment of its own, and an armed forget, so the
        // widest arrangement of each pane is the one that gets drawn.
        m.working.clipboard.direction = ClipboardDirection::FromRemote;
        m.confirm_forget = Some("temper:3389".to_owned());
        for pane in Pane::ALL {
            m.pane = pane;
            assert_eq!(
                frame(&mut m, &favourites),
                SettingsOutcome::Open,
                "{} drew without deciding anything",
                pane.label()
            );
        }
        // And the fullscreen arrangement of Defaults, where the size inputs are dimmed.
        m.pane = Pane::Defaults;
        m.working.defaults.window = WindowMode::Fullscreen;
        assert_eq!(frame(&mut m, &favourites), SettingsOutcome::Open);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn escape_cancels() {
        let favourites = tmpdir().join("favourites.toml");
        let mut m = SettingsModal::new(Settings::default(), None);
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 700.0))),
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let mut outcome = SettingsOutcome::Open;
        let output = ctx.run_ui(input, |ui| {
            outcome = m.ui(ui.ctx(), &favourites);
        });
        output.drop_without_applying_deltas();
        assert_eq!(outcome, SettingsOutcome::Cancelled);
    }
}
