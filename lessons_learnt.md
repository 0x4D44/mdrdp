<!-- lessons-format: index-v1 -->
# Lessons learnt

Newest at the top. The **first line of each entry is the lesson** — self-contained, with a
`file:symbol` pointer, under ~120 characters. Only first lines are injected at session
start, so a line that needs the detail below it to make sense is a line that will not work.
Indented lines below the first are detail: kept for lookup, never injected.

- H.264 reference state belongs to an EGFX surface incarnation, not the channel (`client::h264_decoders`).
  A real display transition can overlap two surface lifetimes, so resetting one shared decoder
  on every numeric-id switch destroys both chains; same-id `CreateSurface` reuse has the opposite
  failure and retains stale references. Keep one lazy decoder per live surface, drop it on delete
  or id reuse, and retain the last painted output while its zero-filled replacement is unpainted.
  MDR-BUG-FLUX-00008.

A lesson is **never dropped to make room** for a new one — prepend it and let the oldest
fall out of the injected window. Soft target ~25 entries; past ~40, say it is due a prune
rather than pruning unasked. Durable project facts and conventions belong in `CLAUDE.md`,
not here.

---

- A healthy Windows audio device can have no active render endpoint; ask WASAPI, not WMI (`tools/audio-probe`).
  `Win32_SoundDevice` said "Realtek — Status OK" and Audiosrv was running, but every
  endpoint was unplugged/not-present and `GetDefaultAudioEndpoint` returned 0x80070490.
  WASAPI loopback had nothing to attach to, so there was no audio to capture.

