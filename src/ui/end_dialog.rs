//! The Session-ended and Session-lost dialogs (handoff §7), shown by the session
//! process after its window closes.
//!
//! Runs a second on-demand cycle on the process's one event loop, hosting a single
//! egui window via [`crate::ui::egui_host::AuxWindow`]. Skipped entirely on the
//! Cmd+Q path (the process is already gone) and for scripted runs.

use crate::disconnect::{LostSession, ServerFarewell};
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

/// Aux windows cannot be dragged bigger, so whatever falls past the bottom edge is
/// simply never seen — that is how a dialog loses its own buttons. Width stays pinned
/// by the mock; height follows the content, measured before the window opens
/// ([`window_size`]) and again on every frame ([`EndApp::follow_content`]), because
/// both the failure text and the Technical details disclosure change how tall the
/// dialog needs to be.
const WARN_WIDTH: f32 = 440.0;
const ENDED_WIDTH: f32 = 420.0;
/// A floor only, so a one-line ending still looks like a dialog rather than a strip.
/// It sits just under what the shortest real variant measures, which is the point: the
/// content decides the height and the floor never pads it. The mock's 280/300 did pad
/// it — the commonest dialog of all, a clean server logoff, opened with a third of its
/// window empty below the buttons.
const MIN_HEIGHT: f32 = 160.0;
/// A taller dialog would start running off small displays, so growth stops here and the
/// failure text scrolls inside [`REASON_MAX_HEIGHT`] instead.
const MAX_HEIGHT: f32 = 640.0;
/// Height the failure text may claim before it scrolls. Every other part of the dialog
/// is bounded, so this is what keeps the footer on-window for any string at all.
const REASON_MAX_HEIGHT: f32 = 260.0;
/// Allowance added to a headless measurement: it runs at one point per pixel and the
/// real window lays out at the display's scale factor, where glyph rounding can land a
/// hair taller.
const HEIGHT_SLACK: f32 = 2.0;

/// How the session finished — one dialog variant per case.
pub enum EndOutcome {
    /// Ended cleanly: the user closed the window, or we disconnected.
    Ended,
    /// The link failed under us, classified into a sentence plus the chain behind it.
    Lost(LostSession),
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
    /// Height last asked of the window manager, so a content height that has not moved
    /// does not re-request the same size on every frame.
    requested_height: Option<f32>,
    /// Whether the Technical details disclosure is open. Held here rather than in
    /// egui's memory so the dialog stays a pure function of what it is told, and both
    /// states are reachable from a test.
    details_open: bool,
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
        let title = match self.info.outcome {
            EndOutcome::Lost(_) => "Session lost",
            EndOutcome::ServerEnded(_) | EndOutcome::Ended => "Session ended",
        };
        let size = window_size(&self.info);
        match AuxWindow::open(event_loop, title, size) {
            Ok(win) => self.window = Some(win),
            Err(e) => {
                // No dialog beats no exit: report and end the epilogue.
                eprintln!("could not open the {title} dialog: {e}");
                event_loop.exit();
            }
        }
    }

    /// Grow or shrink the window to the height the last frame laid out.
    ///
    /// Opening the Technical details disclosure adds a dozen lines under the buttons,
    /// and an aux window has no edge to drag — so the window follows the content
    /// instead. The clamps are [`window_size`]'s, so a disclosure cannot walk the
    /// dialog off the screen any more than a long failure string can.
    fn follow_content(&mut self, height: f32) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let width = window_width(&self.info.outcome);
        let want = fit_height(height);
        if self
            .requested_height
            .is_some_and(|last| (last - want).abs() < 0.5)
        {
            return;
        }
        self.requested_height = Some(want);
        window.resize_to([width, want]);
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
                let mut details_open = self.details_open;
                let mut drawn = Drawn::default();
                window.redraw(|ui| {
                    drawn = draw(ui, info, &mut details_open);
                });
                self.details_open = details_open;
                if let Some(c) = drawn.choice {
                    self.choice = Some(c);
                    event_loop.exit();
                } else {
                    self.follow_content(drawn.height);
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
        requested_height: None,
        details_open: false,
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

/// The fixed width for this dialog variant.
fn window_width(outcome: &EndOutcome) -> f32 {
    if outcome.unexpected() {
        WARN_WIDTH
    } else {
        ENDED_WIDTH
    }
}

/// A content height turned into a window height: floored so a one-line ending is still
/// a dialog, capped so nothing runs off a display.
fn fit_height(content: f32) -> f32 {
    content.clamp(MIN_HEIGHT, MAX_HEIGHT)
}

/// The size this dialog needs for the text it is about to show.
///
/// The window has to be created at some size before anything paints, so the dialog is
/// laid out headlessly at its fixed width first and opened at the height that used —
/// with the disclosure closed, which is how it opens. After that
/// [`EndApp::follow_content`] keeps it honest.
fn window_size(info: &EndInfo) -> [f32; 2] {
    let width = window_width(&info.outcome);
    [width, fit_height(measured_height(info, width, false))]
}

/// One headless layout pass, purely to measure — the only way to know how tall a
/// wrapped, server-supplied string lands. Costs a font atlas per dialog opened, which
/// is once per process.
fn measured_height(info: &EndInfo, width: f32, details_open: bool) -> f32 {
    let ctx = egui::Context::default();
    theme::apply(&ctx);
    let input = egui::RawInput {
        // Tall enough that nothing is height-constrained, so the pass reports the
        // dialog's natural height rather than the room it was given.
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(width, MAX_HEIGHT * 4.0),
        )),
        ..Default::default()
    };
    let mut height = 0.0;
    let mut open = details_open;
    let output = ctx.run_ui(input, |ui| height = draw(ui, info, &mut open).height);
    // FullOutput's destructor panics on unapplied deltas; nothing paints this pass.
    output.drop_without_applying_deltas();
    // The slack belongs to the estimate, not to the clamp: this pass runs at one point
    // per pixel and the real window lays out at the display's scale factor, where glyph
    // rounding can land a hair taller. A live frame's height needs no such allowance.
    height + HEIGHT_SLACK
}

