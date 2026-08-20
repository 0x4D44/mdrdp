//! `mdrdp deploy <host>` — push the rhydra host stack over SSH, idempotently.
//!
//! The shape is preflight-first: every gate that can stop the run fires before
//! anything on the host changes, so a stopped deploy has mutated nothing (the
//! one sanctioned preflight write is uploading the probe scripts themselves —
//! inert data under `C:\mdrdp\scripts\`). Decisions are pure functions of the
//! gathered [`Evidence`]; the runner executes a flat list of [`Action`]s and
//! stops on first failure. See the tranche-2 HLD for the full design and the
//! review findings that shaped it (stable-green verify, presence-keyed quiesce,
//! versioned directories, no pnputil prose parsing).
//!
//! Remote execution discipline: anything beyond a trivial command ships as an
//! uploaded `.ps1` and runs via the absolute PowerShell path, so the host's
//! default ssh shell is irrelevant. Every script prints a `RHYDRA-OK` sentinel
//! as its last line; deploy requires the sentinel AND a zero exit.

use std::path::{Path, PathBuf};

/// The host-side home. Version directories sit under it; `logs\` beside them.
pub const REMOTE_ROOT: &str = r"C:\mdrdp";
/// Where uploaded helper scripts live on the host.
pub const REMOTE_SCRIPTS: &str = r"C:\mdrdp\scripts";
/// Every script's final line on success. Belt to the exit-code brace: exit-code
/// fidelity through Windows OpenSSH is assumption-labelled in the HLD.
pub const SENTINEL: &str = "RHYDRA-OK";

/// The only package deploy will fetch for the VB-CABLE base device. Keep these
/// values in the Rust contract as well as in `VB_CABLE_INSTALL_PS1`: changing
/// the URL without changing the expected bytes must be a review-visible diff.
pub const VB_CABLE_DOWNLOAD_URL: &str =
    "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip";
pub const VB_CABLE_ARCHIVE_SHA256: &str =
    "b950e39f01af1d04ea623c8f6d8eb9b6ea5c477c637295fabf20631c85116bfb";
pub const VB_CABLE_ARCHIVE_BYTES: u64 = 1_318_877;

const VB_CABLE_MARKER: &str = r"C:\mdrdp\vb-cable-pack45.ok";

const POWERSHELL: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";

pub struct Config {
    pub host: String,
    pub user: Option<String>,
    pub artifacts: Option<PathBuf>,
    pub dry_run: bool,
    pub force: bool,
}

pub fn parse_args(args: &[String]) -> Result<Config, String> {
    let mut host: Option<String> = None;
    let mut user = None;
    let mut artifacts = None;
    let mut dry_run = false;
    let mut force = false;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => return Err(usage().to_owned()),
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--force" => {
                force = true;
                i += 1;
            }
            "--user" => {
                user = Some(take_value(args, i)?);
                i += 2;
            }
            "--artifacts" => {
                artifacts = Some(PathBuf::from(take_value(args, i)?));
                i += 2;
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown flag {flag}\n{}", usage()));
            }
            positional => {
                if host.is_some() {
                    return Err(format!(
                        "unexpected extra argument {positional:?}\n{}",
                        usage()
                    ));
                }
                host = Some(positional.to_owned());
                i += 1;
            }
        }
    }
    let host = host.ok_or_else(|| format!("deploy needs a host\n{}", usage()))?;
    Ok(Config {
        host,
        user,
        artifacts,
        dry_run,
        force,
    })
}

fn take_value(args: &[String], i: usize) -> Result<String, String> {
    args.get(i + 1)
        .cloned()
        .ok_or_else(|| format!("{} needs a value", args[i]))
}

pub fn usage() -> &'static str {
    "usage:\n  \
     mdrdp deploy <host> [--user <ssh-user>] [--artifacts <dir>] [--dry-run] [--force]\n\n\
     Pushes the rhydra host stack (agent, server, IDD creator, driver) over SSH\n\
     and verifies the agent comes up healthy. --user is the SSH login (not an RDP\n\
     account). --dry-run prints the gathered evidence and the exact plan, then\n\
     stops. --force overrides the refusal to deploy over a live capture session.\n\
     Artifacts come from the cross builds (server/build.sh, idd/build.sh); deploy\n\
     never builds them."
}

/// The local files a deploy pushes, plus the version identity that names the
/// remote directory. Version comes from the server crate's Cargo.toml, found
/// relative to the artifacts; a custom `--artifacts` tree must carry one too.
pub struct Artifacts {
    pub version: String,
    pub server_exe: PathBuf,
    pub agent_exe: PathBuf,
    pub creator_exe: PathBuf,
    pub driver_dll: PathBuf,
    pub driver_inf: PathBuf,
    pub driver_ver: String,
}

impl Artifacts {
    /// Everything that lands in the remote version directory, with its remote
    /// basename. The driver files go under `driver\`.
    pub fn files(&self) -> Vec<(PathBuf, String)> {
        vec![
            (self.agent_exe.clone(), "rhydra-agent.exe".to_owned()),
            (self.server_exe.clone(), "rhydra-server.exe".to_owned()),
            (self.creator_exe.clone(), "mdrdp-idd-create.exe".to_owned()),
            (self.driver_dll.clone(), r"driver\mdrdp_idd.dll".to_owned()),
            (self.driver_inf.clone(), r"driver\mdrdp-idd.inf".to_owned()),
        ]
    }

    pub fn remote_dir(&self) -> String {
        format!(r"{REMOTE_ROOT}\v{}", self.version)
    }
}

/// Pull `version = "…"` out of a Cargo.toml. First match wins, which is the
/// `[package]` section in every layout this repo uses.
pub fn crate_version_from_toml(toml: &str) -> Option<String> {
    for line in toml.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("version") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let rest = rest.trim();
                if rest.len() >= 2
                    && rest.starts_with('"')
                    && let Some(end) = rest[1..].find('"')
                {
                    return Some(rest[1..1 + end].to_owned());
                }
            }
        }
    }
    None
}

/// Pull the version out of an INF's `DriverVer = mm/dd/yyyy,a.b.c.d` line.
/// This is the driver package's identity: pnputil no-ops a same-version re-add
/// (observed as exit 259 on quench), and a dll change without a DriverVer bump
/// is an authoring error deploy reports loudly rather than masks.
pub fn driver_ver_from_inf(inf: &str) -> Option<String> {
    for line in inf.lines() {
        let line = line.trim();
        if line.to_ascii_lowercase().starts_with("driverver") {
            let value = line.split('=').nth(1)?.trim();
            let version = value.split(',').nth(1)?.trim();
            if !version.is_empty() {
                return Some(version.to_owned());
            }
        }
    }
    None
}

/// Locate the cross-built artifacts. The default roots hang off the repo this
/// binary was built in — deploy is a dev tool and says so when they are absent.
pub fn locate_artifacts(explicit: Option<&Path>) -> Result<Artifacts, String> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (bin_dir, driver_dir, toml_path) = match explicit {
        Some(dir) => (
            dir.to_path_buf(),
            dir.join("driver"),
            dir.join("Cargo.toml"),
        ),
        None => (
            repo.join("tools/latency-spike/server/target/x86_64-pc-windows-msvc/release"),
            repo.join("tools/latency-spike/idd/build"),
            repo.join("tools/latency-spike/server/Cargo.toml"),
        ),
    };
    let need = |p: PathBuf| -> Result<PathBuf, String> {
        if p.is_file() {
            Ok(p)
        } else {
            Err(format!(
                "missing artifact {}\n  build them first:\n    tools/latency-spike/server/build.sh   (rhydra-server.exe, rhydra-agent.exe)\n    tools/latency-spike/idd/build.sh      (mdrdp_idd.dll, mdrdp-idd-create.exe, mdrdp-idd.inf)",
                p.display()
            ))
        }
    };
    let server_exe = need(bin_dir.join("rhydra-server.exe"))?;
    let agent_exe = need(bin_dir.join("rhydra-agent.exe"))?;
    let creator_exe = need(driver_dir.join("mdrdp-idd-create.exe"))?;
    let driver_dll = need(driver_dir.join("mdrdp_idd.dll"))?;
    let driver_inf = need(driver_dir.join("mdrdp-idd.inf"))?;
    let toml = std::fs::read_to_string(&toml_path)
        .map_err(|e| format!("cannot read {} for the version: {e}", toml_path.display()))?;
    let version = crate_version_from_toml(&toml)
        .ok_or_else(|| format!("no version = \"…\" in {}", toml_path.display()))?;
    let inf_text = std::fs::read_to_string(&driver_inf)
        .map_err(|e| format!("cannot read {}: {e}", driver_inf.display()))?;
    let driver_ver = driver_ver_from_inf(&inf_text)
        .ok_or_else(|| format!("no DriverVer in {}", driver_inf.display()))?;
    Ok(Artifacts {
        version,
        server_exe,
        agent_exe,
        creator_exe,
        driver_dll,
        driver_inf,
        driver_ver,
    })
}

