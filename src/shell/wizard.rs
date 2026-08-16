//! The three-step connection wizard: destination, sign-in, display.
//!
//! This is the launcher's front page when `favourites.toml` holds no entries. It is
//! drawn to the 2026-08-16 handoff ("1. Connection wizard"), reading every colour,
//! size and font through [`crate::ui::theme`] so no literal appears here.
//!
//! Two rules shape the code:
//!
//! * **Validation wording is not reinvented.** The messages come from
//!   [`crate::ui::form::FormError`] — the same strings the software-rendered form
//!   showed — so replacing the toolkit does not silently reword every error.
//! * **The typed password never leaves this module in the clear.** It lives in a
//!   [`TypedPassword`], which redacts its own `Debug` and zeroes itself on drop, and it
//!   is never logged, printed, or written to a favourite. `creds::Secret` would be the
//!   natural carrier, but its inner value is private to `creds` and there is no public
//!   constructor, so this is the narrowest honest stand-in.
//!
//! Everything that can be decided without a frame — validation, the favourite it
//! builds, the `mdrdp …` preview — lives in [`Draft`] as pure functions. [`Wizard::ui`]
//! only draws and routes input.

use std::fmt;

use crate::favourites::{DEFAULT_PORT, Favourite, WindowSize};
use crate::shell::widgets;
// FormError/FormField moved here verbatim when the software-rendered form was
// deleted (handoff decision 1): the wizard is the wording's home now.

/// The field a validation error points at. Survives from the deleted
/// software-rendered form; the wizard highlights and focuses by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    DisplayName,
    Host,
    Port,
    Username,
    Password,
    SessionMode,
    Width,
    Height,
    Save,
    Cancel,
}

/// Why validation failed, with the exact wording the original form shipped —
/// the handoff mandates reusing these strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormError {
    BlankDisplayName,
    BlankHost,
    BlankUsername,
    BlankPassword,
    InvalidPort,
    PortOutOfRange,
    InvalidWidth,
    WidthOutOfRange,
    InvalidHeight,
    HeightOutOfRange,
}

impl FormError {
    /// The field that needs attention for this error.
    pub const fn field(self) -> FormField {
        match self {
            Self::BlankDisplayName => FormField::DisplayName,
            Self::BlankHost => FormField::Host,
            Self::BlankUsername => FormField::Username,
            Self::BlankPassword => FormField::Password,
            Self::InvalidPort | Self::PortOutOfRange => FormField::Port,
            Self::InvalidWidth | Self::WidthOutOfRange => FormField::Width,
            Self::InvalidHeight | Self::HeightOutOfRange => FormField::Height,
        }
    }
}

impl std::fmt::Display for FormError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::BlankDisplayName => "display name must not be blank",
            Self::BlankHost => "host must not be blank",
            Self::BlankUsername => "username must not be blank",
            Self::BlankPassword => "password must not be blank",
            Self::InvalidPort => "port must be a whole number from 1 to 65535",
            Self::PortOutOfRange => "port must be between 1 and 65535",
            Self::InvalidWidth => "width must be a whole number from 1 to 65535",
            Self::WidthOutOfRange => "width must be between 1 and 65535",
            Self::InvalidHeight => "height must be a whole number from 1 to 65535",
            Self::HeightOutOfRange => "height must be between 1 and 65535",
        };
        f.write_str(text)
    }
}

impl std::error::Error for FormError {}

use crate::ui::theme;
use egui::{
    Align2, Color32, CornerRadius, Frame, Id, Margin, Rect, Response, RichText, Sense, Stroke,
    StrokeKind, TextEdit, Ui, UiBuilder, pos2, vec2,
};
use zeroize::Zeroize;

// --- geometry, from the handoff ---------------------------------------------------------

/// Horizontal page padding for header, rail, body and footer alike.
const PAGE_PAD_X: i8 = 56;
/// Footer band height.
const FOOTER_H: f32 = 72.0;
/// Step-rail chip diameter, and the gap between a chip and its label.
const CHIP: f32 = 22.0;
const CHIP_LABEL_GAP: f32 = 10.0;
/// Gap between rail items and their connectors.
const RAIL_GAP: f32 = 14.0;
/// Field box heights: the wizard's primary inputs, and the step-1 secondary row.
const FIELD_H: f32 = 44.0;
const FIELD_SMALL_H: f32 = 38.0;
/// Horizontal padding inside an input box.
const FIELD_PAD_X: f32 = 14.0;
/// Display-choice card height, and the width of the `Port` column on step 1.
const CARD_H: f32 = 188.0;
const PORT_COL_W: f32 = 140.0;
const DOMAIN_COL_W: f32 = 220.0;
/// "Ready to connect" summary block height.
const SUMMARY_H: f32 = 146.0;
/// Buttons are 38px on a screen (34px is the dialog size).
const BUTTON_H: f32 = 38.0;

// --- the typed password -----------------------------------------------------------------

/// A password typed into the wizard: redacted in `Debug`, wiped on drop.
///
/// `creds::Secret` is the shape this wants to be, but its field is private to `creds`
/// and it exposes no constructor, so the wizard carries its own equivalent rather than
/// widening that module's API from here.
pub struct TypedPassword(String);

impl TypedPassword {
    /// The password itself. Deliberately named so a call site reads as a decision.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TypedPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TypedPassword(<redacted>)")
    }
}

impl Drop for TypedPassword {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

// --- public surface ---------------------------------------------------------------------

/// What a frame of the wizard asks the launcher to do.
#[derive(Debug)]
pub enum WizardOutcome {
    /// Still going — nothing for the caller to do.
    None,
    /// The user cancelled (footer button or `Esc`).
    Cancelled,
    /// Step 3's "Save and connect". `favourite` has passed every step's validation.
    Finished {
        favourite: Favourite,
        /// Mirrors step 3's "Save as a favourite named …" checkbox.
        save_favourite: bool,
        /// `Some` only when the user typed a password *and* ticked step 2's
        /// "Save the password to the system credential store".
        password: Option<TypedPassword>,
        save_password: bool,
    },
}

/// Which step is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Destination,
    SignIn,
    Display,
}

impl Step {
    /// Zero-based position in the rail.
    fn index(self) -> usize {
        match self {
            Step::Destination => 0,
            Step::SignIn => 1,
            Step::Display => 2,
        }
    }
}

/// Everything the user has typed apart from the password.
///
/// Split out so validation, favourite construction and the command preview are pure
/// functions over plain strings — the wizard's whole testable surface.
#[derive(Debug, Default, Clone)]
struct Draft {
    host: String,
    display_name: String,
    port: String,
    username: String,
    domain: String,
    /// Step 3's card selection: fullscreen, or the explicit size below.
    fullscreen: bool,
    width: String,
    height: String,
}

/// The wizard's state. Fields are private; the launcher drives it through
/// [`Wizard::new`] and [`Wizard::ui`].
pub struct Wizard {
    step: Step,
    draft: Draft,
    /// The typed password. Zeroed on drop and whenever it is not handed back.
    password: String,
    show_password: bool,
    save_password: bool,
    save_favourite: bool,
    /// The last validation failure, shown under the field it names.
    error: Option<FormError>,
    /// Set when the step changes, so the step's first field takes focus once.
    focus_pending: bool,
}

