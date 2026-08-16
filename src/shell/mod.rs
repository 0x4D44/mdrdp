//! The launcher shell: the 900×700 egui window that fronts mdrdp.
//!
//! Hosted by eframe per the 2026-08-16 HLD; the handoff README in `wrk_docs/` owns
//! every visual value, read through `crate::ui::theme` tokens. This module carries the
//! window, the native menus (muda), the connections list, and the process bookkeeping
//! for spawned sessions. The wizard, settings, and dialogs land in sibling modules as
//! they are built.
//!
//! Sessions stay one-process-per-session: connecting spawns this same binary with the
//! favourite's name, exactly as the previous launcher did — but the child handles are
//! kept, because the list shows which favourites have a session running and the Quit
//! dialog must name them.

pub mod dialogs;
pub mod settings_ui;
pub mod widgets;
pub mod wizard;

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Instant;

use crate::favourites::{Favourite, Favourites, WindowSize};
use crate::ui::theme;
use egui::{Align2, CornerRadius, Frame, Margin, RichText, Sense, Stroke, vec2};

/// Fixed shell geometry from the handoff: every window is 900×700 and the chrome rows
/// sum exactly to 700 (menu strip 30 + content + status bar 38).
const WINDOW_SIZE: [f32; 2] = [900.0, 700.0];
const MENU_STRIP_H: f32 = 30.0;
const STATUS_BAR_H: f32 = 38.0;
const ROW_H: f32 = 64.0;

/// A session process this launcher started and still watches.
pub struct SessionHandle {
    pub name: String,
    pub child: Child,
    pub started: Instant,
}

/// One line of the child's `--stage-json` stdout protocol, decoded.
enum ChildEvent {
    Stage {
        name: String,
        elapsed_ms: u64,
        qualifier: Option<String>,
    },
    Connected,
    Failed(String),
    /// The child hit a first-sight certificate and waits on our decision.
    CertPrompt(dialogs::CertPromptInfo),
    /// The pipe closed without a terminal event: the child died mid-connect.
    Eof,
}

/// A connect in progress (or failed), driving the Connecting modal.
struct ConnectFlow {
    name: String,
    host: String,
    port: u16,
    account: String,
    child: Option<Child>,
    /// The child's stdin — the return path for a certificate decision.
    child_stdin: Option<std::process::ChildStdin>,
    rx: std::sync::mpsc::Receiver<ChildEvent>,
    arrived: Vec<(String, u64, Option<String>)>,
    started: Instant,
    failure: Option<dialogs::ConnectFailure>,
    cert_prompt: Option<dialogs::CertPromptInfo>,
}

/// Which modal sits over the list, if any. Placeholder variants fill in as their
/// units land.
enum Modal {
    /// Settings — arrives with the settings-store unit.
    Settings,
    /// About — arrives with the dialogs unit.
    About,
}

/// The launcher application state, per the handoff's State section.
pub struct LauncherApp {
    favourites: Favourites,
    config_path: PathBuf,
    selected: Option<usize>,
    running: Vec<SessionHandle>,
    /// `Some` while the wizard is up — the front page on an empty favourites list,
    /// or opened with `N` / New connection.
    wizard: Option<wizard::Wizard>,
    /// Default account from Settings, seeding the wizard's sign-in step.
    default_username: Option<String>,
    modal: Option<Modal>,
    connect_flow: Option<ConnectFlow>,
    menu: menus::LauncherMenu,
}

impl LauncherApp {
    fn new(
        favourites: Favourites,
        config_path: PathBuf,
        default_username: Option<String>,
        menu: menus::LauncherMenu,
    ) -> Self {
        LauncherApp {
            selected: if favourites.is_empty() { None } else { Some(0) },
            // The front page chooses itself: no favourites means the wizard.
            wizard: favourites
                .is_empty()
                .then(|| wizard::Wizard::new(default_username.clone())),
            favourites,
            config_path,
            running: Vec::new(),
            default_username,
            modal: None,
            connect_flow: None,
            menu,
        }
    }

    fn open_wizard(&mut self) {
        if self.wizard.is_none() {
            self.wizard = Some(wizard::Wizard::new(self.default_username.clone()));
        }
    }

