//! The Session-ended and Session-lost dialogs (handoff §7), shown by the session
//! process after its window closes.
//!
//! Runs a second on-demand cycle on the process's one event loop, hosting a single
//! egui window via [`crate::ui::egui_host::AuxWindow`]. Skipped entirely on the
//! Cmd+Q path (the process is already gone) and for scripted runs.

use crate::disconnect::ServerFarewell;
use crate::ui::egui_host::AuxWindow;
use crate::ui::theme;
use crate::window::SessionEvent;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};

/// What the user asked for from the end dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndChoice {
    Done,
    /// Session-lost only: start a replacement session.
    Reconnect,
    /// Session-ended only: write the metrics report before closing.
    SaveMetricsAndClose,
}

/// Aux windows are not resizable, so each size is pinned by a fit test below. The two
/// unexpected endings share a size: both carry a wrapped explanation whose length is
/// the server's to choose, so both need the same headroom.
const WARN_WINDOW: [f32; 2] = [480.0, 300.0];
const ENDED_WINDOW: [f32; 2] = [460.0, 280.0];

/// How the session finished — one dialog variant per case.
pub enum EndOutcome {
    /// Ended cleanly: the user closed the window, or we disconnected.
    Ended,
    /// The link failed under us; the string is the failure text.
    Lost(String),
    /// The server ended it and said why (MS-RDPBCGR Set Error Info).
    ServerEnded(ServerFarewell),
}

impl EndOutcome {
    /// Both unexpected endings get the warning bar and the Reconnect button.
    fn unexpected(&self) -> bool {
        !matches!(self, Self::Ended)
    }
}

/// What the dialog shows.
pub struct EndInfo {
    pub outcome: EndOutcome,
    pub session_name: String,
    pub duration_secs: u64,
    /// Latency drift in milliseconds, when measured.
    pub drift_ms: Option<f64>,
    /// Share of painted bytes that came from the cache, when the cache was used.
    pub cache_share: Option<f64>,
}

struct EndApp {
    info: EndInfo,
    window: Option<AuxWindow>,
    choice: Option<EndChoice>,
    /// Scripted-verification affordance: `MDRDP_END_DIALOG_AUTOCLOSE_MS` closes the
    /// dialog as Done after this deadline, because nothing can click a button in an
    /// automated run. Absent in normal use.
    autoclose_at: Option<std::time::Instant>,
}

impl EndApp {
    /// Open the dialog window if it does not exist yet.
    ///
    /// Called from `new_events` and `about_to_wait`, NOT `resumed`: winit emits
    /// `Resumed` only on the loop's first-ever activation, and this app runs on the
    /// loop's *second* `run_app_on_demand` cycle — a `resumed`-only creation path
    /// waits forever on a window that never comes (observed live, 2026-08-16).
    fn ensure_window(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let (title, size) = match self.info.outcome {
            EndOutcome::Lost(_) => ("Session lost", WARN_WINDOW),
            EndOutcome::ServerEnded(_) => ("Session ended", WARN_WINDOW),
            EndOutcome::Ended => ("Session ended", ENDED_WINDOW),
        };
        match AuxWindow::open(event_loop, title, size) {
            Ok(win) => self.window = Some(win),
            Err(e) => {
                // No dialog beats no exit: report and end the epilogue.
                eprintln!("could not open the {title} dialog: {e}");
                event_loop.exit();
            }
        }
    }

    fn poll_autoclose(&mut self, event_loop: &ActiveEventLoop) {
        let Some(deadline) = self.autoclose_at else {
            return;
        };
        if std::time::Instant::now() >= deadline {
            self.choice = Some(EndChoice::Done);
            event_loop.exit();
        } else {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(deadline));
        }
    }
}

impl ApplicationHandler<SessionEvent> for EndApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.ensure_window(event_loop);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, _cause: winit::event::StartCause) {
        self.ensure_window(event_loop);
        self.poll_autoclose(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.ensure_window(event_loop);
        self.poll_autoclose(event_loop);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: SessionEvent) {}

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.as_mut() else {
            return;
        };
        // The just-closed session window's tail events (Destroyed, focus churn) can
        // still be queued when this cycle starts; treating its Destroyed as ours
        // closed the dialog before it ever painted (observed live, 2026-08-16).
        if window.window_id() != id {
            return;
        }
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                let info = &self.info;
                let mut choice = None;
                window.redraw(|ui| {
                    choice = draw(ui, info);
                });
                if let Some(c) = choice {
                    self.choice = Some(c);
                    event_loop.exit();
                }
            }
            other => {
                window.on_window_event(&other);
            }
        }
    }
}