impl Wizard {
    /// A fresh wizard. `default_username` pre-fills the sign-in step, as
    /// Settings ▸ Defaults does.
    pub fn new(default_username: Option<String>) -> Self {
        Wizard {
            step: Step::Destination,
            draft: Draft {
                username: default_username.unwrap_or_default(),
                fullscreen: true,
                width: "1920".to_owned(),
                height: "1080".to_owned(),
                ..Draft::default()
            },
            password: String::new(),
            show_password: false,
            save_password: true,
            save_favourite: true,
            error: None,
            focus_pending: true,
        }
    }

    /// Draw the whole page — header, step rail, body, footer — and route its input.
    pub fn ui(&mut self, ui: &mut Ui) -> WizardOutcome {
        let nav = self.footer(ui);
        egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::BG_WINDOW))
            .show(ui, |ui| {
                self.header(ui);
                self.rail(ui);
                self.body(ui);
            });

        let (enter, escape) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if escape {
            return WizardOutcome::Cancelled;
        }
        match nav {
            Nav::Cancel => WizardOutcome::Cancelled,
            Nav::Back => {
                self.back();
                WizardOutcome::None
            }
            Nav::Continue => self.advance(),
            Nav::None if enter => self.advance(),
            Nav::None => WizardOutcome::None,
        }
    }

    // --- step transitions ---------------------------------------------------------------

    /// Validate the current step and move on, finishing on step 3.
    fn advance(&mut self) -> WizardOutcome {
        let checked = match self.step {
            Step::Destination => self.draft.validate_destination(),
            Step::SignIn => self
                .draft
                .validate_sign_in(!self.password.is_empty(), self.save_password),
            Step::Display => self.draft.validate_display(),
        };
        if let Err(error) = checked {
            self.error = Some(error);
            return WizardOutcome::None;
        }
        self.error = None;
        match self.step {
            Step::Destination => {
                self.step = Step::SignIn;
                self.focus_pending = true;
                WizardOutcome::None
            }
            Step::SignIn => {
                self.step = Step::Display;
                self.focus_pending = true;
                WizardOutcome::None
            }
            Step::Display => match self.draft.favourite(self.save_password) {
                Ok(favourite) => WizardOutcome::Finished {
                    favourite,
                    save_favourite: self.save_favourite,
                    password: self.take_password(),
                    save_password: self.save_password,
                },
                Err(error) => {
                    self.error = Some(error);
                    WizardOutcome::None
                }
            },
        }
    }

    fn back(&mut self) {
        self.step = match self.step {
            Step::Destination => Step::Destination,
            Step::SignIn => Step::Destination,
            Step::Display => Step::SignIn,
        };
        self.error = None;
        self.focus_pending = true;
    }

    /// Hand the typed password out — only when it is going to the credential store.
    /// Anything left behind is wiped rather than kept alive in the buffer.
    fn take_password(&mut self) -> Option<TypedPassword> {
        let typed = std::mem::take(&mut self.password);
        if typed.is_empty() || !self.save_password {
            let mut discard = typed;
            discard.zeroize();
            return None;
        }
        Some(TypedPassword(typed))
    }

    // --- chrome -------------------------------------------------------------------------

    /// Header block: 24px title over the 13px `text.muted` sub-line.
    fn header(&mut self, ui: &mut Ui) {
        let title = match self.step {
            Step::Destination => "Set up a connection".to_owned(),
            Step::SignIn => format!("Sign in to {}", self.draft.host.trim()),
            Step::Display => "How the desktop appears".to_owned(),
        };
        Frame::new()
            .inner_margin(Margin {
                left: PAGE_PAD_X,
                right: PAGE_PAD_X,
                top: 34,
                bottom: 0,
            })
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.label(
                    RichText::new(title)
                        .font(theme::sans_semibold(24.0))
                        .color(theme::TEXT_PRIMARY),
                );
                match self.step {
                    Step::Destination => prose(
                        ui,
                        &[
                            Span::body("No saved connections yet. This takes three screens — or run "),
                            Span::code("mdrdp <host>"),
                            Span::body(" in a terminal and skip it entirely."),
                        ],
                    ),
                    Step::SignIn => prose(
                        ui,
                        &[
                            Span::body(
                                "The password goes straight to the Keychain (macOS) or Credential \
                                 Manager (Windows). The saved connection stores only the account key ",
                            ),
                            Span::code(&self.draft.account_key()),
                            Span::body("."),
                        ],
                    ),
                    Step::Display => prose(
                        ui,
                        &[Span::body(
                            "The remote resolution stays fixed at the size chosen here for the \
                             whole session.",
                        )],
                    ),
                }
            });
    }

    /// Step rail: 22px chips joined by 1px connectors, each label showing the answer
    /// once its step is behind us.
    fn rail(&mut self, ui: &mut Ui) {
        let current = self.step.index();
        let labels = [
            if current > 0 {
                self.draft.host_and_port()
            } else {
                "Destination".to_owned()
            },
            if current > 1 {
                self.draft.username.trim().to_owned()
            } else {
                "Sign in".to_owned()
            },
            "Display".to_owned(),
        ];
        Frame::new()
            .inner_margin(Margin {
                left: PAGE_PAD_X,
                right: PAGE_PAD_X,
                top: 30,
                bottom: 0,
            })
            .show(ui, |ui| {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), CHIP), Sense::hover());
                let widths: Vec<f32> = labels
                    .iter()
                    .enumerate()
                    .map(|(i, label)| {
                        let font = if i == current {
                            theme::sans_medium(13.0)
                        } else {
                            theme::sans(13.0)
                        };
                        CHIP + CHIP_LABEL_GAP + text_width(ui, label, font)
                    })
                    .collect();
                let fixed: f32 = widths.iter().sum::<f32>() + RAIL_GAP * 4.0;
                let connector = ((rect.width() - fixed) / 2.0).max(0.0);
                let cy = rect.center().y;
                let mut x = rect.min.x;
                for (i, label) in labels.iter().enumerate() {
                    self.rail_chip(ui, pos2(x + CHIP / 2.0, cy), i, current);
                    let font = if i == current {
                        theme::sans_medium(13.0)
                    } else {
                        theme::sans(13.0)
                    };
                    let colour = match i.cmp(&current) {
                        std::cmp::Ordering::Less => theme::TEXT_SECONDARY,
                        std::cmp::Ordering::Equal => theme::TEXT_PRIMARY,
                        std::cmp::Ordering::Greater => theme::TEXT_MUTED,
                    };
                    ui.painter().text(
                        pos2(x + CHIP + CHIP_LABEL_GAP, cy),
                        Align2::LEFT_CENTER,
                        label,
                        font,
                        colour,
                    );
                    x += widths[i];
                    if i < labels.len() - 1 {
                        x += RAIL_GAP;
                        ui.painter().hline(
                            x..=(x + connector),
                            cy,
                            Stroke::new(1.0, theme::LINE_SUBTLE),
                        );
                        x += connector + RAIL_GAP;
                    }
                }
            });
    }

    fn rail_chip(&self, ui: &mut Ui, centre: egui::Pos2, index: usize, current: usize) {
        let painter = ui.painter();
        let r = CHIP / 2.0;
        match index.cmp(&current) {
            std::cmp::Ordering::Less => {
                painter.circle(
                    centre,
                    r,
                    theme::ACCENT_TINT,
                    Stroke::new(1.0, theme::ACCENT_FILL),
                );
                painter.text(
                    centre,
                    Align2::CENTER_CENTER,
                    "✓",
                    theme::mono(12.0),
                    theme::ACCENT,
                );
            }
            std::cmp::Ordering::Equal => {
                painter.circle_filled(centre, r, theme::ACCENT);
                painter.text(
                    centre,
                    Align2::CENTER_CENTER,
                    format!("{}", index + 1),
                    theme::mono_semibold(11.0),
                    theme::ACCENT_ON,
                );
            }
            std::cmp::Ordering::Greater => {
                painter.circle_stroke(centre, r, Stroke::new(1.0, theme::LINE_STRONG));
                painter.text(
                    centre,
                    Align2::CENTER_CENTER,
                    format!("{}", index + 1),
                    theme::mono(11.0),
                    theme::TEXT_MUTED,
                );
            }
        }
    }

    /// Footer: `Step N of 3` left, Cancel/Back plus the primary action right.
    fn footer(&mut self, ui: &mut Ui) -> Nav {
        let step = self.step;
        let mut nav = Nav::None;
        egui::Panel::bottom("wizard-footer")
            .exact_size(FOOTER_H)
            .frame(
                Frame::new()
                    .fill(theme::BG_CHROME)
                    .inner_margin(Margin::symmetric(PAGE_PAD_X, 0)),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                let rect = ui.max_rect();
                ui.painter().hline(
                    rect.x_range().expand(f32::from(PAGE_PAD_X)),
                    rect.top(),
                    Stroke::new(1.0, theme::LINE_HAIR),
                );
                ui.horizontal_centered(|ui| {
                    ui.label(
                        RichText::new(format!("Step {} of 3", step.index() + 1))
                            .font(theme::sans(13.0))
                            .color(theme::TEXT_DIM),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        let primary = if step == Step::Display {
                            "Save and connect"
                        } else {
                            "Continue"
                        };
                        if widgets::primary_button(ui, primary, BUTTON_H).clicked() {
                            nav = Nav::Continue;
                        }
                        let (label, back) = if step == Step::Destination {
                            ("Cancel", Nav::Cancel)
                        } else {
                            ("Back", Nav::Back)
                        };
                        if widgets::secondary_button(ui, label, BUTTON_H).clicked() {
                            nav = back;
                        }
                    });
                });
            });
        nav
    }

    // --- body ---------------------------------------------------------------------------

    fn body(&mut self, ui: &mut Ui) {
        let (top, gap) = match self.step {
            Step::Display => (36, 22.0),
            Step::SignIn => (40, 24.0),
            Step::Destination => (40, 26.0),
        };
        Frame::new()
            .inner_margin(Margin {
                left: PAGE_PAD_X,
                right: PAGE_PAD_X,
                top,
                bottom: top,
            })
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = gap;
                match self.step {
                    Step::Destination => self.destination_body(ui),
                    Step::SignIn => self.sign_in_body(ui),
                    Step::Display => self.display_body(ui),
                }
            });
    }

    /// Step 1 — host, display name, port, and the terminal-equivalent card.
    fn destination_body(&mut self, ui: &mut Ui) {
        let error = self.error;
        let focus = std::mem::take(&mut self.focus_pending);
        labelled_field(
            ui,
            &FieldSpec {
                id: "wizard-host",
                label: "Host or address",
                suffix: None,
                height: FIELD_H,
                focus_size: 15.0,
                idle_size: 14.0,
                hint: None,
                placeholder: None,
                field: FormField::Host,
            },
            &mut self.draft.host,
            error,
            focus,
            None,
        );
        let name_hint = self.draft.derived_name();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 20.0;
            let total = ui.available_width();
            let name_w = (total - 20.0 - PORT_COL_W).max(0.0);
            ui.allocate_ui_with_layout(
                vec2(name_w, FIELD_SMALL_H + 40.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    labelled_field(
                        ui,
                        &FieldSpec {
                            id: "wizard-display-name",
                            label: "Display name",
                            suffix: None,
                            height: FIELD_SMALL_H,
                            focus_size: 13.0,
                            idle_size: 13.0,
                            hint: Some("Defaults to the host name"),
                            placeholder: Some(&name_hint),
                            field: FormField::DisplayName,
                        },
                        &mut self.draft.display_name,
                        error,
                        false,
                        None,
                    );
                },
            );
            ui.allocate_ui_with_layout(
                vec2(PORT_COL_W, FIELD_SMALL_H + 40.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    labelled_field(
                        ui,
                        &FieldSpec {
                            id: "wizard-port",
                            label: "Port",
                            suffix: None,
                            height: FIELD_SMALL_H,
                            focus_size: 13.0,
                            idle_size: 13.0,
                            hint: Some("Default"),
                            placeholder: Some("3389"),
                            field: FormField::Port,
                        },
                        &mut self.draft.port,
                        error,
                        false,
                        None,
                    );
                },
            );
        });

        let preview = self.draft.command_preview(false);
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            terminal_card(ui, &preview);
        });
    }

    /// Step 2 — username, domain, password, the save checkbox and the TOFU notice.
    fn sign_in_body(&mut self, ui: &mut Ui) {
        let error = self.error;
        let focus = std::mem::take(&mut self.focus_pending);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 20.0;
            let total = ui.available_width();
            let user_w = (total - 20.0 - DOMAIN_COL_W).max(0.0);
            ui.allocate_ui_with_layout(
                vec2(user_w, FIELD_H + 40.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    labelled_field(
                        ui,
                        &FieldSpec {
                            id: "wizard-username",
                            label: "Username",
                            suffix: None,
                            height: FIELD_H,
                            focus_size: 15.0,
                            idle_size: 14.0,
                            hint: Some("From Settings › Defaults"),
                            placeholder: None,
                            field: FormField::Username,
                        },
                        &mut self.draft.username,
                        error,
                        focus,
                        None,
                    );
                },
            );
            ui.allocate_ui_with_layout(
                vec2(DOMAIN_COL_W, FIELD_H + 40.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    labelled_field(
                        ui,
                        &FieldSpec {
                            id: "wizard-domain",
                            label: "Domain",
                            suffix: Some("optional"),
                            height: FIELD_H,
                            focus_size: 15.0,
                            idle_size: 14.0,
                            hint: None,
                            placeholder: None,
                            field: FormField::SessionMode, // never an error target
                        },
                        &mut self.draft.domain,
                        error,
                        false,
                        None,
                    );
                },
            );
        });

        let mut show = self.show_password;
        labelled_field(
            ui,
            &FieldSpec {
                id: "wizard-password",
                label: "Password",
                suffix: None,
                height: FIELD_H,
                focus_size: 15.0,
                idle_size: 15.0,
                hint: None,
                placeholder: None,
                field: FormField::Password,
            },
            &mut self.password,
            error,
            false,
            Some(&mut show),
        );
        self.show_password = show;

        checkbox_row(
            ui,
            &mut self.save_password,
            &[Span::plain(
                "Save the password to the system credential store",
            )],
            Some("Unchecked, mdrdp asks for it each time and keeps it only for that session."),
        );

        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            warning_card(
                ui,
                "First connection to this host: mdrdp will show the server certificate \
                 fingerprint and pin it on your approval. Pinned hosts are listed under \
                 Settings › Certificate trust.",
            );
        });
    }

    /// Step 3 — the two display cards, the summary, and the save-as-favourite checkbox.
    fn display_body(&mut self, ui: &mut Ui) {
        self.focus_pending = false;
        let error = self.error;
        let monitor = ui
            .ctx()
            .input(|i| i.viewport().monitor_size)
            .map(|s| (s.x.round() as u32, s.y.round() as u32));

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            let card_w = ((ui.available_width() - 16.0) / 2.0).max(0.0);
            if self.fullscreen_card(ui, card_w, monitor).clicked() {
                self.draft.fullscreen = true;
                self.error = None;
            }
            if self.explicit_card(ui, card_w, error).clicked() {
                self.draft.fullscreen = false;
                self.error = None;
            }
        });

        if let Some(e) = error
            && matches!(
                e,
                FormError::InvalidWidth
                    | FormError::WidthOutOfRange
                    | FormError::InvalidHeight
                    | FormError::HeightOutOfRange
            )
        {
            ui.label(
                RichText::new(e.to_string())
                    .font(theme::sans(12.0))
                    .color(theme::DANGER),
            );
        }

        self.summary_card(ui, monitor);

        let name = self.draft.name();
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            checkbox_row(
                ui,
                &mut self.save_favourite,
                &[Span::plain("Save as a favourite named "), Span::code(&name)],
                None,
            );
        });
    }

    fn fullscreen_card(&self, ui: &mut Ui, width: f32, monitor: Option<(u32, u32)>) -> Response {
        let selected = self.draft.fullscreen;
        let (rect, response) = ui.allocate_exact_size(vec2(width, CARD_H), Sense::click());
        let strip = card_shell(ui, rect, selected, "Fullscreen");
        ui.painter().rect_filled(
            Rect::from_center_size(strip.center(), vec2(150.0, 56.0)),
            CornerRadius::same(theme::radius::CACHE_CELL),
            theme::ACCENT_FILL.gamma_multiply(0.55),
        );
        let sub = match monitor {
            Some((w, h)) => {
                format!("Borderless, {w}×{h} remote. Geometry is restored if the display sleeps.")
            }
            None => "Borderless at the display's native resolution. Geometry is restored if the \
                     display sleeps."
                .to_owned(),
        };
        card_sub(ui, rect, strip, &sub, selected);
        response
    }

    fn explicit_card(&mut self, ui: &mut Ui, width: f32, error: Option<FormError>) -> Response {
        let selected = !self.draft.fullscreen;
        let (rect, response) = ui.allocate_exact_size(vec2(width, CARD_H), Sense::click());
        let strip = card_shell(ui, rect, selected, "Explicit size");
        // Two 34px numeric inputs either side of a mono ×.
        let box_w = 64.0;
        let cy = strip.center().y;
        let left = Rect::from_center_size(
            pos2(strip.center().x - box_w / 2.0 - 12.0, cy),
            vec2(box_w, 34.0),
        );
        let right = Rect::from_center_size(
            pos2(strip.center().x + box_w / 2.0 + 12.0, cy),
            vec2(box_w, 34.0),
        );
        ui.painter().text(
            pos2(strip.center().x, cy),
            Align2::CENTER_CENTER,
            "×",
            theme::mono(12.0),
            theme::TEXT_DIM,
        );
        let width_bad = matches!(
            error,
            Some(FormError::InvalidWidth | FormError::WidthOutOfRange)
        );
        let height_bad = matches!(
            error,
            Some(FormError::InvalidHeight | FormError::HeightOutOfRange)
        );
        numeric_box(
            ui,
            left,
            "wizard-width",
            &mut self.draft.width,
            selected,
            width_bad,
        );
        numeric_box(
            ui,
            right,
            "wizard-height",
            &mut self.draft.height,
            selected,
            height_bad,
        );
        card_sub(
            ui,
            rect,
            strip,
            "A windowed desktop at a size you pick.",
            selected,
        );
        response
    }

    /// "READY TO CONNECT": the four-column summary over the exact command line.
    fn summary_card(&self, ui: &mut Ui, monitor: Option<(u32, u32)>) {
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), SUMMARY_H), Sense::hover());
        ui.painter().rect(
            rect,
            CornerRadius::same(theme::radius::CARD),
            theme::BG_CHROME,
            Stroke::new(1.0, theme::LINE_HAIR),
            StrokeKind::Inside,
        );
        let left = rect.min.x + 20.0;
        let mut y = rect.min.y + 18.0;
        ui.painter().text(
            pos2(left, y),
            Align2::LEFT_TOP,
            "READY TO CONNECT",
            theme::sans_medium(11.0),
            theme::TEXT_DIM,
        );
        y += 14.0 + 14.0;

        let size = self.draft.size().unwrap_or(WindowSize::Fullscreen);
        let columns = [
            ("HOST", self.draft.host_and_port(), theme::TEXT_PRIMARY),
            (
                "ACCOUNT",
                self.draft.username.trim().to_owned(),
                theme::TEXT_PRIMARY,
            ),
            (
                "DISPLAY",
                display_summary(size, monitor),
                theme::TEXT_PRIMARY,
            ),
            (
                "PASSWORD",
                if self.save_password {
                    "keychain".to_owned()
                } else {
                    "prompt".to_owned()
                },
                if self.save_password {
                    theme::ACCENT
                } else {
                    theme::TEXT_PRIMARY
                },
            ),
        ];
        let mut x = left;
        for (label, value, colour) in &columns {
            ui.painter().text(
                pos2(x, y),
                Align2::LEFT_TOP,
                *label,
                theme::sans(11.0),
                theme::TEXT_DIM,
            );
            ui.painter().text(
                pos2(x, y + 16.0),
                Align2::LEFT_TOP,
                value,
                theme::mono(13.0),
                *colour,
            );
            let w = text_width(ui, label, theme::sans(11.0)).max(text_width(
                ui,
                value,
                theme::mono(13.0),
            ));
            x += w + 44.0;
        }
        y += 33.0 + 14.0;
        ui.painter().hline(
            left..=(rect.max.x - 20.0),
            y,
            Stroke::new(1.0, theme::LINE_HAIR),
        );
        y += 14.0;
        command_line(ui, pos2(left, y), &self.draft.command_preview(true));
    }
}

