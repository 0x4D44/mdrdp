//! The session agent's reconcile loop.
//!
//! Tick-driven and platform-free: the loop is called every [`TICK_SECS`] by the
//! runner and talks to Windows only through [`AgentOps`], so the whole supervision
//! logic unit-tests on macOS with a scripted fake.
//!
//! **No fatal states.** Every step — creator spawn, device arrival, mode set,
//! server spawn — is retried on a later tick forever. Device arrival takes seconds
//! after the creator starts, and an onlogon task can fire before the desktop
//! settles, so "not yet" is normal; the status report names the stuck step rather
//! than the loop giving up.
//!
//! Bring-up order within one tick: creator → device → display mode → server. The
//! server only *spawns* once the device is present (an IDD-source server without
//! the device would just crash-loop through its 30 s section wait), and it is
//! *restarted* whenever the device changes identity or blinks away and back — a
//! server attached to a dead instance's section captures nothing, silently,
//! forever (see [`AgentOps::device_id`]).

use crate::control::{ChildReport, ModeReport, Rung, RungReport, RungState, StatusReport, SCHEMA};

/// How often the runner calls [`Reconciler::tick`]. Cooldowns below are counted in
/// ticks, so every duration here is a multiple of this.
pub const TICK_SECS: u32 = 2;

/// Crash-loop backoff: first respawn after 1 tick, doubling to this cap (15 ticks
/// = 30 s), reset after [`HEALTHY_RESET_TICKS`] of continuous health.
const MAX_BACKOFF_TICKS: u32 = 15;

/// 30 ticks = 60 s of continuous running resets the backoff to its floor.
const HEALTHY_RESET_TICKS: u32 = 30;

/// The mode the virtual display is held at. 1920x1080 is the wire's proven frame
/// geometry; 240 Hz is what the driver offers and the measurements were taken at.
pub const DESIRED_MODE: Mode = Mode {
    width: 1920,
    height: 1080,
    hz: 240,
};

/// A display mode as the reconciler reasons about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
}

impl Mode {
    fn report(self) -> ModeReport {
        ModeReport {
            width: self.width,
            height: self.height,
            hz: self.hz,
        }
    }
}

/// What a poll of a supervised child found.
///
/// Contract for implementors: `Exited` is returned exactly once per death — the
/// poll that reaps the child — and the state is `NotStarted` from then until the
/// next spawn. (`std::process::Child::try_wait` plus dropping the handle gives
/// this shape naturally.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildState {
    NotStarted,
    Running,
    Exited(i32),
}

/// Everything the reconciler needs from the platform. The Windows implementation
/// lives in `win::agent_ops`; tests script a fake.
pub trait AgentOps {
    fn poll_creator(&mut self) -> ChildState;
    fn spawn_creator(&mut self) -> Result<(), String>;
    /// The virtual display's identity, if it is in the session's display set —
    /// the GDI device name (`\\.\DISPLAYn`) on Windows. Identity, not just
    /// presence: device removal is asynchronous, so a freshly killed creator's
    /// display can linger "present" for seconds while its shared section is
    /// already dying, and only the name change betrays the swap (found live
    /// 2026-08-18: the agent's first server attached to the dying section and
    /// captured nothing, with no error, forever).
    fn device_id(&mut self) -> Option<String>;
    /// The display's current mode, if it can be read.
    fn display_mode(&mut self) -> Option<Mode>;
    fn set_display_mode(&mut self, mode: Mode) -> Result<(), String>;
    fn poll_server(&mut self) -> ChildState;
    fn spawn_server(&mut self) -> Result<(), String>;
    /// Kill the capture server (supervision respawns it): the restart-server path.
    fn kill_server(&mut self);
    /// Kill every supervised child: the shutdown path.
    fn kill_all(&mut self);
}

/// Supervision state for one child.
#[derive(Debug, Default)]
struct Supervised {
    running: bool,
    ever_spawned: bool,
    restarts: u32,
    last_exit: Option<i32>,
    /// Ticks to wait before the next spawn attempt.
    cooldown: u32,
    /// The cooldown the *next* death will impose.
    next_backoff: u32,
    healthy_ticks: u32,
}

impl Supervised {
    fn new() -> Self {
        Self {
            next_backoff: 1,
            ..Self::default()
        }
    }

