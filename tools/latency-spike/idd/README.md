# mdrdp-idd — a high-refresh Windows indirect display driver

A UMDF indirect display driver (IddCx) that exposes **one EDID-less virtual monitor**
offering 1920x1080 at **240 / 120 / 60 Hz** (240 Hz preferred), plus 2560x1440 at
120/60 Hz as target modes. It exists to give stage 2 of the mdrdp latency spike a
display whose frame cadence is not capped at 60 Hz by real hardware.

It does no frame processing: the swap-chain thread acquires each frame and releases it
immediately, and every 600 frames writes one line to the debugger —

```
mdrdp-idd: 600 frames in 2.51 s (239.4 fps)
```

— via `OutputDebugStringW`. That cadence line *is* the measurement. Read it with
DebugView / WinDbg on the host.

## Provenance and licence

Derived from Microsoft's `IddSampleDriver`
([microsoft/Windows-driver-samples](https://github.com/microsoft/Windows-driver-samples),
commit `717778a20ba4dd2440fe609f69153a1f8a64f597`), which is MIT-licensed. The upstream
licence is kept verbatim as `LICENSE.upstream`; Microsoft's copyright header is retained
in every derived file. `driver/Driver.cpp` and `driver/Driver.h` stay deliberately close
to the sample so they remain diffable against it.

What we changed:

- WPP tracing stripped entirely (no `Trace.h`, no `Driver.tmh`).
- One monitor, always EDID-less. The sample's static EDID table is gone, so
  `FinishInit` always takes the descriptor-less path and modes come from
  `EvtIddCxMonitorGetDefaultDescriptionModes`. `IddSampleParseMonitorDescription` stays
  wired (IddCx requires a non-null callback) but is unreachable and returns
  `STATUS_INVALID_PARAMETER`.
- Monitor and target mode lists retuned for high-refresh 1080p.
- Telemetry strings renamed: friendly name "mdrdp latency-spike display",
  manufacturer "mdrdp", model "mdrdp-idd".
- Frame-cadence instrumentation added to `SwapChainProcessor::RunCore`.
- `creator/main.cpp`: the sample's interactive "press x to exit" loop replaced by a
  blocking wait (useless over SSH), optionally Ctrl+C-aware.
- `driver/mdrdp-idd.inf` authored fresh in plain ASCII with the sample's structure.

## Cross-building from macOS

```
./build.sh
```

Outputs `build/mdrdp_idd.dll`, `build/mdrdp-idd-create.exe`, and a copy of
`build/mdrdp-idd.inf` — copy the whole `build/` directory to the Windows host.

No Visual Studio and no Windows machine are involved: Apple clang in MSVC driver mode
(`clang --driver-mode=cl -target x86_64-pc-windows-msvc`) compiles, and rustup's
`rust-lld -flavor link` links as `lld-link`. `build.sh` is the toolchain-of-record —
every workaround is commented there.

Prerequisites (one-time):

- **CRT + Windows SDK 10.0.26100** via xwin, exactly as
  `scripts/check-windows.sh` documents: `brew install xwin` then
  `xwin --accept-license --temp splat --output ~/.xwin`.
- **WDK 10.0.26100.6584** — `curl` the `Microsoft.Windows.WDK.x64` NuGet package
  (`https://www.nuget.org/api/v2/package/Microsoft.Windows.WDK.x64/10.0.26100.6584`)
  and unzip it to `~/.xwin/wdk`. It supplies the UMDF 2.25 and IddCx 1.6 headers and
  stub libraries, and (for the host) `Inf2Cat.exe`.
- rustup's `llvm-tools` for `rust-lld`.

Env overrides: `XWIN_DIR`, `WDK_DIR`, `RUST_LLD`, `CLANG`.

**Caveat:** the SDK/WDK headers include each other with inconsistent filename case
(`IddCx.h` includes `<Opmapi.h>` and `<Dxgi.h>`). This build therefore relies on a
case-insensitive volume, which is the macOS default. On a case-sensitive volume you
would need a shim directory of lowercase symlinks — never edit `~/.xwin`.

## Deploy runbook (quench)

Test-signing only. Run from an **elevated** shell on the Windows host, with `build/`
copied to `C:\mdrdp-idd`. `Inf2Cat.exe` and `signtool.exe` come from a WDK/SDK install
on the host (the same `Microsoft.Windows.WDK.x64` package works — it ships
`bin\10.0.26100.0\x86\Inf2Cat.exe`).

**Install**

```powershell
# 1. Enable test signing, then REBOOT.
bcdedit /set testsigning on
shutdown /r /t 0

# 2. Create a self-signed code-signing cert (once).
$cert = New-SelfSignedCertificate -Type CodeSigningCert `
    -Subject "CN=mdrdp latency spike" -CertStoreLocation Cert:\CurrentUser\My
Export-Certificate -Cert $cert -FilePath C:\mdrdp-idd\mdrdp-idd.cer

# 3. Generate the catalogue from the INF.
Inf2Cat.exe /driver:C:\mdrdp-idd /os:10_NI_X64 /verbose

# 4. Sign the catalogue.
signtool sign /fd SHA256 /sha1 $cert.Thumbprint /t http://timestamp.digicert.com `
    C:\mdrdp-idd\mdrdp-idd.cat

# 5. Trust the cert as a root and as a trusted publisher (machine store).
certutil -addstore root C:\mdrdp-idd\mdrdp-idd.cer
certutil -addstore trustedpublisher C:\mdrdp-idd\mdrdp-idd.cer

# 6. Stage and install the driver.
pnputil /add-driver C:\mdrdp-idd\mdrdp-idd.inf /install

# 7. Plug the display in. Holds it open until killed; --wait also honours Ctrl+C.
C:\mdrdp-idd\mdrdp-idd-create.exe --wait
```

**Verify** — a second monitor named "mdrdp latency-spike display" appears in
`Settings > System > Display`, and its Advanced display page offers 1920x1080 at 240,
120 and 60 Hz. `pnputil /enum-devices /connected /class Display` lists it. Watch
DebugView for the `mdrdp-idd:` cadence lines.

**Teardown** (mirror image, elevated)

```powershell
# 1. Stop the creator (Ctrl+C under --wait, or Stop-Process) - the display unplugs.
Stop-Process -Name mdrdp-idd-create -Force

# 2. Remove the driver package. <oem##.inf> from: pnputil /enum-drivers
pnputil /delete-driver <oem##.inf> /uninstall /force

# 3. Drop the trust.
certutil -delstore root "mdrdp latency spike"
certutil -delstore trustedpublisher "mdrdp latency spike"
Get-ChildItem Cert:\CurrentUser\My |
    Where-Object Subject -eq "CN=mdrdp latency spike" | Remove-Item

# 4. Disable test signing, then REBOOT.
bcdedit /set testsigning off
shutdown /r /t 0
```