/// What the probe script reports about the host, one JSON object. Field names
/// are the wire contract with `PROBE_PS1`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Evidence {
    pub elevated: bool,
    /// `Win32_ComputerSystem.UserName` — the interactively logged-on user
    /// (DOMAIN\name), absent when nobody is at the console.
    pub console_user: Option<String>,
    /// The bare SSH login this probe ran as, for the principal-match check.
    pub ssh_user: String,
    /// Every rhydra-agent.exe under C:\mdrdp (flat tranche-1 layout included).
    #[serde(default)]
    pub agent_exes: Vec<String>,
    /// Process name owning the control port, when something listens on it.
    pub port9502_owner: Option<String>,
    /// Process name owning the video port, when something listens on it.
    pub port9500_owner: Option<String>,
    pub device_present: bool,
    /// Driver version bound to the live mdrdp display device, when present.
    #[serde(default)]
    pub active_driver_ver: Option<String>,
    /// Versions of every staged mdrdp-idd driver package.
    #[serde(default)]
    pub staged_driver_vers: Vec<String>,
    pub inf2cat_present: bool,
    pub signtool_present: bool,
    /// Existing version directories under C:\mdrdp (bare names, e.g. "v0.2.0").
    #[serde(default)]
    pub version_dirs: Vec<String>,
    /// name -> size for files in the target version dir, when it exists.
    #[serde(default)]
    pub target_dir_sizes: std::collections::BTreeMap<String, u64>,
    /// The running agent's status, when the control port answered a status query:
    /// (version, green, server_running).
    pub agent: Option<AgentProbe>,
    /// The official VB-CABLE package is staged in the Windows driver store.
    #[serde(default)]
    pub audio_package_staged: bool,
    /// The official package installer has completed on this host. This marker
    /// distinguishes an endpoint that predates deploy from a failed install.
    #[serde(default)]
    pub audio_setup_ran: bool,
    /// Windows reports that a reboot is pending after audio provisioning.
    #[serde(default)]
    pub audio_reboot_pending: bool,
    /// The endpoint's stable root/provider identity, not its friendly name,
    /// matched VB-Audio's base device (`VBAudioVACWDM`).
    #[serde(default)]
    pub cable_identity_ok: bool,
    /// At least one active VB-CABLE render endpoint was found.
    #[serde(default)]
    pub cable_render_active: bool,
    /// The active VB-CABLE capture endpoint was found.
    #[serde(default)]
    pub cable_capture_active: bool,
    /// The endpoint format/loopback preflight passed. The probe is deliberately
    /// conservative: absence of this fact requests configure-audio.
    #[serde(default)]
    pub cable_formats_ok: bool,
    /// A Windows default render endpoint is active.
    #[serde(default)]
    pub audio_default_render_active: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AgentProbe {
    pub version: String,
    pub green: bool,
    pub server_running: bool,
}

/// One executable unit of the plan. Deliberately closed at these two variants:
/// decisions live in [`decide`], polling lives in verify — an `Action` never
/// branches (review finding: a `Step` language grows until it is a bad shell).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Run {
        label: String,
        command: String,
    },
    /// Run an uploaded PowerShell script through `Ssh::run_script`, retaining
    /// its sentinel-validated stdout as deploy evidence.
    Script {
        label: String,
        script: String,
        args: String,
    },
    Copy {
        local: PathBuf,
        remote: String,
    },
}

/// The decided shape of this deploy.
#[derive(Debug)]
pub enum Branch {
    /// Everything already matches: no quiesce, no copy — verify only.
    FastPath,
    /// The runtime stack is exact and healthy; repair/configure only audio.
    AudioOnly(Plan),
    Full(Plan),
}

#[derive(Debug, Default)]
pub struct Plan {
    /// Before the copies (quiesce lands here only when the copy would collide
    /// with the running version's directory).
    pub pre: Vec<Action>,
    pub copies: Vec<Action>,
    /// After the copies: quiesce (fresh-version case), driver install, agent
    /// install. Copy-first keeps the old agent alive through a failed copy.
    pub post: Vec<Action>,
    pub notes: Vec<String>,
}

