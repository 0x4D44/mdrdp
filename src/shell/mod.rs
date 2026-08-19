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
    /// The child has no stored password and waits on one.
    PasswordPrompt {
        account: String,
        reason: String,
    },
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
    password_prompt: Option<dialogs::PasswordPromptState>,
    /// The extra args this connect was spawned with, so a retry can repeat them.
    args: Vec<String>,
    /// Set on an Edit-password respawn: the child's password prompt then goes straight
    /// to the input dialog instead of the "No saved password" explainer.
    reprompt: bool,
}

/// Which modal sits over the list, if any.
enum Modal {
    Settings(Box<settings_ui::SettingsModal>),
    About,
    Shortcuts,
    Edit(dialogs::EditState),
    Remove(dialogs::RemoveState),
    /// Confirm quitting while sessions run.
    Quit,
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
    settings: crate::settings::Settings,
    settings_path: Option<PathBuf>,
    /// The About dialog's icon texture, loaded on first use.
    about_icon: Option<egui::TextureHandle>,
    /// Set once the Quit dialog has approved closing over running sessions.
    allow_close: bool,
}

impl LauncherApp {
    fn new(
        favourites: Favourites,
        config_path: PathBuf,
        settings: crate::settings::Settings,
        settings_path: Option<PathBuf>,
        menu: menus::LauncherMenu,
    ) -> Self {
        let default_username = settings.defaults.username.clone();
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
            settings,
            settings_path,
            about_icon: None,
            allow_close: false,
        }
    }

    fn open_settings(&mut self) {
        let known_hosts = crate::trust::KnownHosts::default_path().ok();
        self.modal = Some(Modal::Settings(Box::new(settings_ui::SettingsModal::new(
            self.settings.clone(),
            known_hosts,
        ))));
    }

    /// Open the Edit dialog for the selected favourite.
    fn open_edit(&mut self) {
        if let Some(i) = self.selected
            && let Some(f) = self.favourites.iter().nth(i)
        {
            self.modal = Some(Modal::Edit(dialogs::EditState::for_favourite(f)));
        }
    }

    fn open_remove(&mut self) {
        if let Some(i) = self.selected
            && let Some(f) = self.favourites.iter().nth(i)
        {
            self.modal = Some(Modal::Remove(dialogs::RemoveState {
                name: f.name.clone(),
                account: f.keychain_account.clone(),
                delete_password: false,
            }));
        }
    }

    /// Duplicate the selected favourite under a derived name.
    fn duplicate_selected(&mut self) {
        let Some(i) = self.selected else { return };
        let Some(f) = self.favourites.iter().nth(i) else {
            return;
        };
        let mut copy = f.clone();
        copy.last_used = None;
        let base = format!("{} copy", copy.name);
        let mut candidate = base.clone();
        let mut n = 2;
        while self.favourites.find(&candidate).is_some() {
            candidate = format!("{base} {n}");
            n += 1;
        }
        copy.name = candidate;
        if let Err(e) = self.favourites.add(copy) {
            eprintln!("could not duplicate: {e}");
            return;
        }
        if let Err(e) = self.favourites.save_to(&self.config_path) {
            eprintln!("warning: could not save favourites: {e}");
        }
    }

    /// Apply a saved edit: favourites entry plus any keychain moves.
    fn apply_edit(
        &mut self,
        original_name: &str,
        favourite: Favourite,
        password: Option<zeroize::Zeroizing<String>>,
    ) {
        let old_account = self
            .favourites
            .find(original_name)
            .and_then(|f| f.keychain_account.clone());
        let original = self.favourites.find(original_name);
        let last_used = original.and_then(|f| f.last_used);
        // The edit dialog does not expose the native-transport fields (config-file
        // only this tranche), so a save must carry them over, not reset them.
        let native = original.map(|f| f.native).unwrap_or_default();
        let ssh_user = original.and_then(|f| f.ssh_user.clone());
        let new_account = favourite.keychain_account.clone();
        let mut favourite = favourite;
        favourite.last_used = last_used;
        favourite.native = native;
        favourite.ssh_user = ssh_user;
        match self.favourites.update(original_name, favourite) {
            Ok(()) => {
                if let Err(e) = self.favourites.save_to(&self.config_path) {
                    eprintln!("warning: could not save favourites: {e}");
                }
                // Keychain moves: a typed replacement wins; otherwise a changed
                // account key migrates the stored password to the new key.
                match (password, &new_account) {
                    (Some(p), Some(account)) => {
                        if let Err(e) = crate::creds::store(account, &p) {
                            eprintln!("could not store the password: {e}");
                        }
                    }
                    (None, Some(account)) if old_account.as_deref() != Some(account) => {
                        if let Some(old) = &old_account {
                            match crate::creds::lookup(old) {
                                Ok(secret) => {
                                    if let Err(e) = crate::creds::store(account, secret.expose()) {
                                        eprintln!("could not move the password: {e}");
                                    } else if let Err(e) = crate::creds::forget(old) {
                                        eprintln!("could not remove the old entry: {e}");
                                    }
                                }
                                Err(e) => eprintln!("password not moved: {e}"),
                            }
                        }
                    }
                    _ => {}
                }
            }
            Err(e) => eprintln!("could not save the edit: {e}"),
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
                    password_prompt: None,
                    args: Vec::new(),
                    reprompt: false,
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
                ChildEvent::PasswordPrompt { account, reason } => {
                    flow.password_prompt = Some(dialogs::PasswordPromptState {
                        account,
                        reason,
                        // A reprompt already knows a password is saved and wrong; the
                        // explainer would claim none is stored. Straight to the input.
                        entering: flow.reprompt,
                        input: zeroize::Zeroizing::new(String::new()),
                        save: false,
                    });
                }
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

        if flow.failure.is_none() && flow.password_prompt.is_some() {
            let mut state = flow.password_prompt.take().expect("checked above");
            match dialogs::credential_dialog(ctx, &mut state) {
                dialogs::PasswordAction::None => {
                    flow.password_prompt = Some(state);
                }
                dialogs::PasswordAction::Connect => {
                    if state.save
                        && let Err(e) = crate::creds::store(&state.account, &state.input)
                    {
                        eprintln!("could not store the password: {e}");
                    }
                    if let Some(stdin) = flow.child_stdin.as_mut() {
                        use std::io::Write as _;
                        let mut line =
                            format!("{}\n", serde_json::json!({ "password": &*state.input }));
                        if stdin.write_all(line.as_bytes()).is_err() {
                            eprintln!("could not send the password to the session");
                        }
                        let _ = stdin.flush();
                        zeroize::Zeroize::zeroize(&mut line);
                    }
                    // state drops here; its Zeroizing buffer wipes the typed copy.
                }
                dialogs::PasswordAction::Cancel => {
                    if let Some(mut child) = flow.child.take() {
                        // Pre-logon: nothing exists on the host yet to abandon.
                        let _ = child.kill();
                        reap(Some(child));
                    }
                    return;
                }
            }
            self.connect_flow = Some(flow);
            return;
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
                    let args = vec!["--port".to_owned(), "3389".to_owned()];
                    if let Ok((mut child, rx)) = spawn_connect(&flow.name, &args, ctx.clone()) {
                        let child_stdin = child.stdin.take();
                        self.connect_flow = Some(ConnectFlow {
                            child: Some(child),
                            child_stdin,
                            rx,
                            arrived: Vec::new(),
                            started: Instant::now(),
                            failure: None,
                            cert_prompt: None,
                            password_prompt: None,
                            port: 3389,
                            args,
                            reprompt: false,
                            ..flow
                        });
                    }
                }
                dialogs::ConnectDialogAction::EditPassword => {
                    // Re-run the connect with the child forced to ask; the credential
                    // dialog answers over the pipe and can overwrite the stored password.
                    let mut args = flow.args.clone();
                    args.push("--ask-password".to_owned());
                    if let Ok((mut child, rx)) = spawn_connect(&flow.name, &args, ctx.clone()) {
                        let child_stdin = child.stdin.take();
                        self.connect_flow = Some(ConnectFlow {
                            child: Some(child),
                            child_stdin,
                            rx,
                            arrived: Vec::new(),
                            started: Instant::now(),
                            failure: None,
                            cert_prompt: None,
                            password_prompt: None,
                            reprompt: true,
                            ..flow
                        });
                    }
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
                Some(menus::MenuAction::Settings) => self.open_settings(),
                Some(menus::MenuAction::About) => self.modal = Some(Modal::About),
                Some(menus::MenuAction::Shortcuts) => self.modal = Some(Modal::Shortcuts),
                Some(menus::MenuAction::RevealFavourites) => reveal(&self.config_path),
                Some(menus::MenuAction::Connect) => {
                    if let Some(i) = self.selected {
                        self.connect(i, ctx);
                    }
                }
                Some(menus::MenuAction::Edit) => self.open_edit(),
                Some(menus::MenuAction::Duplicate) => self.duplicate_selected(),
                Some(menus::MenuAction::Remove) => self.open_remove(),
                Some(menus::MenuAction::CopyCommandLine) => {
                    if let Some(i) = self.selected
                        && let Some(f) = self.favourites.iter().nth(i)
                    {
                        let name = &f.name;
                        let quoted = if name.contains(char::is_whitespace) {
                            format!("'{name}'")
                        } else {
                            name.clone()
                        };
                        ctx.copy_text(format!("mdrdp {quoted}"));
                    }
                }
                Some(menus::MenuAction::CopyAvc444Script) => {
                    ctx.copy_text(crate::hostscripts::ENABLE_AVC444.to_owned());
                }
                Some(menus::MenuAction::Copy60FpsScript) => {
                    ctx.copy_text(crate::hostscripts::ENABLE_60FPS.to_owned());
                }
                Some(menus::MenuAction::CopySshSetupScript) => {
                    // The launcher knows which host is selected, so it can do the
                    // client-side half here too — key and ~/.ssh/config entry —
                    // and leave only the paste for the user. Both steps are
                    // idempotent, so a repeat click is harmless.
                    match crate::sshsetup::ensure_key() {
                        Ok((pubkey, _)) => {
                            if let Some(i) = self.selected
                                && let Some(f) = self.favourites.iter().nth(i)
                            {
                                let user = f
                                    .username
                                    .clone()
                                    .unwrap_or_else(crate::sshsetup::default_user);
                                if let Err(e) = crate::sshsetup::ensure_config(&f.host, &user) {
                                    eprintln!("could not update ~/.ssh/config: {e}");
                                }
                            }
                            ctx.copy_text(crate::hostscripts::setup_ssh(&pubkey));
                        }
                        Err(e) => eprintln!("could not prepare the SSH key: {e}"),
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
        let mut edit_clicked = false;
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
                                if widgets::secondary_button(ui, "Edit", 32.0).clicked() {
                                    edit_clicked = true;
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
        if edit_clicked {
            self.open_edit();
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
                    match self.favourites.add(*favourite) {
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
                                password_prompt: None,
                                args: extra,
                                reprompt: false,
                            });
                        }
                        Err(e) => eprintln!("{e}"),
                    }
                }
            }
        }
    }

    fn modal_ui(&mut self, ctx: &egui::Context) {
        let Some(mut modal) = self.modal.take() else {
            return;
        };
        match &mut modal {
            Modal::Settings(state) => match state.ui(ctx, &self.config_path) {
                settings_ui::SettingsOutcome::Open => self.modal = Some(modal),
                settings_ui::SettingsOutcome::Cancelled => {}
                settings_ui::SettingsOutcome::Saved(settings) => {
                    self.settings = settings;
                    self.default_username = self.settings.defaults.username.clone();
                    match &self.settings_path {
                        Some(path) => {
                            if let Err(e) = self.settings.save_to(path) {
                                eprintln!("could not save settings: {e}");
                            }
                        }
                        None => eprintln!("no settings path; changes apply this run only"),
                    }
                }
            },
            Modal::About => {
                if self.about_icon.is_none() {
                    self.about_icon = crate::ui::help::icon_texture(ctx);
                }
                if !dialogs::about(ctx, self.about_icon.as_ref()) {
                    self.modal = Some(modal);
                }
            }
            Modal::Shortcuts => {
                if !dialogs::shortcuts(ctx) {
                    self.modal = Some(modal);
                }
            }
            Modal::Edit(state) => match dialogs::edit_connection(ctx, state) {
                dialogs::EditAction::None => self.modal = Some(modal),
                dialogs::EditAction::Cancel => {}
                dialogs::EditAction::Remove => {
                    let name = state.original_name.clone();
                    let account = self
                        .favourites
                        .find(&name)
                        .and_then(|f| f.keychain_account.clone());
                    self.modal = Some(Modal::Remove(dialogs::RemoveState {
                        name,
                        account,
                        delete_password: false,
                    }));
                }
                dialogs::EditAction::Save(favourite, password) => {
                    let original = state.original_name.clone();
                    self.apply_edit(&original, favourite, password);
                }
            },
            Modal::Remove(state) => match dialogs::remove_connection(ctx, state) {
                dialogs::RemoveAction::None => self.modal = Some(modal),
                dialogs::RemoveAction::Cancel => {}
                dialogs::RemoveAction::Remove { delete_password } => {
                    match self.favourites.remove(&state.name) {
                        Ok(removed) => {
                            if let Err(e) = self.favourites.save_to(&self.config_path) {
                                eprintln!("warning: could not save favourites: {e}");
                            }
                            if delete_password
                                && let Some(account) = removed.keychain_account
                                && let Err(e) = crate::creds::forget(&account)
                            {
                                eprintln!("could not delete the password: {e}");
                            }
                            let len = self.favourites.len();
                            self.selected = if len == 0 {
                                None
                            } else {
                                Some(self.selected.unwrap_or(0).min(len - 1))
                            };
                        }
                        Err(e) => eprintln!("could not remove: {e}"),
                    }
                }
            },
            Modal::Quit => {
                let sessions: Vec<(String, u32, u64)> = self
                    .running
                    .iter()
                    .map(|s| {
                        (
                            s.name.clone(),
                            s.child.id(),
                            s.started.elapsed().as_secs() / 60,
                        )
                    })
                    .collect();
                match dialogs::quit_with_sessions(ctx, &sessions) {
                    dialogs::QuitAction::None => self.modal = Some(modal),
                    dialogs::QuitAction::Cancel => {}
                    dialogs::QuitAction::QuitAnyway => {
                        self.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
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
        self.modal_ui(&ctx);
        self.connect_flow_ui(&ctx);

        // Closing over running sessions is a choice, not a surprise: intercept the
        // close, ask, and only pass it through once Quit anyway has said so.
        if ctx.input(|i| i.viewport().close_requested())
            && !self.running.is_empty()
            && !self.allow_close
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.modal = Some(Modal::Quit);
        }

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
    settings: crate::settings::Settings,
    settings_path: Option<PathBuf>,
) -> Result<(), String> {
    let icon = crate::ui::help::app_icon();
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
                settings,
                settings_path,
                menu,
            )))
        }),
    )
    .map_err(|e| format!("launcher window failed: {e}"))
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
                Some("password_prompt") => ChildEvent::PasswordPrompt {
                    account: v["account"].as_str().unwrap_or("?").to_owned(),
                    reason: v["reason"].as_str().unwrap_or("").to_owned(),
                },
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
        Shortcuts,
        RevealFavourites,
        Connect,
        Edit,
        Duplicate,
        Remove,
        CopyCommandLine,
        CopyAvc444Script,
        Copy60FpsScript,
        CopySshSetupScript,
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
                // No app menu off macOS, so Settings lands in File. About does not:
                // Windows and Linux both expect it under Help, where it now lives.
                let settings = item("Settings…", MenuAction::Settings);
                let _ = file.append_items(&[&PredefinedMenuItem::separator(), &settings]);
            }
            let _ = menu.append(&file);

            let connection = Submenu::new("Connection", true);
            let connect = item("Connect", MenuAction::Connect);
            let edit = item("Edit…", MenuAction::Edit);
            let duplicate = item("Duplicate", MenuAction::Duplicate);
            let remove = item("Remove…", MenuAction::Remove);
            let copy_cli = item("Copy command line", MenuAction::CopyCommandLine);
            let copy_avc444 = item("Copy AVC444 enable script", MenuAction::CopyAvc444Script);
            let copy_60fps = item("Copy 60 fps enable script", MenuAction::Copy60FpsScript);
            let copy_ssh = item("Copy SSH setup script", MenuAction::CopySshSetupScript);
            let _ = connection.append_items(&[
                &connect,
                &edit,
                &duplicate,
                &remove,
                &PredefinedMenuItem::separator(),
                &copy_cli,
                &copy_avc444,
                &copy_60fps,
                &copy_ssh,
            ]);
            let _ = menu.append(&connection);

            let view = Submenu::new("View", true);
            let _ = menu.append(&view);

            // Help is the last submenu and the only one every platform agrees on. On
            // macOS About stays in the app menu, where the platform puts it.
            let help = Submenu::new("Help", true);
            let shortcuts = item("Keyboard shortcuts…", MenuAction::Shortcuts);
            let _ = help.append(&shortcuts);
            #[cfg(not(target_os = "macos"))]
            {
                let about = item("About mdrdp", MenuAction::About);
                let _ = help.append_items(&[&PredefinedMenuItem::separator(), &about]);
            }
            let _ = menu.append(&help);

            #[cfg(target_os = "macos")]
            {
                let _ = cc;
                menu.init_for_nsapp();
            }
            #[cfg(target_os = "windows")]
            {
                // winit's re-export, not eframe's: eframe 0.36 does not re-export
                // raw_window_handle, and this arm only compiles on Windows, so the
                // cross-check is the only thing standing between this line and a
                // broken Windows build.
                use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
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
