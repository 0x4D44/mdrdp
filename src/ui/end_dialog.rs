//! The Session-ended and Session-lost dialogs (handoff §7), shown by the session
//! process after its window closes.
//!
//! Runs a second on-demand cycle on the process's one event loop, hosting a single
//! egui window via [`crate::ui::egui_host::AuxWindow`]. Skipped entirely on the
//! Cmd+Q path (the process is already gone) and for scripted runs.

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

/// What the dialog shows.
pub struct EndInfo {
    /// `None` = ended cleanly; `Some(reason)` = lost, with the failure text.
    pub lost: Option<String>,
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
}

impl ApplicationHandler<SessionEvent> for EndApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let (title, size) = match self.info.lost {
            Some(_) => ("Session lost", [480.0, 300.0]),
            None => ("Session ended", [460.0, 280.0]),
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

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: SessionEvent) {}

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.as_mut() else {
            return;
        };
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
    let mut app = EndApp {
        info,
        window: None,
        choice: None,
    };
    if let Err(e) = event_loop.run_app_on_demand(&mut app) {
        eprintln!("end dialog loop failed: {e}");
    }
    app.choice
}

fn draw(ui: &mut egui::Ui, info: &EndInfo) -> Option<EndChoice> {
    use crate::shell::widgets;
    use egui::{CornerRadius, Frame, Margin, RichText, Stroke};
    let mut choice = None;
    let lost = info.lost.is_some();
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
                    match &info.lost {
                        Some(reason) => {
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
                        None => {
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
}