    /// Advance one tick given the polled state. `spawn` is invoked at most once.
    /// A failed spawn is a death like any other: it costs the current backoff.
    fn tick(&mut self, state: ChildState, spawn: &mut dyn FnMut() -> Result<(), String>) {
        match state {
            ChildState::Running => {
                self.running = true;
                self.healthy_ticks += 1;
                if self.healthy_ticks >= HEALTHY_RESET_TICKS {
                    self.next_backoff = 1;
                }
            }
            ChildState::Exited(code) => {
                self.running = false;
                self.healthy_ticks = 0;
                self.last_exit = Some(code);
                self.cooldown = self.next_backoff;
                self.next_backoff = (self.next_backoff * 2).min(MAX_BACKOFF_TICKS);
            }
            ChildState::NotStarted => {
                self.running = false;
                self.healthy_ticks = 0;
                if self.cooldown > 0 {
                    self.cooldown -= 1;
                } else {
                    if self.ever_spawned {
                        self.restarts += 1;
                    }
                    self.ever_spawned = true;
                    if spawn().is_ok() {
                        self.running = true;
                    } else {
                        self.cooldown = self.next_backoff;
                        self.next_backoff = (self.next_backoff * 2).min(MAX_BACKOFF_TICKS);
                    }
                }
            }
        }
    }

    fn report(&self) -> ChildReport {
        ChildReport {
            running: self.running,
            restarts: self.restarts,
            last_exit_code: self.last_exit,
            cooldown_s: self.cooldown * TICK_SECS,
        }
    }
}

/// The reconcile loop's state. One instance per agent process.
pub struct Reconciler {
    creator: Supervised,
    server: Supervised,
    desired: Mode,
    device_present: bool,
    /// The identity last observed, for spotting swaps and blinks (see
    /// [`AgentOps::device_id`]).
    last_device_id: Option<String>,
    actual_mode: Option<Mode>,
    mode_ok: bool,
    restart_server_requested: bool,
    ticks: u64,
}

impl Reconciler {
    pub fn new() -> Self {
        Self {
            creator: Supervised::new(),
            server: Supervised::new(),
            desired: DESIRED_MODE,
            device_present: false,
            last_device_id: None,
            actual_mode: None,
            mode_ok: false,
            restart_server_requested: false,
            ticks: 0,
        }
    }

    /// Ask the next tick to kill the server; supervision then respawns it.
    pub fn request_server_restart(&mut self) {
        self.restart_server_requested = true;
    }

    /// One reconcile pass: creator → device → mode → server.
    pub fn tick(&mut self, ops: &mut dyn AgentOps) {
        self.ticks += 1;

        let creator_state = ops.poll_creator();
        self.creator
            .tick(creator_state, &mut || ops.spawn_creator());

        let device_id = ops.device_id();
        self.device_present = device_id.is_some();
        // A server that attached to one device instance's section captures nothing
        // once that instance dies — silently, forever. Restart it whenever the
        // device swaps identity or blinks away and back after the server started.
        let device_replaced = match (&self.last_device_id, &device_id) {
            (Some(old), Some(new)) => old != new,
            (None, Some(_)) => self.server.ever_spawned,
            _ => false,
        };
        if device_replaced {
            self.restart_server_requested = true;
        }
        self.last_device_id = device_id;
        if self.device_present {
            self.actual_mode = ops.display_mode();
            self.mode_ok = self.actual_mode == Some(self.desired);
            if !self.mode_ok {
                // Idempotent: checked every tick, so a device re-created at its
                // default 60 Hz gets put back. A failure is degraded, not down —
                // status shows mode_ok false and the loop moves on.
                if ops.set_display_mode(self.desired).is_ok() {
                    self.actual_mode = ops.display_mode();
                    self.mode_ok = self.actual_mode == Some(self.desired);
                }
            }
        } else {
            self.actual_mode = None;
            self.mode_ok = false;
        }

        if self.restart_server_requested {
            self.restart_server_requested = false;
            ops.kill_server();
        }
        let server_state = ops.poll_server();
        if self.device_present || server_state != ChildState::NotStarted {
            self.server.tick(server_state, &mut || ops.spawn_server());
        }
    }