    /// Forget children that have exited so the list and status bar stay truthful.
    fn prune_running(&mut self) {
        self.running
            .retain_mut(|s| matches!(s.child.try_wait(), Ok(None)));
    }

    /// The running-session entry for a favourite name, if any.
    fn running_for(&self, name: &str) -> Option<&SessionHandle> {
        self.running.iter().find(|s| s.name == name)
    }

    /// Start connecting the favourite at `index`, showing the Connecting modal.
    ///
    /// The child owns the socket and the logon (one process per session); its
    /// `--stage-json` stdout drives the modal, so the logon happens exactly once.
    fn connect(&mut self, index: usize, ctx: &egui::Context) {
        if self.connect_flow.is_some() {
            return; // One connect at a time; the modal owns the screen anyway.
        }
        let Some(f) = self.favourites.iter().nth(index) else {
            return;
        };
        let name = f.name.clone();
        let host = f.host.clone();
        let port = f.port;
        let account = f
            .username
            .clone()
            .unwrap_or_else(|| "(no account)".to_owned());
        match spawn_connect(&name, &[], ctx.clone()) {
            Ok((mut child, rx)) => {
                let child_stdin = child.stdin.take();
                self.connect_flow = Some(ConnectFlow {
                    name,
                    host,
                    port,
                    account,
                    child_stdin,
                    child: Some(child),
                    rx,
                    arrived: Vec::new(),
                    started: Instant::now(),
                    failure: None,
                    cert_prompt: None,
                });
            }
            Err(e) => eprintln!("{e}"),
        }
    }

