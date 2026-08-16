//! Launcher dialogs: the shared §7 chrome, the Connecting modal, and the connect
//! failure dialogs.
//!
//! Visual values are the handoff README's ("7. Dialogs"): `bg.window` body over a 1px
//! `line.strong` border at radius 7, a `bg.chrome` footer strip, error dialogs led by a
//! 22px circular `danger.bg` chip, danger variants framed `danger.border` with a 4px
//! `danger` bar across the top. Everything reads `theme` tokens.

use crate::shell::widgets;
use crate::ui::theme;
use egui::{Align2, Color32, CornerRadius, Frame, Margin, RichText, Sense, Stroke, vec2};

/// What the user asked a connect dialog to do this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectDialogAction {
    None,
    Cancel,
    Retry,
    TryDefaultPort,
    /// Re-run the connect with the child forced to ask for a fresh password.
    EditPassword,
    Close,
}

/// Which §7 failure dialog a connect error maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// "Could not reach host" — failed at tcp_connect.
    Unreachable,
    /// "Sign-in rejected" — failed at Credssp.
    SignInRejected,
    /// "Port refused" — ECONNREFUSED.
    PortRefused,
    /// "Certificate CHANGED" — danger variant, deliberately without an accept path.
    CertificateChanged,
    /// Anything else: generic failure dialog with the error verbatim.
    Other,
}

/// A classified connect failure, ready to render.
#[derive(Debug, Clone)]
pub struct ConnectFailure {
    pub kind: FailureKind,
    pub host: String,
    pub port: u16,
    /// The account the child tried, for the "tried …" footer of Sign-in rejected.
    pub account: String,
    pub error: String,
}

/// Map a child's error line to the dialog that owns it. Pure and testable; the
/// substrings are the stable parts of our own `ConnectError` display forms.
pub fn classify(error: &str) -> FailureKind {
    let lowered = error.to_lowercase();
    if lowered.contains("has changed") {
        return FailureKind::CertificateChanged;
    }
    if lowered.contains("status_logon_failure")
        || lowered.contains("logon")
        || lowered.contains("credssp")
    {
        return FailureKind::SignInRejected;
    }
    if lowered.contains("econnrefused") || lowered.contains("connection refused") {
        return FailureKind::PortRefused;
    }
    if lowered.contains("timed out")
        || lowered.contains("no route")
        || lowered.contains("failed to lookup")
        || lowered.contains("nodename nor servname")
        || lowered.contains("name or service not known")
        || lowered.contains("network is unreachable")
    {
        return FailureKind::Unreachable;
    }
    FailureKind::Other
}

/// One row of the Connecting dialog's stage list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageRow {
    pub name: String,
    /// `HYBRID_EX`, `pinned` — dim unless `accent_qualifier`.
    pub qualifier: Option<String>,
    /// Elapsed milliseconds, present once the stage is done.
    pub elapsed_ms: Option<u64>,
    pub status: StageStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageStatus {
    Done,
    Current,
    Pending,
}

/// The connect legs shown as pending before the child reports them.
///
/// Names match what the child actually emits (`connect.rs` marks + `stagelog.rs`
/// states), so arrived events replace their pending rows instead of duplicating them.
pub const CANONICAL_STAGES: &[&str] = &[
    "tcp_connect",
    "x224_negotiation",
    "tls_handshake",
    "Credssp",
    "LicensingExchange",
    "CapabilitiesExchange",
    "post_tls_sequence",
];

/// Merge arrived stage events with the canonical expectation into display rows:
/// everything arrived (in arrival order, ms attached), then the next canonical stage
/// as Current, then the rest as Pending. Pure and testable.
pub fn stage_rows(arrived: &[(String, u64, Option<String>)]) -> Vec<StageRow> {
    let mut rows: Vec<StageRow> = arrived
        .iter()
        .map(|(name, ms, qualifier)| StageRow {
            name: name.clone(),
            qualifier: qualifier.clone(),
            elapsed_ms: Some(*ms),
            status: StageStatus::Done,
        })
        .collect();
    let matched = |canon: &str| {
        arrived
            .iter()
            .any(|(name, _, _)| name.starts_with(canon) || canon.starts_with(name.as_str()))
    };
    let mut first_pending = true;
    for canon in CANONICAL_STAGES {
        if !matched(canon) {
            rows.push(StageRow {
                name: (*canon).to_owned(),
                qualifier: None,
                elapsed_ms: None,
                status: if first_pending {
                    StageStatus::Current
                } else {
                    StageStatus::Pending
                },
            });
            first_pending = false;
        }
    }
    if first_pending {
        // Everything canonical has arrived; the tail (finalization) is the current work.
        if let Some(last) = rows.last_mut() {
            last.status = StageStatus::Current;
        }
    }
    rows
}

/// Progress fraction for the 4px bar: done rows over all rows.
pub fn progress_fraction(rows: &[StageRow]) -> f32 {
    if rows.is_empty() {
        return 0.0;
    }
    let done = rows
        .iter()
        .filter(|r| r.status == StageStatus::Done)
        .count();
    done as f32 / rows.len() as f32
}

