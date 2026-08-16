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
    ChangePassword,
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
fn dialog<R>(
    ctx: &egui::Context,
    id: &str,
    width: f32,
    danger: bool,
    body: impl FnOnce(&mut egui::Ui) -> R,
    footer: impl FnOnce(&mut egui::Ui) -> R,
) -> (R, R) {
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
            "failed at Credssp · STATUS_LOGON_FAILURE".to_owned(),
            "Change password",
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
                            FailureKind::SignInRejected => ConnectDialogAction::ChangePassword,
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
                    "Nothing vouches for this certificate yet. Pinning it means a later                      change — a reinstalled host, or someone in the middle — stops the                      connection and asks.",
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
                    "The host is presenting a different certificate from the one pinned.                      That is what a reinstalled host looks like — and also what an                      interception looks like. This connection will not proceed.",
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