    /// Drain child events and draw the Connecting modal / failure dialog.
    fn connect_flow_ui(&mut self, ctx: &egui::Context) {
        let Some(mut flow) = self.connect_flow.take() else {
            return;
        };
        // Drain whatever the reader thread queued since the last frame.
        while let Ok(event) = flow.rx.try_recv() {
            match event {
                ChildEvent::Stage {
                    name,
                    elapsed_ms,
                    qualifier,
                } => flow.arrived.push((name, elapsed_ms, qualifier)),
                ChildEvent::Connected => {
                    let child = flow.child.take().expect("child present until terminal");
                    self.favourites.touch(&flow.name);
                    if let Err(e) = self.favourites.save_to(&self.config_path) {
                        eprintln!("warning: could not record last-used time: {e}");
                    }
                    self.running.push(SessionHandle {
                        name: flow.name.clone(),
                        child,
                        started: Instant::now(),
                    });
                    return; // Modal closes; the launcher stays open.
                }
                ChildEvent::Failed(error) => {
                    reap(flow.child.take());
                    flow.failure = Some(dialogs::ConnectFailure {
                        kind: dialogs::classify(&error),
                        host: flow.host.clone(),
                        port: flow.port,
                        account: flow.account.clone(),
                        error,
                    });
                }
                ChildEvent::CertPrompt(info) => flow.cert_prompt = Some(info),
                ChildEvent::Eof => {
                    if flow.failure.is_none() {
                        reap(flow.child.take());
                        flow.failure = Some(dialogs::ConnectFailure {
                            kind: dialogs::FailureKind::Other,
                            host: flow.host.clone(),
                            port: flow.port,
                            account: flow.account.clone(),
                            error: "the session process ended unexpectedly".to_owned(),
                        });
                    }
                }
            }
        }

        if flow.failure.is_none()
            && let Some(info) = flow.cert_prompt.clone()
        {
            match dialogs::first_connection(ctx, &info) {
                dialogs::CertPromptAction::None => {}
                decision => {
                    let word = match decision {
                        dialogs::CertPromptAction::PinAndConnect => "pin",
                        dialogs::CertPromptAction::ConnectOnce => "once",
                        _ => "reject",
                    };
                    if let Some(stdin) = flow.child_stdin.as_mut() {
                        use std::io::Write as _;
                        let line = format!("{}\n", serde_json::json!({ "decision": word }));
                        if stdin.write_all(line.as_bytes()).is_err() {
                            eprintln!("could not answer the certificate prompt");
                        }
                        let _ = stdin.flush();
                    }
                    flow.cert_prompt = None;
                    if decision == dialogs::CertPromptAction::Reject {
                        // The child fails its connect and reports; the failure
                        // dialog (or plain closure) follows from its own event.
                        reap(flow.child.take());
                        return;
                    }
                }
            }
            self.connect_flow = Some(flow);
            return;
        }

        match &flow.failure {
            None => {
                let rows = dialogs::stage_rows(&flow.arrived);
                let elapsed = u64::try_from(flow.started.elapsed().as_millis()).unwrap_or(0);
                let action = dialogs::connecting(ctx, &flow.name, &rows, elapsed);
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                if action == dialogs::ConnectDialogAction::Cancel {
                    if let Some(mut child) = flow.child.take() {
                        // Pre-logon abort: nothing exists on the host yet to abandon.
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    return;
                }
                self.connect_flow = Some(flow);
            }
            Some(failure) if failure.kind == dialogs::FailureKind::CertificateChanged => {
                if !dialogs::certificate_changed(ctx, failure) {
                    self.connect_flow = Some(flow);
                }
            }
            Some(failure) => match dialogs::connect_failed(ctx, failure) {
                dialogs::ConnectDialogAction::Retry => {
                    let index = self.favourites.iter().position(|f| f.name == flow.name);
                    if let Some(i) = index {
                        self.connect(i, ctx);
                    }
                }
                dialogs::ConnectDialogAction::TryDefaultPort => {
                    if let Ok((mut child, rx)) = spawn_connect(
                        &flow.name,
                        &["--port".to_owned(), "3389".to_owned()],
                        ctx.clone(),
                    ) {
                        let child_stdin = child.stdin.take();
                        self.connect_flow = Some(ConnectFlow {
                            child: Some(child),
                            child_stdin,
                            rx,
                            arrived: Vec::new(),
                            started: Instant::now(),
                            failure: None,
                            cert_prompt: None,
                            port: 3389,
                            ..flow
                        });
                    }
                }
                dialogs::ConnectDialogAction::ChangePassword => {
                    // The credential dialogs land in a later unit.
                    eprintln!("change-password flow is not built yet");
                }
                dialogs::ConnectDialogAction::Close | dialogs::ConnectDialogAction::Cancel => {}
                dialogs::ConnectDialogAction::None => self.connect_flow = Some(flow),
            },
        }
    }

    fn handle_menu_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = muda::MenuEvent::receiver().try_recv() {
            match self.menu.action_for(event.id()) {
                Some(menus::MenuAction::NewConnection) => self.open_wizard(),
                Some(menus::MenuAction::Settings) => self.modal = Some(Modal::Settings),
                Some(menus::MenuAction::About) => self.modal = Some(Modal::About),
                Some(menus::MenuAction::RevealFavourites) => reveal(&self.config_path),
                Some(menus::MenuAction::Connect) => {
                    if let Some(i) = self.selected {
                        self.connect(i, ctx);
                    }
                }
                Some(menus::MenuAction::CloseWindow) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                None => {}
            }
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if self.modal.is_some() || self.wizard.is_some() || self.connect_flow.is_some() {
            return; // Each surface owns its own keys.
        }
        let (enter, up, down, n) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::N),
            )
        });
        let len = self.favourites.len();
        if len > 0 {
            if down {
                self.selected = Some(self.selected.map_or(0, |s| (s + 1).min(len - 1)));
            }
            if up {
                self.selected = Some(self.selected.map_or(0, |s| s.saturating_sub(1)));
            }
            if enter && let Some(i) = self.selected {
                self.connect(i, ctx);
            }
        }
        if n {
            self.open_wizard();
        }
    }

    // --- chrome ---------------------------------------------------------------------

    /// The 30px strip: wordmark only — menu *items* live in the native menu bar on
    /// both platforms (HLD decision 3), so the strip is chrome, not a widget.
    fn menu_strip(&self, ui: &mut egui::Ui) {
        egui::Panel::top("menu-strip")
            .exact_size(MENU_STRIP_H)
            .frame(
                Frame::new()
                    .fill(theme::BG_CHROME)
                    .inner_margin(Margin::symmetric(10, 0)),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(
                        RichText::new("MDRDP")
                            .font(theme::mono_semibold(11.0))
                            .color(theme::ACCENT)
                            .extra_letter_spacing(11.0 * 0.14),
                    );
                });
                let rect = ui.max_rect();
                ui.painter().hline(
                    rect.x_range().expand(10.0),
                    rect.bottom(),
                    Stroke::new(1.0, theme::LINE_HAIR),
                );
            });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status-bar")
            .exact_size(STATUS_BAR_H)
            .frame(
                Frame::new()
                    .fill(theme::BG_CHROME)
                    .inner_margin(Margin::symmetric(28, 0)),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                let rect = ui.max_rect();
                ui.painter().hline(
                    rect.x_range().expand(28.0),
                    rect.top(),
                    Stroke::new(1.0, theme::LINE_HAIR),
                );
                ui.horizontal_centered(|ui| {
                    ui.label(
                        RichText::new(display_path(&self.config_path))
                            .font(theme::mono(11.0))
                            .color(theme::TEXT_DIM),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let n = self.running.len();
                        let caption = match n {
                            1 => "1 session running".to_owned(),
                            n => format!("{n} sessions running"),
                        };
                        ui.label(
                            RichText::new(caption)
                                .font(theme::mono(11.0))
                                .color(theme::TEXT_DIM),
                        );
                    });
                });
            });
    }

    // --- connections list -------------------------------------------------------------

    fn connections_page(&mut self, ui: &mut egui::Ui) {
        let mut connect_row: Option<usize> = None;
        egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::BG_WINDOW))
            .show(ui, |ui| {
                // Header: padding 26px 28px 18px.
                Frame::new()
                    .inner_margin(Margin {
                        left: 28,
                        right: 28,
                        top: 26,
                        bottom: 18,
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 6.0;
                                ui.label(
                                    RichText::new("Connections")
                                        .font(theme::sans_semibold(20.0))
                                        .color(theme::TEXT_PRIMARY),
                                );
                                ui.label(
                                    RichText::new(
                                        "Double-click to connect · Enter opens the selection · \
                                         sessions run independently",
                                    )
                                    .font(theme::sans(12.0))
                                    .color(theme::TEXT_MUTED),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Max), |ui| {
                                ui.spacing_mut().item_spacing.x = 10.0;
                                if widgets::primary_button(ui, "New connection", 32.0).clicked() {
                                    self.open_wizard();
                                }
                                if widgets::secondary_button(ui, "Edit", 32.0).clicked()
                                    && self.selected.is_some()
                                {
                                    // Edit dialog arrives with the dialogs unit.
                                }
                            });
                        });
                    });

                // List: padding 0 28px 20px, rows 64px with 8px gaps.
                Frame::new()
                    .inner_margin(Margin {
                        left: 28,
                        right: 28,
                        top: 0,
                        bottom: 20,
                    })
                    .show(ui, |ui| {
                        if self.favourites.is_empty() {
                            self.empty_state(ui);
                            return;
                        }
                        ui.spacing_mut().item_spacing.y = 8.0;
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 8.0;
                            let favourites: Vec<Favourite> =
                                self.favourites.iter().cloned().collect();
                            for (i, f) in favourites.iter().enumerate() {
                                let running = self.running_for(&f.name).map(|s| s.started);
                                let response = self.list_row(ui, i, f, running);
                                if response.clicked() {
                                    self.selected = Some(i);
                                }
                                if response.double_clicked() {
                                    self.selected = Some(i);
                                    connect_row = Some(i);
                                }
                            }
                        });
                    });
            });
        if let Some(i) = connect_row {
            let ctx = ui.ctx().clone();
            self.connect(i, &ctx);
        }
    }

    /// One 64px list row, painted to the handoff spec. Returns its click response.
    fn list_row(
        &self,
        ui: &mut egui::Ui,
        index: usize,
        f: &Favourite,
        running_since: Option<Instant>,
    ) -> egui::Response {
        let selected = self.selected == Some(index);
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
        let painter = ui.painter();
        let (fill, stroke) = if selected {
            (theme::BG_ROW_SELECTED, theme::LINE_SUBTLE)
        } else {
            (theme::BG_RAISED, theme::LINE_HAIR)
        };
        painter.rect(
            rect,
            CornerRadius::same(theme::radius::CARD),
            fill,
            Stroke::new(1.0, stroke),
            egui::StrokeKind::Inside,
        );
        if selected {
            // 2px accent left border.
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min, vec2(2.0, rect.height())),
                CornerRadius::ZERO,
                theme::ACCENT,
            );
        }
        let left = rect.min.x + 18.0;
        painter.text(
            egui::pos2(left, rect.min.y + 14.0),
            Align2::LEFT_TOP,
            &f.name,
            theme::sans_medium(14.0),
            theme::TEXT_PRIMARY,
        );
        painter.text(
            egui::pos2(left, rect.max.y - 14.0),
            Align2::LEFT_BOTTOM,
            row_subtitle(f),
            theme::mono(12.0),
            theme::TEXT_MUTED,
        );
        let right = rect.max.x - 18.0;
        match running_since {
            Some(started) => {
                painter.circle_filled(egui::pos2(right - 3.0, rect.center().y), 3.0, theme::ACCENT);
                let mins = started.elapsed().as_secs() / 60;
                painter.text(
                    egui::pos2(right - 14.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    format!("connected {mins}m"),
                    theme::mono(11.0),
                    theme::ACCENT,
                );
            }
            None => {
                painter.text(
                    egui::pos2(right, rect.center().y),
                    Align2::RIGHT_CENTER,
                    last_used_caption(f.last_used),
                    theme::mono(11.0),
                    theme::TEXT_DIM,
                );
            }
        }
        response
    }

    /// The cancelled-first-run state: an empty list that still teaches the CLI path.
    fn empty_state(&self, ui: &mut egui::Ui) {
        ui.add_space(120.0);
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.label(
                RichText::new("No saved connections")
                    .font(theme::sans_semibold(16.0))
                    .color(theme::TEXT_PRIMARY),
            );
            ui.label(
                RichText::new("Press N for the wizard — or skip it entirely:")
                    .font(theme::sans(13.0))
                    .color(theme::TEXT_MUTED),
            );
            ui.label(
                RichText::new("$ mdrdp <host>")
                    .font(theme::mono(13.0))
                    .color(theme::TEXT_SECONDARY),
            );
        });
    }

    // --- placeholders until their units land ------------------------------------------

    fn wizard_page(&mut self, ui: &mut egui::Ui) {
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        let outcome = egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::BG_WINDOW))
            .show(ui, |ui| wizard.ui(ui))
            .inner;
        match outcome {
            wizard::WizardOutcome::None => {}
            wizard::WizardOutcome::Cancelled => self.wizard = None,
            wizard::WizardOutcome::Finished {
                favourite,
                save_favourite,
                password,
                save_password,
            } => {
                self.wizard = None;
                // Password first: the favourite's keychain_account points at this
                // entry, so it should exist before anything tries to read it.
                if save_password && let Some(p) = password {
                    let account = favourite
                        .keychain_account
                        .clone()
                        .unwrap_or_else(|| favourite.name.clone());
                    if let Err(e) = crate::creds::store(&account, p.expose()) {
                        eprintln!("could not store the password: {e}");
                    }
                }
                let ctx = ui.ctx().clone();
                if save_favourite {
                    let name = favourite.name.clone();
                    match self.favourites.add(favourite) {
                        Ok(()) => {
                            if let Err(e) = self.favourites.save_to(&self.config_path) {
                                eprintln!("warning: could not save favourites: {e}");
                            }
                            let index = self.favourites.iter().position(|f| f.name == name);
                            if let Some(i) = index {
                                self.selected = Some(i);
                                self.connect(i, &ctx);
                            }
                        }
                        Err(e) => eprintln!("could not save the favourite: {e}"),
                    }
                } else {
                    // Not saved: target the host directly, carrying the typed fields.
                    let mut extra = vec!["--port".to_owned(), favourite.port.to_string()];
                    if let Some(user) = &favourite.username {
                        extra.push("--user".to_owned());
                        extra.push(user.clone());
                    }
                    if let Some(domain) = &favourite.domain {
                        extra.push("--domain".to_owned());
                        extra.push(domain.clone());
                    }
                    if let crate::favourites::WindowSize::Explicit { width, height } =
                        favourite.window_size
                    {
                        extra.push("--size".to_owned());
                        extra.push(format!("{width}x{height}"));
                    }
                    match spawn_connect(&favourite.host, &extra, ctx.clone()) {
                        Ok((mut child, rx)) => {
                            let child_stdin = child.stdin.take();
                            self.connect_flow = Some(ConnectFlow {
                                name: favourite.host.clone(),
                                host: favourite.host.clone(),
                                port: favourite.port,
                                account: favourite
                                    .username
                                    .clone()
                                    .unwrap_or_else(|| "(no account)".to_owned()),
                                child_stdin,
                                child: Some(child),
                                rx,
                                arrived: Vec::new(),
                                started: Instant::now(),
                                failure: None,
                                cert_prompt: None,
                            });
                        }
                        Err(e) => eprintln!("{e}"),
                    }
                }
            }
        }
    }

    fn modal_placeholder(&mut self, ctx: &egui::Context, title: &str) {
        let close = egui::Area::new(egui::Id::new("modal"))
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                Frame::new()
                    .fill(theme::BG_WINDOW)
                    .stroke(Stroke::new(1.0, theme::LINE_STRONG))
                    .corner_radius(CornerRadius::same(theme::radius::MODAL))
                    .inner_margin(Margin::same(24))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(title)
                                .font(theme::sans_semibold(16.0))
                                .color(theme::TEXT_PRIMARY),
                        );
                        ui.label(
                            RichText::new("Under construction on this branch.")
                                .font(theme::sans(13.0))
                                .color(theme::TEXT_MUTED),
                        );
                        ui.add_space(10.0);
                        widgets::secondary_button(ui, "Close", 34.0).clicked()
                    })
                    .inner
            })
            .inner;
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.modal = None;
        }
    }
}