impl Drop for Wizard {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

/// What the footer (or a key) asked for this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Nav {
    None,
    Cancel,
    Back,
    Continue,
}

// --- pure logic ---------------------------------------------------------------------------

impl Draft {
    /// The port, defaulting to 3389 when the field is left blank.
    fn port(&self) -> Result<u16, FormError> {
        if self.port.trim().is_empty() {
            return Ok(DEFAULT_PORT);
        }
        parse_number(&self.port, FormField::Port)
    }

    /// `host` or `host:port`, as the rail chip and the summary show it.
    fn host_and_port(&self) -> String {
        let host = self.host.trim();
        match self.port() {
            Ok(DEFAULT_PORT) | Err(_) => host.to_owned(),
            Ok(port) => format!("{host}:{port}"),
        }
    }

    /// The keychain account key: `user@host:port`.
    fn account_key(&self) -> String {
        format!(
            "{}@{}:{}",
            self.username.trim(),
            self.host.trim(),
            self.port().unwrap_or(DEFAULT_PORT)
        )
    }

    /// The display name the wizard would derive from the host alone.
    fn derived_name(&self) -> String {
        derived_name(self.host.trim())
    }

    /// The favourite's name: what was typed, else the derived one.
    fn name(&self) -> String {
        let typed = self.display_name.trim();
        if typed.is_empty() {
            self.derived_name()
        } else {
            typed.to_owned()
        }
    }