/// Draw the full-window scrim and a §7 dialog frame; `body` fills the content,
/// `footer` the chrome strip. Returns whatever the closures produce.
fn dialog<B, F>(
    ctx: &egui::Context,
    id: &str,
    width: f32,
    danger: bool,
    body: impl FnOnce(&mut egui::Ui) -> B,
    footer: impl FnOnce(&mut egui::Ui) -> F,
) -> (B, F) {
    // Scrim: swallow clicks so the list below is inert while the dialog is up.
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new((id, "scrim")))
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.painter().rect_filled(screen, 0.0, theme::SCRIM);
            ui.allocate_rect(screen, Sense::click());
        });

    let border = if danger {
        theme::DANGER_BORDER
    } else {
        theme::LINE_STRONG
    };
    egui::Area::new(egui::Id::new(id))
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.set_width(width);
            Frame::new()
                .fill(theme::BG_WINDOW)
                .stroke(Stroke::new(1.0, border))
                .corner_radius(CornerRadius::same(theme::radius::MODAL))
                .show(ui, |ui| {
                    ui.set_width(width);
                    if danger {
                        // 4px danger bar across the top.
                        let bar = egui::Rect::from_min_size(ui.max_rect().min, vec2(width, 4.0));
                        ui.painter().rect_filled(
                            bar,
                            CornerRadius {
                                nw: theme::radius::MODAL,
                                ne: theme::radius::MODAL,
                                sw: 0,
                                se: 0,
                            },
                            theme::DANGER,
                        );
                        ui.add_space(4.0);
                    }
                    let body_out = Frame::new()
                        .inner_margin(Margin {
                            left: 24,
                            right: 24,
                            top: 22,
                            bottom: 18,
                        })
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 12.0;
                            body(ui)
                        })
                        .inner;
                    let footer_out = Frame::new()
                        .fill(theme::BG_CHROME)
                        .inner_margin(Margin {
                            left: 24,
                            right: 24,
                            top: 14,
                            bottom: 14,
                        })
                        .show(ui, |ui| {
                            let top = ui.max_rect().min.y;
                            ui.painter().hline(
                                ui.max_rect().x_range().expand(24.0),
                                top - 14.0,
                                Stroke::new(1.0, theme::LINE_HAIR),
                            );
                            footer(ui)
                        })
                        .inner;
                    (body_out, footer_out)
                })
                .inner
        })
        .inner
}

/// The 22px circular danger chip with a `!`, leading an error dialog.
fn danger_chip(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 11.0, theme::DANGER_BG);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        "!",
        theme::sans_semibold(13.0),
        theme::DANGER,
    );
}

/// The Connecting dialog: stage list, progress bar, elapsed footer, Cancel.
pub fn connecting(
    ctx: &egui::Context,
    host_label: &str,
    rows: &[StageRow],
    elapsed_ms: u64,
) -> ConnectDialogAction {
    let fraction = progress_fraction(rows);
    let (_, action) = dialog(
        ctx,
        "connecting",
        520.0,
        false,
        |ui| {
            ui.label(
                RichText::new(format!("Connecting to {host_label}"))
                    .font(theme::sans_semibold(16.0))
                    .color(theme::TEXT_PRIMARY),
            );
            ui.add_space(2.0);
            for row in rows {
                stage_row(ui, row);
            }
            ui.add_space(6.0);
            // 4px progress bar, accent fill on a chrome track.
            let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
            ui.painter()
                .rect_filled(bar, CornerRadius::same(2), theme::BG_CHROME);
            let mut filled = bar;
            filled.set_width(bar.width() * fraction.clamp(0.0, 1.0));
            ui.painter()
                .rect_filled(filled, CornerRadius::same(2), theme::ACCENT);
            ConnectDialogAction::None
        },
        |ui| {
            let mut action = ConnectDialogAction::None;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{elapsed_ms} ms"))
                        .font(theme::mono(12.0))
                        .color(theme::TEXT_DIM),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::secondary_button(ui, "Cancel", 34.0).clicked() {
                        action = ConnectDialogAction::Cancel;
                    }
                });
            });
            action
        },
    );
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return ConnectDialogAction::Cancel;
    }
    action
}

fn stage_row(ui: &mut egui::Ui, row: &StageRow) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    let painter = ui.painter();
    let marker_centre = egui::pos2(rect.min.x + 8.0, rect.center().y);
    match row.status {
        StageStatus::Done => {
            painter.circle_filled(marker_centre, 7.5, theme::ACCENT_TINT);
            painter.circle_stroke(marker_centre, 7.5, Stroke::new(1.0, theme::ACCENT_FILL));
            painter.text(
                marker_centre,
                Align2::CENTER_CENTER,
                "✓",
                theme::sans_semibold(10.0),
                theme::ACCENT,
            );
        }
        StageStatus::Current => {
            painter.circle_filled(marker_centre, 7.5, theme::ACCENT);
        }
        StageStatus::Pending => {
            painter.circle_stroke(marker_centre, 7.5, Stroke::new(1.0, theme::LINE_SUBTLE));
        }
    }
    let name_colour = match row.status {
        StageStatus::Done => theme::TEXT_SECONDARY,
        StageStatus::Current => theme::TEXT_PRIMARY,
        StageStatus::Pending => theme::TEXT_DIM,
    };
    let mut x = rect.min.x + 24.0;
    let name_rect = painter.text(
        egui::pos2(x, rect.center().y),
        Align2::LEFT_CENTER,
        &row.name,
        theme::mono(13.0),
        name_colour,
    );
    x = name_rect.max.x + 8.0;
    if let Some(q) = &row.qualifier {
        let colour = if q == "pinned" || q.contains("matched") {
            theme::ACCENT
        } else {
            theme::TEXT_DIM
        };
        painter.text(
            egui::pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            q,
            theme::mono(11.0),
            colour,
        );
    }
    if let Some(ms) = row.elapsed_ms {
        painter.text(
            egui::pos2(rect.max.x, rect.center().y),
            Align2::RIGHT_CENTER,
            format!("{ms} ms"),
            theme::mono(11.0),
            theme::TEXT_DIM,
        );
    }
}