impl eframe::App for LauncherApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.prune_running();
        self.handle_menu_events(&ctx);
        self.handle_keys(&ctx);

        self.menu_strip(ui);
        self.status_bar(ui);
        if self.wizard.is_some() {
            self.wizard_page(ui);
        } else {
            self.connections_page(ui);
        }
        match self.modal {
            Some(Modal::Settings) => self.modal_placeholder(&ctx, "Settings"),
            Some(Modal::About) => self.modal_placeholder(&ctx, "About mdrdp"),
            None => {}
        }
        self.connect_flow_ui(&ctx);

        if !self.running.is_empty() {
            // The "connected Xm" captions and liveness pruning need a clock; one
            // repaint a second is plenty and only while sessions run.
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }
}

/// Run the launcher shell. Blocks until the window closes; children keep running.
pub fn run(
    favourites: Favourites,
    config_path: PathBuf,
    default_username: Option<String>,
) -> Result<(), String> {
    let icon = window_icon();
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(WINDOW_SIZE)
        .with_min_inner_size(WINDOW_SIZE)
        .with_max_inner_size(WINDOW_SIZE)
        .with_resizable(false)
        .with_title("mdrdp");
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "mdrdp",
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            let menu = menus::LauncherMenu::install(cc);
            Ok(Box::new(LauncherApp::new(
                favourites,
                config_path,
                default_username,
                menu,
            )))
        }),
    )
    .map_err(|e| format!("launcher window failed: {e}"))
}