    /// The chosen session size.
    fn size(&self) -> Result<WindowSize, FormError> {
        if self.fullscreen {
            return Ok(WindowSize::Fullscreen);
        }
        Ok(WindowSize::Explicit {
            width: parse_number(&self.width, FormField::Width)?,
            height: parse_number(&self.height, FormField::Height)?,
        })
    }

    /// Step 1: a host is required; the port must parse if given.
    fn validate_destination(&self) -> Result<(), FormError> {
        if self.host.trim().is_empty() {
            return Err(FormError::BlankHost);
        }
        self.port()?;
        Ok(())
    }

    /// Step 2: a username is required; a password is required only if it is going to
    /// the credential store (unticked means mdrdp asks each time).
    fn validate_sign_in(&self, password_typed: bool, save_password: bool) -> Result<(), FormError> {
        if self.username.trim().is_empty() {
            return Err(FormError::BlankUsername);
        }
        if save_password && !password_typed {
            return Err(FormError::BlankPassword);
        }
        Ok(())
    }

    /// Step 3: an explicit size must parse.
    fn validate_display(&self) -> Result<(), FormError> {
        self.size()?;
        Ok(())
    }

    /// The validated favourite. `save_password` decides whether it carries a keychain
    /// account key at all — `None` means "prompt", which is exactly what an unticked
    /// checkbox asked for.
    fn favourite(&self, save_password: bool) -> Result<Favourite, FormError> {
        let host = self.host.trim();
        if host.is_empty() {
            return Err(FormError::BlankHost);
        }
        let port = self.port()?;
        let username = self.username.trim();
        if username.is_empty() {
            return Err(FormError::BlankUsername);
        }
        let window_size = self.size()?;
        let domain = {
            let d = self.domain.trim();
            if d.is_empty() {
                None
            } else {
                Some(d.to_owned())
            }
        };
        Ok(Favourite {
            name: self.name(),
            host: host.to_owned(),
            port,
            username: Some(username.to_owned()),
            domain,
            window_size,
            keychain_account: if save_password {
                Some(self.account_key())
            } else {
                None
            },
            last_used: None,
        })
    }