/// What one laid-out frame of the dialog produced.
#[derive(Default)]
struct Drawn {
    choice: Option<EndChoice>,
    /// Height the content took, in points — what the window is sized to.
    height: f32,
}

fn draw(ui: &mut egui::Ui, info: &EndInfo, details_open: &mut bool) -> Drawn {
    use crate::shell::widgets;
    use egui::{CornerRadius, Frame, Margin, RichText, Stroke};
    let mut choice = None;
    let lost = info.outcome.unexpected();
    let outer = Frame::new()
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
                        EndOutcome::Lost(lost) => {
                            ui.label(
                                RichText::new(format!("Session to {} lost", info.session_name))
                                    .font(theme::sans_semibold(16.0))
                                    .color(theme::TEXT_PRIMARY),
                            );
                            // One classified sentence (`disconnect::LostSession`), not
                            // the raw chain: the reader has lost their desktop and
                            // needs to know what happened and whether to reconnect.
                            ui.label(
                                RichText::new(&lost.detail)
                                    .font(theme::sans(13.0))
                                    .color(theme::TEXT_SECONDARY),
                            );
                            technical_details(ui, &lost.technical, details_open);
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
    Drawn {
        choice,
        height: outer.response.rect.height(),
    }
}

/// The error chain, behind a disclosure that starts closed.
///
/// Closed because it is written for us, not for the person reading it, and shown at
/// all because it is what a bug report is made of. The window follows the disclosure
/// ([`EndApp::follow_content`]); past [`REASON_MAX_HEIGHT`] the chain scrolls, so even
/// an unbounded one cannot push the footer off the bottom.
fn technical_details(ui: &mut egui::Ui, technical: &str, open: &mut bool) {
    // No animation: the window is re-sized to whatever this lays out, and an animated
    // open would drag the window through a dozen intermediate heights on its way.
    ui.style_mut().animation_time = 0.0;
    let header = egui::CollapsingHeader::new(
        egui::RichText::new("Technical details")
            .font(theme::sans(12.0))
            .color(theme::TEXT_MUTED),
    )
    .id_salt("end_dialog_technical")
    .open(Some(*open))
    .show(ui, |ui| {
        egui::ScrollArea::vertical()
            .max_height(REASON_MAX_HEIGHT)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(technical)
                        .font(theme::mono(11.0))
                        .color(theme::TEXT_MUTED),
                );
            });
    });
    // The state is ours, not egui's (`.open(Some(..))` forces what it draws), so the
    // click has to be applied here or the disclosure never moves.
    if header.header_response.clicked() {
        *open = !*open;
    }
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
    use ironrdp::pdu::rdp::server_error_info::{ErrorInfo, ProtocolIndependentCode};
    use ironrdp::session::GracefulDisconnectReason;

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
    /// text it drew with the rect that text is *visible* in — the galley clipped to its
    /// own clip rect, so text a scroll area has scrolled out of view is not counted as
    /// having overflowed the window.
    fn frame(size: [f32; 2], info: &EndInfo) -> Vec<(String, egui::Rect)> {
        frame_with_details(size, info, false)
    }

    /// As [`frame`], with the Technical details disclosure forced open or closed.
    fn frame_with_details(
        size: [f32; 2],
        info: &EndInfo,
        details_open: bool,
    ) -> Vec<(String, egui::Rect)> {
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
        let mut open = details_open;
        let output = ctx.run_ui(input, |ui| {
            choice = draw(ui, info, &mut open).choice;
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
                let visible = egui::Rect::from_min_size(egui::pos2(min_x, t.pos.y), size)
                    .intersect(clipped.clip_rect);
                if visible.is_positive() {
                    texts.push((t.galley.text().to_owned(), visible));
                }
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
    /// Only the overflow half of the fit is asserted. The window height is floored at
    /// the mock's designed size, so a short message legitimately leaves background
    /// below it.
    fn assert_fits(texts: &[(String, egui::Rect)], size: [f32; 2]) {
        let window = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(size[0], size[1]));
        for (text, rect) in texts {
            assert!(
                window.contains_rect(*rect),
                "{text:?} at {rect:?} falls outside the {size:?} window"
            );
        }
    }

    /// The floor is a floor, not a size: every ending a user actually meets is short,
    /// so each one must open at the height its own content measured. The mock's 300 px
    /// left the commonest dialog of all — a clean server logoff — with a third of its
    /// window empty below the buttons.
    #[test]
    fn no_ordinary_ending_opens_with_dead_space_below_its_buttons() {
        let cases = [
            info(EndOutcome::Ended),
            lost(crate::connect::ConnectError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "connection reset by peer",
            ))),
            info(EndOutcome::ServerEnded(
                crate::disconnect::classify(&GracefulDisconnectReason::ErrorInfo(
                    ErrorInfo::ProtocolIndependentCode(ProtocolIndependentCode::LogoffByUser),
                ))
                .expect("a logoff is a server farewell"),
            )),
        ];
        for info in &cases {
            let size = window_size(info);
            assert_eq!(
                size[1],
                measured_height(info, size[0], false),
                "the floor padded a {size:?} window past its own content"
            );
            assert!(size[1] <= 220.0, "a short ending opened at {size:?}");
        }
    }

    /// The spec's own sentence for the commonest ending runs to three dialog lines and
    /// leads with a label about MS-RDPBCGR's tables. Ours says the same thing in one.
    #[test]
    fn a_user_logoff_reads_as_one_short_line() {
        let farewell = crate::disconnect::classify(&GracefulDisconnectReason::ErrorInfo(
            ErrorInfo::ProtocolIndependentCode(ProtocolIndependentCode::LogoffByUser),
        ))
        .expect("a logoff is a server farewell");
        let info = info(EndOutcome::ServerEnded(farewell));
        let size = window_size(&info);
        let texts = frame(size, &info);
        let drawn: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        assert!(
            drawn.contains(&"User logged out of their session on the server"),
            "{drawn:?}"
        );
        assert_fits(&texts, size);
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
        let size = window_size(&info);
        let texts = frame(size, &info);
        let drawn: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        assert!(drawn.contains(&"Kiln is restarting"), "{drawn:?}");
        assert!(drawn.contains(&"Reconnect"), "{drawn:?}");
        assert_fits(&texts, size);
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
        let info = info(EndOutcome::ServerEnded(detail));
        let size = window_size(&info);
        let texts = frame(size, &info);
        assert_fits(&texts, size);
    }

    /// A lost session, classified the way the live path classifies it.
    fn lost(error: crate::connect::ConnectError) -> EndInfo {
        info(EndOutcome::Lost(LostSession::from_connect_error(&error)))
    }

    #[test]
    fn a_lost_session_still_fits_its_window() {
        let info = lost(crate::connect::ConnectError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "connection reset by peer",
        )));
        let size = window_size(&info);
        assert_eq!(size[0], WARN_WIDTH);
        let texts = frame(size, &info);
        let drawn: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        assert!(drawn.contains(&"Session to Kiln lost"), "{drawn:?}");
        assert_fits(&texts, size);
    }

    #[test]
    fn a_clean_end_still_fits_its_window() {
        let info = info(EndOutcome::Ended);
        let size = window_size(&info);
        assert_eq!(size[0], ENDED_WIDTH);
        let texts = frame(size, &info);
        let drawn: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        assert!(drawn.contains(&"Session Kiln ended"), "{drawn:?}");
        assert!(drawn.contains(&"12m 30s"), "{drawn:?}");
        assert_fits(&texts, size);
        assert!(drawn.contains(&"Save metrics JSON"), "{drawn:?}");
    }

    /// The live session's own failure (2026-08-19), classified: IronRDP reports one as
    /// a nested error chain with source locations, and this one wraps to ten lines. At
    /// the old fixed 300 px it pushed the entire footer — Reconnect included — off the
    /// bottom edge of a window nobody can resize, and every word of it was addressed to
    /// the wrong reader.
    fn real_decode_failure() -> EndInfo {
        lost(crate::disconnect::tests::real_decode_failure())
    }

    /// The bug this fixes (MDR-BUG-FLUX-00014): what the user is shown is a sentence,
    /// and no part of the chain — least of all the machine that compiled the binary —
    /// is on screen until they ask for it.
    #[test]
    fn a_closed_dialog_shows_a_sentence_and_no_chain() {
        let info = real_decode_failure();
        let texts = frame(window_size(&info), &info);
        let drawn: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        assert!(
            drawn.contains(
                &"The host sent something this client could not handle. \
                             The desktop is probably still alive on the host — \
                             reconnecting resumes it."
            ),
            "{drawn:?}"
        );
        assert!(drawn.contains(&"Technical details"), "{drawn:?}");
        // Nothing of the chain, by its own words rather than by the leaks the strip
        // already removes — this is the half that proves the disclosure starts closed.
        for hidden in ["Unknown GFX PDU type", "decode error", "PDU error"] {
            assert!(
                !drawn.iter().any(|t| t.contains(hidden)),
                "{hidden:?} is on screen with the disclosure closed: {drawn:?}"
            );
        }
    }

    /// Opening the disclosure is what the chain is for, and it must not cost the
    /// buttons — the window grows to carry it (MDR-BUG-FLUX-00013's rule, still).
    #[test]
    fn opening_the_details_grows_the_window_instead_of_losing_the_buttons() {
        let info = real_decode_failure();
        let width = window_width(&info.outcome);
        let closed = measured_height(&info, width, false);
        let open = measured_height(&info, width, true);
        assert!(
            open > closed,
            "the disclosure added no height: {closed} -> {open}"
        );
        let size = [width, fit_height(open)];
        let texts = frame_with_details(size, &info, true);
        let drawn: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        assert!(
            drawn.iter().any(|t| t.contains("Unknown GFX PDU type")),
            "{drawn:?}"
        );
        assert!(drawn.contains(&"Reconnect"), "{drawn:?}");
        assert!(drawn.contains(&"Close"), "{drawn:?}");
        assert_fits(&texts, size);
    }

    /// Nothing bounds the failure text, so the growth has to stop somewhere: past that
    /// the chain scrolls and the dialog still fits on a small display.
    #[test]
    fn a_runaway_chain_stops_growing_the_window() {
        let info = lost(crate::connect::ConnectError::Protocol(
            "connection reset by peer. ".repeat(400),
        ));
        let width = window_width(&info.outcome);
        let size = [width, fit_height(measured_height(&info, width, true))];
        assert!(size[1] <= MAX_HEIGHT, "{size:?} exceeds the height cap");
        let texts = frame_with_details(size, &info, true);
        // The chain itself runs to thousands of characters, so the buttons are
        // reported by presence alone — printing `drawn` here would bury the failure.
        let drawn: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        assert!(
            drawn.contains(&"Reconnect"),
            "no Reconnect button in a {size:?} window"
        );
        assert!(
            drawn.contains(&"Close"),
            "no Close button in a {size:?} window"
        );
        assert_fits(&texts, size);
    }
}