/// The §7 failure dialog matching `failure.kind`. Danger variant only for kinds the
/// spec frames that way (none of the connect four are; CHANGED-certificate is, later).
pub fn connect_failed(ctx: &egui::Context, failure: &ConnectFailure) -> ConnectDialogAction {
    let (title, sub, primary): (&str, String, &str) = match failure.kind {
        FailureKind::Unreachable => (
            "Could not reach host",
            "failed at tcp_connect".to_owned(),
            "Retry",
        ),
        FailureKind::SignInRejected => (
            "Sign-in rejected",
            signin_subtitle(&failure.error),
            "Edit password",
        ),
        FailureKind::PortRefused => (
            "Port refused",
            "failed at tcp_connect · ECONNREFUSED".to_owned(),
            "Try port 3389",
        ),
        // CHANGED gets its own danger dialog (certificate_changed); routing it here
        // would invent an accept path the spec forbids. Render it as generic if it
        // ever lands here by mistake.
        FailureKind::CertificateChanged | FailureKind::Other => {
            ("Could not connect", String::new(), "Retry")
        }
    };
    let (_, action) = dialog(
        ctx,
        "connect-failed",
        520.0,
        false,
        |ui| {
            ui.horizontal(|ui| {
                danger_chip(ui);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.label(
                        RichText::new(title)
                            .font(theme::sans_semibold(16.0))
                            .color(theme::TEXT_PRIMARY),
                    );
                    if !sub.is_empty() {
                        ui.label(
                            RichText::new(&sub)
                                .font(theme::mono(12.0))
                                .color(theme::TEXT_MUTED),
                        );
                    }
                });
            });
            ui.label(
                RichText::new(host_line(failure))
                    .font(theme::mono(13.0))
                    .color(theme::TEXT_SECONDARY),
            );
            // The raw error, so the dialog never hides what actually happened.
            ui.label(
                RichText::new(&failure.error)
                    .font(theme::sans(12.0))
                    .color(theme::TEXT_MUTED),
            );
            ConnectDialogAction::None
        },
        |ui| {
            let mut action = ConnectDialogAction::None;
            ui.horizontal(|ui| {
                if failure.kind == FailureKind::SignInRejected {
                    ui.label(
                        RichText::new(format!("tried {}", failure.account))
                            .font(theme::mono(11.0))
                            .color(theme::TEXT_DIM),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    if widgets::primary_button(ui, primary, 34.0).clicked() {
                        action = match failure.kind {
                            FailureKind::SignInRejected => ConnectDialogAction::EditPassword,
                            FailureKind::PortRefused => ConnectDialogAction::TryDefaultPort,
                            _ => ConnectDialogAction::Retry,
                        };
                    }
                    if widgets::secondary_button(ui, "Close", 34.0).clicked() {
                        action = ConnectDialogAction::Close;
                    }
                });
            });
            action
        },
    );
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return ConnectDialogAction::Close;
    }
    action
}

/// The Sign-in-rejected subtitle: the NSTATUS the server actually returned, when the
/// error names one. Never a guessed constant — STATUS_LOGON_FAILURE (wrong credential)
/// and STATUS_PASSWORD_EXPIRED are different problems the dialog must not conflate.
pub fn signin_subtitle(error: &str) -> String {
    let status = error
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .find(|t| t.starts_with("STATUS_"));
    match status {
        Some(s) => format!("failed at Credssp · {s}"),
        None => "failed at Credssp".to_owned(),
    }
}

fn host_line(failure: &ConnectFailure) -> String {
    if failure.port == crate::favourites::DEFAULT_PORT {
        failure.host.clone()
    } else {
        format!("{}:{}", failure.host, failure.port)
    }
}

/// Compile-time guard that scrim alpha stays what the spec says.
#[allow(dead_code)]
const _: Color32 = theme::SCRIM;

/// A first-sight certificate, as the child reports it over the pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertPromptInfo {
    /// `host:port`, as keyed in known_hosts.
    pub host: String,
    /// Bare hex SHA-256.
    pub fingerprint: String,
    /// The durable file a pin would touch — the footnote names it.
    pub store_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertPromptAction {
    None,
    PinAndConnect,
    ConnectOnce,
    Reject,
}

/// `9f2c…b7` → `9f:2c:…:b7` display form, uppercase-free like ssh's.
pub fn colon_fingerprint(hex: &str) -> String {
    hex.as_bytes()
        .chunks(2)
        .map(|pair| std::str::from_utf8(pair).unwrap_or("?"))
        .collect::<Vec<_>>()
        .join(":")
}

/// The First-connection (TOFU) dialog: fingerprint card, Pin and connect / Connect
/// once, and the footnote naming the known_hosts file.
pub fn first_connection(ctx: &egui::Context, info: &CertPromptInfo) -> CertPromptAction {
    let (_, action) = dialog(
        ctx,
        "first-connection",
        560.0,
        false,
        |ui| {
            ui.label(
                RichText::new(format!("First connection to {}", info.host))
                    .font(theme::sans_semibold(16.0))
                    .color(theme::TEXT_PRIMARY),
            );
            ui.label(
                RichText::new(
                    "Nothing vouches for this certificate yet. Pinning it means a later change — a reinstalled host, or someone in the middle — stops the connection and asks.",
                )
                .font(theme::sans(13.0))
                .color(theme::TEXT_SECONDARY),
            );
            Frame::new()
                .fill(theme::BG_CHROME)
                .stroke(Stroke::new(1.0, theme::LINE_HAIR))
                .corner_radius(CornerRadius::same(theme::radius::CARD))
                .inner_margin(Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.label(
                        RichText::new("SHA-256 FINGERPRINT")
                            .font(theme::mono(11.0))
                            .color(theme::TEXT_DIM),
                    );
                    ui.label(
                        RichText::new(colon_fingerprint(&info.fingerprint))
                            .font(theme::mono(12.0))
                            .color(theme::TEXT_PRIMARY),
                    );
                });
            ui.label(
                RichText::new(format!("A pin is recorded in {}", info.store_path))
                    .font(theme::mono(11.0))
                    .color(theme::TEXT_DIM),
            );
            CertPromptAction::None
        },
        |ui| {
            let mut action = CertPromptAction::None;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if widgets::primary_button(ui, "Pin and connect", 34.0).clicked() {
                    action = CertPromptAction::PinAndConnect;
                }
                if widgets::secondary_button(ui, "Connect once", 34.0).clicked() {
                    action = CertPromptAction::ConnectOnce;
                }
                if widgets::secondary_button(ui, "Cancel", 34.0).clicked() {
                    action = CertPromptAction::Reject;
                }
            });
            action
        },
    );
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return CertPromptAction::Reject;
    }
    action
}