    /// The first unsatisfied step in bring-up order.
    fn stuck(&self) -> Option<&'static str> {
        if !self.creator.running {
            Some("creator")
        } else if !self.device_present {
            Some("device")
        } else if !self.mode_ok {
            Some("display-mode")
        } else if !self.server.running {
            Some("server")
        } else {
            None
        }
    }

    /// The ladder as this reconciler can currently answer it (HLD tranche 4 §6).
    ///
    /// Rungs whose samplers have not landed yet report [`RungState::Unknown`]
    /// rather than being omitted or guessed green — the four-way vocabulary
    /// exists precisely so "not implemented in this build" and "broken" are
    /// different answers.
    fn rungs(&self) -> Vec<RungReport> {
        let ok_or_fail = |ok: bool| {
            if ok {
                RungState::Ok
            } else {
                RungState::Fail
            }
        };
        let unimplemented = |rung| RungReport {
            rung,
            state: RungState::Unknown,
            detail: Some("not sampled by this build".to_owned()),
        };
        vec![
            RungReport {
                rung: Rung::Creator,
                state: ok_or_fail(self.creator.running),
                detail: None,
            },
            RungReport {
                rung: Rung::Device,
                state: ok_or_fail(self.device_present),
                detail: None,
            },
            unimplemented(Rung::Pool),
            RungReport {
                rung: Rung::DisplayMode,
                state: ok_or_fail(self.mode_ok),
                detail: self
                    .actual_mode
                    .filter(|_| !self.mode_ok)
                    .map(|m| format!("{}x{} @ {} Hz", m.width, m.height, m.hz)),
            },
            RungReport {
                rung: Rung::Server,
                state: ok_or_fail(self.server.running),
                detail: None,
            },
            unimplemented(Rung::InputDesktop),
            unimplemented(Rung::Liveness),
        ]
    }

    /// Assemble the wire status. `uptime_s` comes from the runner, which owns time.
    pub fn status(&self, uptime_s: u64) -> StatusReport {
        StatusReport {
            schema: SCHEMA,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            uptime_s,
            creator: self.creator.report(),
            device_present: self.device_present,
            display_mode: self.actual_mode.map(Mode::report),
            mode_ok: self.mode_ok,
            server: self.server.report(),
            // Deliberately NOT `stuck_from_rungs(&rungs)` yet. `stuck` feeds
            // `green`, which gates the client's native connect, and the pool rung
            // still reports `Unknown` — routing that through the rung derivation
            // would make every host read as stuck on a rung this build cannot yet
            // sample. `stuck` keeps its schema-2 meaning until every bring-up
            // rung has a real sampler, and the two agree by construction then.
            stuck: self.stuck().map(str::to_owned),
            rungs: self.rungs(),
            pool: None,
            viewer_connected: None,
        }
    }
}