/// Show the dialog; blocks until a choice or close. `None` means plain close.
pub fn show(event_loop: &mut EventLoop<SessionEvent>, info: EndInfo) -> Option<EndChoice> {
    use winit::platform::run_on_demand::EventLoopExtRunOnDemand as _;
    let autoclose_at = std::env::var("MDRDP_END_DIALOG_AUTOCLOSE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));
    let mut app = EndApp {
        info,
        window: None,
        choice: None,
        autoclose_at,
    };
    // Prime the pump: a freshly re-entered on-demand loop on macOS parks until an
    // external event arrives, so with nothing queued the dialog window is never even
    // created (observed live, 2026-08-16 — the loop woke only when a debugger
    // attached). One queued user event forces the first callback batch.
    let _ = event_loop.create_proxy().send_event(SessionEvent::Damaged);
    if let Err(e) = event_loop.run_app_on_demand(&mut app) {
        eprintln!("end dialog loop failed: {e}");
    }
    app.choice
}

fn draw(ui: &mut egui::Ui, info: &EndInfo) -> Option<EndChoice> {
    use crate::shell::widgets;
    use egui::{CornerRadius, Frame, Margin, RichText, Stroke};
    let mut choice = None;
    let lost = info.outcome.unexpected();
    Frame::new()
        .fill(theme::BG_WINDOW)
        .inner_margin(Margin::same(0))
        .show(ui, |ui| {
            if lost {
                // 4px warn bar across the top (§7 warning variant).
                let bar = egui::Rect::from_min_size(
                    ui.max_rect().min,
                    egui::vec2(ui.available_width(), 4.0),
                );
                ui.painter()
                    .rect_filled(bar, CornerRadius::ZERO, theme::WARN);
                ui.add_space(4.0);
            }
            Frame::new()
                .inner_margin(Margin {
                    left: 24,
                    right: 24,
                    top: 22,
                    bottom: 18,
                })
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 12.0;
                    match &info.outcome {
                        // The server told us why, so lead with that instead of a
                        // dropped-link guess: "Kiln is restarting".
                        EndOutcome::ServerEnded(farewell) => {
                            ui.label(
                                RichText::new(format!(
                                    "{} {}",
                                    info.session_name, farewell.headline
                                ))
                                .font(theme::sans_semibold(16.0))
                                .color(theme::TEXT_PRIMARY),
                            );
                            ui.label(
                                RichText::new(&farewell.detail)
                                    .font(theme::sans(13.0))
                                    .color(theme::TEXT_SECONDARY),
                            );
                        }
                        EndOutcome::Lost(reason) => {
                            ui.label(
                                RichText::new(format!("Session to {} lost", info.session_name))
                                    .font(theme::sans_semibold(16.0))
                                    .color(theme::TEXT_PRIMARY),
                            );
                            ui.label(
                                RichText::new(
                                    "The connection dropped. The session is probably still \
                                     alive on the host — reconnecting resumes it.",
                                )
                                .font(theme::sans(13.0))
                                .color(theme::TEXT_SECONDARY),
                            );
                            ui.label(
                                RichText::new(reason)
                                    .font(theme::mono(11.0))
                                    .color(theme::TEXT_MUTED),
                            );
                        }
                        EndOutcome::Ended => {
                            ui.label(
                                RichText::new(format!("Session {} ended", info.session_name))
                                    .font(theme::sans_semibold(16.0))
                                    .color(theme::TEXT_PRIMARY),
                            );
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 30.0;
                                stat(ui, "DURATION", &duration_str(info.duration_secs));
                                stat(
                                    ui,
                                    "DRIFT",
                                    &info
                                        .drift_ms
                                        .map(|d| format!("{d:+.1} ms"))
                                        .unwrap_or_else(|| "—".to_owned()),
                                );
                                stat(
                                    ui,
                                    "PIXELS CACHED",
                                    &info
                                        .cache_share
                                        .map(|s| format!("{:.0}%", s * 100.0))
                                        .unwrap_or_else(|| "—".to_owned()),
                                );
                            });
                        }
                    }
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
                    ui.horizontal(|ui| {
                        if !lost
                            && widgets::secondary_button(ui, "Save metrics JSON", 34.0).clicked()
                        {
                            choice = Some(EndChoice::SaveMetricsAndClose);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            if lost {
                                if widgets::primary_button(ui, "Reconnect", 34.0).clicked() {
                                    choice = Some(EndChoice::Reconnect);
                                }
                                if widgets::secondary_button(ui, "Close", 34.0).clicked() {
                                    choice = Some(EndChoice::Done);
                                }
                            } else if widgets::primary_button(ui, "Done", 34.0).clicked() {
                                choice = Some(EndChoice::Done);
                            }
                        });
                    });
                });
        });
    if ui.input(|i| i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::Enter)) {
        choice = Some(EndChoice::Done);
    }
    choice
}