/// Decode the embedded window icon. `None` on decode failure — cosmetic, never fatal.
fn window_icon() -> Option<egui::IconData> {
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

/// Launch a connecting session child with `--stage-json`, and a reader thread that
/// decodes its stdout protocol into `ChildEvent`s.
///
/// `--` first: a favourite legitimately named like a flag must still parse as a target.
/// The reader stays alive until the child's stdout closes — holding the pipe open for
/// the child's whole life means a late stray print can never hit a closed pipe.
fn spawn_connect(
    name: &str,
    extra_args: &[String],
    ctx: egui::Context,
) -> Result<(Child, std::sync::mpsc::Receiver<ChildEvent>), String> {
    use std::io::BufRead as _;
    let exe = std::env::current_exe().map_err(|e| format!("cannot find own path: {e}"))?;
    let mut child = Command::new(&exe)
        .arg("--")
        .arg(name)
        .arg("--stage-json")
        .args(extra_args)
        .stdout(std::process::Stdio::piped())
        // Stdin is the certificate-decision return path; unused otherwise.
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start a session for {name:?}: {e}"))?;
    let stdout = child.stdout.take().expect("stdout was requested piped");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let event = match v["event"].as_str() {
                Some("stage") => ChildEvent::Stage {
                    name: v["state"].as_str().unwrap_or("?").to_owned(),
                    elapsed_ms: v["elapsed_ms"].as_f64().unwrap_or(0.0).round() as u64,
                    qualifier: v["qualifier"].as_str().map(str::to_owned),
                },
                Some("connected") => ChildEvent::Connected,
                Some("failed") => {
                    ChildEvent::Failed(v["error"].as_str().unwrap_or("unknown error").to_owned())
                }
                Some("cert_prompt") => ChildEvent::CertPrompt(dialogs::CertPromptInfo {
                    host: v["host"].as_str().unwrap_or("?").to_owned(),
                    fingerprint: v["fingerprint"].as_str().unwrap_or("").to_owned(),
                    store_path: v["store_path"].as_str().unwrap_or("").to_owned(),
                }),
                _ => continue,
            };
            if tx.send(event).is_err() {
                return; // The flow is gone (cancelled); nothing to report to.
            }
            ctx.request_repaint();
        }
        let _ = tx.send(ChildEvent::Eof);
        ctx.request_repaint();
    });
    Ok((child, rx))
}