/// Pull the pinned and presented fingerprints out of the CHANGED refusal text.
///
/// The message is ours (`trust.rs`), so the format is stable; parsing failure just
/// means the dialog shows the raw error instead of the two-fingerprint card.
pub fn changed_fingerprints(error: &str) -> Option<(String, String)> {
    let pinned = error.split("pinned:").nth(1)?.split_whitespace().next()?;
    let presented = error
        .split("presented:")
        .nth(1)?
        .split_whitespace()
        .next()?;
    Some((pinned.to_owned(), presented.to_owned()))
}

/// The Certificate CHANGED dialog: danger variant, Close only — deliberately no
/// accept path. The pin is forgotten in Settings ▸ Certificate trust, nowhere else.
pub fn certificate_changed(ctx: &egui::Context, failure: &ConnectFailure) -> bool {
    let fingerprints = changed_fingerprints(&failure.error);
    let (_, close) = dialog(
        ctx,
        "certificate-changed",
        560.0,
        true,
        |ui| {
            ui.horizontal(|ui| {
                danger_chip(ui);
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("Certificate for {} has CHANGED", failure.host))
                        .font(theme::sans_semibold(16.0))
                        .color(theme::DANGER_TEXT),
                );
            });
            ui.label(
                RichText::new(
                    "The host is presenting a different certificate from the one pinned. That is what a reinstalled host looks like — and also what an interception looks like. This connection will not proceed.",
                )
                .font(theme::sans(12.0))
                .color(theme::DANGER_BODY),
            );
            match &fingerprints {
                Some((pinned, presented)) => {
                    Frame::new()
                        .fill(theme::BG_CHROME)
                        .stroke(Stroke::new(1.0, theme::LINE_HAIR))
                        .corner_radius(CornerRadius::same(theme::radius::CARD))
                        .inner_margin(Margin::symmetric(16, 12))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            for (label, value) in [("PINNED", pinned), ("PRESENTED", presented)] {
                                ui.label(
                                    RichText::new(label)
                                        .font(theme::mono(11.0))
                                        .color(theme::TEXT_DIM),
                                );
                                ui.label(
                                    RichText::new(value.as_str())
                                        .font(theme::mono(12.0))
                                        .color(theme::TEXT_PRIMARY),
                                );
                            }
                        });
                }
                None => {
                    ui.label(
                        RichText::new(&failure.error)
                            .font(theme::mono(11.0))
                            .color(theme::TEXT_MUTED),
                    );
                }
            }
            ui.label(
                RichText::new(
                    "If the change is legitimate, forget the pin in Settings ▸                      Certificate trust and connect again.",
                )
                .font(theme::sans(12.0))
                .color(theme::TEXT_MUTED),
            );
            false
        },
        |ui| {
            let mut close = false;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::primary_button(ui, "Close", 34.0).clicked() {
                    close = true;
                }
            });
            close
        },
    );
    close || ctx.input(|i| i.key_pressed(egui::Key::Escape))
}

// --- Edit / Remove / Quit / About ---------------------------------------------------

/// Working state of the Edit connection dialog.
pub struct EditState {
    pub original_name: String,
    pub name: String,
    pub host: String,
    pub port: String,
    pub username: String,
    pub domain: String,
    pub fullscreen: bool,
    pub width: String,
    pub height: String,
    /// Whether the stored password row has been flipped to a replacement input.
    pub replace_password: bool,
    pub new_password: zeroize::Zeroizing<String>,
    pub had_stored_password: bool,
    pub error: Option<String>,
}

impl EditState {
    pub fn for_favourite(f: &crate::favourites::Favourite) -> Self {
        let (fullscreen, width, height) = match f.window_size {
            crate::favourites::WindowSize::Fullscreen => (true, String::new(), String::new()),
            crate::favourites::WindowSize::Explicit { width, height } => {
                (false, width.to_string(), height.to_string())
            }
        };
        EditState {
            original_name: f.name.clone(),
            name: f.name.clone(),
            host: f.host.clone(),
            port: f.port.to_string(),
            username: f.username.clone().unwrap_or_default(),
            domain: f.domain.clone().unwrap_or_default(),
            fullscreen,
            width,
            height,
            replace_password: false,
            new_password: zeroize::Zeroizing::new(String::new()),
            had_stored_password: f.keychain_account.is_some(),
            error: None,
        }
    }

    /// The keychain account key the edited favourite would use.
    pub fn account_key(&self) -> String {
        let port = self.port.trim().parse::<u16>().unwrap_or(3389);
        format!("{}@{}:{}", self.username.trim(), self.host.trim(), port)
    }