- eframe sets the Dock icon; raw winit needs a long-lived raw-RGBA AppKit image (`dock::set_label`).
  eframe calls `-[NSApplication setApplicationIconImage:]` through
  `ViewportBuilder::with_icon`; the raw-winit session window did not. Compose the tile in
  portable Rust and pass AppKit raw RGBA, not PNG: some macOS builds load an arbitrary
  libpng for `NSImage`-from-PNG and SIGBUS (egui#7155). `NSBitmapImageRep` does not copy its
  planes, so the buffer must outlive the call.

- A Windows clipboard lock does not persist on quench; a 30 s hold ends 0x8007058A (`tools/clipboard-hold`).
  PowerShell P/Invoke and two native implementations all reported `opened=true`, then
  `closed=false — thread does not have a clipboard open`, while the app wrote throughout.
  Any clipboard holder must report `CloseClipboard`; otherwise a false hold reads as a pass.

- Windows display/input probes need an unlocked console via `tscon` (`agent.rs:input_desktop_rung`).
  SSH and disconnected sessions can enumerate no displays, making the agent report a healthy
  IDD as gone. A locked console still reports `WinSta0\Default` and successful `SendInput`,
  but the secure `Winlogon` input desktop eats the events. Run display-facing work from an
  Interactive scheduled task after `tscon`; never cycle a healthy device for this symptom.

- IDD input needs `dmPosition`; the default zero origin misplaces clicks (`win/idd_source.rs:IddSource::origin`).
  The IDD pool header has no desktop coordinates, so query `DEVMODEW::dmPosition`
  (`agent_ops::idd_display_origin`). This stayed latent while IDD was the only display;
  attaching a console moved it to (1920,0), making every click land 1920 pixels left.

- Windows SSH setup needs PS 5.1, its own firewall rule, and the admin key file (`hostscripts::setup_ssh`).
  PowerShell 7 failed DISM with "Class not registered" and returned silent empties from
  `Get-Net*`; self-elevate into Windows PowerShell 5.1. Installing OpenSSH rewrites its
  built-in firewall rule to Private, so add `mdrdp-sshd-in` after installation. Admin users
  authenticate only through `administrators_authorized_keys`, ACLed to SYSTEM + Administrators.

- An RDP console logon rebuilds quench's IDD and strands capture (`agent.rs:request_device_cycle`).
  The display renumbers and the server keeps its old `Global\mdrdp-idd` generation. Recover
  in order: `mdrdp-cycle-idd`; stop `mdrdp-idd-create.exe` so the agent republishes the
  section; restart `rhydra-server.exe`.

- A static desktop gives a new rhydra viewer no frame despite its keyframe request (`win/pipeline.rs:capture_loop`).
  `request_keyframe()` fires, then `acquire()` times out, so no access unit exists to encode.
  Idle one-second reconnects decoded 1/1/0/0/4; with the desktop changing, 5/5. State desktop
  activity before diagnosing a blank connection. MDR-BUG-FLUX-00011.

- Intel MF accepts 4:4:4 profiles but emits 4:2:0; verify the SPS (`tools/mf-caps-probe`).
  On quench, Intel HEVC accepted Main_444_8 + ARGB32 but emitted profile 1 /
  `chroma_format_idc=1`; Intel VP9 ignored the profile attribute. D3D12 Video Encode does
  expose HEVC Main_444/Main10_444 with AYUV/Y410, but not through the MFT.

- M5 Max VideoToolbox decodes H.264/HEVC 4:4:4 in hardware at 5K; VP9 has no VT decoder (`tools/vt-caps-probe`).
  The probe feeds real 4:2:0/4:2:2/4:4:4 streams to a hardware-required session. AV1 is
  hardware for 4:2:0 only. The 4096x2304 H.264 clamp is the Windows encoder's ceiling, not
  the Mac decoder's.

- A metric must encode the failure: B-spikes rose after the blue-flash fix, while blue-flips hit zero (`avcreplay`).
  "Pixel moved for one frame" counted deliberate flat-average softening as failure. Hue
  inversion—blue for one frame on yellow content—isolated the impossible rendering and
  tracked MDR-BUG-FLUX-00010 correctly.

- softbuffer colour-converts each whole frame on CPU; IOSurface contents avoid it (`present.rs`).
  An idle 1440p session burned 20–90% of a core in CoreAnimation's ColorSync path plus
  softbuffer zero-allocation. `present::LayerPresenter` moves conversion to the GPU. Never
  rewrite the surface currently displayed: CoreAnimation may ignore `setContents` naming
  the same object, hence the three-surface pool. `MDRDP_PRESENT=soft` keeps the A/B path.

- spike-server captures nothing without a viewer; a viewer-less smoke proves nothing (`win/pipeline.rs:609`).
  The capture loop sleeps until the video port has a client. Arm it through an SSH tunnel
  with `nc -d 127.0.0.1 9500 > /dev/null`; without `-d`, stdin EOF closes it. Scheduled
  tasks cannot `SetForegroundWindow`, and ending the task kills only its cmd wrapper, so use
  `WM_CHAR` stimulus and clean orphaned server processes explicitly.

- Monitor power-off poisons DXGI duplication without an error; restart capture (`win/dxgi.rs:read_rects`).
  Frames remain "successful" with valid dirty metadata but black pixels, so AccessLost
  recovery never fires; GDI still sees the desktop. quench disables monitor sleep, and
  waking it requires `SetThreadExecutionState` inside the interactive session.

- Reliable glass runs need HID-source PID events plus a display-wake assertion (`probe::glass::inject`, `wakelock`).
  A NULL-source `CGEventPostToPid` reports success but drops the event; create a
  HIDSystemState source. Focus-routed events lose keystrokes when another app becomes
  frontmost, so post to the target PID. ScreenCaptureKit exposes no display at the lock
  screen, so hold an IOPM display-wake assertion between runs.

- A trusted self-signed UMDF driver loads under Secure Boot without testsigning (`src/deploy.rs:DRIVER_INSTALL_PS1`).
  Trust the certificate in Root + TrustedPublisher and install with pnputil. This works for
  user-mode IddCx, not virtual audio: ACX and PortCls are kernel-mode and still meet the
  Secure Boot code-integrity gate. The runbook lives in `src/deploy.rs:DRIVER_INSTALL_PS1`.

- AVC444 sends no SurfaceToCache PDUs; a quiet EGFX cache is server behaviour (`gfx::apply_surface_to_cache`).
  Measured on quench while 117 MB of AVC444v2 painted: hits, misses and entries all stayed
  zero. Full-frame H.264 carries the pixels, so `hit_rate()` correctly returns `None`.

- EGFX capability versions, not codec toggles, control AVC; Windows can strip AVC420 (`gfx::capabilities`).
  V8.1 is the only offer that can say AVC420 without AVC444. The offer can negotiate cleanly
  while the server keeps sending ClearCodec/Progressive until host H.264 policy is enabled.
  Check host policy before debugging the client.

- Windows drops EDISP layouts sent before caps; gate on `DisplayControlClient::ready` (`session::service_resize`).
  An open Display Control channel is not enough. Sending early succeeds locally but the
  server ignores the layout without error. A successful resize may complete through EGFX
  ResetGraphics and new surfaces without any DeactivateAll.

- Measure stages separately; success-only counters and blob spans hide failures (`audio::AudioStats`, `stagelog`).
  `current_format` once meant "a wave played", not "a format negotiated". Decode failure
  totals hid primary causes behind cascades. One span across CredSSP, MCS, licensing and
  finalization falsely attributed most connect time to CredSSP. Instrument the actual stage
  boundaries and preserve failure reasons.

- macOS keychain ACLs bind to a binary hash, so each unsigned rebuild re-prompts (`creds`).
  "Always allow" expires with the next `cargo build`; `-A` is the development escape.
  Stable code signing is functional for a launcher that starts one process per session.