fn stat(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.label(
            egui::RichText::new(label)
                .font(theme::sans_medium(11.0))
                .color(theme::TEXT_DIM),
        );
        ui.label(
            egui::RichText::new(value)
                .font(theme::mono(15.0))
                .color(theme::TEXT_PRIMARY),
        );
    });
}

/// `1h 12m` / `12m 30s` / `42s`, matching the mock's compact style.
pub fn duration_str(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_compactly_at_every_scale() {
        assert_eq!(duration_str(42), "42s");
        assert_eq!(duration_str(750), "12m 30s");
        assert_eq!(duration_str(4320), "1h 12m");
    }

    fn info(outcome: EndOutcome) -> EndInfo {
        EndInfo {
            outcome,
            session_name: "Kiln".to_owned(),
            duration_secs: 750,
            drift_ms: Some(1.4),
            cache_share: Some(0.62),
        }
    }

    /// Lay out one real frame of the dialog at the size it opens at, and return every
    /// text it drew with the rect that text occupies.
    fn frame(size: [f32; 2], info: &EndInfo) -> Vec<(String, egui::Rect)> {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(size[0], size[1]),
            )),
            ..Default::default()
        };
        // Seeded with a choice nothing clicked, so a body that never runs cannot pass
        // the caller's assert.
        let mut choice = Some(EndChoice::Reconnect);
        let output = ctx.run_ui(input, |ui| {
            choice = draw(ui, info);
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
        assert!(choice.is_none(), "an untouched end dialog made a choice");
        texts
    }

    /// Nobody can resize an aux window, so anything laid out past its bottom edge is
    /// simply never seen — that is how a dialog loses its own buttons.
    ///
    /// Only the overflow half of the fit is asserted. These §7 dialogs are drawn at a
    /// fixed size with the footer stacked under the content, so a short message
    /// legitimately leaves background below it; the length of the message is the
    /// server's to choose, not ours.
    fn assert_fits(texts: &[(String, egui::Rect)], size: [f32; 2]) {
        let window = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(size[0], size[1]));
        for (text, rect) in texts {
            assert!(
                window.contains_rect(*rect),
                "{text:?} at {rect:?} falls outside the {size:?} window"
            );
        }
    }

    /// The point of the whole change: a host that restarted says so by name, instead of
    /// showing the decode error its Set Error Info PDU used to provoke.
    #[test]
    fn a_restarting_host_is_named_in_the_headline() {
        let info = info(EndOutcome::ServerEnded(ServerFarewell {
            headline: "is restarting",
            detail: "The host is rebooting. It will take connections again once it is back."
                .to_owned(),
        }));
        let texts = frame(WARN_WINDOW, &info);
        let drawn: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        assert!(drawn.contains(&"Kiln is restarting"), "{drawn:?}");
        assert!(drawn.contains(&"Reconnect"), "{drawn:?}");
        assert_fits(&texts, WARN_WINDOW);
    }

    /// The detail line is whatever the protocol says, and the wordiest code in
    /// MS-RDPBCGR 2.2.5.1.1 runs to three wrapped lines. It has to fit too.
    #[test]
    fn the_wordiest_server_reason_still_fits() {
        let detail = crate::disconnect::classify(
            &ironrdp::session::GracefulDisconnectReason::ErrorInfo(
                ironrdp::pdu::rdp::server_error_info::ErrorInfo::ProtocolIndependentCode(
                    ironrdp::pdu::rdp::server_error_info::ProtocolIndependentCode::
                        ServerFreshCredentialsRequired,
                ),
            ),
        )
        .expect("an error-info reason is a farewell");
        let texts = frame(WARN_WINDOW, &info(EndOutcome::ServerEnded(detail)));
        assert_fits(&texts, WARN_WINDOW);
    }

    #[test]
    fn a_lost_session_still_fits_its_window() {
        let info = info(EndOutcome::Lost("connection reset by peer".to_owned()));
        let texts = frame(WARN_WINDOW, &info);
        let drawn: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        assert!(drawn.contains(&"Session to Kiln lost"), "{drawn:?}");
        assert_fits(&texts, WARN_WINDOW);
    }

    #[test]
    fn a_clean_end_still_fits_its_window() {
        let texts = frame(ENDED_WINDOW, &info(EndOutcome::Ended));
        let drawn: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        assert!(drawn.contains(&"Session Kiln ended"), "{drawn:?}");
        assert!(drawn.contains(&"12m 30s"), "{drawn:?}");
        assert_fits(&texts, ENDED_WINDOW);
        assert!(drawn.contains(&"Save metrics JSON"), "{drawn:?}");
    }
}