    /// The equivalent command line. `full` adds the account and size flags — step 1's
    /// card shows only what is known by then.
    fn command_preview(&self, full: bool) -> String {
        let host = self.host.trim();
        let mut out = String::from("mdrdp ");
        out.push_str(if host.is_empty() { "<host>" } else { host });
        if let Ok(port) = self.port()
            && port != DEFAULT_PORT
        {
            out.push_str(&format!(" --port {port}"));
        }
        if full {
            let user = self.username.trim();
            if !user.is_empty() {
                out.push_str(&format!(" --user {user}"));
            }
            let domain = self.domain.trim();
            if !domain.is_empty() {
                out.push_str(&format!(" --domain {domain}"));
            }
            if let Ok(WindowSize::Explicit { width, height }) = self.size() {
                out.push_str(&format!(" --size {width}x{height}"));
            }
        }
        out
    }
}

/// `FormError`'s numeric wording, reused rather than restated: the same parse and the
/// same range check `ui/form.rs` applies.
fn parse_number(text: &str, field: FormField) -> Result<u16, FormError> {
    let invalid = match field {
        FormField::Port => FormError::InvalidPort,
        FormField::Width => FormError::InvalidWidth,
        _ => FormError::InvalidHeight,
    };
    let out_of_range = match field {
        FormField::Port => FormError::PortOutOfRange,
        FormField::Width => FormError::WidthOutOfRange,
        _ => FormError::HeightOutOfRange,
    };
    let parsed = text.trim().parse::<u32>().map_err(|_| invalid)?;
    if !(1..=u32::from(u16::MAX)).contains(&parsed) {
        return Err(out_of_range);
    }
    Ok(parsed as u16)
}

/// The display name a host implies: the first DNS label, capitalised. An address is
/// left alone — "10" is not a name.
fn derived_name(host: &str) -> String {
    if host.is_empty() || host.parse::<std::net::IpAddr>().is_ok() {
        return host.to_owned();
    }
    let label = host.split('.').next().unwrap_or(host);
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The summary block's DISPLAY value.
fn display_summary(size: WindowSize, monitor: Option<(u32, u32)>) -> String {
    match size {
        WindowSize::Fullscreen => match monitor {
            Some((w, h)) => format!("fullscreen {w}×{h}"),
            None => "fullscreen".to_owned(),
        },
        WindowSize::Explicit { width, height } => format!("{width}×{height}"),
    }
}

// --- drawing helpers -----------------------------------------------------------------------

/// One run of the mixed prose/mono sub-lines.
struct Span<'a> {
    text: &'a str,
    mono: bool,
    /// `text.primary` rather than the muted sub-line colour.
    prominent: bool,
}

impl<'a> Span<'a> {
    fn body(text: &'a str) -> Self {
        Span {
            text,
            mono: false,
            prominent: false,
        }
    }
    fn code(text: &'a str) -> Self {
        Span {
            text,
            mono: true,
            prominent: false,
        }
    }
    fn plain(text: &'a str) -> Self {
        Span {
            text,
            mono: false,
            prominent: true,
        }
    }
}

/// A wrapped 13px sub-line built from mixed prose and mono runs.
fn prose(ui: &mut Ui, spans: &[Span<'_>]) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for span in spans {
            let (font, colour) = if span.mono {
                (theme::mono(13.0), theme::TEXT_SECONDARY)
            } else if span.prominent {
                (theme::sans(13.0), theme::TEXT_PRIMARY)
            } else {
                (theme::sans(13.0), theme::TEXT_MUTED)
            };
            ui.label(RichText::new(span.text).font(font).color(colour));
        }
    });
}

/// A labelled input: uppercase label, the box, then the hint or the error under it.
struct FieldSpec<'a> {
    id: &'a str,
    label: &'a str,
    /// Rendered after the label, lowercase and untracked (the `DOMAIN optional` case).
    suffix: Option<&'a str>,
    height: f32,
    focus_size: f32,
    idle_size: f32,
    hint: Option<&'a str>,
    /// Shown in `text.dim` while the field is empty — the derived display name, the
    /// default port.
    placeholder: Option<&'a str>,
    /// The error variant that belongs under this field.
    field: FormField,
}