impl Default for Reconciler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted platform. Children run until `kill_*` or a scripted death;
    /// spawns are recorded with the tick-relative call order.
    #[derive(Default)]
    struct FakeOps {
        calls: Vec<String>,
        creator_running: bool,
        creator_pending_exit: Option<i32>,
        device_id: Option<String>,
        mode: Option<Mode>,
        mode_set_fails: bool,
        server_running: bool,
        server_pending_exit: Option<i32>,
        /// When true every server spawn dies before the next poll (crash loop).
        server_dies_instantly: bool,
    }

    impl AgentOps for FakeOps {
        fn poll_creator(&mut self) -> ChildState {
            if let Some(code) = self.creator_pending_exit.take() {
                self.creator_running = false;
                return ChildState::Exited(code);
            }
            if self.creator_running {
                ChildState::Running
            } else {
                ChildState::NotStarted
            }
        }
        fn spawn_creator(&mut self) -> Result<(), String> {
            self.calls.push("spawn_creator".into());
            self.creator_running = true;
            Ok(())
        }
        fn device_id(&mut self) -> Option<String> {
            self.device_id.clone()
        }
        fn display_mode(&mut self) -> Option<Mode> {
            self.mode
        }
        fn set_display_mode(&mut self, mode: Mode) -> Result<(), String> {
            self.calls.push(format!("set_mode {}hz", mode.hz));
            if self.mode_set_fails {
                Err("scripted failure".into())
            } else {
                self.mode = Some(mode);
                Ok(())
            }
        }
        fn poll_server(&mut self) -> ChildState {
            if let Some(code) = self.server_pending_exit.take() {
                self.server_running = false;
                return ChildState::Exited(code);
            }
            if self.server_running {
                ChildState::Running
            } else {
                ChildState::NotStarted
            }
        }
        fn spawn_server(&mut self) -> Result<(), String> {
            self.calls.push("spawn_server".into());
            if self.server_dies_instantly {
                self.server_pending_exit = Some(1);
                self.server_running = true;
            } else {
                self.server_running = true;
            }
            Ok(())
        }
        fn kill_server(&mut self) {
            self.calls.push("kill_server".into());
            if self.server_running {
                self.server_running = false;
                self.server_pending_exit = Some(-1);
            }
        }
        fn kill_all(&mut self) {
            self.calls.push("kill_all".into());
            self.creator_running = false;
            self.server_running = false;
        }
    }

    #[test]
    fn server_spawn_is_gated_on_the_device() {
        let mut ops = FakeOps::default();
        let mut rec = Reconciler::new();

        rec.tick(&mut ops);
        assert_eq!(ops.calls, vec!["spawn_creator"]);
        assert_eq!(rec.status(0).stuck.as_deref(), Some("device"));

        // Device arrives at its default 60 Hz: same tick sets the mode AND starts
        // the server — a degraded mode is not a reason to withhold capture.
        ops.device_id = Some("dpy-1".to_owned());
        ops.mode = Some(Mode {
            width: 1920,
            height: 1080,
            hz: 60,
        });
        rec.tick(&mut ops);
        assert_eq!(
            ops.calls,
            vec!["spawn_creator", "set_mode 240hz", "spawn_server"]
        );
        assert_eq!(rec.status(0).stuck, None);
    }

    #[test]
    fn mode_is_re_asserted_when_the_device_drifts_back() {
        let mut ops = FakeOps {
            device_id: Some("dpy-1".to_owned()),
            mode: Some(DESIRED_MODE),
            ..FakeOps::default()
        };
        let mut rec = Reconciler::new();
        rec.tick(&mut ops);
        assert!(!ops.calls.contains(&"set_mode 240hz".to_owned()));

        // The device blinks and comes back at 60 Hz (its default after re-create).
        ops.mode = Some(Mode {
            hz: 60,
            ..DESIRED_MODE
        });
        rec.tick(&mut ops);
        assert!(ops.calls.contains(&"set_mode 240hz".to_owned()));
        assert_eq!(ops.mode, Some(DESIRED_MODE));
    }

    #[test]
    fn a_device_swap_restarts_the_server() {
        let mut ops = FakeOps {
            device_id: Some("dpy-1".to_owned()),
            mode: Some(DESIRED_MODE),
            ..FakeOps::default()
        };
        let mut rec = Reconciler::new();
        rec.tick(&mut ops); // brings everything up against dpy-1
        assert!(ops.server_running);

        // The live 2026-08-18 failure: the creator is replaced, its old display
        // lingers, then the new instance appears under a new name. The running
        // server holds the dead instance's section and must be restarted.
        ops.device_id = Some("dpy-2".to_owned());
        rec.tick(&mut ops);
        assert!(ops.calls.contains(&"kill_server".to_owned()));
        rec.tick(&mut ops); // cooldown
        rec.tick(&mut ops); // respawn against dpy-2
        assert!(ops.server_running);
        assert_eq!(ops.calls.iter().filter(|c| *c == "spawn_server").count(), 2);
    }

    #[test]
    fn a_device_blink_restarts_the_server_but_bring_up_does_not() {
        let mut ops = FakeOps::default();
        let mut rec = Reconciler::new();
        rec.tick(&mut ops); // creator only; no device yet

        // First arrival is bring-up, not a blink: no kill, just a spawn.
        ops.device_id = Some("dpy-1".to_owned());
        ops.mode = Some(DESIRED_MODE);
        rec.tick(&mut ops);
        assert!(!ops.calls.contains(&"kill_server".to_owned()));
        assert!(ops.server_running);

        // Away and back under the SAME name: the section behind it still died
        // with the instance, so the server is restarted anyway.
        ops.device_id = None;
        rec.tick(&mut ops);
        ops.device_id = Some("dpy-1".to_owned());
        rec.tick(&mut ops);
        assert!(ops.calls.contains(&"kill_server".to_owned()));
    }

    #[test]
    fn crash_loop_backs_off_doubling_to_the_cap() {
        let mut ops = FakeOps {
            device_id: Some("dpy-1".to_owned()),
            mode: Some(DESIRED_MODE),
            server_dies_instantly: true,
            ..FakeOps::default()
        };
        let mut rec = Reconciler::new();

        let mut spawn_ticks = Vec::new();
        for tick in 1..=80u32 {
            let before = ops.calls.iter().filter(|c| *c == "spawn_server").count();
            rec.tick(&mut ops);
            let after = ops.calls.iter().filter(|c| *c == "spawn_server").count();
            if after > before {
                spawn_ticks.push(tick);
            }
        }
        // Death is observed the tick after each spawn (1 tick), then the cooldown
        // (1, 2, 4, 8, 15, 15… ticks) runs down, then the next tick respawns —
        // so successive spawns sit 3, 4, 6, 10, 17, 17… ticks apart.
        let gaps: Vec<u32> = spawn_ticks.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(&gaps[..5], &[3, 4, 6, 10, 17]);
        assert!(gaps[5..].iter().all(|g| *g == 17), "cap holds: {gaps:?}");

        let status = rec.status(0);
        assert!(!status.server.running || status.server.cooldown_s > 0);
        assert_eq!(status.server.last_exit_code, Some(1));
        assert!(status.server.restarts >= 4);
        assert_eq!(status.stuck.as_deref(), Some("server"));
    }

    #[test]
    fn an_hour_of_health_resets_the_backoff() {
        let mut ops = FakeOps {
            device_id: Some("dpy-1".to_owned()),
            mode: Some(DESIRED_MODE),
            ..FakeOps::default()
        };
        let mut rec = Reconciler::new();

        // Crash twice to raise next_backoff past its floor.
        rec.tick(&mut ops); // spawn
        ops.server_pending_exit = Some(3);
        rec.tick(&mut ops); // exit -> cooldown 1, next 2
        rec.tick(&mut ops); // cooldown
        rec.tick(&mut ops); // respawn
        ops.server_pending_exit = Some(3);
        rec.tick(&mut ops); // exit -> cooldown 2, next 4

        // Now stay healthy long enough for the reset...
        for _ in 0..3 {
            rec.tick(&mut ops); // cooldown, cooldown, respawn
        }
        for _ in 0..HEALTHY_RESET_TICKS {
            rec.tick(&mut ops);
        }
        // ...then die once more: the cooldown must be back at the 1-tick floor.
        ops.server_pending_exit = Some(4);
        rec.tick(&mut ops);
        assert_eq!(rec.status(0).server.cooldown_s, TICK_SECS);
        assert_eq!(rec.status(0).server.last_exit_code, Some(4));
    }

    #[test]
    fn restart_request_kills_and_supervision_respawns() {
        let mut ops = FakeOps {
            device_id: Some("dpy-1".to_owned()),
            mode: Some(DESIRED_MODE),
            ..FakeOps::default()
        };
        let mut rec = Reconciler::new();
        rec.tick(&mut ops); // brings the server up
        assert!(ops.server_running);

        rec.request_server_restart();
        rec.tick(&mut ops); // kill observed as an exit
        assert!(ops.calls.contains(&"kill_server".to_owned()));
        rec.tick(&mut ops); // cooldown tick
        rec.tick(&mut ops); // respawn
        assert_eq!(ops.calls.iter().filter(|c| *c == "spawn_server").count(), 2);
        assert!(ops.server_running);
    }

    #[test]
    fn creator_exit_is_counted_and_respawned() {
        let mut ops = FakeOps::default();
        let mut rec = Reconciler::new();
        rec.tick(&mut ops);
        assert!(ops.creator_running);

        ops.creator_pending_exit = Some(9);
        rec.tick(&mut ops); // exit -> cooldown 1
        assert_eq!(rec.status(0).stuck.as_deref(), Some("creator"));
        rec.tick(&mut ops); // cooldown
        rec.tick(&mut ops); // respawn
        let status = rec.status(0);
        assert!(status.creator.running);
        assert_eq!(status.creator.restarts, 1);
        assert_eq!(status.creator.last_exit_code, Some(9));
    }

    #[test]
    fn status_reports_the_two_children_distinctly() {
        let mut ops = FakeOps {
            device_id: Some("dpy-1".to_owned()),
            mode: Some(DESIRED_MODE),
            ..FakeOps::default()
        };
        let mut rec = Reconciler::new();
        rec.tick(&mut ops);

        // Kill only the server, twice, so its counters diverge from the creator's.
        for _ in 0..2 {
            ops.server_pending_exit = Some(11);
            rec.tick(&mut ops);
            rec.tick(&mut ops);
            rec.tick(&mut ops);
            rec.tick(&mut ops);
        }
        let status = rec.status(77);
        assert_eq!(status.uptime_s, 77);
        assert_eq!(status.creator.restarts, 0);
        assert_eq!(status.creator.last_exit_code, None);
        assert!(status.server.restarts >= 1);
        assert_eq!(status.server.last_exit_code, Some(11));
        assert_eq!(
            status.display_mode,
            Some(ModeReport {
                width: 1920,
                height: 1080,
                hz: 240
            })
        );
    }
}
