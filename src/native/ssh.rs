//! The ssh child that carries a native session.
//!
//! One `ssh -N` child serves both the probe and the session: three `-L` forwards
//! (video, input, control), spawned before the transport decision and killed the
//! moment the decision is "not native". Everything here is deliberately testable
//! without ssh: argument construction and stderr classification are pure, and
//! readiness polling takes the child's liveness as a closure.
//!
//! Requirements this module encodes (HLD §4.2):
//! - **BatchMode** — a GUI app owns no tty; key auth is host config, like auto-logon.
//! - **Explicit `127.0.0.1` bind addresses plus `GatewayPorts=no`** — a user's
//!   `~/.ssh/config` must not be able to put the input-injection forward on
//!   `0.0.0.0`.
//! - **`ExitOnForwardFailure`** — a lost local-port race kills the child rather
//!   than leaving a half-tunnel, and the exit is classified from stderr.
//! - **`ServerAliveInterval/CountMax`** — a dead transport kills ssh within ~15 s,
//!   which closes the forwards and surfaces the failure; a frozen picture must
//!   never be silent.
//! - stderr is **piped** (the only classification signal ssh emits — its exit code
//!   is 255 for everything); stdin/stdout are null so nothing can pollute
//!   `--stage-json` output or block on a tty.

use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The rhydra server's loopback ports on the host (video, input), and the agent's
/// control port. The remote ends of the three forwards.
pub const REMOTE_VIDEO_PORT: u16 = 9500;
pub const REMOTE_INPUT_PORT: u16 = 9501;
pub const REMOTE_CONTROL_PORT: u16 = 9502;
/// The auxiliary session channel: clipboard now, audio in tranche 6. Kept off
/// the video port because video latency is the product's premise and nothing
/// lower-priority may back it up; kept off the input port because that dialect
/// is frameless and an unknown record kind there is terminal.
pub const REMOTE_AUX_PORT: u16 = 9503;

/// How long ssh itself gets to establish TCP to the host.
const SSH_CONNECT_TIMEOUT_SECS: u32 = 4;

/// The local (Mac-side) ports the four forwards bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForwardPorts {
    pub video: u16,
    pub input: u16,
    pub control: u16,
    pub aux: u16,
}

impl ForwardPorts {
    pub fn aux_addr(&self) -> SocketAddr {
        (Ipv4Addr::LOCALHOST, self.aux).into()
    }
    pub fn video_addr(&self) -> SocketAddr {
        (Ipv4Addr::LOCALHOST, self.video).into()
    }
    pub fn input_addr(&self) -> SocketAddr {
        (Ipv4Addr::LOCALHOST, self.input).into()
    }
    pub fn control_addr(&self) -> SocketAddr {
        (Ipv4Addr::LOCALHOST, self.control).into()
    }
}

/// Pick four free loopback ports by bind-and-drop.
///
/// The race window between the drop and ssh's own bind spans ssh's whole
/// connect+auth phase — real, not theoretical. `ExitOnForwardFailure` turns a lost
/// race into a child exit classified as [`ProbeFailure::SshBind`], and the caller
/// retries once with fresh ports.
pub fn allocate_ports() -> std::io::Result<ForwardPorts> {
    let bind = || -> std::io::Result<(TcpListener, u16)> {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = l.local_addr()?.port();
        Ok((l, port))
    };
    // Hold all four listeners until every port is chosen, so they are distinct.
    let (a, video) = bind()?;
    let (b, input) = bind()?;
    let (c, control) = bind()?;
    let (d, aux) = bind()?;
    drop((a, b, c, d));
    Ok(ForwardPorts {
        video,
        input,
        control,
        aux,
    })
}

/// What to connect to and as whom.
#[derive(Debug, Clone)]
pub struct TunnelSpec {
    pub host: String,
    /// `None` lets `~/.ssh/config` decide the login name.
    pub ssh_user: Option<String>,
    pub local: ForwardPorts,
}