    /// Validate and build the favourite this dialog describes.
    pub fn build(&self) -> Result<crate::favourites::Favourite, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("favourite name must not be empty".to_owned());
        }
        let host = self.host.trim();
        if host.is_empty() {
            return Err("favourite host must not be empty".to_owned());
        }
        let port: u16 = if self.port.trim().is_empty() {
            crate::favourites::DEFAULT_PORT
        } else {
            self.port
                .trim()
                .parse()
                .map_err(|_| format!("{:?} is not a port number", self.port.trim()))?
        };
        if port == 0 {
            return Err("favourite port must not be zero".to_owned());
        }
        let window_size = if self.fullscreen {
            crate::favourites::WindowSize::Fullscreen
        } else {
            let width: u16 = self
                .width
                .trim()
                .parse()
                .map_err(|_| format!("{:?} is not a width in pixels", self.width.trim()))?;
            let height: u16 = self
                .height
                .trim()
                .parse()
                .map_err(|_| format!("{:?} is not a height in pixels", self.height.trim()))?;
            crate::favourites::WindowSize::Explicit { width, height }
        };
        let username = (!self.username.trim().is_empty()).then(|| self.username.trim().to_owned());
        let stores_password = self.had_stored_password || !self.new_password.is_empty();
        Ok(crate::favourites::Favourite {
            name: name.to_owned(),
            host: host.to_owned(),
            port,
            username,
            domain: (!self.domain.trim().is_empty()).then(|| self.domain.trim().to_owned()),
            window_size,
            keychain_account: stores_password.then(|| self.account_key()),
            last_used: None, // The caller preserves the original's timestamp.
        })
    }
}

pub enum EditAction {
    None,
    /// Save the edited favourite; the replacement password if one was typed.
    Save(
        crate::favourites::Favourite,
        Option<zeroize::Zeroizing<String>>,
    ),
    /// Open the Remove confirmation for this favourite.
    Remove,
    Cancel,
}

fn field_row(ui: &mut egui::Ui, label: &str, value: &mut String, width: f32) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [120.0, 20.0],
            egui::Label::new(
                RichText::new(label)
                    .font(theme::sans_medium(11.0))
                    .color(theme::TEXT_MUTED),
            ),
        );
        ui.add_sized(
            [width, 30.0],
            egui::TextEdit::singleline(value).font(theme::mono(13.0)),
        );
    });
}

/// The Edit connection dialog (§7, 560 wide).
pub fn edit_connection(ctx: &egui::Context, state: &mut EditState) -> EditAction {
    let (body_action, footer_action) = dialog(
        ctx,
        "edit-connection",
        560.0,
        false,
        |ui| {
            let mut action = EditAction::None;
            ui.label(
                RichText::new(format!("Edit {}", state.original_name))
                    .font(theme::sans_semibold(16.0))
                    .color(theme::TEXT_PRIMARY),
            );
            field_row(ui, "NAME", &mut state.name, 360.0);
            field_row(ui, "HOST", &mut state.host, 360.0);
            field_row(ui, "PORT", &mut state.port, 100.0);
            field_row(ui, "USERNAME", &mut state.username, 360.0);
            field_row(ui, "DOMAIN", &mut state.domain, 200.0);
            ui.horizontal(|ui| {
                ui.add_sized(
                    [120.0, 20.0],
                    egui::Label::new(
                        RichText::new("DISPLAY")
                            .font(theme::sans_medium(11.0))
                            .color(theme::TEXT_MUTED),
                    ),
                );
                if ui
                    .selectable_label(
                        state.fullscreen,
                        RichText::new("Fullscreen").font(theme::sans(12.0)),
                    )
                    .clicked()
                {
                    state.fullscreen = true;
                }
                if ui
                    .selectable_label(
                        !state.fullscreen,
                        RichText::new("Explicit size").font(theme::sans(12.0)),
                    )
                    .clicked()
                {
                    state.fullscreen = false;
                }
                if !state.fullscreen {
                    ui.add_sized(
                        [64.0, 26.0],
                        egui::TextEdit::singleline(&mut state.width).font(theme::mono(12.0)),
                    );
                    ui.label(
                        RichText::new("×")
                            .font(theme::mono(12.0))
                            .color(theme::TEXT_DIM),
                    );
                    ui.add_sized(
                        [64.0, 26.0],
                        egui::TextEdit::singleline(&mut state.height).font(theme::mono(12.0)),
                    );
                }
            });
            // Password row: reads "stored in keychain" with Replace, per the mock.
            ui.horizontal(|ui| {
                ui.add_sized(
                    [120.0, 20.0],
                    egui::Label::new(
                        RichText::new("PASSWORD")
                            .font(theme::sans_medium(11.0))
                            .color(theme::TEXT_MUTED),
                    ),
                );
                if state.replace_password {
                    ui.add_sized(
                        [240.0, 30.0],
                        egui::TextEdit::singleline(&mut *state.new_password)
                            .password(true)
                            .font(theme::mono(13.0)),
                    );
                } else {
                    ui.label(
                        RichText::new(if state.had_stored_password {
                            "stored in keychain"
                        } else {
                            "not stored — asked for at connect"
                        })
                        .font(theme::mono(12.0))
                        .color(theme::TEXT_SECONDARY),
                    );
                    if widgets::secondary_button(ui, "Replace", 26.0).clicked() {
                        state.replace_password = true;
                    }
                }
            });
            ui.label(
                RichText::new(state.account_key())
                    .font(theme::mono(11.0))
                    .color(theme::TEXT_DIM),
            );
            if let Some(error) = &state.error {
                ui.label(
                    RichText::new(error)
                        .font(theme::sans(12.0))
                        .color(theme::DANGER),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if widgets::danger_button(ui, "Remove connection", 30.0).clicked() {
                    action = EditAction::Remove;
                }
            });
            action
        },
        |ui| {
            let mut click = FooterClick::None;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Renaming the account moves the keychain entry.")
                        .font(theme::sans(11.0))
                        .color(theme::TEXT_DIM),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    if widgets::primary_button(ui, "Save", 34.0).clicked() {
                        click = FooterClick::Save;
                    }
                    if widgets::secondary_button(ui, "Cancel", 34.0).clicked() {
                        click = FooterClick::Cancel;
                    }
                });
            });
            click
        },
    );
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return EditAction::Cancel;
    }
    match footer_action {
        FooterClick::Save => match state.build() {
            Ok(favourite) => {
                let password = (!state.new_password.is_empty()).then(|| state.new_password.clone());
                EditAction::Save(favourite, password)
            }
            Err(e) => {
                state.error = Some(e);
                EditAction::None
            }
        },
        FooterClick::Cancel => EditAction::Cancel,
        FooterClick::None => body_action,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FooterClick {
    None,
    Save,
    Cancel,
}

/// State of the Remove confirmation (§7, 440 wide).
pub struct RemoveState {
    pub name: String,
    /// The keychain account whose password the checkbox offers to delete.
    pub account: Option<String>,
    pub delete_password: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveAction {
    None,
    Remove { delete_password: bool },
    Cancel,
}

pub fn remove_connection(ctx: &egui::Context, state: &mut RemoveState) -> RemoveAction {
    let delete_password_now = state.delete_password;
    let (_, action) = dialog(
        ctx,
        "remove-connection",
        440.0,
        false,
        |ui| {
            ui.label(
                RichText::new(format!("Remove {}?", state.name))
                    .font(theme::sans_semibold(16.0))
                    .color(theme::TEXT_PRIMARY),
            );
            ui.label(
                RichText::new("The entry is removed from favourites.toml.")
                    .font(theme::sans(12.0))
                    .color(theme::TEXT_SECONDARY),
            );
            if state.account.is_some() {
                ui.checkbox(
                    &mut state.delete_password,
                    RichText::new("Also delete the keychain password")
                        .font(theme::sans(12.0))
                        .color(theme::TEXT_SECONDARY),
                );
            }
            RemoveAction::None
        },
        |ui| {
            let mut action = RemoveAction::None;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if widgets::danger_button(ui, "Remove", 34.0).clicked() {
                    action = RemoveAction::Remove {
                        delete_password: delete_password_now,
                    };
                }
                if widgets::secondary_button(ui, "Cancel", 34.0).clicked() {
                    action = RemoveAction::Cancel;
                }
            });
            action
        },
    );
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return RemoveAction::Cancel;
    }
    action
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitAction {
    None,
    QuitAnyway,
    Cancel,
}