fn labelled_field(
    ui: &mut Ui,
    spec: &FieldSpec<'_>,
    value: &mut String,
    error: Option<FormError>,
    request_focus: bool,
    reveal: Option<&mut bool>,
) -> Response {
    let mine = error.filter(|e| e.field() == spec.field);
    ui.spacing_mut().item_spacing.y = 8.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        ui.label(
            RichText::new(spec.label.to_uppercase())
                .font(theme::sans_medium(11.0))
                .color(theme::TEXT_MUTED)
                .extra_letter_spacing(11.0 * 0.09),
        );
        if let Some(suffix) = spec.suffix {
            ui.label(
                RichText::new(suffix)
                    .font(theme::sans(11.0))
                    .color(theme::TEXT_DIM),
            );
        }
    });

    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, spec.height), Sense::hover());
    let id = Id::new(spec.id);
    let focused = ui.ctx().memory(|m| m.has_focus(id));
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(theme::radius::INPUT),
        theme::BG_CHROME,
    );

    // "Show" sits inside the right padding and steals that much width from the value.
    let mut reveal_w = 0.0;
    let showing = reveal.as_ref().map(|r| **r).unwrap_or(false);
    if let Some(flag) = reveal {
        let label = if showing { "Hide" } else { "Show" };
        let w = text_width(ui, label, theme::sans(12.0));
        let hit = Rect::from_min_size(
            pos2(rect.max.x - FIELD_PAD_X - w, rect.min.y),
            vec2(w, rect.height()),
        );
        if ui
            .interact(hit, Id::new((spec.id, "reveal")), Sense::click())
            .clicked()
        {
            *flag = !*flag;
        }
        ui.painter().text(
            pos2(rect.max.x - FIELD_PAD_X, rect.center().y),
            Align2::RIGHT_CENTER,
            label,
            theme::sans(12.0),
            theme::TEXT_MUTED,
        );
        reveal_w = w + 12.0;
    }

    let size = if focused {
        spec.focus_size
    } else {
        spec.idle_size
    };
    let font = theme::mono(size);
    let row = ui.fonts_mut(|f| f.row_height(&font));
    let inner = Rect::from_min_size(
        pos2(
            rect.min.x + FIELD_PAD_X,
            (rect.center().y - row / 2.0).round(),
        ),
        vec2((rect.width() - FIELD_PAD_X * 2.0 - reveal_w).max(0.0), row),
    );
    let mut edit = TextEdit::singleline(value)
        .id(id)
        .font(font)
        .margin(Margin::ZERO)
        .frame(Frame::NONE)
        .text_color(theme::TEXT_PRIMARY)
        .desired_width(inner.width());
    if let Some(placeholder) = spec.placeholder {
        edit = edit.hint_text(
            RichText::new(placeholder)
                .font(theme::mono(size))
                .color(theme::TEXT_DIM),
        );
    }
    if reveal_w > 0.0 {
        edit = edit.password(!showing);
    }
    let response = ui.place(inner, edit);
    if request_focus {
        response.request_focus();
    }
    let border = if focused {
        theme::ACCENT
    } else if mine.is_some() {
        theme::DANGER
    } else {
        theme::LINE_SUBTLE
    };
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(theme::radius::INPUT),
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );

    match (mine, spec.hint) {
        (Some(error), _) => {
            ui.label(
                RichText::new(error.to_string())
                    .font(theme::sans(12.0))
                    .color(theme::DANGER),
            );
        }
        (None, Some(hint)) => {
            ui.label(
                RichText::new(hint)
                    .font(theme::sans(12.0))
                    .color(theme::TEXT_DIM),
            );
        }
        (None, None) => {}
    }
    response
}

/// One of the two 34px numeric boxes inside the explicit-size card's preview strip.
fn numeric_box(ui: &mut Ui, rect: Rect, id: &str, value: &mut String, active: bool, bad: bool) {
    let id = Id::new(id);
    let focused = ui.ctx().memory(|m| m.has_focus(id));
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(theme::radius::MENU_ITEM),
        theme::BG_CHROME,
    );
    let font = theme::mono(13.0);
    let row = ui.fonts_mut(|f| f.row_height(&font));
    let inner = Rect::from_min_size(
        pos2(rect.min.x + 10.0, (rect.center().y - row / 2.0).round()),
        vec2(rect.width() - 20.0, row),
    );
    ui.place(
        inner,
        TextEdit::singleline(value)
            .id(id)
            .font(font)
            .margin(Margin::ZERO)
            .frame(Frame::NONE)
            .text_color(if active {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_DIM
            })
            .desired_width(inner.width()),
    );
    let border = if bad {
        theme::DANGER
    } else if focused {
        theme::ACCENT
    } else {
        theme::LINE_SUBTLE
    };
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(theme::radius::MENU_ITEM),
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
}

/// Paint a display-choice card's frame, title, radio and preview strip. Returns the
/// strip's rect so the caller can fill it.
fn card_shell(ui: &mut Ui, rect: Rect, selected: bool, title: &str) -> Rect {
    ui.painter().rect(
        rect,
        CornerRadius::same(theme::radius::CARD),
        theme::BG_RAISED,
        Stroke::new(
            1.0,
            if selected {
                theme::ACCENT
            } else {
                theme::LINE_SUBTLE
            },
        ),
        StrokeKind::Inside,
    );
    let pad = 18.0;
    ui.painter().text(
        pos2(rect.min.x + pad, rect.min.y + pad),
        Align2::LEFT_TOP,
        title,
        theme::sans_semibold(14.0),
        if selected {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_SECONDARY
        },
    );
    let radio = pos2(rect.max.x - pad - 8.0, rect.min.y + pad + 8.0);
    if selected {
        ui.painter().circle_filled(radio, 8.0, theme::ACCENT);
        ui.painter().text(
            radio,
            Align2::CENTER_CENTER,
            "✓",
            theme::sans_semibold(11.0),
            theme::ACCENT_ON,
        );
    } else {
        ui.painter()
            .circle_stroke(radio, 8.0, Stroke::new(1.0, theme::LINE_STRONG));
    }
    let strip = Rect::from_min_size(
        pos2(rect.min.x + pad, rect.min.y + pad + 17.0 + 10.0),
        vec2(rect.width() - pad * 2.0, 76.0),
    );
    ui.painter().rect(
        strip,
        CornerRadius::same(theme::radius::MENU_ITEM),
        theme::BG_CHROME,
        Stroke::new(1.0, theme::LINE_HAIR),
        StrokeKind::Inside,
    );
    strip
}

/// A display card's 12px/19px sub-copy, under its preview strip.
fn card_sub(ui: &mut Ui, card: Rect, strip: Rect, text: &str, selected: bool) {
    let area = Rect::from_min_max(
        pos2(card.min.x + 18.0, strip.max.y + 10.0),
        pos2(card.max.x - 18.0, card.max.y - 18.0),
    );
    let mut child = ui.new_child(UiBuilder::new().max_rect(area));
    child.label(
        RichText::new(text)
            .font(theme::sans(12.0))
            .color(if selected {
                theme::TEXT_MUTED
            } else {
                theme::TEXT_DIM
            })
            .line_height(Some(19.0)),
    );
}

