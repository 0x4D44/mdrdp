# Deploy the mdrdp latency-spike indirect display driver on the Windows test host.
#
# Two phases, because test signing needs a reboot in the middle:
#
#   .\deploy.ps1 -Phase pre        # enable test signing, then reboots the machine
#   .\deploy.ps1 -Phase install    # cert -> catalogue -> sign -> trust -> pnputil
#   .\deploy.ps1 -Phase teardown   # mirror image, then offers to disable test signing
#
# Run from an ELEVATED PowerShell in the directory that holds the build outputs
# (mdrdp_idd.dll, mdrdp-idd-create.exe, mdrdp-idd.inf) — i.e. a copy of build/.
# Inf2Cat.exe must be reachable: either on PATH or passed via -Inf2Cat. It ships in
# the Microsoft.Windows.WDK.x64 NuGet at c\bin\10.0.26100.0\x86\Inf2Cat.exe (runs
# fine on x64). signtool.exe ships in the Windows SDK; -SignTool overrides likewise.
#
# The script is deliberately verbose and stops on the first error: a driver install
# that half-happened is worse than one that failed loudly.

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('pre', 'install', 'teardown')]
    [string]$Phase,

    [string]$Inf2Cat = 'Inf2Cat.exe',
    [string]$SignTool = 'signtool.exe',

    # Windows version key for Inf2Cat catalogue generation (Win11 x64).
    [string]$CatOs = '10_NI_X64'
)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$certSubject = 'CN=mdrdp latency spike'
$certFile = Join-Path $here 'mdrdp-idd.cer'

function Require-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $p = New-Object Security.Principal.WindowsPrincipal($id)
    if (-not $p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'This phase must run elevated.'
    }
}

switch ($Phase) {
    'pre' {
        Require-Admin
        bcdedit /set testsigning on
        if ($LASTEXITCODE -ne 0) { throw 'bcdedit failed' }
        Write-Host 'Test signing enabled. Rebooting in 10 seconds (Ctrl+C to abort)...'
        Start-Sleep -Seconds 10
        shutdown /r /t 0
    }

    'install' {
        Require-Admin
        foreach ($f in 'mdrdp_idd.dll', 'mdrdp-idd-create.exe', 'mdrdp-idd.inf') {
            if (-not (Test-Path (Join-Path $here $f))) { throw "missing $f beside deploy.ps1" }
        }
        $ts = (bcdedit /enum '{current}' | Select-String -SimpleMatch 'testsigning')
        if (-not ($ts -match 'Yes')) {
            throw 'test signing is not enabled — run -Phase pre (and reboot) first'
        }

        # One code-signing cert, reused across reinstalls.
        $cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
            Where-Object Subject -eq $certSubject | Select-Object -First 1
        if (-not $cert) {
            $cert = New-SelfSignedCertificate -Type CodeSigningCert `
                -Subject $certSubject -CertStoreLocation Cert:\CurrentUser\My
        }
        Export-Certificate -Cert $cert -FilePath $certFile | Out-Null

        & $Inf2Cat "/driver:$here" "/os:$CatOs" /verbose
        if ($LASTEXITCODE -ne 0) { throw 'Inf2Cat failed' }

        & $SignTool sign /fd SHA256 /sha1 $cert.Thumbprint (Join-Path $here 'mdrdp-idd.cat')
        if ($LASTEXITCODE -ne 0) { throw 'signtool failed' }

        certutil -addstore root $certFile
        if ($LASTEXITCODE -ne 0) { throw 'certutil (root) failed' }
        certutil -addstore trustedpublisher $certFile
        if ($LASTEXITCODE -ne 0) { throw 'certutil (trustedpublisher) failed' }

        pnputil /add-driver (Join-Path $here 'mdrdp-idd.inf') /install
        if ($LASTEXITCODE -ne 0) { throw 'pnputil /add-driver failed' }

        Write-Host ''
        Write-Host 'Installed. Plug the display in with:'
        Write-Host "  $(Join-Path $here 'mdrdp-idd-create.exe') --wait"
        Write-Host 'Verify: Settings > System > Display shows "mdrdp latency-spike display"'
        Write-Host 'offering 1920x1080 at 240/120/60 Hz; DebugView shows mdrdp-idd: cadence lines.'
    }

    'teardown' {
        Require-Admin
        Get-Process mdrdp-idd-create -ErrorAction SilentlyContinue | Stop-Process -Force
        $pub = pnputil /enum-drivers | Out-String
        # Find every staged copy of our INF and delete it.
        $blocks = $pub -split "(?m)^Published Name\s*:" | Where-Object { $_ -match 'mdrdp-idd\.inf' }
        foreach ($b in $blocks) {
            if ($b -match '(oem\d+\.inf)') {
                pnputil /delete-driver $Matches[1] /uninstall /force
            } elseif ($b -match '^\s*(\S+\.inf)') {
                pnputil /delete-driver $Matches[1] /uninstall /force
            }
        }
        certutil -delstore root $certSubject.Substring(3)
        certutil -delstore trustedpublisher $certSubject.Substring(3)
        Write-Host 'Driver and cert removed. To leave test signing (needs a reboot):'
        Write-Host '  bcdedit /set testsigning off; shutdown /r /t 0'
    }
}
