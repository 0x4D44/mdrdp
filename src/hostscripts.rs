//! Ready-to-paste host-side setup scripts the client offers via its menus.
//!
//! Each script targets the *remote* Windows host, not this machine. The client
//! copies one to the local clipboard; CLIPRDR carries it into the session, and
//! the user pastes it into a PowerShell window on the host.

/// Enables the H.264/AVC444 graphics policy on a Windows RDP host.
///
/// Sets the registry backing of the two group policies "Prioritize H.264/AVC 444
/// graphics mode" and "Configure H.264/AVC hardware encoding" (Remote Session
/// Environment), then refreshes policy. Measured on a live host: `gpupdate`
/// suffices, no reboot — but only *new* connections pick the policy up.
///
/// The snippet is pasteable into any non-elevated PowerShell: it relaunches
/// itself elevated (one UAC prompt) and leaves the elevated window open so the
/// `gpupdate` result is visible.
pub const ENABLE_AVC444: &str = r#"# mdrdp: enable H.264/AVC444 for RDP on this host (run on the REMOTE host).
# Paste into any PowerShell window; it re-launches itself elevated (one UAC
# prompt). Applies to NEW connections - reconnect after it finishes.
$k = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services'
$cmd = "New-Item -Path '$k' -Force | Out-Null; " +
       "Set-ItemProperty -Path '$k' -Name AVC444ModePreferred -Value 1 -Type DWord; " +
       "Set-ItemProperty -Path '$k' -Name AVCHardwareEncodePreferred -Value 1 -Type DWord; " +
       "gpupdate /force"
Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-NoExit','-Command',$cmd
"#;