/// The exact ssh argument vector. Pure so the safety-bearing flags are testable.
pub fn ssh_args(spec: &TunnelSpec) -> Vec<String> {
    let dest = match &spec.ssh_user {
        Some(user) => format!("{user}@{}", spec.host),
        None => spec.host.clone(),
    };
    let fwd = |local: u16, remote: u16| format!("127.0.0.1:{local}:127.0.0.1:{remote}");
    vec![
        "-N".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        format!("ConnectTimeout={SSH_CONNECT_TIMEOUT_SECS}"),
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-o".into(),
        "GatewayPorts=no".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        "-o".into(),
        "ServerAliveInterval=5".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
        "-L".into(),
        fwd(spec.local.video, REMOTE_VIDEO_PORT),
        "-L".into(),
        fwd(spec.local.input, REMOTE_INPUT_PORT),
        "-L".into(),
        fwd(spec.local.control, REMOTE_CONTROL_PORT),
        "-L".into(),
        fwd(spec.local.aux, REMOTE_AUX_PORT),
        dest,
    ]
}

/// Why the native connect could not proceed. Each variant maps to a stage-json
/// qualifier and, under `--native`, a remedy-naming error message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeFailure {
    /// BatchMode refused — no usable key.
    SshAuth,
    /// The host's key is not trusted (changed key; `accept-new` covers first contact).
    SshHostKey,
    /// TCP to the host failed (down, unresolvable, filtered).
    SshUnreachable,
    /// A local forward port was taken between allocation and ssh's bind.
    SshBind,
    /// ssh exited for a reason the patterns do not cover; carries the stderr tail.
    SshFailed(String),
    /// The tunnel is up but nothing answers on the agent control port.
    NoAgent,
    /// The agent answered but the stack is not green; carries what it is waiting on.
    NotGreen(String),
    /// The agent is green but the video port refused — the bring-up window.
    NoServer,
    /// The server speaks a wire version outside this client's range.
    VersionMismatch { host: u32, client: u32 },
    /// The probe deadline expired in the named phase.
    Deadline(&'static str),
    /// An I/O failure not covered above.
    Io(String),
}

impl ProbeFailure {
    /// The stage-json qualifier for this failure.
    pub fn qualifier(&self) -> &'static str {
        match self {
            ProbeFailure::SshAuth => "ssh-auth",
            ProbeFailure::SshHostKey => "ssh-hostkey",
            ProbeFailure::SshUnreachable => "ssh-unreachable",
            ProbeFailure::SshBind => "ssh-bind",
            ProbeFailure::SshFailed(_) => "ssh-failed",
            ProbeFailure::NoAgent => "no-agent",
            ProbeFailure::NotGreen(_) => "not-green",
            ProbeFailure::NoServer => "no-server",
            ProbeFailure::VersionMismatch { .. } => "version-mismatch",
            ProbeFailure::Deadline(_) => "deadline",
            ProbeFailure::Io(_) => "io",
        }
    }

    /// The `--native` hard-error text: what happened and what to do about it.
    pub fn remedy(&self, host: &str) -> String {
        match self {
            ProbeFailure::SshAuth => format!(
                "ssh to {host} refused key authentication: native connect needs a key \
                 (BatchMode). Install one — for an admin account that is \
                 ProgramData\\ssh\\administrators_authorized_keys on the host."
            ),
            ProbeFailure::SshHostKey => format!(
                "ssh does not trust {host}'s host key (it changed?). Verify and fix \
                 ~/.ssh/known_hosts, then retry."
            ),
            ProbeFailure::SshUnreachable => {
                format!(
                    "ssh could not reach {host}: host down, name unresolvable, or port 22 filtered."
                )
            }
            ProbeFailure::SshBind => {
                "a local forward port was taken while ssh started; retry the connect.".into()
            }
            ProbeFailure::SshFailed(tail) => format!("ssh failed: {tail}"),
            ProbeFailure::NoAgent => format!(
                "the tunnel is up but no rhydra agent answers on {host}. \
                 Run `mdrdp deploy {host}` (or check `rhydra-agent status` on the host)."
            ),
            ProbeFailure::NotGreen(waiting) => format!(
                "rhydra on {host} is not ready (waiting on: {waiting}). \
                 Check `rhydra-agent status --wait` on the host."
            ),
            ProbeFailure::NoServer => format!(
                "rhydra's agent on {host} is green but the video server is not \
                 accepting yet; retry in a moment."
            ),
            ProbeFailure::VersionMismatch { host: h, client } => {
                if *h < *client {
                    format!(
                        "{host} runs wire v{h} but this mdrdp speaks v{client}: \
                         run `mdrdp deploy {host}` to update the host."
                    )
                } else {
                    format!(
                        "{host} runs wire v{h} but this mdrdp only speaks v{client}: \
                         update mdrdp."
                    )
                }
            }
            ProbeFailure::Deadline(phase) => {
                format!("native probe of {host} timed out during {phase}.")
            }
            ProbeFailure::Io(e) => format!("native connect to {host} failed: {e}"),
        }
    }
}

