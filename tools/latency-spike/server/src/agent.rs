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

use crate::control::{
    ChildReport, ModeReport, PoolReport, Rung, RungReport, RungState, StatusReport, SCHEMA,
};

/// How often the runner calls [`Reconciler::tick`]. Cooldowns below are counted in
/// ticks, so every duration here is a multiple of this.
pub const TICK_SECS: u32 = 2;

/// Crash-loop backoff: first respawn after 1 tick, doubling to this cap (15 ticks
/// = 30 s), reset after [`HEALTHY_RESET_TICKS`] of continuous health.
const MAX_BACKOFF_TICKS: u32 = 15;

/// 30 ticks = 60 s of continuous running resets the backoff to its floor.
const HEALTHY_RESET_TICKS: u32 = 30;

/// Safe mode before the first client request. Product experiments start at 1440p;
/// 1080p is deliberately no longer part of the native transport plan.
pub const DESIRED_MODE: Mode = Mode {
    width: 2560,
    height: 1440,
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

/// The IDD's placement in the Windows virtual desktop.
///
/// Rhydra captures the IDD, so it must also own the desktop origin and primary
/// taskbar. Keeping this observation in the reconciler makes a topology drift
/// a normal health failure instead of leaving the server capturing a secondary
/// display while applications open on another monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayPlacement {
    pub primary: bool,
    pub origin: (i32, i32),
}

impl DisplayPlacement {
    pub const fn primary_at_origin() -> Self {
        Self {
            primary: true,
            origin: (0, 0),
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
    /// Whether this host has an audio render endpoint to capture from.
    ///
    /// `Some(true)` an endpoint exists, `Some(false)` there is none, `None` the
    /// question could not be answered. Three-valued because "I could not ask"
    /// and "the answer is no" want different diagnoses, and reporting the first
    /// as the second is how a health ladder tells a confident wrong story.
    fn audio_endpoint(&mut self) -> Option<bool>;
    /// The display's current mode, if it can be read.
    fn display_mode(&mut self) -> Option<Mode>;
    /// Effective Windows UI scale on that display, as a percentage.
    fn display_scale_percent(&mut self) -> Option<u32>;
    /// Whether the IDD is the primary display and where its physical origin is.
    fn display_placement(&mut self) -> Option<DisplayPlacement>;
    fn set_display_mode(&mut self, mode: Mode) -> Result<(), String>;
    fn poll_server(&mut self) -> ChildState;
    fn spawn_server(&mut self) -> Result<(), String>;
    /// Kill the capture server (supervision respawns it): the restart-server path.
    fn kill_server(&mut self);
    /// Kill the creator (supervision respawns it), which takes the virtual device
    /// down with it and so makes the driver rebuild and republish the section.
    /// This — not a server restart — is the remedy when the driver publishes no
    /// pool: a fresh server against generation 0 refuses it and dies into
    /// backoff, turning a silent wedge into a crash loop (HLD tranche 4 §6).
    fn kill_creator(&mut self);
    /// Kill every supervised child: the shutdown path.
    fn kill_all(&mut self);
    /// What the driver's shared section currently says.
    ///
    /// Read directly by the agent every tick, which is the tranche's central
    /// observation: the section name is fixed, its generation is 0 exactly when
    /// the driver publishes no pool, and its `frame_seq` is advanced by the
    /// driver as the compositor presents — none of it needing a viewer, or even
    /// a capture server, to be true.
    fn pool(&mut self) -> PoolObservation;
    /// Whether anything is LISTENING on the capture server's video port.
    ///
    /// `None` when it could not be determined. Asked of the OS rather than by
    /// connecting: a probe connect would take the server's single viewer slot
    /// and unpark its capture loop, so the check would disturb the very thing it
    /// measures. `Supervised::running` is set optimistically the instant a spawn
    /// returns, so without this a server that is alive but not yet listening —
    /// its whole 30 s wait for the pool — reports green.
    fn server_listening(&mut self) -> Option<bool>;
    /// Whether injected input can currently land on the desktop.
    fn input_desktop(&mut self) -> InputDesktopObservation;
    /// Whether a viewer currently holds the capture server's single slot.
    ///
    /// `None` when it could not be determined. Read from the OS's connection
    /// table alongside the LISTEN check — the same read, no extra cost, and no
    /// contact with the server.
    fn viewer_connected(&mut self) -> Option<bool>;
}

/// Whether the desktop that would receive injected input is the one the stack is
/// on (HLD tranche 4 §6 rung 5).
///
/// This is Incident B: `SendInput` returns success, the injector thread honestly
/// reports `WinSta0\Default`, and nothing reaches the desktop, because a locked
/// session's *input* desktop is the secure `Winlogon` one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputDesktopObservation {
    /// The input desktop is ours: injected input can land.
    Matches { desktop: String },
    /// Something else holds the input desktop — a locked console puts it on
    /// `Winlogon`. Nothing injected will land until that clears.
    Differs { ours: String, input: String },
    /// Could not be judged. **Access-denied lands here, not in `Differs`**: it is
    /// equally what a caller in another session or window station gets, so
    /// reporting "the console is locked" on that evidence would be a confident
    /// wrong story — the failure mode this whole tranche exists to avoid.
    Unknown(String),
}

/// One tick's reading of the shared section.
///
/// The distinction between [`Self::Absent`]/[`Self::NoPool`] and
/// [`Self::Unreadable`] is the whole point: the first two are the driver
/// *telling* us there is nothing, which is actionable; the last is us failing to
/// find out, which is not. Acting on ignorance is how a health check earns its
/// reputation for making things worse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolObservation {
    /// The section does not exist. No driver instance is publishing at all.
    Absent,
    /// The section exists and the driver has explicitly advertised no pool
    /// (`generation == 0`, written by the driver's `AdvertiseNoPool` on
    /// teardown).
    NoPool,
    /// The section could not be read this tick — a torn mid-write copy, a
    /// zero-filled section the driver has not written yet, or a layout this
    /// build does not speak. Transient or a contract mismatch; either way not
    /// something to remediate blind.
    Unreadable(String),
    Present {
        generation: u32,
        frame_seq: u64,
    },
}

/// What a pool fault calls for. Two faults, two different remedies — applying
/// the wrong one is worse than doing nothing (HLD tranche 4 §6 rung 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolRemedy {
    /// No pool published: rebuild the device by restarting its owner.
    RestartCreator,
    /// A pool exists but the server is attached to a superseded generation.
    RestartServer,
}