/// Wait out a child that failed to connect, off the UI thread, so it never zombies.
fn reap(child: Option<Child>) {
    if let Some(mut child) = child {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Reveal the favourites file in the platform file manager. Best-effort.
fn reveal(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let result = Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = Command::new("xdg-open")
        .arg(path.parent().unwrap_or(path))
        .spawn();
    if let Err(e) = result {
        eprintln!("could not reveal {}: {e}", path.display());
    }
}

/// Home-relative rendering of the config path, as the mock's status bar shows it.
fn display_path(path: &std::path::Path) -> String {
    let text = path.display().to_string();
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy().to_string();
        if let Some(rest) = text.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    text
}

/// `user @ host[:port] · window size`, the row subtitle from the mock.
pub fn row_subtitle(f: &Favourite) -> String {
    let host = if f.port == crate::favourites::DEFAULT_PORT {
        f.host.clone()
    } else {
        format!("{}:{}", f.host, f.port)
    };
    let account = match (&f.domain, &f.username) {
        (Some(d), Some(u)) => format!("{d}\\{u}"),
        (None, Some(u)) => u.clone(),
        _ => "(no account)".to_owned(),
    };
    let size = match f.window_size {
        WindowSize::Fullscreen => "fullscreen".to_owned(),
        WindowSize::Explicit { width, height } => format!("{width}×{height}"),
    };
    format!("{account} @ {host} · {size}")
}

/// The `last used …` caption: relative wording, coarse on purpose.
pub fn last_used_caption(last_used: Option<u64>) -> String {
    let Some(then) = last_used else {
        return "never used".to_owned();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let ago = now.saturating_sub(then);
    let caption = match ago {
        0..=89 => "just now".to_owned(),
        90..=5399 => format!("{}m ago", ago / 60),
        5400..=129_599 => format!("{}h ago", ago / 3600),
        129_600..=1_209_599 => format!("{}d ago", ago / 86_400),
        _ => format!("{}w ago", ago / 604_800),
    };
    format!("last used {caption}")
}

mod menus {
    //! Native menus via muda: macOS system menu bar, Windows window menu bar.

    use muda::{Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};

    /// What a menu item asks the app to do.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum MenuAction {
        NewConnection,
        Settings,
        About,
        RevealFavourites,
        Connect,
        CloseWindow,
    }

    /// The installed launcher menu. Holds the muda objects alive — dropping a `Menu`
    /// tears the native bar down.
    pub struct LauncherMenu {
        _menu: Menu,
        ids: Vec<(MenuId, MenuAction)>,
    }

    impl LauncherMenu {
        /// Build and attach the launcher menu bar.
        ///
        /// Launcher menus per the handoff: File · Connection · View · Help. Items the
        /// current build cannot honour yet (Import, sort toggles) are omitted rather
        /// than shipped dead; they arrive with their features.
        pub fn install(cc: &eframe::CreationContext<'_>) -> LauncherMenu {
            let menu = Menu::new();
            let mut ids = Vec::new();

            let mut item = |label: &str, action: MenuAction| -> MenuItem {
                let item = MenuItem::new(label, true, None);
                ids.push((item.id().clone(), action));
                item
            };

            #[cfg(target_os = "macos")]
            {
                let app = Submenu::new("mdrdp", true);
                let about = item("About mdrdp", MenuAction::About);
                let settings = item("Settings…", MenuAction::Settings);
                let _ = app.append_items(&[
                    &about,
                    &PredefinedMenuItem::separator(),
                    &settings,
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::quit(None),
                ]);
                let _ = menu.append(&app);
            }

            let file = Submenu::new("File", true);
            let new_connection = item("New connection…", MenuAction::NewConnection);
            let reveal = item("Reveal favourites.toml", MenuAction::RevealFavourites);
            let close = item("Close window", MenuAction::CloseWindow);
            let _ = file.append_items(&[
                &new_connection,
                &PredefinedMenuItem::separator(),
                &reveal,
                &PredefinedMenuItem::separator(),
                &close,
            ]);
            #[cfg(not(target_os = "macos"))]
            {
                let settings = item("Settings…", MenuAction::Settings);
                let about = item("About mdrdp", MenuAction::About);
                let _ = file.append_items(&[&PredefinedMenuItem::separator(), &settings, &about]);
            }
            let _ = menu.append(&file);

            let connection = Submenu::new("Connection", true);
            let connect = item("Connect", MenuAction::Connect);
            let _ = connection.append_items(&[&connect]);
            let _ = menu.append(&connection);

            let view = Submenu::new("View", true);
            let _ = menu.append(&view);
            let help = Submenu::new("Help", true);
            let _ = menu.append(&help);

            #[cfg(target_os = "macos")]
            {
                let _ = cc;
                menu.init_for_nsapp();
            }
            #[cfg(target_os = "windows")]
            {
                use eframe::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                if let Ok(handle) = cc.window_handle()
                    && let RawWindowHandle::Win32(h) = handle.as_raw()
                {
                    let hwnd = h.hwnd.get();
                    // SAFETY: the HWND comes from the live eframe window on this thread.
                    unsafe {
                        let _ = menu.init_for_hwnd(hwnd);
                    }
                }
            }

            LauncherMenu { _menu: menu, ids }
        }

        pub fn action_for(&self, id: &MenuId) -> Option<MenuAction> {
            self.ids
                .iter()
                .find(|(known, _)| known == id)
                .map(|(_, action)| *action)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn favourite() -> Favourite {
        Favourite {
            username: Some("alice".into()),
            window_size: WindowSize::Explicit {
                width: 1600,
                height: 1000,
            },
            ..Favourite::new("Quench", "quench")
        }
    }

    #[test]
    fn the_row_subtitle_matches_the_mock_format() {
        let mut f = favourite();
        f.port = 3391;
        assert_eq!(row_subtitle(&f), "alice @ quench:3391 · 1600×1000");
    }

    #[test]
    fn the_default_port_is_omitted_and_fullscreen_is_named() {
        let mut f = favourite();
        f.window_size = WindowSize::Fullscreen;
        assert_eq!(row_subtitle(&f), "alice @ quench · fullscreen");
    }

    #[test]
    fn a_domain_account_renders_backslashed() {
        let mut f = favourite();
        f.domain = Some("CORP".into());
        f.host = "10.0.4.19".into();
        f.window_size = WindowSize::Fullscreen;
        assert_eq!(row_subtitle(&f), "CORP\\alice @ 10.0.4.19 · fullscreen");
    }

    #[test]
    fn last_used_captions_scale_with_age() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(last_used_caption(None), "never used");
        assert_eq!(last_used_caption(Some(now - 10)), "last used just now");
        assert_eq!(last_used_caption(Some(now - 600)), "last used 10m ago");
        assert_eq!(last_used_caption(Some(now - 7200)), "last used 2h ago");
        assert_eq!(
            last_used_caption(Some(now - 3 * 86_400)),
            "last used 3d ago"
        );
        assert_eq!(
            last_used_caption(Some(now - 21 * 86_400)),
            "last used 3w ago"
        );
    }
}
