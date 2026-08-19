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

/// Installs OpenSSH Server on a Windows host and trusts `public_key`.
///
/// Unlike the two constants above this is a function: the script has to carry
/// *this* machine's public key, so there is nothing static to hold.
///
/// Three details here were learned the hard way against live hosts, and each
/// looks arbitrary until it bites:
///
/// - **It self-elevates into `powershell`, which is Windows PowerShell 5.1.**
///   Under PowerShell 7 the DISM-backed cmdlets fail with "Class not
///   registered" and the CIM-backed `Get-Net*` cmdlets return silent empties —
///   which reads as a broken servicing stack, and as sshd not listening when it
///   is. Self-elevating gets a 5.1 child from whatever shell the user pasted
///   into.
/// - **The firewall rule is named `mdrdp-sshd-in`, not the built-in name, and is
///   added *after* the capability install.** Installing the OpenSSH capability
///   rewrites any rule sharing its own name (`OpenSSH-Server-In-TCP`) back to
///   the Private profile, which silently undid a working rule on one host.
/// - **`Profile Any`.** The built-in rule is Private-profile only and these
///   hosts sit on a network Windows categorises Public, so it never applies. The
///   symptom is a connect *timeout* while sshd listens perfectly.
///
/// The key goes to `administrators_authorized_keys`, the only file Windows
/// OpenSSH consults for members of Administrators — a key in
/// `~/.ssh/authorized_keys` is ignored for those accounts. That file is
/// machine-wide, so every admin on the box can use the key.
pub fn setup_ssh(public_key: &str) -> String {
    // The key is one line of base64 plus a comment, embedded in a single-quoted
    // PowerShell string. The ssh-ed25519 alphabet cannot produce a quote, so the
    // strip is belt-and-braces against a hand-edited `.pub`.
    let key = public_key.trim().replace('\'', "");
    format!(
        r#"# mdrdp: install OpenSSH Server on this host and trust this client's key (run on the REMOTE host).
# Paste into any PowerShell window; it re-launches itself elevated (one UAC
# prompt). Elevating also gets Windows PowerShell 5.1, where the DISM and
# firewall cmdlets work - under PowerShell 7 they fail or return nothing.
$cmd = @'
$key = '{key}'
# 1. OpenSSH Server.
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0 | Out-Null
# 2. Running now, and again after a reboot.
Set-Service -Name sshd -StartupType Automatic
Start-Service sshd
# 3. Firewall, AFTER the install: the capability rewrites any rule sharing its
#    own name back to the Private profile. Ours has its own name and Profile Any,
#    because these hosts sit on a network Windows categorises Public and the
#    built-in Private-only rule therefore never applies.
if (-not (Get-NetFirewallRule -Name 'mdrdp-sshd-in' -ErrorAction SilentlyContinue)) {{
  New-NetFirewallRule -Name 'mdrdp-sshd-in' -DisplayName 'OpenSSH sshd inbound' -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22 -Profile Any | Out-Null
}}
# 4. Trust the key. Administrators are read ONLY from this file.
$f = 'C:\ProgramData\ssh\administrators_authorized_keys'
$lines = @()
if (Test-Path $f) {{ $lines = @(Get-Content $f | Where-Object {{ $_.Trim() -ne '' }}) }}
if ($lines -notcontains $key) {{ $lines += $key }}
Set-Content -Path $f -Value $lines -Encoding ascii
# 5. sshd refuses the file unless only SYSTEM and Administrators can write it.
icacls $f /inheritance:r /grant 'SYSTEM:F' /grant 'BUILTIN\Administrators:F' | Out-Null
# 6. Evidence, not hope.
'sshd:      ' + (Get-Service sshd).Status
'listening: ' + [bool](Get-NetTCPConnection -LocalPort 22 -State Listen -ErrorAction SilentlyContinue)
'user:      ' + (whoami)
'@
Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-NoExit','-Command',$cmd
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative key, shaped like a real one so the quoting is exercised.
    const SAMPLE_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAqn4qJo3hDtlouTffs/fUfvHofQpoa9QHt3nS/IMek6 mdrdp-fleet-key";

    /// Every host script must self-elevate and keep its window open: they are
    /// pasted into whatever non-elevated PowerShell the user has, and a script
    /// whose elevated window closes swallows its own evidence.
    #[test]
    fn every_script_self_elevates_and_keeps_the_result_visible() {
        let setup_ssh = setup_ssh(SAMPLE_KEY);
        for (name, script) in [
            ("ENABLE_AVC444", ENABLE_AVC444),
            ("ENABLE_60FPS", ENABLE_60FPS),
            ("setup_ssh", setup_ssh.as_str()),
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
    fn the_ssh_script_carries_the_key_it_was_given() {
        let s = setup_ssh(SAMPLE_KEY);
        assert!(
            s.contains(SAMPLE_KEY),
            "the host cannot trust a key we omit"
        );
    }

    #[test]
    fn the_ssh_script_writes_the_only_file_windows_reads_for_admins() {
        // A key in ~/.ssh/authorized_keys is silently ignored for members of
        // Administrators, which is every account we use.
        let s = setup_ssh(SAMPLE_KEY);
        assert!(s.contains(r"C:\ProgramData\ssh\administrators_authorized_keys"));
        // sshd refuses the file outright if anyone else can write it.
        assert!(s.contains("icacls"));
        assert!(s.contains("BUILTIN\\Administrators:F"));
    }

    #[test]
    fn the_ssh_script_adds_its_own_firewall_rule_with_profile_any() {
        // Two lessons in one assertion pair. The built-in rule is Private-profile
        // only and these hosts are on a Public-categorised network, so a rule
        // without Profile Any never applies — and the symptom is a connect
        // timeout while sshd listens perfectly, which points away from the cause.
        let s = setup_ssh(SAMPLE_KEY);
        assert!(
            s.contains("-Profile Any"),
            "a Private-only rule never applies here"
        );
        assert!(
            s.contains("mdrdp-sshd-in") && !s.contains("OpenSSH-Server-In-TCP"),
            "the rule must NOT share the built-in name: installing the OpenSSH \
             capability rewrites a same-named rule back to Private"
        );
    }

    #[test]
    fn the_ssh_script_installs_the_capability_before_adding_the_rule() {
        // Order is load-bearing: the capability install rewrites same-named
        // rules, and has clobbered a working rule on a live host.
        let s = setup_ssh(SAMPLE_KEY);
        let install = s.find("Add-WindowsCapability").expect("installs OpenSSH");
        let rule = s.find("New-NetFirewallRule").expect("adds a rule");
        assert!(install < rule, "the rule must be added after the install");
    }

    #[test]
    fn the_ssh_script_reports_evidence_rather_than_assuming_success() {
        // Every earlier failure was silent. The script must show the user the
        // service state and whether anything actually holds port 22.
        let s = setup_ssh(SAMPLE_KEY);
        assert!(s.contains("Get-Service sshd"));
        assert!(s.contains("Get-NetTCPConnection -LocalPort 22"));
        assert!(
            s.contains("whoami"),
            "the login name decides the client's User line"
        );
    }

    #[test]
    fn a_key_bearing_a_quote_cannot_break_out_of_its_powershell_string() {
        let s = setup_ssh("ssh-ed25519 AAAA'; whoami; '  bad-comment");
        assert!(
            !s.contains("'; whoami; '"),
            "quotes must not survive into the script"
        );
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