/// How many consecutive ticks a pool fault must persist before the reconciler
/// acts on it.
///
/// A device cycle legitimately passes through "absent" and "no pool" on its way
/// back up, and the driver's own rebuild issues unassign/assign pairs in quick
/// succession. Remediating inside that window would fight the rebuild it is
/// watching. Three ticks is 6 s — longer than any rebuild race observed, far
/// shorter than a human notices.
const POOL_FAULT_TICKS: u32 = 3;

/// A device cycle in progress (HLD tranche 4 §6 rung 7).
///
/// A *state*, not three loose operations. Killing the creator and then walking
/// away would race the very supervision that is supposed to rebuild: `Supervised`
/// respawns on its own backoff, and device removal is asynchronous — the display
/// can linger "present" for seconds after its owner dies, which is exactly how a
/// server ended up attached to a dying section on 2026-08-18. So bring-up is
/// suppressed until the device has actually gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cycling {
    /// Ticks spent waiting for the device to disappear.
    waited: u32,
}

/// How long a cycle waits for the device to go before giving up and resuming
/// bring-up anyway. One pass, never a loop: if the device will not leave, that is
/// reported, not retried for ever.
const CYCLE_DEADLINE_TICKS: u32 = 15;

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
    desired_scale_percent: u32,
    device_present: bool,
    /// Last answer from `audio_endpoint`; `None` until first sampled.
    audio_endpoint: Option<bool>,
    /// The identity last observed, for spotting swaps and blinks (see
    /// [`AgentOps::device_id`]).
    last_device_id: Option<String>,
    actual_mode: Option<Mode>,
    actual_scale_percent: Option<u32>,
    actual_placement: Option<DisplayPlacement>,
    mode_ok: bool,
    restart_server_requested: bool,
    ticks: u64,
    /// This tick's reading of the shared section.
    pool: PoolObservation,
    /// The generation that was published when the capture server was last
    /// spawned. A server attached to an older generation captures a section
    /// nothing writes to any more — silently, forever — which is the failure
    /// no structural check caught on 2026-08-19. Recorded here rather than
    /// asked of the server, so it needs no cooperation from it.
    server_generation: Option<u32>,
    /// Consecutive ticks the current pool fault has persisted. Reset by any
    /// healthy reading, so a rebuild race never accumulates toward an action.
    pool_fault_ticks: u32,
    /// Whether anything is listening on the video port, as of this tick.
    server_listening: Option<bool>,
    /// This tick's input-desktop reading.
    input_desktop: InputDesktopObservation,
    /// `Some` while a device cycle is running; bring-up is suppressed until it
    /// finishes so supervision cannot fight the rebuild.
    cycling: Option<Cycling>,
    /// The driver's presented-frame counter as of the previous tick, for the
    /// liveness rung. `None` until two readings exist — one sample cannot show
    /// movement.
    last_frame_seq: Option<u64>,
    /// Consecutive ticks `frame_seq` has not moved. Not a fault counter: an idle
    /// desktop legitimately presents nothing, so this only ever feeds a report.
    frame_static_ticks: u32,
    /// Whether a viewer holds the capture server's slot, as of this tick.
    viewer_connected: Option<bool>,
}

impl Reconciler {
    pub fn new() -> Self {
        Self {
            creator: Supervised::new(),
            server: Supervised::new(),
            desired: DESIRED_MODE,
            desired_scale_percent: 100,
            device_present: false,
            audio_endpoint: None,
            last_device_id: None,
            actual_mode: None,
            actual_scale_percent: None,
            actual_placement: None,
            mode_ok: false,
            restart_server_requested: false,
            ticks: 0,
            pool: PoolObservation::Unreadable("not sampled yet".to_owned()),
            server_generation: None,
            pool_fault_ticks: 0,
            server_listening: None,
            input_desktop: InputDesktopObservation::Unknown("not sampled yet".to_owned()),
            cycling: None,
            last_frame_seq: None,
            frame_static_ticks: 0,
            viewer_connected: None,
        }
    }

    /// Begin a device cycle: the explicit, destructive rung 7 remedy.
    ///
    /// Idempotent — a request while one is already running is ignored, so a
    /// caller that retries after a read timeout cannot restart the cycle it may
    /// already have started.
    pub fn request_device_cycle(&mut self) {
        if self.cycling.is_none() {
            self.cycling = Some(Cycling { waited: 0 });
        }
    }

    /// Whether a cycle is in progress, for the status report.
    pub fn is_cycling(&self) -> bool {
        self.cycling.is_some()
    }

    /// Ask the next tick to kill the server; supervision then respawns it.
    pub fn request_server_restart(&mut self) {
        self.restart_server_requested = true;
    }

    /// Adopt a client-selected IDD mode. The accepted set is exactly the finite
    /// catalogue the driver publishes, so status can never promise an impossible
    /// mode and leave the client polling forever.
    pub fn request_display_mode(&mut self, mode: Mode, scale_percent: u32) -> Result<(), String> {
        let geometry_ok = matches!((mode.width, mode.height), (2560, 1440) | (5120, 2880));
        let refresh_ok = matches!(mode.hz, 60 | 120 | 240);
        if !geometry_ok || !refresh_ok {
            return Err(format!(
                "unsupported display mode {}x{} @ {} Hz (supported: 2560x1440 or 5120x2880 at 60/120/240 Hz)",
                mode.width, mode.height, mode.hz
            ));
        }
        if !(100..=500).contains(&scale_percent) {
            return Err(format!(
                "desktop scale {scale_percent}% is outside the supported 100..=500% range"
            ));
        }
        if self.desired != mode || self.desired_scale_percent != scale_percent {
            self.desired = mode;
            self.desired_scale_percent = scale_percent;
            self.mode_ok = false;
            // Converter and encoder geometry is fixed at server construction.
            self.restart_server_requested = true;
        }
        Ok(())
    }