fn quiesce_actions(evidence: &Evidence) -> Vec<Action> {
    // Presence-keyed, never gated on the control port answering (review
    // finding): the existing exe's own uninstall handles both the clean arm and
    // the wedged arm, and tolerates an unregistered task.
    evidence
        .agent_exes
        .first()
        .map(|exe| {
            vec![Action::Run {
                label: "quiesce existing agent".to_owned(),
                command: format!("\"{exe}\" uninstall"),
            }]
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioState {
    Healthy,
    NeedsInstall,
    NeedsConfigure,
    RebootRequired,
    Broken,
}

impl Evidence {
    fn audio_state(&self) -> AudioState {
        if self.audio_reboot_pending {
            return AudioState::RebootRequired;
        }
        if !self.audio_package_staged {
            return AudioState::NeedsInstall;
        }
        if !self.cable_render_active || !self.cable_capture_active || !self.cable_identity_ok {
            return if self.audio_setup_ran {
                AudioState::Broken
            } else {
                AudioState::NeedsInstall
            };
        }
        if !self.cable_formats_ok || !self.audio_default_render_active {
            return AudioState::NeedsConfigure;
        }
        AudioState::Healthy
    }
}

fn vb_cable_install_action() -> Action {
    Action::Script {
        label: "install VB-CABLE Pack45".to_owned(),
        script: "vbcable-install.ps1".to_owned(),
        args: format!("-MarkerPath \"{VB_CABLE_MARKER}\""),
    }
}

fn configure_audio_action(remote_dir: &str) -> Action {
    Action::Run {
        label: "configure audio".to_owned(),
        command: format!("\"{remote_dir}\\rhydra-agent.exe\" configure-audio"),
    }
}

/// The pure heart: evidence in, plan or stop out. Every stop happens here,
/// before the runner mutates anything.
pub fn decide(cfg: &Config, artifacts: &Artifacts, evidence: &Evidence) -> Result<Branch, String> {
    if !evidence.elevated {
        return Err(
            "the SSH session is not elevated; deploy needs an administrators-group login \
                    with a full token (Windows OpenSSH grants one to admin users by default)"
                .to_owned(),
        );
    }
    let console_user = evidence.console_user.as_deref().ok_or(
        "no interactive console session: the agent's onlogon task cannot run without one. \
         Log the host on at the console (or via auto-logon) and re-run",
    )?;
    let console_bare = console_user
        .rsplit('\\')
        .next()
        .unwrap_or(console_user)
        .to_ascii_lowercase();
    if console_bare != evidence.ssh_user.to_ascii_lowercase() {
        return Err(format!(
            "console user {console_user:?} is not the SSH user {:?}: the onlogon task binds to \
             the installing principal and would never fire. Deploy as the console user",
            evidence.ssh_user
        ));
    }
    // A foreign holder of our ports is a stop-and-say, never a silent kill: the
    // rig era proved a stray server here is usually someone's measurement.
    for (port, owner) in [
        (9502u16, &evidence.port9502_owner),
        (9500u16, &evidence.port9500_owner),
    ] {
        if let Some(name) = owner {
            let ours = name.eq_ignore_ascii_case("rhydra-agent")
                || name.eq_ignore_ascii_case("rhydra-server");
            if !ours {
                return Err(format!(
                    "port {port} is held by {name:?}, which is not part of a rhydra install; \
                     stop it (or finish its work) and re-run"
                ));
            }
        }
    }
    let audio_state = evidence.audio_state();
    match audio_state {
        AudioState::RebootRequired => {
            return Err(
                "VB-CABLE provisioning requires a reboot; reboot the host and re-run deploy"
                    .to_owned(),
            );
        }
        AudioState::Broken => {
            return Err(
                "VB-CABLE setup completed but no stable active endpoints were found; host is \
                 broken and deploy will not retry in a loop"
                    .to_owned(),
            );
        }
        AudioState::Healthy | AudioState::NeedsInstall | AudioState::NeedsConfigure => {}
    }

    // Fast path: the exact version is already deployed, byte-sizes match, and
    // the running agent reports that version healthy. (Same-version different
    // bytes falls through to a full deploy: sizes are the tell.) Checked BEFORE
    // the live-server guard: a fast path mutates nothing, so the normal state
    // after a successful deploy — server up — must not force-gate the no-op
    // re-run the idempotence criterion demands.
    let vdir = format!("v{}", artifacts.version);
    let sizes_match = artifacts.files().iter().all(|(local, remote)| {
        let name = remote.rsplit('\\').next().unwrap_or(remote);
        match (
            std::fs::metadata(local).map(|m| m.len()),
            evidence.target_dir_sizes.get(name),
        ) {
            (Ok(local_len), Some(remote_len)) => local_len == *remote_len,
            _ => false,
        }
    });
    let exact_stack = evidence.version_dirs.iter().any(|d| d == &vdir)
        && sizes_match
        && evidence
            .agent
            .as_ref()
            .is_some_and(|a| a.green && a.version == artifacts.version);
    let remote_dir = artifacts.remote_dir();
    if exact_stack {
        match audio_state {
            AudioState::Healthy => return Ok(Branch::FastPath),
            AudioState::NeedsInstall | AudioState::NeedsConfigure => {
                let mut plan = Plan::default();
                if audio_state == AudioState::NeedsInstall {
                    plan.pre.push(vb_cable_install_action());
                }
                plan.post.push(configure_audio_action(&remote_dir));
                plan.notes.push(
                    "exact runtime stack: repairing audio only; no agent quiesce, copy, or driver \
                     replacement"
                        .to_owned(),
                );
                return Ok(Branch::AudioOnly(plan));
            }
            AudioState::RebootRequired | AudioState::Broken => unreachable!(),
        }
    }

    if let Some(agent) = &evidence.agent
        && agent.server_running
        && !cfg.force
    {
        return Err(
            "a capture server is live: deploying now drops the virtual display and \
             any native session. Re-run with --force to proceed"
                .to_owned(),
        );
    }

    // A live device is current only when it is actually bound to this package.
    // When no device exists yet, a staged matching package is sufficient: the
    // creator will bind it after the agent starts.
    let desired_staged = evidence
        .staged_driver_vers
        .iter()
        .any(|v| v == &artifacts.driver_ver);
    let driver_current = if evidence.device_present {
        evidence.active_driver_ver.as_deref() == Some(artifacts.driver_ver.as_str())
    } else {
        desired_staged
    };
    if !(driver_current || (evidence.inf2cat_present && evidence.signtool_present)) {
        return Err(format!(
            "the driver ({}) is not installed and the host lacks Inf2Cat/signtool to install \
             it. Unzip the Microsoft.Windows.WDK.x64 NuGet on the host (idd/README.md) and \
             re-run",
            artifacts.driver_ver
        ));
    }

    let mut plan = Plan::default();

    // The VB-CABLE package is independently verified before any action that can
    // quiesce the old agent or replace its artifacts. Script actions retain the
    // sentinel-validated output for the deploy evidence report.
    if audio_state == AudioState::NeedsInstall {
        plan.pre.push(vb_cable_install_action());
    }

    // Copy-collision: replacing the same version dir the running agent lives in
    // means quiescing first; a fresh version dir keeps copy-first atomicity.
    let target_exists = evidence.version_dirs.iter().any(|d| d == &vdir);
    let quiesce = quiesce_actions(evidence);
    let quiesce_first = target_exists && !quiesce.is_empty();
    if quiesce_first {
        plan.notes.push(format!(
            "same-version redeploy over {vdir}: quiescing before the copy (the running agent \
             holds those images)"
        ));
        plan.pre.extend(quiesce.clone());
    }

    plan.pre.push(Action::Run {
        label: "create version directory".to_owned(),
        command: format!(
            "cmd /c if not exist \"{remote_dir}\\driver\" mkdir \"{remote_dir}\\driver\""
        ),
    });
    for (local, remote) in artifacts.files() {
        plan.copies.push(Action::Copy {
            local,
            remote: format!("{remote_dir}\\{remote}"),
        });
    }

    if !quiesce_first {
        plan.post.extend(quiesce);
    }
    if driver_current {
        plan.notes
            .push(format!("driver current ({})", artifacts.driver_ver));
    } else {
        plan.post.push(Action::Run {
            label: "install driver".to_owned(),
            command: format!(
                "{POWERSHELL} -NoProfile -ExecutionPolicy Bypass -File {REMOTE_SCRIPTS}\\driver-install.ps1 -DriverDir \"{remote_dir}\\driver\""
            ),
        });
    }
    plan.post.push(Action::Run {
        label: "install agent".to_owned(),
        command: format!("\"{remote_dir}\\rhydra-agent.exe\" install"),
    });
    // A fresh version directory never inherits the one-word source selector.
    // Re-run the verified configure operation after every full deploy so the
    // new agent starts loopback; the exact-stack fast path remains a true no-op.
    plan.post.push(configure_audio_action(&remote_dir));
    Ok(Branch::Full(plan))
}

// ---------------------------------------------------------------------------
// Remote scripts. Uploaded to REMOTE_SCRIPTS in preflight, run by -File with
// the absolute interpreter path. Contract: last stdout line is RHYDRA-OK.
// ---------------------------------------------------------------------------

/// Gathers [`Evidence`] as one JSON line, then the sentinel.
pub const PROBE_PS1: &str = r#"
param([string]$TargetDir = '', [int]$AgentPort = 9502)
$ErrorActionPreference = 'Stop'
$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($id)
$elevated = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$consoleUser = (Get-CimInstance Win32_ComputerSystem).UserName
$sshUser = $env:USERNAME
$agentExes = @()
if (Test-Path 'C:\mdrdp') {
    $agentExes = @(Get-ChildItem -Path 'C:\mdrdp' -Recurse -Filter 'rhydra-agent.exe' -ErrorAction SilentlyContinue | ForEach-Object FullName)
}
function PortOwner([int]$port) {
    $c = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($c) { (Get-Process -Id $c.OwningProcess -ErrorAction SilentlyContinue).ProcessName } else { $null }
}
$device = $null -ne (Get-PnpDevice -Class Display -ErrorAction SilentlyContinue |
    Where-Object { $_.FriendlyName -eq 'mdrdp latency-spike display' -and $_.Status -eq 'OK' })
$activeDriver = Get-CimInstance Win32_PnPSignedDriver -ErrorAction SilentlyContinue |
    Where-Object { $_.DeviceName -eq 'mdrdp latency-spike display' } |
    Select-Object -First 1 -ExpandProperty DriverVersion
$staged = @(Get-WindowsDriver -Online -ErrorAction SilentlyContinue |
    Where-Object { $_.OriginalFileName -like '*mdrdp-idd.inf' } | ForEach-Object { "$($_.Version)" })
$inf2cat = $null -ne (Get-Command Inf2Cat.exe -ErrorAction SilentlyContinue)
if (-not $inf2cat) { $inf2cat = Test-Path 'C:\mdrdp\wdk\bin\10.0.26100.0\x86\Inf2Cat.exe' }
$signtool = $null -ne (Get-Command signtool.exe -ErrorAction SilentlyContinue)
if (-not $signtool) { $signtool = Test-Path 'C:\mdrdp\wdk\bin\10.0.26100.0\x64\signtool.exe' }
$versionDirs = @()
if (Test-Path 'C:\mdrdp') {
    $versionDirs = @(Get-ChildItem -Path 'C:\mdrdp' -Directory -Filter 'v*' -ErrorAction SilentlyContinue | ForEach-Object Name)
}
$sizes = @{}
if ($TargetDir -and (Test-Path $TargetDir)) {
    Get-ChildItem -Path $TargetDir -Recurse -File | ForEach-Object { $sizes[$_.Name] = $_.Length }
}
$agent = $null
try {
    $client = New-Object Net.Sockets.TcpClient('127.0.0.1', $AgentPort)
    $client.ReceiveTimeout = 5000
    $stream = $client.GetStream()
    $writer = New-Object IO.StreamWriter($stream)
    $writer.WriteLine('{"cmd":"status"}')
    $writer.Flush()
    $reader = New-Object IO.StreamReader($stream)
    $reply = $reader.ReadLine() | ConvertFrom-Json
    if ($reply.ok) {
        $s = $reply.status
        $green = $s.device_present -and $s.mode_ok -and $s.server.running -and ($null -eq $s.stuck)
        $agent = @{ version = "$($s.version)"; green = [bool]$green; server_running = [bool]$s.server.running }
    }
    $client.Close()
} catch { $agent = $null }
$audioSetupRan = Test-Path 'C:\mdrdp\vb-cable-pack45.ok'
$audioRebootPending = (Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending') -or
    (Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired')
try {
    $rename = Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager' -Name PendingFileRenameOperations -ErrorAction Stop
    if ($rename.PendingFileRenameOperations) { $audioRebootPending = $true }
} catch {}
function Device-Properties([string]$InstanceId) {
    $props = @{}
    Get-PnpDeviceProperty -InstanceId $InstanceId -ErrorAction SilentlyContinue |
        ForEach-Object { $props[$_.KeyName] = "$($_.Data)" }
    $props
}
function Cable-Endpoint-Identity($endpoint) {
    if ($endpoint.Status -ne 'OK') { return $null }
    $endpointProps = Device-Properties $endpoint.InstanceId
    $parentId = "$($endpointProps['DEVPKEY_Device_Parent'])"
    if (-not $parentId) { return $null }
    $root = Get-PnpDevice -InstanceId $parentId -ErrorAction SilentlyContinue
    if (-not $root -or $root.Status -ne 'OK') { return $null }
    $rootProps = Device-Properties $parentId
    $hardwareIds = "$($rootProps['DEVPKEY_Device_HardwareIds'])"
    $service = "$($rootProps['DEVPKEY_Device_Service'])"
    $manufacturer = "$($rootProps['DEVPKEY_Device_Manufacturer'])"
    $provider = "$($rootProps['DEVPKEY_Device_DriverProvider'])"
    if ($hardwareIds -notmatch 'VBAudioVACWDM' -or $service -notmatch 'VBAudioVACMME') { return $null }
    if ($manufacturer -notmatch 'VB-Audio Software' -or $provider -notmatch 'VB-Audio Software') { return $null }
    $flow = if ($endpoint.InstanceId -match '\{0\.0\.0\.') { 'render' } elseif ($endpoint.InstanceId -match '\{0\.0\.1\.') { 'capture' } else { 'other' }
    if ($flow -eq 'other') { return $null }
    [pscustomobject]@{
        name = "$($endpoint.FriendlyName)"
        instance = "$($endpoint.InstanceId)"
        parent = $parentId
        flow = $flow
        root_hardware_ids = $hardwareIds
        root_service = $service
        root_manufacturer = $manufacturer
        root_provider = $provider
    }
}
$audioEndpoints = @(Get-PnpDevice -Class AudioEndpoint -PresentOnly -ErrorAction SilentlyContinue |
    Where-Object { $_.Status -eq 'OK' })
$cableRenderActive = $false
$cableCaptureActive = $false
$cableIdentityOk = $false
$cableNames = @()
$cableEndpointFacts = @()
foreach ($endpoint in $audioEndpoints) {
    $identity = Cable-Endpoint-Identity $endpoint
    if ($identity) {
        $cableIdentityOk = $true
        $cableNames += $identity.name
        $cableEndpointFacts += $identity
        if ($identity.flow -eq 'render') {
            $cableRenderActive = $true
        }
        if ($identity.flow -eq 'capture') {
            $cableCaptureActive = $true
        }
    }
}
# A machine-wide pending rename is not evidence that this audio install needs a
# reboot (Edge routinely leaves one behind). Attribute it to VB-CABLE only when
# setup ran and its verified endpoints are still unavailable.
$audioRebootPending = [bool]($audioSetupRan -and $audioRebootPending -and (-not $cableRenderActive -or -not $cableCaptureActive))
$audioPackageStaged = $audioSetupRan
try {
    $audioPackageStaged = $audioPackageStaged -or ($null -ne (Get-WindowsDriver -Online -ErrorAction Stop |
        Where-Object { $_.OriginalFileName -match 'vbMmeCable64|VBAudio' } | Select-Object -First 1))
} catch {}
$audioDefaultRenderActive = [bool]($null -ne ($s.rungs |
    Where-Object { $_.rung -eq 'audio' -and $_.state -eq 'ok' } |
    Select-Object -First 1))
# Endpoint activity and stable identity are the deploy-time facts. The native
# audio probe records format/loopback viability after configure-audio; retaining
# this fact explicitly prevents an active but unusable endpoint from being called
# healthy by the planner.
$audioFormatMarker = Test-Path 'C:\mdrdp\vb-cable-format.ok'
$audioSourceLoopback = $false
$audioPolicyOk = $false
if ($TargetDir -and (Test-Path -LiteralPath (Join-Path $TargetDir 'audio-source'))) {
    $audioSourceLoopback = ((Get-Content -LiteralPath (Join-Path $TargetDir 'audio-source') -Raw).Trim() -eq 'loopback')
    $audioAgent = Join-Path $TargetDir 'rhydra-agent.exe'
    if (Test-Path -LiteralPath $audioAgent) {
        $audioCheck = Start-Process -FilePath $audioAgent -ArgumentList 'check-audio' -Wait -PassThru -WindowStyle Hidden
        $audioPolicyOk = ($audioCheck.ExitCode -eq 0)
    }
}
$audioFormatsOk = [bool]($audioFormatMarker -and $audioPolicyOk -and $audioSourceLoopback -and $cableRenderActive -and $cableCaptureActive)
$out = @{
    elevated = [bool]$elevated
    console_user = $consoleUser
    ssh_user = $sshUser
    agent_exes = $agentExes
    port9502_owner = PortOwner 9502
    port9500_owner = PortOwner 9500
    device_present = [bool]$device
    active_driver_ver = if ($activeDriver) { "$activeDriver" } else { $null }
    staged_driver_vers = $staged
    inf2cat_present = [bool]$inf2cat
    signtool_present = [bool]$signtool
    version_dirs = $versionDirs
    target_dir_sizes = $sizes
    agent = $agent
    audio_package_staged = [bool]$audioPackageStaged
    audio_setup_ran = [bool]$audioSetupRan
    audio_reboot_pending = [bool]$audioRebootPending
    cable_identity_ok = [bool]$cableIdentityOk
    cable_render_active = [bool]$cableRenderActive
    cable_capture_active = [bool]$cableCaptureActive
    cable_formats_ok = [bool]$audioFormatsOk
    audio_default_render_active = [bool]$audioDefaultRenderActive
    cable_endpoint_names = $cableNames
    cable_endpoint_facts = $cableEndpointFacts
}
Write-Output ($out | ConvertTo-Json -Compress -Depth 4)
Write-Output 'RHYDRA-OK'
"#;

/// Downloads, verifies, and installs the official VB-CABLE Pack45 package.
/// This script is intentionally self-contained: it runs before quiesce or
/// artifact replacement and emits the signature and endpoint facts that made
/// the install safe to audit after the fact.
pub const VB_CABLE_INSTALL_PS1: &str = r#"
param([string]$MarkerPath = 'C:\mdrdp\vb-cable-pack45.ok')
$ErrorActionPreference = 'Stop'
$url = 'https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip'
$expectedHash = 'b950e39f01af1d04ea623c8f6d8eb9b6ea5c477c637295fabf20631c85116bfb'
$expectedBytes = [uint64]1318877
$stageRoot = Join-Path $env:TEMP ('mdrdp-vbcable-' + [Guid]::NewGuid().ToString('N'))
$archive = Join-Path $stageRoot 'VBCABLE_Driver_Pack45.zip'
$signatureFacts = @()

function Require-Signature([string]$Path, [string]$SubjectPrefix, [string]$IssuerContains) {
    $sig = Get-AuthenticodeSignature -LiteralPath $Path
    if ($sig.Status -ne 'Valid' -or $null -eq $sig.SignerCertificate) {
        throw "invalid Authenticode signature for $Path ($($sig.Status))"
    }
    $subject = "$($sig.SignerCertificate.Subject)"
    $issuer = "$($sig.SignerCertificate.Issuer)"
    if (-not $subject.StartsWith($SubjectPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "unexpected signer for $Path ($subject)"
    }
    if ($IssuerContains -and $issuer -notlike "*$IssuerContains*") {
        throw "unexpected issuer for $Path ($issuer)"
    }
    [pscustomobject]@{ path = $Path; status = "$($sig.Status)"; subject = $subject; issuer = $issuer }
}

function Device-Properties([string]$InstanceId) {
    $props = @{}
    Get-PnpDeviceProperty -InstanceId $InstanceId -ErrorAction SilentlyContinue |
        ForEach-Object { $props[$_.KeyName] = "$($_.Data)" }
    $props
}

function Cable-Endpoint-Identity($endpoint) {
    if ($endpoint.Status -ne 'OK') { return $null }
    $endpointProps = Device-Properties $endpoint.InstanceId
    $parentId = "$($endpointProps['DEVPKEY_Device_Parent'])"
    if (-not $parentId) { return $null }
    $root = Get-PnpDevice -InstanceId $parentId -ErrorAction SilentlyContinue
    if (-not $root -or $root.Status -ne 'OK') { return $null }
    $rootProps = Device-Properties $parentId
    $hardwareIds = "$($rootProps['DEVPKEY_Device_HardwareIds'])"
    $service = "$($rootProps['DEVPKEY_Device_Service'])"
    $manufacturer = "$($rootProps['DEVPKEY_Device_Manufacturer'])"
    $provider = "$($rootProps['DEVPKEY_Device_DriverProvider'])"
    if ($hardwareIds -notmatch 'VBAudioVACWDM' -or $service -notmatch 'VBAudioVACMME') { return $null }
    if ($manufacturer -notmatch 'VB-Audio Software' -or $provider -notmatch 'VB-Audio Software') { return $null }
    $flow = if ($endpoint.InstanceId -match '\{0\.0\.0\.') { 'render' } elseif ($endpoint.InstanceId -match '\{0\.0\.1\.') { 'capture' } else { 'other' }
    if ($flow -eq 'other') { return $null }
    [pscustomobject]@{
        name = "$($endpoint.FriendlyName)"
        instance = "$($endpoint.InstanceId)"
        parent = $parentId
        flow = $flow
        root_hardware_ids = $hardwareIds
        root_service = $service
        root_manufacturer = $manufacturer
        root_provider = $provider
    }
}

function Cable-Endpoints {
    $found = @()
    $devices = @(Get-PnpDevice -Class AudioEndpoint -PresentOnly -ErrorAction SilentlyContinue |
        Where-Object { $_.Status -eq 'OK' })
    foreach ($device in $devices) {
        $identity = Cable-Endpoint-Identity $device
        if ($identity) { $found += $identity }
    }
    $found
}

function Reboot-Pending {
    $pending = (Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending') -or
        (Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired')
    try {
        $rename = Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager' -Name PendingFileRenameOperations -ErrorAction Stop
        if ($rename.PendingFileRenameOperations) { $pending = $true }
    } catch {}
    [bool]$pending
}

try {
    New-Item -ItemType Directory -Force -Path $stageRoot | Out-Null
    Invoke-WebRequest -Uri $url -OutFile $archive -UseBasicParsing
    $actualBytes = (Get-Item -LiteralPath $archive).Length
    if ([uint64]$actualBytes -ne $expectedBytes) {
        throw "VB-CABLE archive size mismatch: $actualBytes, expected $expectedBytes"
    }
    $actualHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $expectedHash) {
        throw "VB-CABLE archive hash mismatch: $actualHash, expected $expectedHash"
    }
    $extractRoot = Join-Path $stageRoot 'extract'
    Expand-Archive -LiteralPath $archive -DestinationPath $extractRoot -Force
    $setup = Get-ChildItem -LiteralPath $extractRoot -Recurse -File -Filter 'VBCABLE_Setup_x64.exe' |
        Select-Object -First 1
    $inf = Get-ChildItem -LiteralPath $extractRoot -Recurse -File -Filter 'vbMmeCable64_win10.inf' |
        Select-Object -First 1
    $payloadDir = if ($inf) { $inf.DirectoryName } else { $null }
    $catalog = if ($payloadDir) {
        Get-ChildItem -LiteralPath $payloadDir -File -Filter '*.cat' | Select-Object -First 1
    }
    $sys = if ($payloadDir) {
        Get-ChildItem -LiteralPath $payloadDir -File -Filter '*.sys' | Select-Object -First 1
    }
    if (-not $setup -or -not $inf -or -not $catalog -or -not $sys) {
        throw 'VB-CABLE archive is missing its x64 setup, INF, catalog, or SYS payload'
    }
    $infText = Get-Content -LiteralPath $inf.FullName -Raw
    $vbAudioProvider = $infText -match '(?im)^\s*Provider\s*=\s*%VBAudio%\s*$' -and
        $infText -match '(?im)^\s*VBAudio\s*=\s*"VB-Audio Software"\s*$'
    $manufacturerProvider = $infText -match '(?im)^\s*Provider\s*=\s*%ManufacturerName%\s*$' -and
        $infText -match '(?im)^\s*ManufacturerName\s*=\s*"VB-Audio Software"\s*$'
    if (-not $vbAudioProvider -and -not $manufacturerProvider) {
        throw 'VB-CABLE INF provider identity check failed (expected VBAudio or ManufacturerName = VB-Audio Software)'
    }
    if ($infText -notmatch 'VBAudioVACWDM') { throw 'VB-CABLE INF hardware identity check failed' }
    $providerIdentity = if ($vbAudioProvider) { 'Provider=%VBAudio%; VBAudio="VB-Audio Software"' } else { 'Provider=%ManufacturerName%; ManufacturerName="VB-Audio Software"' }
    $signatureFacts += Require-Signature $setup.FullName 'CN=BUREL VINCENT Entrepreneur individuel' ''
    $signatureFacts += Require-Signature $sys.FullName 'CN=BUREL VINCENT Entrepreneur individuel' ''
    $signatureFacts += Require-Signature $catalog.FullName 'CN=Microsoft Windows Hardware Compatibility Publisher' 'Microsoft Windows Third Party Component CA 2014'

    # Official command: VBCABLE_Setup_x64.exe -i -h
    $process = Start-Process -FilePath $setup.FullName -ArgumentList @('-i', '-h') -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "VBCABLE_Setup_x64.exe -i -h failed ($($process.ExitCode))" }
    $markerDir = Split-Path -Parent $MarkerPath
    if ($markerDir) { New-Item -ItemType Directory -Force -Path $markerDir | Out-Null }
    # Persist setup completion before the endpoint postcheck. A failed postcheck
    # must not be mistaken for a package that was never installed.
    Set-Content -LiteralPath $MarkerPath -Value 'VB-CABLE Pack45 setup completed; endpoint postcheck pending' -Encoding ASCII
    # One device rescan is enough to surface a package that installed without a
    # reboot. Repeating the scan would hide a failed install behind a loop.
    & pnputil.exe /scan-devices | Out-Null
    $scanExit = $LASTEXITCODE
    $deadline = (Get-Date).AddSeconds(30)
    $endpoints = @()
    do {
        $endpoints = @(Cable-Endpoints)
        $hasRender = $endpoints | Where-Object flow -eq 'render'
        $hasCapture = $endpoints | Where-Object flow -eq 'capture'
        if ($hasRender -and $hasCapture) { break }
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $deadline)
    if (-not $hasRender -or -not $hasCapture) {
        if (Reboot-Pending) {
            throw 'VB-CABLE install requires a reboot before active endpoints appear'
        }
        throw "VB-CABLE install produced no active render/capture endpoints (pnputil exit $scanExit)"
    }
    Set-Content -LiteralPath $MarkerPath -Value 'VB-CABLE Pack45 setup completed; endpoint postcheck passed' -Encoding ASCII
    $evidence = [pscustomobject]@{
        archive_url = $url
        archive_bytes = $actualBytes
        archive_sha256 = $actualHash
        package = 'VBCABLE_Driver_Pack45.zip'
        setup = 'VBCABLE_Setup_x64.exe'
        signatures = $signatureFacts
        inf_identity = @($providerIdentity, 'VBAudioVACWDM')
        inf_authenticode = 'UnknownError (INF is catalog-signed; no INF Authenticode signature is required)'
        endpoints = $endpoints
        installer_exit = $process.ExitCode
        rescan_exit = $scanExit
        donationware = 'VB-CABLE by VB-Audio (www.vb-cable.com) is donationware; all participations are welcome.'
    }
    Write-Output ($evidence | ConvertTo-Json -Compress -Depth 6)
    Write-Output $evidence.donationware
} finally {
    if (Test-Path -LiteralPath $stageRoot) {
        Remove-Item -LiteralPath $stageRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
Write-Output 'RHYDRA-OK'
"#;

/// Prints name->size JSON for a directory, then the sentinel. Used after the
/// copies to verify what actually landed (a truncated scp otherwise surfaces
/// as an opaque verify failure much later).
pub const SIZES_PS1: &str = r#"
param([Parameter(Mandatory=$true)][string]$Dir)
$ErrorActionPreference = 'Stop'
$sizes = @{}
Get-ChildItem -Path $Dir -Recurse -File | ForEach-Object { $sizes[$_.Name] = $_.Length }
Write-Output ($sizes | ConvertTo-Json -Compress)
Write-Output 'RHYDRA-OK'
"#;

/// The productised `idd/deploy.ps1 -Phase install`, minus the testsigning gate:
/// mdrdp-idd is UMDF and loads on the trusted cert chain alone (proven live on
/// quench 2026-08-18 with testsigning off). Cert is created once and reused.
pub const DRIVER_INSTALL_PS1: &str = r#"
param([Parameter(Mandatory=$true)][string]$DriverDir)
$ErrorActionPreference = 'Stop'
$certSubject = 'CN=mdrdp latency spike'
$certFile = Join-Path $DriverDir 'mdrdp-idd.cer'
foreach ($f in 'mdrdp_idd.dll', 'mdrdp-idd.inf') {
    if (-not (Test-Path (Join-Path $DriverDir $f))) { throw "missing $f in $DriverDir" }
}
function Find-Tool([string]$name, [string]$fallback) {
    $c = Get-Command $name -ErrorAction SilentlyContinue
    if ($c) { return $c.Source }
    if (Test-Path $fallback) { return $fallback }
    throw "$name not found (looked at $fallback) - unzip the WDK NuGet per idd/README.md"
}
$inf2cat = Find-Tool 'Inf2Cat.exe' 'C:\mdrdp\wdk\bin\10.0.26100.0\x86\Inf2Cat.exe'
$signtool = Find-Tool 'signtool.exe' 'C:\mdrdp\wdk\bin\10.0.26100.0\x64\signtool.exe'
$cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
    Where-Object Subject -eq $certSubject | Select-Object -First 1
if (-not $cert) {
    $cert = New-SelfSignedCertificate -Type CodeSigningCert `
        -Subject $certSubject -CertStoreLocation Cert:\CurrentUser\My
}
Write-Output "cert: $($cert.Thumbprint) NotAfter $($cert.NotAfter)"
Export-Certificate -Cert $cert -FilePath $certFile | Out-Null
& $inf2cat "/driver:$DriverDir" '/os:10_NI_X64'
if ($LASTEXITCODE -ne 0) { throw 'Inf2Cat failed' }
& $signtool sign /fd SHA256 /sha1 $cert.Thumbprint (Join-Path $DriverDir 'mdrdp-idd.cat')
if ($LASTEXITCODE -ne 0) { throw 'signtool failed' }
certutil -addstore root $certFile | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'certutil (root) failed' }
certutil -addstore trustedpublisher $certFile | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'certutil (trustedpublisher) failed' }
pnputil /add-driver (Join-Path $DriverDir 'mdrdp-idd.inf') /install
if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 259) { throw "pnputil failed ($LASTEXITCODE)" }
Write-Output 'RHYDRA-OK'
"#;

// ---------------------------------------------------------------------------
// The runner: thin, sequential, stops on first failure.
// ---------------------------------------------------------------------------

struct Ssh {
    dest: String,
}

impl Ssh {
    fn new(cfg: &Config) -> Self {
        let dest = match &cfg.user {
            Some(user) => format!("{user}@{}", cfg.host),
            None => cfg.host.clone(),
        };
        Ssh { dest }
    }

    /// Run one remote command; return stdout. Non-zero exit is an error carrying
    /// both streams.
    fn run(&self, command: &str) -> Result<String, String> {
        let output = std::process::Command::new("ssh")
            .arg(&self.dest)
            .arg(command)
            .output()
            .map_err(|e| format!("ssh: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "remote command failed ({}): {command}\n{stdout}{stderr}",
                output.status
            ));
        }
        Ok(stdout)
    }

    /// Run an uploaded script through the absolute PowerShell path and require
    /// the sentinel; returns stdout with the sentinel line removed.
    fn run_script(&self, script: &str, args: &str) -> Result<String, String> {
        let command = format!(
            "{POWERSHELL} -NoProfile -ExecutionPolicy Bypass -File {REMOTE_SCRIPTS}\\{script} {args}"
        );
        let stdout = self.run(&command)?;
        let trimmed = stdout.trim_end();
        match trimmed.strip_suffix(SENTINEL) {
            Some(rest) => Ok(rest.trim_end().to_owned()),
            None => Err(format!(
                "{script} exited 0 without the {SENTINEL} sentinel — output:\n{stdout}"
            )),
        }
    }

    /// Whether key (BatchMode) auth works. The native connect path requires it —
    /// a GUI session owns no tty for ssh password prompts — so deploy's verify
    /// reports the posture. A one-shot `exit` over a bounded connect: any prompt
    /// makes BatchMode fail immediately instead of hanging.
    fn batchmode_ok(&self) -> bool {
        std::process::Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ConnectTimeout=4")
            .arg(&self.dest)
            .arg("exit")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn copy(&self, local: &Path, remote: &str) -> Result<(), String> {
        // scp needs forward-slash-tolerant quoting of the remote path; Windows
        // OpenSSH accepts backslashes verbatim inside the remote spec.
        let output = std::process::Command::new("scp")
            .arg("-q")
            .arg(local)
            .arg(format!("{}:{}", self.dest, remote))
            .output()
            .map_err(|e| format!("scp: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "scp {} -> {remote} failed: {}",
                local.display(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(())
    }
}

fn upload_scripts(ssh: &Ssh) -> Result<(), String> {
    ssh.run(&format!(
        "cmd /c if not exist \"{REMOTE_SCRIPTS}\" mkdir \"{REMOTE_SCRIPTS}\""
    ))?;
    let dir = std::env::temp_dir().join("mdrdp-deploy-scripts");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (name, body) in [
        ("probe.ps1", PROBE_PS1),
        ("sizes.ps1", SIZES_PS1),
        ("driver-install.ps1", DRIVER_INSTALL_PS1),
        ("vbcable-install.ps1", VB_CABLE_INSTALL_PS1),
    ] {
        let local = dir.join(name);
        std::fs::write(&local, body).map_err(|e| e.to_string())?;
        ssh.copy(&local, &format!("{REMOTE_SCRIPTS}\\{name}"))?;
    }
    Ok(())
}

fn parse_evidence(json: &str) -> Result<Evidence, String> {
    serde_json::from_str(json.trim())
        .map_err(|e| format!("probe output did not parse: {e}\n{json}"))
}

#[derive(Debug, Clone)]
struct ScriptEvidence {
    label: String,
    stdout: String,
}

fn execute(
    ssh: &Ssh,
    actions: &[Action],
    evidence: &mut Vec<ScriptEvidence>,
) -> Result<(), String> {
    for action in actions {
        match action {
            Action::Run { label, command } => {
                eprintln!("  run   {label}");
                ssh.run(command).map_err(|e| format!("{label}: {e}"))?;
            }
            Action::Script {
                label,
                script,
                args,
            } => {
                eprintln!("  script [{label}]");
                let stdout = ssh
                    .run_script(script, args)
                    .map_err(|e| format!("{label}: {e}"))?;
                evidence.push(ScriptEvidence {
                    label: label.clone(),
                    stdout,
                });
            }
            Action::Copy { local, remote } => {
                eprintln!("  copy  {} -> {remote}", local.display());
                ssh.copy(local, remote)?;
            }
        }
    }
    Ok(())
}

fn report_script_evidence(evidence: &[ScriptEvidence]) {
    for item in evidence {
        if !item.stdout.is_empty() {
            eprintln!("deploy: evidence [{}]:\n{}", item.label, item.stdout);
        }
    }
}

fn check_sizes(ssh: &Ssh, artifacts: &Artifacts) -> Result<(), String> {
    let out = ssh.run_script("sizes.ps1", &format!("-Dir \"{}\"", artifacts.remote_dir()))?;
    let sizes: std::collections::BTreeMap<String, u64> =
        serde_json::from_str(out.trim()).map_err(|e| format!("sizes.ps1 output: {e}"))?;
    for (local, remote) in artifacts.files() {
        let name = remote.rsplit('\\').next().unwrap_or(&remote).to_owned();
        let local_len = std::fs::metadata(&local).map_err(|e| e.to_string())?.len();
        match sizes.get(&name) {
            Some(remote_len) if *remote_len == local_len => {}
            other => {
                return Err(format!(
                    "size mismatch after copy for {name}: local {local_len}, remote {other:?}"
                ));
            }
        }
    }
    Ok(())
}

fn verify(ssh: &Ssh, artifacts: &Artifacts) -> Result<String, String> {
    let command = format!(
        "\"{}\\rhydra-agent.exe\" status --wait 30",
        artifacts.remote_dir()
    );
    match ssh.run(&command) {
        Ok(stdout) => {
            let line = stdout.lines().next().unwrap_or("");
            let value: serde_json::Value =
                serde_json::from_str(line).map_err(|e| format!("status output: {e}\n{stdout}"))?;
            let version = value["status"]["version"].as_str().unwrap_or("");
            if version != artifacts.version {
                return Err(format!(
                    "the running agent reports version {version:?}, expected {:?} — the old \
                     agent survived the deploy",
                    artifacts.version
                ));
            }
            Ok(format!(
                "agent v{version} stably green ({}x{} @ {} Hz)",
                value["status"]["display_mode"]["width"],
                value["status"]["display_mode"]["height"],
                value["status"]["display_mode"]["hz"],
            ))
        }
        Err(e) => Err(format!(
            "verify failed. First diagnosis on this host class: is an interactive console \
             session present? (An SSH context cannot host the capture stack.)\n{e}"
        )),
    }
}

/// The subcommand entry: parse, gather, decide, (maybe) execute, verify, report.
/// Returns the process exit code (0 ok, 1 failed, 2 usage) — main exits with it.
pub fn run(args: &[String]) -> i32 {
    let cfg = match parse_args(args) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let artifacts = match locate_artifacts(cfg.artifacts.as_deref()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("deploy: {e}");
            return 2;
        }
    };
    eprintln!(
        "deploy: rhydra v{} (driver {}) -> {}",
        artifacts.version, artifacts.driver_ver, cfg.host
    );

    let ssh = Ssh::new(&cfg);
    if let Err(e) = upload_scripts(&ssh) {
        eprintln!("deploy: uploading probe scripts: {e}");
        // This is deploy's first contact with the host, so it is also where "SSH
        // was never set up" surfaces — as a raw scp error that names no cause.
        // Ask the diagnosis for one before giving up.
        let d = crate::sshsetup::diagnose(&cfg.host);
        if !d.is_working() {
            eprintln!("deploy: {}.", d.headline());
            eprintln!("deploy: {}", d.remedy());
            eprintln!("deploy: run `mdrdp ssh-setup {}` to set it up.", cfg.host);
        }
        return 1;
    }
    let probe_out = match ssh.run_script(
        "probe.ps1",
        &format!("-TargetDir \"{}\"", artifacts.remote_dir()),
    ) {
        Ok(out) => out,
        Err(e) => {
            eprintln!("deploy: probe failed: {e}");
            return 1;
        }
    };
    let evidence = match parse_evidence(&probe_out) {
        Ok(ev) => ev,
        Err(e) => {
            eprintln!("deploy: {e}");
            return 1;
        }
    };
    report_evidence(&evidence);

    let branch = match decide(&cfg, &artifacts, &evidence) {
        Ok(b) => b,
        Err(stop) => {
            eprintln!("deploy: stopped before changing anything: {stop}");
            return 1;
        }
    };

    match branch {
        Branch::FastPath => {
            eprintln!(
                "deploy: v{} already deployed and healthy — verify only",
                artifacts.version
            );
            if cfg.dry_run {
                eprintln!("deploy: --dry-run, stopping before verify");
                return 0;
            }
        }
        Branch::AudioOnly(plan) => {
            for note in &plan.notes {
                eprintln!("deploy: note: {note}");
            }
            if cfg.dry_run {
                eprintln!("deploy: --dry-run audio-only plan:");
                for a in plan.pre.iter().chain(&plan.post) {
                    match a {
                        Action::Run { label, command } => eprintln!("  run   [{label}] {command}"),
                        Action::Script {
                            label,
                            script,
                            args,
                        } => eprintln!("  script [{label}] {script} {args}"),
                        Action::Copy { local, remote } => {
                            eprintln!("  copy  {} -> {remote}", local.display());
                        }
                    }
                }
                eprintln!("  then: agent verify (status --wait 30, stable green)");
                return 0;
            }
            let mut script_evidence = Vec::new();
            let steps: Result<(), String> = (|| {
                execute(&ssh, &plan.pre, &mut script_evidence)?;
                execute(&ssh, &plan.post, &mut script_evidence)?;
                Ok(())
            })();
            report_script_evidence(&script_evidence);
            match steps {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("deploy: {e}");
                    return 1;
                }
            }
        }
        Branch::Full(plan) => {
            for note in &plan.notes {
                eprintln!("deploy: note: {note}");
            }
            if cfg.dry_run {
                eprintln!("deploy: --dry-run plan:");
                for a in plan.pre.iter().chain(&plan.copies).chain(&plan.post) {
                    match a {
                        Action::Run { label, command } => eprintln!("  run   [{label}] {command}"),
                        Action::Script {
                            label,
                            script,
                            args,
                        } => eprintln!("  script [{label}] {script} {args}"),
                        Action::Copy { local, remote } => {
                            eprintln!("  copy  {} -> {remote}", local.display());
                        }
                    }
                }
                eprintln!("  then: size check, agent verify (status --wait 30, stable green)");
                return 0;
            }
            let mut script_evidence = Vec::new();
            let steps: Result<(), String> = (|| {
                execute(&ssh, &plan.pre, &mut script_evidence)?;
                execute(&ssh, &plan.copies, &mut script_evidence)?;
                check_sizes(&ssh, &artifacts)?;
                execute(&ssh, &plan.post, &mut script_evidence)?;
                Ok(())
            })();
            report_script_evidence(&script_evidence);
            match steps {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("deploy: {e}");
                    return 1;
                }
            }
        }
    }

    match verify(&ssh, &artifacts) {
        Ok(summary) => {
            eprintln!("deploy: {summary}");
            // The native connect needs key auth (a GUI session has no tty for ssh
            // prompts). Report the posture, never fail the deploy over it: the host
            // itself is healthy either way, and key install is a documented manual
            // step (Windows admin-key ACL rules make silent automation a foot-gun).
            if ssh.batchmode_ok() {
                eprintln!(
                    "deploy: key auth (BatchMode) verified — `mdrdp {} --native` can connect",
                    cfg.host
                );
            } else {
                eprintln!(
                    "deploy: warning: native connect requires key auth, and ssh {} refused \
                     BatchMode.\n  Install a key — for an admin account that is \
                     C:\\ProgramData\\ssh\\administrators_authorized_keys on the host.\n  \
                     (This deploy still succeeded; only `--native`/auto-detect needs the key.)",
                    ssh.dest
                );
            }
            // Record the host so `native = auto` may probe it (the deploy is the
            // opt-in the record carries to the connect path). Best-effort: a failed
            // write only costs auto-detection until the next deploy.
            match crate::native::deployed::default_path() {
                Some(path) => {
                    let mut record = crate::native::deployed::NativeHosts::load_from(&path);
                    record.record(&cfg.host, &artifacts.version, crate::presence::unix_now());
                    match record.save_to(&path) {
                        Ok(()) => eprintln!(
                            "deploy: recorded {} in {} — auto-detect will prefer native",
                            cfg.host,
                            path.display()
                        ),
                        Err(e) => eprintln!(
                            "deploy: warning: could not write {}: {e} — auto-detect will \
                             not probe this host until a deploy records it",
                            path.display()
                        ),
                    }
                }
                None => eprintln!(
                    "deploy: warning: no config directory, so the deploy record was not \
                     written — auto-detect will not probe this host (--native still works)"
                ),
            }
            0
        }
        Err(e) => {
            eprintln!("deploy: {e}");
            1
        }
    }
}

fn report_evidence(ev: &Evidence) {
    eprintln!(
        "deploy: host evidence: elevated={} console={} agent={} device={} staged_driver={:?} \
         audio={{package={} setup={} reboot={} identity={} render={} capture={} formats={} \
         default_active={}}}",
        ev.elevated,
        ev.console_user.as_deref().unwrap_or("<nobody>"),
        ev.agent
            .as_ref()
            .map(|a| format!("v{} green={}", a.version, a.green))
            .unwrap_or_else(|| "none".to_owned()),
        ev.device_present,
        ev.staged_driver_vers,
        ev.audio_package_staged,
        ev.audio_setup_ran,
        ev.audio_reboot_pending,
        ev.cable_identity_ok,
        ev.cable_render_active,
        ev.cable_capture_active,
        ev.cable_formats_ok,
        ev.audio_default_render_active,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_args_full_and_minimal() {
        let cfg = parse_args(&args(&["quench"])).unwrap();
        assert_eq!(cfg.host, "quench");
        assert!(cfg.user.is_none() && !cfg.dry_run && !cfg.force);

        let cfg = parse_args(&args(&[
            "quench",
            "--user",
            "ano",
            "--artifacts",
            "/tmp/a",
            "--dry-run",
            "--force",
        ]))
        .unwrap();
        assert_eq!(cfg.user.as_deref(), Some("ano"));
        assert_eq!(cfg.artifacts.as_deref(), Some(Path::new("/tmp/a")));
        assert!(cfg.dry_run && cfg.force);
    }

    #[test]
    fn parse_args_rejects_unknowns_and_missing_host() {
        assert!(parse_args(&args(&[])).is_err());
        assert!(parse_args(&args(&["quench", "--frobnicate"])).is_err());
        assert!(parse_args(&args(&["quench", "extra"])).is_err());
        assert!(parse_args(&args(&["--user"])).is_err());
    }

    #[test]
    fn crate_version_parses_this_repo_layout() {
        let toml = "[package]\nname = \"rhydra\"\nversion = \"0.2.0\"\nedition = \"2021\"\n";
        assert_eq!(crate_version_from_toml(toml).as_deref(), Some("0.2.0"));
        assert_eq!(crate_version_from_toml("no version here"), None);
    }

    #[test]
    fn driver_ver_parses_inf_line() {
        let inf = "[Version]\nSignature=\"$WINDOWS NT$\"\nDriverVer = 08/16/2026,1.0.0.1\n";
        assert_eq!(driver_ver_from_inf(inf).as_deref(), Some("1.0.0.1"));
        assert_eq!(driver_ver_from_inf("[Version]"), None);
    }

    // -- decide() scenarios ---------------------------------------------------

    fn test_artifacts(dir: &Path) -> Artifacts {
        // Real files so the fast path's size comparison has something to read.
        let mk = |name: &str, len: usize| -> PathBuf {
            let p = dir.join(name);
            std::fs::write(&p, vec![0u8; len]).unwrap();
            p
        };
        Artifacts {
            version: "0.2.0".to_owned(),
            server_exe: mk("rhydra-server.exe", 10),
            agent_exe: mk("rhydra-agent.exe", 20),
            creator_exe: mk("mdrdp-idd-create.exe", 30),
            driver_dll: mk("mdrdp_idd.dll", 40),
            driver_inf: mk("mdrdp-idd.inf", 50),
            driver_ver: "1.0.0.1".to_owned(),
        }
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mdrdp-deploy-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn healthy_evidence() -> Evidence {
        Evidence {
            elevated: true,
            console_user: Some(r"QUENCH\ano".to_owned()),
            ssh_user: "ano".to_owned(),
            agent_exes: vec![r"C:\mdrdp\v0.1.0\rhydra-agent.exe".to_owned()],
            port9502_owner: Some("rhydra-agent".to_owned()),
            port9500_owner: Some("rhydra-server".to_owned()),
            device_present: true,
            active_driver_ver: Some("1.0.0.1".to_owned()),
            staged_driver_vers: vec!["1.0.0.1".to_owned()],
            inf2cat_present: false,
            signtool_present: false,
            version_dirs: vec!["v0.1.0".to_owned()],
            target_dir_sizes: Default::default(),
            agent: Some(AgentProbe {
                version: "0.1.0".to_owned(),
                green: true,
                server_running: false,
            }),
            audio_package_staged: true,
            audio_setup_ran: true,
            audio_reboot_pending: false,
            cable_identity_ok: true,
            cable_render_active: true,
            cable_capture_active: true,
            cable_formats_ok: true,
            audio_default_render_active: true,
        }
    }

    fn cfg() -> Config {
        Config {
            host: "quench".to_owned(),
            user: Some("ano".to_owned()),
            artifacts: None,
            dry_run: false,
            force: false,
        }
    }

    #[test]
    fn upgrade_deploy_orders_copy_before_quiesce() {
        let dir = scratch_dir("upgrade");
        let art = test_artifacts(&dir);
        let ev = healthy_evidence();
        let Branch::Full(plan) = decide(&cfg(), &art, &ev).unwrap() else {
            panic!("expected a full plan");
        };
        // Fresh version dir: quiesce must be POST-copy (old agent survives a
        // failed copy), and install comes after quiesce.
        assert!(
            plan.pre
                .iter()
                .all(|a| !matches!(a, Action::Run { label, .. } if label.contains("quiesce")))
        );
        let labels: Vec<&str> = plan
            .post
            .iter()
            .map(|a| match a {
                Action::Run { label, .. } => label.as_str(),
                Action::Script { label, .. } => label.as_str(),
                Action::Copy { .. } => "copy",
            })
            .collect();
        assert_eq!(
            labels,
            ["quiesce existing agent", "install agent", "configure audio"],
            "the new version directory must receive its own loopback source selector"
        );
        assert_eq!(plan.copies.len(), 5);
    }

    #[test]
    fn same_version_redeploy_quiesces_before_copy() {
        let dir = scratch_dir("redeploy");
        let art = test_artifacts(&dir);
        let mut ev = healthy_evidence();
        ev.version_dirs.push("v0.2.0".to_owned());
        // Different bytes on the host (sizes absent), so not the fast path.
        let Branch::Full(plan) = decide(&cfg(), &art, &ev).unwrap() else {
            panic!("expected a full plan");
        };
        assert!(matches!(&plan.pre[0], Action::Run { label, .. } if label.contains("quiesce")));
    }

    #[test]
    fn fresh_host_skips_quiesce_entirely() {
        let dir = scratch_dir("fresh");
        let art = test_artifacts(&dir);
        let mut ev = healthy_evidence();
        ev.agent_exes.clear();
        ev.agent = None;
        ev.port9502_owner = None;
        ev.port9500_owner = None;
        let Branch::Full(plan) = decide(&cfg(), &art, &ev).unwrap() else {
            panic!("expected a full plan");
        };
        let all: Vec<&Action> = plan
            .pre
            .iter()
            .chain(&plan.copies)
            .chain(&plan.post)
            .collect();
        assert!(
            all.iter()
                .all(|a| !matches!(a, Action::Run { label, .. } if label.contains("quiesce")))
        );
    }

    #[test]
    fn fast_path_needs_version_sizes_and_green_agent() {
        let dir = scratch_dir("fastpath");
        let art = test_artifacts(&dir);
        let mut ev = healthy_evidence();
        ev.version_dirs.push("v0.2.0".to_owned());
        ev.agent = Some(AgentProbe {
            version: "0.2.0".to_owned(),
            green: true,
            server_running: false,
        });
        for (name, len) in [
            ("rhydra-server.exe", 10u64),
            ("rhydra-agent.exe", 20),
            ("mdrdp-idd-create.exe", 30),
            ("mdrdp_idd.dll", 40),
            ("mdrdp-idd.inf", 50),
        ] {
            ev.target_dir_sizes.insert(name.to_owned(), len);
        }
        assert!(matches!(
            decide(&cfg(), &art, &ev).unwrap(),
            Branch::FastPath
        ));

        // The state every successful deploy leaves behind — server running — must
        // still take the fast path without --force (it mutates nothing).
        ev.agent = Some(AgentProbe {
            version: "0.2.0".to_owned(),
            green: true,
            server_running: true,
        });
        assert!(matches!(
            decide(&cfg(), &art, &ev).unwrap(),
            Branch::FastPath
        ));

        // One byte off: not the fast path any more — and with the server live,
        // the mutation now force-gates.
        ev.target_dir_sizes
            .insert("rhydra-server.exe".to_owned(), 11);
        assert!(decide(&cfg(), &art, &ev).unwrap_err().contains("--force"));
        ev.agent = Some(AgentProbe {
            version: "0.2.0".to_owned(),
            green: true,
            server_running: false,
        });
        assert!(matches!(
            decide(&cfg(), &art, &ev).unwrap(),
            Branch::Full(_)
        ));
    }

    #[test]
    fn stops_mutating_nothing() {
        let dir = scratch_dir("stops");
        let art = test_artifacts(&dir);

        let mut ev = healthy_evidence();
        ev.elevated = false;
        assert!(decide(&cfg(), &art, &ev).unwrap_err().contains("elevated"));

        let mut ev = healthy_evidence();
        ev.console_user = None;
        assert!(
            decide(&cfg(), &art, &ev)
                .unwrap_err()
                .contains("no interactive console session")
        );

        let mut ev = healthy_evidence();
        ev.console_user = Some(r"QUENCH\someoneelse".to_owned());
        assert!(
            decide(&cfg(), &art, &ev)
                .unwrap_err()
                .contains("console user")
        );

        let mut ev = healthy_evidence();
        ev.port9500_owner = Some("spike-server-inc3".to_owned());
        assert!(
            decide(&cfg(), &art, &ev)
                .unwrap_err()
                .contains("spike-server-inc3")
        );

        let mut ev = healthy_evidence();
        ev.agent = Some(AgentProbe {
            version: "0.1.0".to_owned(),
            green: true,
            server_running: true,
        });
        assert!(decide(&cfg(), &art, &ev).unwrap_err().contains("--force"));
        let mut forced = cfg();
        forced.force = true;
        assert!(decide(&forced, &art, &ev).is_ok());
    }

    #[test]
    fn missing_driver_needs_signing_tools() {
        let dir = scratch_dir("driver");
        let art = test_artifacts(&dir);
        let mut ev = healthy_evidence();
        ev.device_present = false;
        ev.staged_driver_vers.clear();
        // No tools: stop with instructions.
        assert!(decide(&cfg(), &art, &ev).unwrap_err().contains("Inf2Cat"));
        // Tools present: full plan includes the driver install before the agent.
        ev.inf2cat_present = true;
        ev.signtool_present = true;
        let Branch::Full(plan) = decide(&cfg(), &art, &ev).unwrap() else {
            panic!("expected full plan");
        };
        let labels: Vec<&str> = plan
            .post
            .iter()
            .filter_map(|a| match a {
                Action::Run { label, .. } => Some(label.as_str()),
                Action::Script { label, .. } => Some(label.as_str()),
                Action::Copy { .. } => None,
            })
            .collect();
        let driver_pos = labels.iter().position(|l| *l == "install driver").unwrap();
        let agent_pos = labels.iter().position(|l| *l == "install agent").unwrap();
        assert!(driver_pos < agent_pos);
    }

    #[test]
    fn stale_active_driver_is_reinstalled_even_while_the_device_is_present() {
        let dir = scratch_dir("stale-driver");
        let art = test_artifacts(&dir);
        let mut ev = healthy_evidence();
        ev.active_driver_ver = Some("0.9.0.0".to_owned());
        ev.inf2cat_present = true;
        ev.signtool_present = true;

        let Branch::Full(plan) = decide(&cfg(), &art, &ev).unwrap() else {
            panic!("a stale live driver requires a full deploy");
        };
        assert!(plan.post.iter().any(
            |action| matches!(action, Action::Run { label, .. } if label == "install driver")
        ));
    }

    #[test]
    fn probe_evidence_json_round_trips() {
        // The wire contract with PROBE_PS1, exercised over a representative line
        // (shape mirrors real quench output; ConvertTo-Json emits exactly these
        // key spellings because the script builds them literally).
        let json = r#"{
            "elevated": true,
            "console_user": "QUENCH\\ano",
            "ssh_user": "ano",
            "agent_exes": ["C:\\mdrdp\\rhydra-agent.exe"],
            "port9502_owner": null,
            "port9500_owner": "spike-server-inc3",
            "device_present": true,
            "staged_driver_vers": ["1.0.0.1"],
            "inf2cat_present": false,
            "signtool_present": false,
            "version_dirs": [],
            "target_dir_sizes": {},
            "agent": null
        }"#;
        let ev = parse_evidence(json).unwrap();
        assert!(ev.elevated);
        assert_eq!(ev.port9500_owner.as_deref(), Some("spike-server-inc3"));
        assert!(ev.agent.is_none());

        // PowerShell omits nothing here, but absent optional collections must
        // default rather than fail (a fresh host has no C:\mdrdp at all).
        let minimal = r#"{
            "elevated": true, "console_user": null, "ssh_user": "ano",
            "port9502_owner": null, "port9500_owner": null,
            "device_present": false, "inf2cat_present": false,
            "signtool_present": false, "agent": null
        }"#;
        let ev = parse_evidence(minimal).unwrap();
        assert!(ev.agent_exes.is_empty() && ev.version_dirs.is_empty());
    }

    #[test]
    fn scripts_end_with_the_sentinel() {
        for (name, body) in [
            ("probe", PROBE_PS1),
            ("sizes", SIZES_PS1),
            ("driver-install", DRIVER_INSTALL_PS1),
            ("vbcable-install", VB_CABLE_INSTALL_PS1),
        ] {
            let last = body.trim_end().lines().last().unwrap();
            assert!(
                last.contains(SENTINEL),
                "{name}.ps1 must end by printing the sentinel, ends with {last:?}"
            );
        }
    }

    #[test]
    fn vbcable_script_is_pinned_and_verifies_before_install() {
        assert_eq!(VB_CABLE_ARCHIVE_BYTES, 1_318_877);
        assert_eq!(
            VB_CABLE_ARCHIVE_SHA256,
            "b950e39f01af1d04ea623c8f6d8eb9b6ea5c477c637295fabf20631c85116bfb"
        );
        assert_eq!(
            VB_CABLE_DOWNLOAD_URL,
            "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip"
        );
        let script = VB_CABLE_INSTALL_PS1;
        for required in [
            "VBCABLE_Driver_Pack45.zip",
            "VBCABLE_Setup_x64.exe",
            "Get-FileHash",
            "Get-AuthenticodeSignature",
            "CN=BUREL VINCENT Entrepreneur individuel",
            "Microsoft Windows Hardware Compatibility Publisher",
            "Microsoft Windows Third Party Component CA 2014",
            "UnknownError",
            "ManufacturerName",
            "Provider=%VBAudio%",
            "Provider=%ManufacturerName%",
            "VBAudioVACWDM",
            "VBAudioVACMME",
            "VB-Audio Software",
            "Start-Process",
            "-i",
            "-h",
            "[Guid]::NewGuid",
            "finally",
            "RHYDRA-OK",
            "donationware",
        ] {
            assert!(script.contains(required), "script is missing {required:?}");
        }
        assert!(
            script.find("Get-FileHash").unwrap() < script.find("Start-Process").unwrap(),
            "package verification must precede installer execution"
        );
        assert!(
            script.find("Get-AuthenticodeSignature").unwrap()
                < script.find("Start-Process").unwrap(),
            "signature verification must precede installer execution"
        );
        assert!(
            script.contains("Where-Object flow -eq 'render'")
                && script.contains("Where-Object flow -eq 'capture'"),
            "the postcheck must filter the `flow` field emitted by Cable-Endpoint-Identity"
        );
    }

    #[test]
    fn audio_preflight_covers_healthy_install_configure_reboot_and_broken() {
        assert!(
            PROBE_PS1.contains("Start-Process -FilePath $audioAgent")
                && PROBE_PS1.contains("$audioCheck.ExitCode -eq 0"),
            "preflight must read back the live endpoint formats, not trust a stale marker"
        );
        assert!(
            PROBE_PS1.contains(
                "$audioSetupRan -and $audioRebootPending -and (-not $cableRenderActive -or -not $cableCaptureActive)"
            ),
            "an unrelated system pending rename must not be attributed to healthy VB-CABLE"
        );
        let mut ev = healthy_evidence();
        assert_eq!(ev.audio_state(), AudioState::Healthy);

        ev.audio_package_staged = false;
        assert_eq!(ev.audio_state(), AudioState::NeedsInstall);

        ev.audio_package_staged = true;
        ev.cable_formats_ok = false;
        assert_eq!(ev.audio_state(), AudioState::NeedsConfigure);

        ev.audio_reboot_pending = true;
        assert_eq!(ev.audio_state(), AudioState::RebootRequired);

        ev.audio_reboot_pending = false;
        ev.cable_formats_ok = true;
        ev.audio_setup_ran = true;
        ev.cable_render_active = false;
        assert_eq!(ev.audio_state(), AudioState::Broken);
    }

    #[test]
    fn audio_only_repair_does_not_quiesce_or_replace_a_healthy_stack() {
        let dir = scratch_dir("audio-only");
        let art = test_artifacts(&dir);
        let mut ev = healthy_evidence();
        ev.version_dirs.push("v0.2.0".to_owned());
        for (name, len) in [
            ("rhydra-server.exe", 10u64),
            ("rhydra-agent.exe", 20),
            ("mdrdp-idd-create.exe", 30),
            ("mdrdp_idd.dll", 40),
            ("mdrdp-idd.inf", 50),
        ] {
            ev.target_dir_sizes.insert(name.to_owned(), len);
        }
        ev.cable_render_active = false;
        ev.cable_capture_active = false;
        ev.audio_package_staged = false;
        ev.audio_setup_ran = false;
        ev.agent = Some(AgentProbe {
            version: "0.2.0".to_owned(),
            green: true,
            server_running: true,
        });

        let Branch::AudioOnly(plan) = decide(&cfg(), &art, &ev).unwrap() else {
            panic!("a broken cable on an exact healthy stack is audio-only repair");
        };
        assert!(plan.copies.is_empty());
        assert!(
            plan.pre
                .iter()
                .chain(&plan.post)
                .all(|a| !matches!(a, Action::Run { label, .. } if label.contains("quiesce")))
        );
        assert!(matches!(
            plan.pre.first(),
            Some(Action::Script { label, .. }) if label.contains("VB-CABLE")
        ));
    }

    #[test]
    fn cable_install_is_first_before_any_full_deploy_mutation() {
        let dir = scratch_dir("audio-order");
        let art = test_artifacts(&dir);
        let mut ev = healthy_evidence();
        ev.audio_package_staged = false;
        ev.audio_setup_ran = false;
        ev.cable_render_active = false;
        ev.cable_capture_active = false;
        let Branch::Full(plan) = decide(&cfg(), &art, &ev).unwrap() else {
            panic!("expected a full plan");
        };
        assert!(matches!(
            plan.pre.first(),
            Some(Action::Script { label, .. }) if label.contains("VB-CABLE")
        ));
        let all: Vec<&Action> = plan
            .pre
            .iter()
            .chain(&plan.copies)
            .chain(&plan.post)
            .collect();
        let audio = all
            .iter()
            .position(|a| matches!(a, Action::Script { label, .. } if label.contains("VB-CABLE")))
            .unwrap();
        let quiesce = all
            .iter()
            .position(|a| matches!(a, Action::Run { label, .. } if label.contains("quiesce")));
        let copy = all.iter().position(|a| matches!(a, Action::Copy { .. }));
        assert!(quiesce.is_none_or(|i| audio < i));
        assert!(copy.is_none_or(|i| audio < i));
    }

    #[test]
    fn staged_cable_without_endpoint_after_setup_stops_without_a_loop() {
        let dir = scratch_dir("audio-broken");
        let art = test_artifacts(&dir);
        let mut ev = healthy_evidence();
        ev.audio_package_staged = true;
        ev.audio_setup_ran = true;
        ev.cable_render_active = false;
        ev.cable_capture_active = false;
        ev.audio_reboot_pending = false;
        let error = decide(&cfg(), &art, &ev).unwrap_err();
        assert!(error.contains("broken") || error.contains("reboot"));

        ev.audio_reboot_pending = true;
        assert!(decide(&cfg(), &art, &ev).unwrap_err().contains("reboot"));
    }
}