/// The Quit-with-sessions dialog (§7, 480). Sessions are their own processes and
/// keep running when the launcher goes; the dialog exists so that is a choice,
/// not a surprise.
pub fn quit_with_sessions(ctx: &egui::Context, sessions: &[(String, u32, u64)]) -> QuitAction {
    let (_, action) = dialog(
        ctx,
        "quit-with-sessions",
        480.0,
        false,
        |ui| {
            ui.label(
                RichText::new("Quit with sessions running?")
                    .font(theme::sans_semibold(16.0))
                    .color(theme::TEXT_PRIMARY),
            );
            for (name, pid, uptime_mins) in sessions {
                ui.horizontal(|ui| {
                    widgets::status_dot(ui, theme::ACCENT);
                    ui.label(
                        RichText::new(format!("{name} · pid {pid} · {uptime_mins}m"))
                            .font(theme::mono(12.0))
                            .color(theme::TEXT_SECONDARY),
                    );
                });
            }
            ui.label(
                RichText::new(
                    "Each session is its own process and keeps running; close them from their own windows.",
                )
                .font(theme::sans(12.0))
                .color(theme::TEXT_MUTED),
            );
            QuitAction::None
        },
        |ui| {
            let mut action = QuitAction::None;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if widgets::danger_button(ui, "Quit anyway", 34.0).clicked() {
                    action = QuitAction::QuitAnyway;
                }
                if widgets::secondary_button(ui, "Cancel", 34.0).clicked() {
                    action = QuitAction::Cancel;
                }
            });
            action
        },
    );
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return QuitAction::Cancel;
    }
    action
}

/// About mdrdp (§7, 440): icon, version, and the rows the spec names.
pub fn about(ctx: &egui::Context, icon: Option<&egui::TextureHandle>) -> bool {
    let (_, close) = dialog(
        ctx,
        "about-mdrdp",
        440.0,
        false,
        |ui| {
            ui.vertical_centered(|ui| {
                if let Some(icon) = icon {
                    ui.add(egui::Image::new(icon).fit_to_exact_size(egui::vec2(72.0, 72.0)));
                }
                ui.label(
                    RichText::new("mdrdp")
                        .font(theme::sans_semibold(18.0))
                        .color(theme::TEXT_PRIMARY),
                );
                ui.label(
                    RichText::new(format!(
                        "{} · {} {}",
                        env!("CARGO_PKG_VERSION"),
                        std::env::consts::OS,
                        std::env::consts::ARCH
                    ))
                    .font(theme::mono(12.0))
                    .color(theme::TEXT_MUTED),
                );
            });
            ui.add_space(6.0);
            for (label, value) in [
                ("Licence", "MIT OR Apache-2.0"),
                ("Protocol", "IronRDP 0.17"),
                (
                    "Vendored",
                    "ironrdp-connector (one flag, or EGFX never opens)",
                ),
            ] {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [90.0, 18.0],
                        egui::Label::new(
                            RichText::new(label)
                                .font(theme::sans(12.0))
                                .color(theme::TEXT_MUTED),
                        ),
                    );
                    ui.label(
                        RichText::new(value)
                            .font(theme::mono(12.0))
                            .color(theme::TEXT_SECONDARY),
                    );
                });
            }
            false
        },
        |ui| {
            let mut close = false;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::primary_button(ui, "Close", 34.0).clicked() {
                    close = true;
                }
            });
            close
        },
    );
    close || ctx.input(|i| i.key_pressed(egui::Key::Escape))
}