    /// One reconcile pass: creator → device → mode → server.
    pub fn tick(&mut self, ops: &mut dyn AgentOps) {
        self.ticks += 1;

        if let Some(mut cycle) = self.cycling {
            // Everything the stack owns goes first: the server so it is not
            // holding the outgoing section, then the creator, which owns the
            // device's lifetime. Both are idempotent.
            ops.kill_server();
            ops.kill_creator();
            self.device_present = ops.device_id().is_some();
            self.pool = ops.pool();
            if !self.device_present {
                // Gone. Ordinary bring-up resumes next tick and rebuilds
                // creator -> device -> mode -> server on the NEW instance.
                self.cycling = None;
                self.last_device_id = None;
                self.server_generation = None;
                self.pool_fault_ticks = 0;
                return;
            }
            cycle.waited += 1;
            if cycle.waited >= CYCLE_DEADLINE_TICKS {
                // One pass, never a loop. Give up and let bring-up carry on;
                // the rungs will report whatever is actually wrong.
                eprintln!(
                    "cycle: the device is still present after {}s; resuming bring-up and \
                     reporting rather than retrying",
                    cycle.waited * TICK_SECS
                );
                self.cycling = None;
                self.pool_fault_ticks = 0;
            } else {
                self.cycling = Some(cycle);
            }
            return;
        }

        let creator_state = ops.poll_creator();
        self.creator
            .tick(creator_state, &mut || ops.spawn_creator());

        let device_id = ops.device_id();
        self.device_present = device_id.is_some();
        self.audio_endpoint = ops.audio_endpoint();
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
            self.actual_placement = ops.display_placement();
            let placement_needs_correction =
                self.actual_placement != Some(DisplayPlacement::primary_at_origin());
            if self.actual_mode != Some(self.desired) || placement_needs_correction {
                // Idempotent: checked every tick, so a device re-created at its
                // default 60 Hz or a display topology moved by Windows gets put
                // back. A failure is degraded, not down — status shows the
                // mismatch and the loop moves on.
                let corrected = ops.set_display_mode(self.desired).is_ok();
                self.actual_mode = ops.display_mode();
                self.actual_placement = ops.display_placement();
                // Capture and absolute-input mapping cache the display origin at
                // server start. Restart only after the platform reports that the
                // correction actually landed, and only when a server was running.
                if corrected
                    && self.server.running
                    && self.actual_mode == Some(self.desired)
                    && self.actual_placement == Some(DisplayPlacement::primary_at_origin())
                {
                    self.restart_server_requested = true;
                }
            }
            self.actual_scale_percent = ops.display_scale_percent();
            self.mode_ok = self.actual_mode == Some(self.desired)
                && self.actual_scale_percent == Some(self.desired_scale_percent)
                && self.actual_placement == Some(DisplayPlacement::primary_at_origin());
        } else {
            self.actual_mode = None;
            self.actual_scale_percent = None;
            self.actual_placement = None;
            self.mode_ok = false;
        }

        // Sample the section before deciding anything about the server: whether
        // the server needs restarting depends on what the driver publishes now.
        self.pool = ops.pool();
        if self.pool_remedy().is_some() {
            self.pool_fault_ticks = self.pool_fault_ticks.saturating_add(1);
        } else {
            self.pool_fault_ticks = 0;
        }
        // Act only once the same fault has outlived a rebuild race.
        if self.pool_fault_ticks >= POOL_FAULT_TICKS {
            match self.pool_remedy() {
                Some(PoolRemedy::RestartCreator) => {
                    // The device's lifetime belongs to the creator, so this is
                    // what makes the driver rebuild and republish. Restarting the
                    // server instead would meet the same generation 0 and die.
                    ops.kill_creator();
                    self.pool_fault_ticks = 0;
                }
                Some(PoolRemedy::RestartServer) => {
                    self.restart_server_requested = true;
                    self.pool_fault_ticks = 0;
                }
                None => {}
            }
        }

        if self.restart_server_requested {
            self.restart_server_requested = false;
            ops.kill_server();
        }
        let server_state = ops.poll_server();
        // A server created before the requested mode lands would publish a header
        // and encoder geometry the client explicitly did not ask for.
        if self.mode_ok || server_state != ChildState::NotStarted {
            let pool = &self.pool;
            let server_generation = &mut self.server_generation;
            self.server.tick(server_state, &mut || {
                let spawned = ops.spawn_server();
                if spawned.is_ok() {
                    // Remember what the server is about to attach to, so a later
                    // generation bump is recognisable as staleness rather than
                    // guessed at.
                    *server_generation = match pool {
                        PoolObservation::Present { generation, .. } => Some(*generation),
                        _ => None,
                    };
                }
                spawned
            });
        }

        // Sampled last, so they describe the state this tick's actions produced
        // rather than the one they replaced. Neither drives any remediation:
        // both are report-only rungs (HLD §6).
        self.server_listening = ops.server_listening();
        self.input_desktop = ops.input_desktop();
        self.viewer_connected = ops.viewer_connected();

