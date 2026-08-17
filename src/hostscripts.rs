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

/// Raises the host's RDP frame-rate cap from the ~30 fps default to 60 fps.
///
/// Sets `DWMFRAMEINTERVAL` under the Terminal Server WinStations key. Measured on
/// a live host (2026-08-17): sustained motion goes from ~30 to ~60 fps and motion
/// round-trip latency halves; typing latency is unaffected (its floor is in the
/// server's input path). 15 is the useful floor — the session's 60 Hz virtual
/// display caps the cadence, so lower values buy nothing. Applies to NEW
/// connections; no policy refresh needed because this is not a group policy.
pub const ENABLE_60FPS: &str = r#"# mdrdp: raise this host's RDP frame cap from ~30 to 60 fps (run on the REMOTE host).
# Paste into any PowerShell window; it re-launches itself elevated (one UAC
# prompt). Applies to NEW connections - reconnect after it finishes.
$k = 'HKLM:\SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations'
$cmd = "Set-ItemProperty -Path '$k' -Name DWMFRAMEINTERVAL -Value 15 -Type DWord; " +
       "'DWMFRAMEINTERVAL is now ' + (Get-ItemProperty -Path '$k').DWMFRAMEINTERVAL"
Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-NoExit','-Command',$cmd
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// Every host script must self-elevate and keep its window open: they are
    /// pasted into whatever non-elevated PowerShell the user has, and a script
    /// whose elevated window closes swallows its own evidence.
    #[test]
    fn every_script_self_elevates_and_keeps_the_result_visible() {
        for (name, script) in [
            ("ENABLE_AVC444", ENABLE_AVC444),
            ("ENABLE_60FPS", ENABLE_60FPS),
        ] {
            assert!(
                script.contains("Start-Process powershell -Verb RunAs"),
                "{name} must relaunch itself elevated"
            );
            assert!(
                script.contains("'-NoProfile','-NoExit'"),
                "{name} must keep the elevated window open"
            );
            assert!(
                script.starts_with("# mdrdp:"),
                "{name} must open with a comment naming its purpose"
            );
            assert!(script.is_ascii(), "{name} must survive any clipboard hop");
        }
    }

    #[test]
    fn the_60fps_script_sets_the_measured_useful_floor() {
        assert!(ENABLE_60FPS.contains("DWMFRAMEINTERVAL"));
        assert!(
            ENABLE_60FPS.contains("-Value 15"),
            "15 is the floor that matters; lower values are dead weight"
        );
        assert!(
            ENABLE_60FPS.contains("Get-ItemProperty"),
            "the script must read the value back so the user sees it took"
        );
    }
}