/// The step-1 card: "SAME THING FROM A TERMINAL" over the equivalent invocation.
fn terminal_card(ui: &mut Ui, command: &str) {
    let height = 16.0 + 14.0 + 8.0 + 17.0 + 16.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(theme::radius::CARD),
        theme::BG_CHROME,
        Stroke::new(1.0, theme::LINE_HAIR),
        StrokeKind::Inside,
    );
    ui.painter().text(
        pos2(rect.min.x + 18.0, rect.min.y + 16.0),
        Align2::LEFT_TOP,
        "SAME THING FROM A TERMINAL",
        theme::sans_medium(11.0),
        theme::TEXT_DIM,
    );
    command_line(
        ui,
        pos2(rect.min.x + 18.0, rect.min.y + 16.0 + 14.0 + 8.0),
        command,
    );
}

/// `$ mdrdp …` with the prompt in `accent` and the command in `text.secondary`.
fn command_line(ui: &mut Ui, at: egui::Pos2, command: &str) {
    let prompt = "$ ";
    ui.painter().text(
        at,
        Align2::LEFT_TOP,
        prompt,
        theme::mono(13.0),
        theme::ACCENT,
    );
    let w = text_width(ui, prompt, theme::mono(13.0));
    ui.painter().text(
        pos2(at.x + w, at.y),
        Align2::LEFT_TOP,
        command,
        theme::mono(13.0),
        theme::TEXT_SECONDARY,
    );
}

/// The step-2 first-connection notice.
fn warning_card(ui: &mut Ui, body: &str) {
    let width = ui.available_width();
    let height = 14.0 + 60.0 + 14.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(theme::radius::CARD),
        theme::WARN_CARD_BG,
        Stroke::new(1.0, theme::WARN_CARD_BORDER),
        StrokeKind::Inside,
    );
    ui.painter().text(
        pos2(rect.min.x + 16.0, rect.min.y + 14.0),
        Align2::LEFT_TOP,
        "!",
        theme::mono(13.0),
        theme::DANGER,
    );
    let area = Rect::from_min_max(
        pos2(rect.min.x + 16.0 + 12.0 + 12.0, rect.min.y + 14.0),
        pos2(rect.max.x - 16.0, rect.max.y - 14.0),
    );
    let mut child = ui.new_child(UiBuilder::new().max_rect(area));
    child.label(
        RichText::new(body)
            .font(theme::sans(12.0))
            .color(theme::DANGER_BODY)
            .line_height(Some(20.0)),
    );
}