        // Liveness: did the driver's presented-frame counter move? Two readings
        // are the minimum that can show movement, so the first tick after a
        // (re)build establishes a baseline and claims nothing.
        if let PoolObservation::Present { frame_seq, .. } = self.pool {
            match self.last_frame_seq {
                Some(previous) if frame_seq > previous => self.frame_static_ticks = 0,
                Some(_) => self.frame_static_ticks = self.frame_static_ticks.saturating_add(1),
                None => self.frame_static_ticks = 0,
            }
            self.last_frame_seq = Some(frame_seq);
        } else {
            self.last_frame_seq = None;
            self.frame_static_ticks = 0;
        }
    }

    /// What, if anything, this tick's pool reading calls for — before debouncing.
    ///
    /// Cause-specific by construction: the two faults have different remedies and
    /// applying the wrong one makes things worse rather than merely not better.
    fn pool_remedy(&self) -> Option<PoolRemedy> {
        match &self.pool {
            // Us failing to read is not the driver failing to publish. Never act.
            PoolObservation::Unreadable(_) => None,
            // The driver says there is nothing. Only a rebuilt device fixes that,
            // and the creator owns the device's lifetime.
            PoolObservation::Absent | PoolObservation::NoPool => {
                self.creator.running.then_some(PoolRemedy::RestartCreator)
            }
            PoolObservation::Present { generation, .. } => {
                // A server attached to a superseded generation is capturing a
                // section nothing writes to. Restarting it is correct AND cheap
                // here: the session it drops is already receiving nothing.
                match self.server_generation {
                    Some(attached) if attached != *generation && self.server.running => {
                        Some(PoolRemedy::RestartServer)
                    }
                    _ => None,
                }
            }
        }
    }

    /// The `pool` rung: is there a pool, and is the running server attached to
    /// the generation the driver publishes *now*?
    ///
    /// An unreadable section is `Unknown`, never `Fail`. `Fail` drives a creator
    /// restart, and restarting the stack because we could not read a page would
    /// be a health check causing the outage it claims to detect.
    fn pool_rung(&self) -> RungReport {
        let (state, detail) = match &self.pool {
            PoolObservation::Unreadable(why) => (RungState::Unknown, Some(why.clone())),
            PoolObservation::Absent => (
                RungState::Fail,
                Some("the shared section does not exist: no driver instance is publishing".into()),
            ),
            PoolObservation::NoPool => (
                RungState::Fail,
                Some("the driver publishes no pool (generation 0)".into()),
            ),
            PoolObservation::Present { generation, .. } => match self.server_generation {
                Some(attached) if attached != *generation && self.server.running => (
                    RungState::Fail,
                    Some(format!(
                        "the capture server is attached to generation {attached}, but the driver \
                         now publishes {generation}: it is reading a section nothing writes to"
                    )),
                ),
                _ => (RungState::Ok, None),
            },
        };
        RungReport {
            rung: Rung::Pool,
            state,
            detail,
        }
    }

    /// What the section says, for the wire.
    fn pool_report(&self) -> Option<PoolReport> {
        match &self.pool {
            PoolObservation::Present {
                generation,
                frame_seq,
            } => Some(PoolReport {
                generation: *generation,
                frame_seq: *frame_seq,
                server_generation: self.server_generation,
            }),
            // `NoPool` is a real reading, and reporting generation 0 is exactly
            // what a doctor needs to see. Absent and Unreadable have no numbers
            // to give and must not invent any.
            PoolObservation::NoPool => Some(PoolReport {
                generation: 0,
                frame_seq: 0,
                server_generation: self.server_generation,
            }),
            PoolObservation::Absent | PoolObservation::Unreadable(_) => None,
        }
    }

    /// The `liveness` rung: are pixels actually being presented?
    ///
    /// **Never `Fail`.** A static counter on an idle desktop is not a fault — it
    /// is an idle desktop, and calling that broken would be the confident wrong
    /// story this ladder exists to avoid. It reports movement when there is
    /// movement and says "nothing is drawing" when there is not, leaving the
    /// diagnosis to whichever rung below it is actually red.
    fn liveness_rung(&self) -> RungReport {
        let (state, detail) = match (&self.pool, self.last_frame_seq) {
            (PoolObservation::Present { .. }, None) => (
                RungState::Untested,
                Some("no baseline yet — one sample cannot show movement".to_owned()),
            ),
            (PoolObservation::Present { frame_seq, .. }, Some(_)) => {
                if self.frame_static_ticks == 0 {
                    (
                        RungState::Ok,
                        Some(format!(
                            "frames are being presented (frame_seq {frame_seq})"
                        )),
                    )
                } else {
                    (
                        RungState::Untested,
                        Some(format!(
                            "frame_seq has been {frame_seq} for {}s — nothing is drawing, which \
                             is normal on an idle desktop",
                            self.frame_static_ticks * TICK_SECS
                        )),
                    )
                }
            }
            _ => (
                RungState::Untested,
                Some("no pool to measure — see the pool rung".to_owned()),
            ),
        };
        RungReport {
            rung: Rung::Liveness,
            state,
            detail,
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
            self.pool_rung(),
            RungReport {
                rung: Rung::DisplayMode,
                state: ok_or_fail(self.mode_ok),
                detail: (!self.mode_ok).then(|| {
                    let mode = self.actual_mode.map_or_else(
                        || "unknown mode".to_owned(),
                        |m| format!("{}x{} @ {} Hz", m.width, m.height, m.hz),
                    );
                    let scale = self.actual_scale_percent.map_or_else(
                        || "unknown scale".to_owned(),
                        |percent| format!("{percent}% scale"),
                    );
                    let placement = self.actual_placement.map_or_else(
                        || "unknown placement".to_owned(),
                        |placement| {
                            let role = if placement.primary {
                                "primary"
                            } else {
                                "secondary"
                            };
                            format!(
                                "{role} at ({}, {})",
                                placement.origin.0, placement.origin.1
                            )
                        },
                    );
                    format!(
                        "{mode}, {scale}, {placement}; wanted {}x{} @ {} Hz, {}% scale, primary at (0, 0)",
                        self.desired.width,
                        self.desired.height,
                        self.desired.hz,
                        self.desired_scale_percent
                    )
                }),
            },
            self.server_rung(),
            self.input_desktop_rung(),
            self.liveness_rung(),
            self.audio_rung(),
        ]
    }

    /// The `audio` rung: can this host capture audio at all?
    ///
    /// Red on both fleet hosts today, and that is the honest answer rather than
    /// a defect: neither has a render endpoint, so there is nothing to capture.
    /// It never gates a connect — see `Rung::gates_bring_up` — because a remote
    /// desktop without sound is a working remote desktop. It exists so that
    /// missing audio is *visible* instead of silent, which is the difference
    /// between a known limitation and a bug report.
    fn audio_rung(&self) -> RungReport {
        let (state, detail) = match self.audio_endpoint {
            // Green, with the scope stated. Endpoint loopback captures the
            // system mix rather than this session's audio, which is an accepted
            // compromise (per-session capture is not something WASAPI offers) --
            // and an accepted compromise that nobody can see is indistinguishable
            // from a defect, which is the same reason this rung exists at all.
            Some(true) => (
                RungState::Ok,
                Some("system mix, including system sounds — not session-scoped".to_owned()),
            ),
            Some(false) => (
                RungState::Fail,
                Some(
                    "no audio render endpoint on this host, so there is nothing to \
                     capture — audio is unavailable, not broken; fix: install VB-CABLE \
                     with mdrdp deploy"
                        .to_owned(),
                ),
            ),
            None => (RungState::Unknown, None),
        };
        RungReport {
            rung: Rung::Audio,
            state,
            detail,
        }
    }

    /// The `server` rung: supervised **and** actually accepting.
    ///
    /// The two halves are separate facts and the second is the one that was
    /// missing: `running` goes true the instant a spawn returns, so a server
    /// still inside its 30 s wait for the pool reported green for that whole
    /// window.
    fn server_rung(&self) -> RungReport {
        let (state, detail) = if !self.server.running {
            (RungState::Fail, None)
        } else {
            match self.server_listening {
                Some(true) => (RungState::Ok, None),
                Some(false) => (
                    RungState::Fail,
                    Some(
                        "the process is up but nothing is listening on the video port yet"
                            .to_owned(),
                    ),
                ),
                // Supervised and alive; we simply could not ask the OS. Saying
                // "not accepting" on that would be inventing a fault.
                None => (
                    RungState::Unknown,
                    Some("could not read the listening sockets".to_owned()),
                ),
            }
        };
        RungReport {
            rung: Rung::Server,
            state,
            detail,
        }
    }

    /// The `input-desktop` rung: report-only, and deliberately cautious.
    fn input_desktop_rung(&self) -> RungReport {
        let (state, detail) = match &self.input_desktop {
            InputDesktopObservation::Matches { .. } => (RungState::Ok, None),
            InputDesktopObservation::Differs { ours, input } => (
                RungState::Fail,
                Some(format!(
                    "the input desktop is {input:?} but the stack is on {ours:?}: injected input \
                     cannot land. A locked console does this; unlock it on the host."
                )),
            ),
            InputDesktopObservation::Unknown(why) => (RungState::Unknown, Some(why.clone())),
        };
        RungReport {
            rung: Rung::InputDesktop,
            state,
            detail,
        }
    }

    /// Assemble the wire status. `uptime_s` comes from the runner, which owns time.
    pub fn status(&self, uptime_s: u64) -> StatusReport {
        // Derived from the ladder, so there is exactly one definition of "which
        // step is broken". Only `Fail` gates the connect: see `stuck_from_rungs`.
        let rungs = self.rungs();
        StatusReport {
            schema: SCHEMA,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            uptime_s,
            creator: self.creator.report(),
            device_present: self.device_present,
            display_mode: self.actual_mode.map(Mode::report),
            desired_display_mode: self.desired.report(),
            desktop_scale_percent: self.actual_scale_percent.unwrap_or(0),
            desired_desktop_scale_percent: self.desired_scale_percent,
            mode_ok: self.mode_ok,
            server: self.server.report(),
            stuck: crate::control::stuck_from_rungs(&rungs),
            rungs,
            pool: self.pool_report(),
            viewer_connected: self.viewer_connected,
            cycling: self.is_cycling(),
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
        /// What `audio_endpoint()` reports. `None` (the Default) means "could
        /// not judge", which keeps every pre-existing test's ladder unchanged
        /// apart from one new Unknown rung that gates nothing.
        audio_endpoint: Option<bool>,
        mode: Option<Mode>,
        scale_percent: Option<u32>,
        placement: Option<DisplayPlacement>,
        mode_set_fails: bool,
        server_running: bool,
        server_pending_exit: Option<i32>,
        /// When true every server spawn dies before the next poll (crash loop).
        server_dies_instantly: bool,
        /// What `pool()` reports. Defaults to a healthy generation so existing
        /// tests are unaffected by the rung's arrival.
        pool: Option<PoolObservation>,
        /// What `server_listening()` reports. `None` here means "use the default
        /// healthy answer"; `Some(None)` means the OS could not be asked.
        listening: Option<Option<bool>>,
        input_desktop: Option<InputDesktopObservation>,
        viewer: Option<Option<bool>>,
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
        fn audio_endpoint(&mut self) -> Option<bool> {
            self.audio_endpoint
        }
        fn display_mode(&mut self) -> Option<Mode> {
            self.mode
        }
        fn display_scale_percent(&mut self) -> Option<u32> {
            Some(self.scale_percent.unwrap_or(100))
        }
        fn display_placement(&mut self) -> Option<DisplayPlacement> {
            Some(
                self.placement
                    .unwrap_or(DisplayPlacement::primary_at_origin()),
            )
        }
        fn set_display_mode(&mut self, mode: Mode) -> Result<(), String> {
            self.calls.push(format!("set_mode {}hz", mode.hz));
            if self.mode_set_fails {
                Err("scripted failure".into())
            } else {
                self.mode = Some(mode);
                self.placement = Some(DisplayPlacement::primary_at_origin());
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
        fn kill_creator(&mut self) {
            self.calls.push("kill_creator".into());
            if self.creator_running {
                self.creator_running = false;
                self.creator_pending_exit = Some(-1);
            }
        }
        fn pool(&mut self) -> PoolObservation {
            self.pool.clone().unwrap_or(PoolObservation::Present {
                generation: 1,
                frame_seq: 100,
            })
        }
        fn server_listening(&mut self) -> Option<bool> {
            self.listening.unwrap_or(Some(self.server_running))
        }
        fn viewer_connected(&mut self) -> Option<bool> {
            self.viewer.unwrap_or(Some(false))
        }
        fn input_desktop(&mut self) -> InputDesktopObservation {
            self.input_desktop
                .clone()
                .unwrap_or(InputDesktopObservation::Matches {
                    desktop: "Default".into(),
                })
        }
    }

    /// Bring a fake to a fully green steady state, so a pool test starts from
    /// "everything else is fine" and the rung under test is the only variable.
    fn settled() -> (Reconciler, FakeOps) {
        let mut rec = Reconciler::new();
        let mut ops = FakeOps {
            device_id: Some(r"\\.\DISPLAY1".into()),
            mode: Some(DESIRED_MODE),
            ..Default::default()
        };
        for _ in 0..4 {
            rec.tick(&mut ops);
        }
        ops.calls.clear();
        (rec, ops)
    }

    fn rung_state(rec: &Reconciler, rung: Rung) -> RungState {
        rec.rungs()
            .into_iter()
            .find(|r| r.rung == rung)
            .expect("every rung is reported")
            .state
    }

    #[test]
    fn a_live_but_not_yet_listening_server_is_not_green() {
        // The blind spot this rung closes: `running` is set the instant a spawn
        // returns, so a server still inside its 30 s wait for the pool used to
        // report green for that whole window.
        let (mut rec, mut ops) = settled();
        assert_eq!(rung_state(&rec, Rung::Server), RungState::Ok);
        ops.listening = Some(Some(false));
        rec.tick(&mut ops);
        assert_eq!(rung_state(&rec, Rung::Server), RungState::Fail);
        assert!(ops.server_running, "the process is still alive");
    }

    #[test]
    fn a_listening_check_that_cannot_be_answered_is_unknown_not_a_failure() {
        let (mut rec, mut ops) = settled();
        ops.listening = Some(None);
        rec.tick(&mut ops);
        assert_eq!(rung_state(&rec, Rung::Server), RungState::Unknown);
    }

    #[test]
    fn a_dead_server_fails_the_rung_whatever_the_socket_says() {
        // Ordering matters: a stale LISTEN entry must not outvote a dead process.
        let (mut rec, mut ops) = settled();
        ops.server_running = false;
        ops.server_pending_exit = Some(1);
        ops.listening = Some(Some(true));
        rec.tick(&mut ops);
        assert_eq!(rung_state(&rec, Rung::Server), RungState::Fail);
    }

    #[test]
    fn a_foreign_input_desktop_fails_the_rung_but_never_reaches_stuck() {
        // Incident B. It stops injected input, not pixels — so it must be loudly
        // reported and must NOT gate the connect, or a merely-locked console
        // would make every client on the fleet refuse to open a session.
        let (mut rec, mut ops) = settled();
        ops.input_desktop = Some(InputDesktopObservation::Differs {
            ours: "Default".into(),
            input: "Winlogon".into(),
        });
        rec.tick(&mut ops);

        assert_eq!(rung_state(&rec, Rung::InputDesktop), RungState::Fail);
        let status = rec.status(1);
        assert_eq!(
            status.stuck, None,
            "a locked console must not gate bring-up"
        );
        assert!(
            crate::control::green(&status),
            "green() must still hold: no pixel is stopped by a locked console"
        );
        // Asserted through the DERIVATION too, not only through today's
        // field-based `stuck`. Without this the check is vacuous until unit 4
        // wires the derivation in: it would pass against a ladder that puts
        // input-desktop into `stuck`, which is exactly the fleet-wide connect
        // refusal this rule exists to prevent.
        assert_eq!(
            crate::control::stuck_from_rungs(&status.rungs),
            None,
            "the rung derivation must agree: input-desktop does not gate bring-up"
        );
    }

    #[test]
    fn an_access_denied_input_desktop_is_unknown_not_a_confident_lock_report() {
        // Access-denied is equally what a caller in another session or window
        // station gets. Reporting "the console is locked" on that evidence is
        // the confident-wrong-story failure this tranche exists to avoid.
        let (mut rec, mut ops) = settled();
        ops.input_desktop = Some(InputDesktopObservation::Unknown(
            "OpenInputDesktop: access denied".into(),
        ));
        rec.tick(&mut ops);
        assert_eq!(rung_state(&rec, Rung::InputDesktop), RungState::Unknown);
        assert_eq!(rec.status(1).stuck, None);
    }

    #[test]
    fn neither_report_only_rung_ever_remediates() {
        // Rungs above the bring-up ladder report and nothing else — the reversal
        // the review pass forced (HLD §6).
        let (mut rec, mut ops) = settled();
        ops.input_desktop = Some(InputDesktopObservation::Differs {
            ours: "Default".into(),
            input: "Winlogon".into(),
        });
        ops.listening = Some(Some(false));
        for _ in 0..POOL_FAULT_TICKS + 5 {
            rec.tick(&mut ops);
        }
        assert!(
            !ops.calls
                .iter()
                .any(|c| c == "kill_server" || c == "kill_creator"),
            "report-only rungs must provoke no remediation: {:?}",
            ops.calls
        );
    }

    #[test]
    fn stuck_now_comes_from_the_ladder_and_can_name_the_pool() {
        // The wiring's whole point: `stuck` is derived from the rungs, so it can
        // name a step the old field-based version had no concept of. Before this,
        // a stranded pool was invisible to `stuck` and therefore to `green` —
        // the client connected happily to a server capturing nothing.
        let (mut rec, mut ops) = settled();
        assert_eq!(rec.status(1).stuck, None, "healthy to begin with");

        ops.pool = Some(PoolObservation::NoPool);
        rec.tick(&mut ops);
        let status = rec.status(1);
        assert_eq!(status.stuck.as_deref(), Some("pool"));
        assert!(
            !crate::control::green(&status),
            "a host publishing no pool must not read as green"
        );
    }

    #[test]
    fn an_unreadable_section_does_not_gate_the_connect() {
        // The counterpart rule. A torn read on the tick a client happens to probe
        // must not refuse a session that would have worked: ignorance is not
        // health, but it is not breakage either.
        let (mut rec, mut ops) = settled();
        ops.pool = Some(PoolObservation::Unreadable("mid-write".into()));
        rec.tick(&mut ops);
        let status = rec.status(1);
        assert_eq!(status.stuck, None, "ignorance must not gate");
        assert!(crate::control::green(&status));
        // …but it is still visible to a human.
        assert_eq!(
            crate::control::first_unsatisfied_rung(&status.rungs).as_deref(),
            Some("pool")
        );
    }

    #[test]
    fn a_cycle_tears_the_stack_down_and_waits_for_the_device_to_actually_go() {
        // The reason this is a state and not three loose calls: supervision
        // respawns on its own backoff, and device removal is asynchronous. If
        // bring-up ran during the wait it would race the rebuild it is watching.
        let (mut rec, mut ops) = settled();
        rec.request_device_cycle();

        // Device still present: keep tearing down, spawn nothing.
        for _ in 0..3 {
            rec.tick(&mut ops);
        }
        assert!(rec.is_cycling(), "still waiting for the device to go");
        assert!(
            !ops.calls.iter().any(|c| c.starts_with("spawn")),
            "bring-up must be suppressed during a cycle: {:?}",
            ops.calls
        );
        assert!(ops.calls.iter().any(|c| c == "kill_creator"));

        // The device goes: the cycle ends and ordinary bring-up resumes.
        ops.device_id = None;
        rec.tick(&mut ops);
        assert!(!rec.is_cycling(), "the cycle ends when the device has gone");
        assert_eq!(
            rec.server_generation, None,
            "the old attachment is forgotten"
        );

        ops.device_id = Some(r"\\.\DISPLAY2".into());
        ops.calls.clear();
        // Several ticks, not one: the first reaps the creator's death and pays
        // its backoff, and the new device identity legitimately triggers a
        // server restart. What matters is that bring-up resumes at all.
        for _ in 0..4 {
            rec.tick(&mut ops);
        }
        assert!(
            ops.calls
                .iter()
                .any(|c| c == "spawn_creator" || c == "spawn_server"),
            "bring-up must resume after the cycle: {:?}",
            ops.calls
        );
    }

    #[test]
    fn a_cycle_gives_up_at_its_deadline_rather_than_looping() {
        // "One pass, never a loop" (parent HLD §3.4). A device that will not
        // leave is reported, not retried for ever — otherwise the stack stays
        // torn down indefinitely and the host is worse off than when it started.
        let (mut rec, mut ops) = settled();
        rec.request_device_cycle();
        for _ in 0..CYCLE_DEADLINE_TICKS + 1 {
            rec.tick(&mut ops);
        }
        assert!(
            !rec.is_cycling(),
            "the cycle must give up at its deadline, not wait for ever"
        );
        ops.calls.clear();
        for _ in 0..4 {
            rec.tick(&mut ops);
        }
        assert!(
            ops.calls.iter().any(|c| c.starts_with("spawn")),
            "bring-up must resume after giving up: {:?}",
            ops.calls
        );
    }

    #[test]
    fn a_second_cycle_request_while_one_runs_does_not_extend_it() {
        // Idempotent by design: a caller retrying after a read timeout must not
        // restart the cycle it may already have started, leaving the stack torn
        // down for longer each time it asks.
        //
        // Tested through the DEADLINE, because that is where a re-request is
        // observable. Ending the cycle via device-gone cannot tell the two apart
        // — proven by mutation: resetting the counter left that version green.
        let (mut rec, mut ops) = settled();
        rec.request_device_cycle();
        for _ in 0..CYCLE_DEADLINE_TICKS - 1 {
            rec.tick(&mut ops);
        }
        assert!(rec.is_cycling(), "not at the deadline yet");

        rec.request_device_cycle(); // must be ignored, not a fresh start
        rec.tick(&mut ops);
        assert!(
            !rec.is_cycling(),
            "a repeated request restarted the clock and extended the teardown"
        );
    }

    #[test]
    fn liveness_reads_ok_while_the_frame_counter_moves() {
        // The MOVING direction only — an always-Ok implementation satisfies this
        // test, and that is fine because the "only" half is its sibling's job
        // (`a_static_frame_counter_is_untested_never_a_failure`). Naming it
        // "only while" would have been a claim this test does not make; proven
        // by mutation, which left it green.
        let (mut rec, mut ops) = settled();
        let mut seq = 100;
        for _ in 0..3 {
            seq += 5;
            ops.pool = Some(PoolObservation::Present {
                generation: 1,
                frame_seq: seq,
            });
            rec.tick(&mut ops);
        }
        assert_eq!(rung_state(&rec, Rung::Liveness), RungState::Ok);
    }

    #[test]
    fn a_static_frame_counter_is_untested_never_a_failure() {
        // An idle desktop presents nothing. Calling that broken would be the
        // confident wrong story this whole ladder exists to avoid — and it is
        // exactly what a naive "no frames = wedged" check would say.
        let (mut rec, mut ops) = settled();
        ops.pool = Some(PoolObservation::Present {
            generation: 1,
            frame_seq: 100,
        });
        for _ in 0..5 {
            rec.tick(&mut ops);
        }
        assert_eq!(
            rung_state(&rec, Rung::Liveness),
            RungState::Untested,
            "a static counter must never read as Fail"
        );
        let status = rec.status(1);
        assert_eq!(status.stuck, None, "and it must not gate the connect");
    }

    #[test]
    fn liveness_claims_nothing_from_a_single_sample() {
        // Movement needs two readings. A fresh reconciler that reported Ok from
        // one sample would be asserting something it cannot know.
        let (mut rec, mut ops) = settled();
        rec.last_frame_seq = None;
        ops.pool = Some(PoolObservation::Present {
            generation: 2,
            frame_seq: 7,
        });
        // The tick that establishes the baseline must not claim movement.
        rec.last_frame_seq = None;
        rec.frame_static_ticks = 0;
        let baseline = rec.liveness_rung();
        assert_eq!(baseline.state, RungState::Untested);
        assert!(baseline
            .detail
            .as_deref()
            .unwrap_or("")
            .contains("baseline"));
    }

    #[test]
    fn a_healthy_pool_leaves_the_rung_green_and_remediates_nothing() {
        let (mut rec, mut ops) = settled();
        for _ in 0..POOL_FAULT_TICKS + 3 {
            rec.tick(&mut ops);
        }
        assert_eq!(rung_state(&rec, Rung::Pool), RungState::Ok);
        assert!(
            !ops.calls
                .iter()
                .any(|c| c == "kill_creator" || c == "kill_server"),
            "a healthy pool must provoke nothing: {:?}",
            ops.calls
        );
    }

    #[test]
    fn an_unreadable_section_is_unknown_and_never_remediated() {
        // Us failing to read is not the driver failing to publish. This is the
        // rule that stops the health check causing the outage it detects.
        let (mut rec, mut ops) = settled();
        ops.pool = Some(PoolObservation::Unreadable("mid-write".into()));
        for _ in 0..POOL_FAULT_TICKS + 5 {
            rec.tick(&mut ops);
        }
        assert_eq!(rung_state(&rec, Rung::Pool), RungState::Unknown);
        assert!(
            !ops.calls
                .iter()
                .any(|c| c == "kill_creator" || c == "kill_server"),
            "an unreadable section must never be acted on: {:?}",
            ops.calls
        );
    }

    #[test]
    fn no_pool_restarts_the_creator_not_the_server() {
        // Generation 0 means the driver published nothing. A fresh SERVER would
        // meet the same 0, refuse it, and die into backoff — turning a silent
        // wedge into a crash loop. Only a rebuilt device fixes it, and the
        // creator owns the device's lifetime.
        let (mut rec, mut ops) = settled();
        ops.pool = Some(PoolObservation::NoPool);
        for _ in 0..POOL_FAULT_TICKS {
            rec.tick(&mut ops);
        }
        assert_eq!(rung_state(&rec, Rung::Pool), RungState::Fail);
        assert!(
            ops.calls.iter().any(|c| c == "kill_creator"),
            "expected a creator restart: {:?}",
            ops.calls
        );
        assert!(
            !ops.calls.iter().any(|c| c == "kill_server"),
            "the server must NOT be restarted for a missing pool: {:?}",
            ops.calls
        );
    }

    #[test]
    fn a_pool_fault_is_debounced_past_a_rebuild_race() {
        // A device cycle passes through "no pool" on its way back up. Acting
        // inside that window would fight the rebuild being watched.
        //
        // Tick counts here are LITERAL, not derived from POOL_FAULT_TICKS. A loop
        // written `0..POOL_FAULT_TICKS - 1` runs zero times when the constant is
        // 1, so it passes vacuously against an implementation with no debounce at
        // all — proven by mutation. The literal 2 encodes the real requirement.
        const {
            assert!(
                POOL_FAULT_TICKS >= 2,
                "the debounce must span more than one tick to outlive a rebuild race"
            )
        };

        let (mut rec, mut ops) = settled();
        ops.pool = Some(PoolObservation::NoPool);
        rec.tick(&mut ops);
        rec.tick(&mut ops);
        assert!(
            !ops.calls.iter().any(|c| c == "kill_creator"),
            "acted after only two faulty ticks: {:?}",
            ops.calls
        );
        // Recovery inside the window must clear the count, not bank it.
        ops.pool = None;
        rec.tick(&mut ops);
        ops.pool = Some(PoolObservation::NoPool);
        rec.tick(&mut ops);
        rec.tick(&mut ops);
        assert!(
            !ops.calls.iter().any(|c| c == "kill_creator"),
            "a healthy tick must reset the fault count, not bank it: {:?}",
            ops.calls
        );
    }

    #[test]
    fn a_server_left_on_a_superseded_generation_is_restarted() {
        // Incident A's signature: everything structurally healthy, the server
        // reading a section nothing writes to any more.
        let (mut rec, mut ops) = settled();
        assert_eq!(rec.server_generation, Some(1), "attached at spawn");
        ops.pool = Some(PoolObservation::Present {
            generation: 2,
            frame_seq: 5,
        });
        for _ in 0..POOL_FAULT_TICKS {
            rec.tick(&mut ops);
        }
        assert!(
            ops.calls.iter().any(|c| c == "kill_server"),
            "expected a server restart: {:?}",
            ops.calls
        );
        assert!(
            !ops.calls.iter().any(|c| c == "kill_creator"),
            "the creator is fine; only the server is stale: {:?}",
            ops.calls
        );
    }

    #[test]
    fn a_restarted_server_records_the_generation_it_attached_to() {
        // Without this the staleness check compares against a stale memory and
        // restarts for ever.
        let (mut rec, mut ops) = settled();
        ops.pool = Some(PoolObservation::Present {
            generation: 9,
            frame_seq: 1,
        });
        for _ in 0..POOL_FAULT_TICKS + 3 {
            rec.tick(&mut ops);
        }
        assert_eq!(
            rec.server_generation,
            Some(9),
            "the respawned server must be recorded against the CURRENT generation"
        );
        assert_eq!(rung_state(&rec, Rung::Pool), RungState::Ok);
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
    fn display_health_requires_the_idd_to_be_primary_at_origin() {
        for placement in [
            DisplayPlacement {
                primary: false,
                origin: (0, 0),
            },
            DisplayPlacement {
                primary: true,
                origin: (1920, 0),
            },
        ] {
            let mut ops = FakeOps {
                device_id: Some("dpy-1".into()),
                mode: Some(DESIRED_MODE),
                placement: Some(placement),
                mode_set_fails: true,
                ..FakeOps::default()
            };
            let mut rec = Reconciler::new();

            rec.tick(&mut ops);

            let status = rec.status(0);
            assert!(!status.mode_ok, "wrong placement must not be healthy");
            assert_eq!(status.stuck.as_deref(), Some("display-mode"));
            assert!(!ops.server_running, "server must wait for placement");
        }
    }

    #[test]
    fn placement_is_corrected_even_when_mode_matches() {
        let wrong = DisplayPlacement {
            primary: false,
            origin: (1920, 0),
        };
        let mut ops = FakeOps {
            device_id: Some("dpy-1".into()),
            mode: Some(DESIRED_MODE),
            placement: Some(wrong),
            ..FakeOps::default()
        };
        let mut rec = Reconciler::new();

        rec.tick(&mut ops);

        assert!(ops.calls.contains(&"set_mode 240hz".into()));
        assert_eq!(ops.placement, Some(DisplayPlacement::primary_at_origin()));
        assert!(rec.status(0).mode_ok);
    }

    #[test]
    fn placement_correction_restarts_a_running_server() {
        let mut ops = FakeOps {
            device_id: Some("dpy-1".into()),
            mode: Some(DESIRED_MODE),
            placement: Some(DisplayPlacement::primary_at_origin()),
            ..FakeOps::default()
        };
        let mut rec = Reconciler::new();
        rec.tick(&mut ops);
        assert!(ops.server_running);
        ops.calls.clear();

        ops.placement = Some(DisplayPlacement {
            primary: false,
            origin: (1920, 0),
        });
        rec.tick(&mut ops);

        assert!(ops.calls.contains(&"set_mode 240hz".into()));
        assert!(ops.calls.contains(&"kill_server".into()));
        assert!(!ops.server_running);
    }

    #[test]
    fn status_detail_names_wrong_primary_and_origin() {
        let mut ops = FakeOps {
            device_id: Some("dpy-1".into()),
            mode: Some(DESIRED_MODE),
            placement: Some(DisplayPlacement {
                primary: false,
                origin: (1920, 0),
            }),
            mode_set_fails: true,
            ..FakeOps::default()
        };
        let mut rec = Reconciler::new();
        rec.tick(&mut ops);

        let detail = rec
            .status(0)
            .rungs
            .into_iter()
            .find(|rung| rung.rung == Rung::DisplayMode)
            .and_then(|rung| rung.detail)
            .expect("display rung explains a placement mismatch");
        assert!(detail.contains("secondary"), "{detail}");
        assert!(detail.contains("(1920, 0)"), "{detail}");
        assert!(detail.contains("primary at (0, 0)"), "{detail}");
    }

    #[test]
    fn a_client_mode_request_stops_the_old_server_until_the_new_mode_is_active() {
        let mut ops = FakeOps {
            device_id: Some("dpy-1".to_owned()),
            mode: Some(DESIRED_MODE),
            scale_percent: Some(200),
            server_running: true,
            ..FakeOps::default()
        };
        let mut rec = Reconciler::new();
        rec.request_display_mode(
            Mode {
                width: 5120,
                height: 2880,
                hz: 240,
            },
            200,
        )
        .expect("5K is a supported client request");

        rec.tick(&mut ops);

        assert_eq!(
            ops.mode,
            Some(Mode {
                width: 5120,
                height: 2880,
                hz: 240
            })
        );
        assert!(ops.calls.contains(&"kill_server".to_owned()));
        let status = rec.status(0);
        assert_eq!(
            status.desired_display_mode,
            ModeReport {
                width: 5120,
                height: 2880,
                hz: 240
            }
        );
        assert_eq!(status.desktop_scale_percent, 200);
        assert_eq!(status.desired_desktop_scale_percent, 200);
    }

    #[test]
    fn a_requested_scale_is_measured_not_echoed_and_gates_the_encoder() {
        let mut ops = FakeOps {
            device_id: Some("dpy-1".to_owned()),
            mode: Some(Mode {
                width: 5120,
                height: 2880,
                hz: 240,
            }),
            scale_percent: Some(100),
            ..FakeOps::default()
        };
        let mut rec = Reconciler::new();
        rec.request_display_mode(ops.mode.unwrap(), 200).unwrap();

        rec.tick(&mut ops);

        let status = rec.status(0);
        assert_eq!(status.desktop_scale_percent, 100);
        assert_eq!(status.desired_desktop_scale_percent, 200);
        assert!(!status.mode_ok);
        assert!(!ops.server_running, "encoder started at the wrong UI scale");
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
        assert_eq!(status.display_mode, Some(DESIRED_MODE.report()));
    }
    #[test]
    fn a_host_with_no_audio_endpoint_reports_it_red_without_blocking_anything() {
        // Both fleet hosts are in exactly this state. The rung must be RED --
        // silence that is never reported is indistinguishable from a broken
        // feature -- and must gate nothing, because a remote desktop without
        // sound is a working remote desktop.
        let (mut rec, mut ops) = settled();
        ops.audio_endpoint = Some(false);
        rec.tick(&mut ops);
        assert_eq!(rung_state(&rec, Rung::Audio), RungState::Fail);
        assert_eq!(
            crate::control::stuck_from_rungs(&rec.rungs()),
            None,
            "a red audio rung must never make the host stuck"
        );

        // And with an endpoint it goes green, so the red above is a measurement
        // rather than a constant this test would report either way.
        ops.audio_endpoint = Some(true);
        rec.tick(&mut ops);
        assert_eq!(rung_state(&rec, Rung::Audio), RungState::Ok);

        // Unknown stays distinct from both: "could not ask" is not "no".
        ops.audio_endpoint = None;
        rec.tick(&mut ops);
        assert_eq!(rung_state(&rec, Rung::Audio), RungState::Unknown);
    }
}