/// Classify an exited ssh's stderr. ssh exits 255 for every failure, so the text
/// is the only signal; patterns are matched loosely and the fallback carries the
/// tail rather than guessing.
pub fn classify_ssh_stderr(stderr: &str) -> ProbeFailure {
    let s = stderr.to_ascii_lowercase();
    if s.contains("permission denied") {
        ProbeFailure::SshAuth
    } else if s.contains("host key verification failed") {
        ProbeFailure::SshHostKey
    } else if s.contains("address already in use") {
        ProbeFailure::SshBind
    } else if s.contains("connection refused")
        || s.contains("connection timed out")
        || s.contains("timed out")
        || s.contains("no route to host")
        || s.contains("could not resolve hostname")
        || s.contains("network is unreachable")
    {
        ProbeFailure::SshUnreachable
    } else {
        // The last few lines are where ssh puts the reason.
        let tail: Vec<&str> = stderr.lines().rev().take(3).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        ProbeFailure::SshFailed(tail.join(" | "))
    }
}

/// A running ssh tunnel. Killed on drop so no `ssh -N` outlives its session.
pub struct Tunnel {
    child: Child,
    stderr: Arc<Mutex<String>>,
}

impl Tunnel {
    pub fn spawn(spec: &TunnelSpec) -> std::io::Result<Tunnel> {
        let mut child = Command::new("ssh")
            .args(ssh_args(spec))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(pipe) = child.stderr.take() {
            let sink = Arc::clone(&stderr);
            std::thread::Builder::new()
                .name("ssh-stderr".into())
                .spawn(move || {
                    let mut pipe = pipe;
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = pipe.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        if let Ok(mut s) = sink.lock() {
                            s.push_str(&String::from_utf8_lossy(&buf[..n]));
                        }
                    }
                })?;
        }
        Ok(Tunnel { child, stderr })
    }

    /// `Some(stderr so far)` if the child has exited.
    pub fn poll_exit(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(Some(_status)) => Some(self.stderr_snapshot()),
            _ => None,
        }
    }

    pub fn stderr_snapshot(&self) -> String {
        self.stderr.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Kill and reap. Safe to call more than once.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Wait for a local forward to accept, bounded by `deadline`.
///
/// ssh binds its `-L` listeners only after the transport is up and authenticated —
/// hundreds of ms to seconds after spawn — and the bind-and-dropped port refuses
/// instantly in the gap. So **ECONNREFUSED here means "not yet", never
/// "no agent"**: the remote end's refusal shows up as an accepted-then-closed
/// connection, not a failed local connect. `child_exited` is polled each tick so
/// an ssh that died (auth, bind, unreachable) short-circuits with its
/// classification instead of burning the deadline.
pub fn await_forward_ready(
    addr: SocketAddr,
    deadline: Instant,
    mut child_exited: impl FnMut() -> Option<String>,
) -> Result<TcpStream, ProbeFailure> {
    loop {
        if let Some(stderr) = child_exited() {
            return Err(classify_ssh_stderr(&stderr));
        }
        if Instant::now() >= deadline {
            return Err(ProbeFailure::Deadline("tunnel readiness"));
        }
        match TcpStream::connect_timeout(&addr, Duration::from_millis(250)) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(_) => {
                // Refused (ssh not bound yet) or transient — retry until deadline.
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> TunnelSpec {
        TunnelSpec {
            host: "quench.lan.example".into(),
            ssh_user: Some("ano".into()),
            local: ForwardPorts {
                video: 50001,
                input: 50002,
                control: 50003,
                aux: 50004,
            },
        }
    }

    #[test]
    fn ssh_args_carry_every_safety_flag_and_explicit_loopback_binds() {
        let args = ssh_args(&spec());
        let joined = args.join(" ");
        // The safety-bearing flags, each load-bearing per HLD §4.2.
        for required in [
            "-N",
            "BatchMode=yes",
            "ConnectTimeout=4",
            "ExitOnForwardFailure=yes",
            "GatewayPorts=no",
            "StrictHostKeyChecking=accept-new",
            "ServerAliveInterval=5",
            "ServerAliveCountMax=3",
        ] {
            assert!(joined.contains(required), "missing {required} in: {joined}");
        }
        // Every forward names an explicit loopback bind address on BOTH ends.
        assert!(joined.contains("127.0.0.1:50001:127.0.0.1:9500"));
        assert!(joined.contains("127.0.0.1:50002:127.0.0.1:9501"));
        assert!(joined.contains("127.0.0.1:50003:127.0.0.1:9502"));
        // The auxiliary channel. Loopback on both ends like the rest: ssh is
        // the security boundary, and nothing here may be reachable off-host.
        assert!(joined.contains("127.0.0.1:50004:127.0.0.1:9503"));
        // Destination is last, with the user applied.
        assert_eq!(args.last().unwrap(), "ano@quench.lan.example");
    }

    #[test]
    fn ssh_dest_without_user_lets_ssh_config_decide() {
        let mut s = spec();
        s.ssh_user = None;
        assert_eq!(ssh_args(&s).last().unwrap(), "quench.lan.example");
    }

    #[test]
    fn allocated_ports_are_distinct_and_nonzero() {
        let p = allocate_ports().unwrap();
        let all = [p.video, p.input, p.control, p.aux];
        assert!(all.iter().all(|&port| port != 0));
        // Set-based rather than a hand-written chain of pairs: with four ports a
        // chain is easy to write with a pair missing, and it would still pass.
        let distinct: std::collections::HashSet<u16> = all.iter().copied().collect();
        assert_eq!(distinct.len(), all.len(), "ports collided: {all:?}");
    }

    #[test]
    fn stderr_classification_maps_the_known_patterns() {
        assert_eq!(
            classify_ssh_stderr("ano@quench: Permission denied (publickey,password)."),
            ProbeFailure::SshAuth
        );
        assert_eq!(
            classify_ssh_stderr("Host key verification failed."),
            ProbeFailure::SshHostKey
        );
        assert_eq!(
            classify_ssh_stderr("bind [127.0.0.1]:50001: Address already in use"),
            ProbeFailure::SshBind
        );
        assert_eq!(
            classify_ssh_stderr("ssh: connect to host quench port 22: Connection refused"),
            ProbeFailure::SshUnreachable
        );
        assert_eq!(
            classify_ssh_stderr(
                "ssh: Could not resolve hostname nowhere: nodename nor servname provided"
            ),
            ProbeFailure::SshUnreachable
        );
        // Fallback carries the tail rather than guessing a category.
        match classify_ssh_stderr("something unprecedented\nhappened here") {
            ProbeFailure::SshFailed(tail) => {
                assert!(tail.contains("happened here"), "tail was: {tail}")
            }
            other => panic!("expected SshFailed, got {other:?}"),
        }
    }

    #[test]
    fn readiness_returns_a_stream_once_a_listener_exists() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = await_forward_ready(addr, deadline, || None).unwrap();
        assert_eq!(stream.peer_addr().unwrap(), addr);
    }

    #[test]
    fn readiness_short_circuits_when_the_child_died() {
        // A port with no listener, and a child that reports auth failure.
        let addr: SocketAddr = (Ipv4Addr::LOCALHOST, 1).into();
        let deadline = Instant::now() + Duration::from_secs(30);
        let start = Instant::now();
        let err = await_forward_ready(addr, deadline, || {
            Some("Permission denied (publickey).".into())
        })
        .unwrap_err();
        assert_eq!(err, ProbeFailure::SshAuth);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "did not short-circuit"
        );
    }

    #[test]
    fn readiness_gives_up_at_the_deadline_when_nothing_listens() {
        let addr: SocketAddr = (Ipv4Addr::LOCALHOST, 1).into();
        let deadline = Instant::now() + Duration::from_millis(300);
        let err = await_forward_ready(addr, deadline, || None).unwrap_err();
        assert_eq!(err, ProbeFailure::Deadline("tunnel readiness"));
    }

    #[test]
    fn version_mismatch_remedy_names_the_lagging_side() {
        let old_host = ProbeFailure::VersionMismatch { host: 2, client: 3 };
        assert!(old_host.remedy("quench").contains("mdrdp deploy"));
        let old_client = ProbeFailure::VersionMismatch { host: 4, client: 3 };
        assert!(old_client.remedy("quench").contains("update mdrdp"));
    }
}