/// A 16px checkbox with its label (and optional sub-line) to the right.
fn checkbox_row(ui: &mut Ui, checked: &mut bool, label: &[Span<'_>], sub: Option<&str>) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
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
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            prose(ui, label);
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

fn text_width(ui: &Ui, text: &str, font: egui::FontId) -> f32 {
    ui.fonts_mut(|f| {
        f.layout_no_wrap(text.to_owned(), font, Color32::WHITE)
            .size()
            .x
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A draft with a distinct value in every field, so a construction bug that copies
    /// one field into another cannot pass.
    fn draft() -> Draft {
        Draft {
            host: "quench".to_owned(),
            display_name: "Bench box".to_owned(),
            port: "3391".to_owned(),
            username: "ano".to_owned(),
            domain: "CORP".to_owned(),
            fullscreen: false,
            width: "1600".to_owned(),
            height: "1000".to_owned(),
        }
    }

    #[test]
    fn a_blank_host_reuses_the_form_error_wording() {
        let mut d = draft();
        d.host = "   ".to_owned();
        let err = d.validate_destination().unwrap_err();
        assert_eq!(err, FormError::BlankHost);
        assert_eq!(err.to_string(), "host must not be blank");
    }

    #[test]
    fn a_blank_port_means_the_default_and_a_bad_one_carries_the_form_wording() {
        let mut d = draft();
        d.port = String::new();
        assert_eq!(d.port().unwrap(), DEFAULT_PORT);
        assert_eq!(d.port().unwrap(), 3389);

        d.port = "not-a-port".to_owned();
        let err = d.validate_destination().unwrap_err();
        assert_eq!(err, FormError::InvalidPort);
        assert_eq!(
            err.to_string(),
            "port must be a whole number from 1 to 65535"
        );

        d.port = "70000".to_owned();
        assert_eq!(
            d.validate_destination().unwrap_err(),
            FormError::PortOutOfRange
        );
        d.port = "0".to_owned();
        assert_eq!(
            d.validate_destination().unwrap_err(),
            FormError::PortOutOfRange
        );
    }

    #[test]
    fn sign_in_needs_a_username_and_needs_a_password_only_when_saving_it() {
        let mut d = draft();
        d.username = " ".to_owned();
        let err = d.validate_sign_in(true, true).unwrap_err();
        assert_eq!(err, FormError::BlankUsername);
        assert_eq!(err.to_string(), "username must not be blank");

        let d = draft();
        // Ticked "save to the credential store" with nothing typed.
        let err = d.validate_sign_in(false, true).unwrap_err();
        assert_eq!(err, FormError::BlankPassword);
        assert_eq!(err.to_string(), "password must not be blank");
        // Unticked: mdrdp asks each time, so an empty field is fine.
        assert!(d.validate_sign_in(false, false).is_ok());
        assert!(d.validate_sign_in(true, true).is_ok());
    }

    #[test]
    fn an_explicit_size_must_parse_and_says_which_dimension_failed() {
        let mut d = draft();
        d.width = String::new();
        assert_eq!(d.validate_display().unwrap_err(), FormError::InvalidWidth);
        d.width = "0".to_owned();
        assert_eq!(
            d.validate_display().unwrap_err(),
            FormError::WidthOutOfRange
        );
        d.width = "1600".to_owned();
        d.height = "x".to_owned();
        let err = d.validate_display().unwrap_err();
        assert_eq!(err, FormError::InvalidHeight);
        assert_eq!(
            err.to_string(),
            "height must be a whole number from 1 to 65535"
        );
        d.height = "99999".to_owned();
        assert_eq!(
            d.validate_display().unwrap_err(),
            FormError::HeightOutOfRange
        );
        // Fullscreen never consults the size fields.
        d.fullscreen = true;
        assert!(d.validate_display().is_ok());
    }

    #[test]
    fn the_favourite_carries_every_typed_field() {
        let f = draft().favourite(true).expect("valid draft");
        assert_eq!(f.name, "Bench box");
        assert_eq!(f.host, "quench");
        assert_eq!(f.port, 3391);
        assert_eq!(f.username.as_deref(), Some("ano"));
        assert_eq!(f.domain.as_deref(), Some("CORP"));
        assert_eq!(
            f.window_size,
            WindowSize::Explicit {
                width: 1600,
                height: 1000
            }
        );
        assert_eq!(f.keychain_account.as_deref(), Some("ano@quench:3391"));
        assert_eq!(f.last_used, None);
    }

    #[test]
    fn an_unticked_credential_store_leaves_the_favourite_prompting() {
        let f = draft().favourite(false).expect("valid draft");
        assert_eq!(f.keychain_account, None);
    }

    #[test]
    fn a_blank_display_name_falls_back_to_the_host() {
        let mut d = draft();
        d.display_name = "  ".to_owned();
        assert_eq!(d.name(), "Quench");
        d.host = "temper.lan.example".to_owned();
        assert_eq!(d.name(), "Temper");
        d.host = "10.0.4.19".to_owned();
        assert_eq!(d.name(), "10.0.4.19");
    }

    #[test]
    fn the_command_preview_names_only_what_differs_from_the_defaults() {
        // Step 1 knows the host and port only.
        let mut d = draft();
        assert_eq!(d.command_preview(false), "mdrdp quench --port 3391");
        assert_eq!(
            d.command_preview(true),
            "mdrdp quench --port 3391 --user ano --domain CORP --size 1600x1000"
        );

        // The mock's own case: default port, fullscreen, no domain.
        d.port = String::new();
        d.domain = String::new();
        d.fullscreen = true;
        assert_eq!(d.command_preview(true), "mdrdp quench --user ano");
        assert_eq!(d.command_preview(false), "mdrdp quench");

        // Nothing typed yet: the card still teaches the shape.
        let empty = Draft::default();
        assert_eq!(empty.command_preview(false), "mdrdp <host>");
    }

    #[test]
    fn the_rail_and_summary_render_the_host_the_way_the_mock_does() {
        let mut d = draft();
        assert_eq!(d.host_and_port(), "quench:3391");
        assert_eq!(d.account_key(), "ano@quench:3391");
        d.port = String::new();
        assert_eq!(d.host_and_port(), "quench");
        assert_eq!(d.account_key(), "ano@quench:3389");
    }

    #[test]
    fn the_display_summary_names_the_monitor_only_when_fullscreen() {
        assert_eq!(
            display_summary(WindowSize::Fullscreen, Some((1920, 1080))),
            "fullscreen 1920×1080"
        );
        assert_eq!(display_summary(WindowSize::Fullscreen, None), "fullscreen");
        assert_eq!(
            display_summary(
                WindowSize::Explicit {
                    width: 1600,
                    height: 1000
                },
                Some((1920, 1080))
            ),
            "1600×1000"
        );
    }

    #[test]
    fn the_typed_password_never_prints_itself() {
        let p = TypedPassword("hunter2-correct-horse".to_owned());
        assert_eq!(format!("{p:?}"), "TypedPassword(<redacted>)");
        assert_eq!(p.expose(), "hunter2-correct-horse");
    }

    #[test]
    fn a_password_is_handed_back_only_when_it_is_going_to_the_store() {
        let mut w = Wizard::new(Some("ano".to_owned()));
        assert!(w.take_password().is_none(), "nothing typed");

        w.password = "typed".to_owned();
        w.save_password = false;
        assert!(
            w.take_password().is_none(),
            "unticked means it is not stored"
        );
        assert!(w.password.is_empty(), "and the buffer is cleared");

        w.password = "typed".to_owned();
        w.save_password = true;
        assert_eq!(w.take_password().expect("stored").expose(), "typed");
        assert!(w.password.is_empty());
    }

    #[test]
    fn a_new_wizard_starts_on_step_one_with_the_default_username() {
        let w = Wizard::new(Some("ano".to_owned()));
        assert_eq!(w.step, Step::Destination);
        assert_eq!(w.draft.username, "ano");
        assert!(w.draft.fullscreen);
        assert!(w.save_password);
        assert!(w.save_favourite);
    }

    #[test]
    fn advancing_walks_the_steps_and_stops_on_a_bad_one() {
        let mut w = Wizard::new(None);
        // Step 1 with no host must not move.
        assert!(matches!(w.advance(), WizardOutcome::None));
        assert_eq!(w.step, Step::Destination);
        assert_eq!(w.error, Some(FormError::BlankHost));

        w.draft.host = "quench".to_owned();
        assert!(matches!(w.advance(), WizardOutcome::None));
        assert_eq!(w.step, Step::SignIn);
        assert_eq!(w.error, None);

        // Step 2: no username.
        assert!(matches!(w.advance(), WizardOutcome::None));
        assert_eq!(w.step, Step::SignIn);
        assert_eq!(w.error, Some(FormError::BlankUsername));

        w.draft.username = "ano".to_owned();
        w.password = "typed".to_owned();
        assert!(matches!(w.advance(), WizardOutcome::None));
        assert_eq!(w.step, Step::Display);

        match w.advance() {
            WizardOutcome::Finished {
                favourite,
                save_favourite,
                password,
                save_password,
            } => {
                assert_eq!(favourite.host, "quench");
                assert_eq!(favourite.port, DEFAULT_PORT);
                assert_eq!(favourite.window_size, WindowSize::Fullscreen);
                assert!(save_favourite);
                assert!(save_password);
                assert_eq!(password.expect("typed").expose(), "typed");
            }
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn back_walks_the_steps_the_other_way_and_clears_the_error() {
        let mut w = Wizard::new(None);
        w.step = Step::Display;
        w.error = Some(FormError::InvalidWidth);
        w.back();
        assert_eq!(w.step, Step::SignIn);
        assert_eq!(w.error, None);
        w.back();
        assert_eq!(w.step, Step::Destination);
        w.back();
        assert_eq!(
            w.step,
            Step::Destination,
            "step 1 has nowhere to go back to"
        );
    }

    /// Run one real egui frame of the wizard, delivering `events` to it.
    ///
    /// `ui()` cannot be asserted on pixel by pixel, but it can be *run*: this lays out
    /// and tessellates the whole page, which is what catches a bad rect, a panel id
    /// clash, or a widget placed outside its parent.
    fn frame(w: &mut Wizard, events: Vec<egui::Event>) -> WizardOutcome {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let input = egui::RawInput {
            // The launcher's 900×700 window, less its 30px menu strip.
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 670.0))),
            events,
            ..Default::default()
        };
        let mut outcome = WizardOutcome::None;
        let output = ctx.run_ui(input, |ui| {
            outcome = w.ui(ui);
        });
        output.drop_without_applying_deltas();
        outcome
    }

    fn press(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn a_real_frame_draws_every_step_and_routes_enter_and_escape() {
        let mut w = Wizard::new(Some("ano".to_owned()));
        // Step 1 draws. Enter with no host holds the step and names the field.
        assert!(matches!(
            frame(&mut w, vec![press(egui::Key::Enter)]),
            WizardOutcome::None
        ));
        assert_eq!(w.step, Step::Destination);
        assert_eq!(w.error, Some(FormError::BlankHost));

        w.draft.host = "quench".to_owned();
        frame(&mut w, vec![press(egui::Key::Enter)]);
        assert_eq!(w.step, Step::SignIn);

        w.password = "typed".to_owned();
        frame(&mut w, vec![press(egui::Key::Enter)]);
        assert_eq!(w.step, Step::Display);

        // Step 3 draws, and Esc cancels from any step.
        assert!(matches!(
            frame(&mut w, vec![press(egui::Key::Escape)]),
            WizardOutcome::Cancelled
        ));
    }
}