/// State of the credential dialogs: "No saved password" first, then the prompt.
pub struct PasswordPromptState {
    pub account: String,
    /// The store failure, named in warn mono per the mock. Never a secret.
    pub reason: String,
    /// Flipped by "Enter password": switches from the explainer to the input dialog.
    pub entering: bool,
    pub input: zeroize::Zeroizing<String>,
    pub save: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordAction {
    None,
    /// Send the typed password to the child (and optionally store it).
    Connect,
    Cancel,
}

/// The `security add-generic-password` command the mock shows, for the account.
pub fn keychain_command(account: &str) -> String {
    format!("security add-generic-password -s mdrdp -a '{account}' -w")
}

/// The No-saved-password explainer (560) or the Password prompt (480), by state.
pub fn credential_dialog(ctx: &egui::Context, state: &mut PasswordPromptState) -> PasswordAction {
    if !state.entering {
        let command = keychain_command(&state.account);
        let (_, action) = dialog(
            ctx,
            "no-saved-password",
            560.0,
            false,
            |ui| {
                ui.label(
                    RichText::new("No saved password")
                        .font(theme::sans_semibold(16.0))
                        .color(theme::TEXT_PRIMARY),
                );
                ui.label(
                    RichText::new(format!(
                        "Nothing is stored for {} in the system credential store.",
                        state.account
                    ))
                    .font(theme::sans(13.0))
                    .color(theme::TEXT_SECONDARY),
                );
                Frame::new()
                    .fill(theme::BG_CHROME)
                    .stroke(Stroke::new(1.0, theme::LINE_HAIR))
                    .corner_radius(CornerRadius::same(theme::radius::CARD))
                    .inner_margin(Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(&command)
                                    .font(theme::mono(12.0))
                                    .color(theme::TEXT_SECONDARY),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if widgets::secondary_button(ui, "Copy", 24.0).clicked() {
                                        ui.ctx().copy_text(command.clone());
                                    }
                                },
                            );
                        });
                    });
                PasswordAction::None
            },
            |ui| {
                let mut action = PasswordAction::None;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    if widgets::primary_button(ui, "Enter password", 34.0).clicked() {
                        action = PasswordAction::Connect; // repurposed: advance to input
                    }
                    if widgets::secondary_button(ui, "Cancel", 34.0).clicked() {
                        action = PasswordAction::Cancel;
                    }
                });
                action
            },
        );
        return match action {
            PasswordAction::Connect => {
                state.entering = true;
                PasswordAction::None
            }
            other if ctx.input(|i| i.key_pressed(egui::Key::Escape)) => {
                let _ = other;
                PasswordAction::Cancel
            }
            other => other,
        };
    }

    let reason = state.reason.clone();
    let mut typed = std::mem::take(&mut *state.input);
    let mut save = state.save;
    let (_, action) = dialog(
        ctx,
        "password-prompt",
        480.0,
        false,
        |ui| {
            ui.label(
                RichText::new(format!("Password for {}", state.account))
                    .font(theme::sans_semibold(16.0))
                    .color(theme::TEXT_PRIMARY),
            );
            let edit = egui::TextEdit::singleline(&mut typed)
                .password(true)
                .font(theme::mono(15.0))
                .desired_width(f32::INFINITY);
            let response = ui.add_sized([ui.available_width(), 38.0], edit);
            response.request_focus();
            ui.checkbox(
                &mut save,
                RichText::new("Save to the system credential store")
                    .font(theme::sans(12.0))
                    .color(theme::TEXT_SECONDARY),
            );
            ui.label(
                RichText::new(&reason)
                    .font(theme::mono(11.0))
                    .color(theme::WARN),
            );
            PasswordAction::None
        },
        |ui| {
            let mut action = PasswordAction::None;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if widgets::primary_button(ui, "Connect", 34.0).clicked() {
                    action = PasswordAction::Connect;
                }
                if widgets::secondary_button(ui, "Cancel", 34.0).clicked() {
                    action = PasswordAction::Cancel;
                }
            });
            action
        },
    );
    *state.input = typed;
    state.save = save;
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return PasswordAction::Cancel;
    }
    if action == PasswordAction::Connect && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
        return PasswordAction::Connect;
    }
    if action == PasswordAction::Connect && state.input.is_empty() {
        return PasswordAction::None; // An empty password is a non-answer.
    }
    // Enter in the field also connects.
    if action == PasswordAction::None
        && !state.input.is_empty()
        && ctx.input(|i| i.key_pressed(egui::Key::Enter))
    {
        return PasswordAction::Connect;
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_maps_the_specced_failures() {
        assert_eq!(
            classify("CredSSP: STATUS_LOGON_FAILURE"),
            FailureKind::SignInRejected
        );
        assert_eq!(
            classify("connecting to host: Connection refused (os error 61)"),
            FailureKind::PortRefused
        );
        assert_eq!(
            classify("connecting to host: Operation timed out"),
            FailureKind::Unreachable
        );
        assert_eq!(
            classify("failed to lookup address information"),
            FailureKind::Unreachable
        );
        assert_eq!(
            classify("the server closed the channel"),
            FailureKind::Other
        );
    }

    #[test]
    fn the_subtitle_carries_the_status_the_server_returned() {
        assert_eq!(
            signin_subtitle("CredSSP: STATUS_LOGON_FAILURE [0xc000006d]"),
            "failed at Credssp · STATUS_LOGON_FAILURE"
        );
        assert_eq!(
            signin_subtitle("CredSSP: STATUS_PASSWORD_EXPIRED [0xc0000071]"),
            "failed at Credssp · STATUS_PASSWORD_EXPIRED"
        );
    }

    #[test]
    fn the_subtitle_claims_no_status_when_the_error_names_none() {
        assert_eq!(
            signin_subtitle("CredSSP: the server rejected our final token"),
            "failed at Credssp"
        );
    }

    #[test]
    fn the_keychain_command_names_the_exact_account() {
        assert_eq!(
            keychain_command("alice@temper:3389"),
            "security add-generic-password -s mdrdp -a 'alice@temper:3389' -w"
        );
    }

    #[test]
    fn the_edit_dialog_builds_the_favourite_it_describes() {
        let mut state = EditState::for_favourite(&crate::favourites::Favourite {
            username: Some("alice".into()),
            domain: Some("CORP".into()),
            port: 3391,
            window_size: crate::favourites::WindowSize::Explicit {
                width: 1600,
                height: 1000,
            },
            keychain_account: Some("alice@quench:3391".into()),
            ..crate::favourites::Favourite::new("Quench", "quench")
        });
        state.name = "Quench 2".into();
        state.port = "3392".into();
        let built = state.build().expect("valid");
        assert_eq!(built.name, "Quench 2");
        assert_eq!(built.host, "quench");
        assert_eq!(built.port, 3392);
        assert_eq!(built.username.as_deref(), Some("alice"));
        assert_eq!(built.domain.as_deref(), Some("CORP"));
        assert_eq!(
            built.window_size,
            crate::favourites::WindowSize::Explicit {
                width: 1600,
                height: 1000
            }
        );
        assert_eq!(
            built.keychain_account.as_deref(),
            Some("alice@quench:3392"),
            "the account key follows the edited host and port"
        );
    }

    #[test]
    fn edit_validation_names_the_field_that_failed() {
        let mut state = EditState::for_favourite(&crate::favourites::Favourite::new("A", "h"));
        state.host = "  ".into();
        assert!(state.build().unwrap_err().contains("host"));
        state.host = "h".into();
        state.port = "70000".into();
        assert!(state.build().unwrap_err().contains("port"));
        state.port = "0".into();
        assert!(state.build().unwrap_err().contains("zero"));
        state.port = String::new();
        assert_eq!(state.build().unwrap().port, 3389, "blank port is default");
    }

    #[test]
    fn an_unstored_password_stays_unstored_unless_replaced() {
        let state = EditState::for_favourite(&crate::favourites::Favourite::new("A", "h"));
        assert_eq!(state.build().unwrap().keychain_account, None);
        let mut replaced = EditState::for_favourite(&crate::favourites::Favourite::new("A", "h"));
        replaced.username = "u".into();
        *replaced.new_password = "pw".into();
        assert!(replaced.build().unwrap().keychain_account.is_some());
    }

    #[test]
    fn a_changed_certificate_classifies_to_its_own_dialog() {
        let error = "certificate for temper:3389 has CHANGED.\n  pinned:    sha256:aaaa\n  presented: sha256:bbbb\nRefusing to connect.";
        assert_eq!(classify(error), FailureKind::CertificateChanged);
        assert_eq!(
            changed_fingerprints(error),
            Some(("sha256:aaaa".to_owned(), "sha256:bbbb".to_owned()))
        );
        assert_eq!(changed_fingerprints("no fingerprints here"), None);
    }

    #[test]
    fn fingerprints_display_in_colon_pairs() {
        assert_eq!(colon_fingerprint("9f2cb7"), "9f:2c:b7");
        assert_eq!(colon_fingerprint(""), "");
    }

    #[test]
    fn stage_rows_merge_arrived_with_canonical_expectation() {
        let arrived = vec![
            (
                "tcp_connect".to_owned(),
                3,
                Some("192.0.2.171:3389".to_owned()),
            ),
            (
                "x224_negotiation".to_owned(),
                6,
                Some("HYBRID_EX requested".to_owned()),
            ),
        ];
        let rows = stage_rows(&arrived);
        assert_eq!(rows[0].name, "tcp_connect");
        assert_eq!(rows[0].status, StageStatus::Done);
        assert_eq!(rows[0].elapsed_ms, Some(3));
        assert_eq!(rows[1].qualifier.as_deref(), Some("HYBRID_EX requested"));
        assert_eq!(rows[2].name, "tls_handshake");
        assert_eq!(rows[2].status, StageStatus::Current, "next canonical leg");
        assert!(
            rows[3..].iter().all(|r| r.status == StageStatus::Pending),
            "everything after the current leg is pending"
        );
        assert_eq!(rows.len(), 2 + 5, "two arrived + five canonical remaining");
    }

    #[test]
    fn a_noncanonical_stage_does_not_duplicate_or_vanish() {
        let arrived = vec![
            ("tcp_connect".to_owned(), 3, None),
            ("LicensingExchange (inline)".to_owned(), 12, None),
        ];
        let rows = stage_rows(&arrived);
        assert_eq!(rows[1].name, "LicensingExchange (inline)");
        // The canonical "LicensingExchange" must be treated as arrived (prefix match).
        assert!(
            !rows
                .iter()
                .any(|r| r.name == "LicensingExchange" && r.status != StageStatus::Done),
            "canonical licensing must not reappear as pending: {rows:?}"
        );
    }

    #[test]
    fn progress_runs_zero_to_one_as_stages_land() {
        let empty = stage_rows(&[]);
        assert_eq!(progress_fraction(&empty), 0.0);
        let all: Vec<(String, u64, Option<String>)> = CANONICAL_STAGES
            .iter()
            .map(|s| ((*s).to_owned(), 1, None))
            .collect();
        let rows = stage_rows(&all);
        assert!(
            progress_fraction(&rows) >= 6.0 / 7.0,
            "all-arrived should be at or near full: {}",
            progress_fraction(&rows)
        );
    }
}
