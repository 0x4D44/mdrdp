<!-- lessons-format: index-v1 -->
# Lessons learnt

Newest at the top. The **first line of each entry is the lesson** — self-contained, with a
`file:symbol` pointer, under ~120 characters. Only first lines are injected at session
start, so a line that needs the detail below it to make sense is a line that will not work.
Indented lines below the first are detail: kept for lookup, never injected.

- A headless Windows box reports healthy audio hardware while having NO active render endpoint (`tools/audio-probe`).
  `Win32_SoundDevice` says "Realtek — Status OK" and Audiosrv is Running, but with nothing in the jack every
  endpoint is unplugged/not-present and `GetDefaultAudioEndpoint` returns 0x80070490. WASAPI loopback capture
  has nothing to attach to, and apps cannot render audio at all — so there is no audio to capture, not merely a
  capture problem. Ask WASAPI, never WMI. Parked tranche 6; see the PARK doc in wrk_docs.

- A test fixture that silently fails to run makes the harness report the failure it exists to detect (`tools/clipboard-soak/src/main.rs`).
  Cost time twice on tranche 5. A clipboard holder reported success while holding nothing, and later
  `clipboard-cycle`'s launch died because the previous instance still held its log file, so a shell
  redirect could not open — the soak then ran with no generator and reported host->mac misses that had
  nothing to do with the clipboard. Two in a row is a wedge, so the run's headline finding was an
  artefact of its own fixture. Fix is a startup probe: the harness refuses to begin until it observes
  the fixture actually working, rather than assuming it launched.

- A new mdrdp dependency also needs `tools/latency-spike/viewer/Cargo.lock` refreshed, or `deltic integrate` fails.
  The viewer is its own workspace that depends on the root library by path, so a dep added to
  mdrdp changes the viewer's graph too. Deltic's bump step runs `cargo metadata --locked` over
  every sub-manifest and refuses to update a lock, so the integration dies after the rebase with
  "cannot update the lock file … because --locked was passed". Refresh it in the same commit:
  `cargo metadata --manifest-path tools/latency-spike/viewer/Cargo.toml --offline --format-version 1`.

- eframe sets the macOS Dock icon for you; a raw-winit window gets the generic "exec" tile (`dock::set_label`).
  macOS gives an unbundled executable a placeholder tile unless something calls
  `-[NSApplication setApplicationIconImage:]`. eframe makes that call from
  `ViewportBuilder::with_icon`, so the launcher always looked right — the session window is
  raw winit and never did. The fix composes the tile in portable Rust (icon + a hostname pill
  rasterised with ab_glyph) and hands AppKit one RGBA buffer, so the layout and the pixels are
  unit-testable. Build the rep from raw RGBA, never from PNG: some macOS builds load an
  arbitrary libpng for `NSImage`-from-PNG and SIGBUS (egui#7155). `NSBitmapImageRep` does not
  copy the planes, and the Dock re-renders whenever it likes, so the buffer must outlive the call.

- Resetting the H.264 decoder on ResetGraphics fails every P-frame until the next IDR (`client::handle_reset_graphics`).
  `H264Decoder::reset` drops the VideoToolbox session, and a session rebuilt mid-GOP holds no
  reference frames — while the server, which never asked for this, keeps sending P-frames. The
  decoder already rebuilds itself when SPS/PPS change, so the reset was redundant as well as
  fatal. ResetGraphics is ROUTINE on any host with a screen attached (idle power-off, backlight,
  lid, dock, and every window resize), so this blacked out the whole desktop until something
  forced a keyframe — which is why resizing the window a few times "fixed" it. MDR-BUG-FLUX-00008.

- Every fleet test host is headless, so display-mode transitions cannot be reproduced on any of them.
  A physical panel produces idle power-off, backlight, lid and dock transitions that a headless box
  never generates, and each makes the RDP server send ResetGraphics. A green sweep across all four
  hosts is therefore not coverage of that class at all — MDR-BUG-FLUX-00008 was invisible to every
  test we could have written here while a real laptop sat black. MDR-BUG-FLUX-00015.

- A disconnected Windows session enumerates NO displays, so rhydra's agent reports a healthy IDD device as gone (`agent` device rung).
  `query session` is the check: if the console is a different session id from the one the agent runs
  in, `EnumDisplayDevices` returns nothing and the ladder says `device FAIL` with the driver present
  and OK in Device Manager. The remedy is `tscon` (the rig's `mdrdp-tocon` task), NOT `cycle-device`
  — which rebuilds the display and drops every session on the host for no reason. The real clue sits
  under `input-desktop`, two rungs BELOW the symptom. MDR-BUG-FLUX-00017.

- A Windows clipboard lock does NOT persist on quench: a 30 s hold ends with 0x8007058A (`tools/clipboard-hold`).
  Three runs, two implementations (PowerShell P/Invoke, native sleep, native with a message pump) all
  report `opened=true`, `held_secs=30.00`, `closed=false — thread does not have a clipboard open`,
  while the process under test writes happily throughout. Something on that host breaks the lock, so
  a test that needs `OpenClipboard` to be refused CANNOT be staged there. Make any clipboard-holder
  report its own `CloseClipboard` result: one that only prints "held for 30s" hides the fact that it
  held nothing, and the run reads as a pass.

- Never hold a lock across an OS clipboard call — a stuck write freezes the other direction too (`clipboard::apply_remote`).
  `EmptyClipboard` SENDS `WM_DESTROYCLIPBOARD` to a hung previous owner, so a write can block for
  arbitrarily long. If the reader holds the shared clipboard handle *or* the bridge mutex while it
  does, the 250 ms poll stalls behind it and a one-direction wedge becomes both. Give each thread its
  own clipboard handle (the OS then serialises with bounded-retry `OpenClipboard`, which fails fast),
  and take the bridge lock only to DECIDE, never across the write.

- Record clipboard content as applied only AFTER the write succeeds, never when deciding (`Bridge::on_remote_text`).
  Recording at the decision means that while a write is stuck the bridge believes the clipboard holds
  the new text while it still holds the old — so the poll reads the OLD content, calls it a change,
  and sends it back as though the user had copied it, clobbering what they actually copied.

- `check-windows.sh` checked mdrdp only, so rhydra's whole `host` half went untype-checked (`scripts/check-windows.sh`).
  mdrdp takes rhydra with `default-features = false`, so a `cargo check` from the repo root never
  compiles `tools/latency-spike/server/src/win/**` — DXGI duplication, the MF encoder, the SendInput
  injector, the Win32 clipboard. A new module whose FIRST import was wrong passed the script and
  failed a direct check of the same target. Fixed in v0.1.86 to run both; if you trusted a green run
  after touching `win/` before that, it proved nothing about that code.

- A condvar wake-up test passes with the notify deleted unless it asserts elapsed time (`native::auxchan::Slot`).
  A taker parked on `wait_timeout` reaches the right answer anyway when the timeout fires — so
  "assert it eventually returns Closed" is green whether or not `close()` ever notified. The only
  oracle that separates "woken" from "timed out" is the clock. Make the park interval a constructor
  parameter, set it far beyond the test's patience, and assert the taker returned inside a fraction
  of it. The same trap hides any wake-up built on a polling fallback: the fallback is the bug's alibi.

- Echo suppression needs the last content seen from EITHER end, never the last applied (`native::clipboard::Bridge`).
  Keeping the last wire-applied fingerprint and refusing to send anything matching it loses a copy
  silently and permanently: apply X, copy Y (sent), copy X again — X matches, is suppressed, and the
  peer still holds Y. A last-*sent* slot has the identical bug. Fingerprint the canonical (LF) form,
  not local bytes, or a Mac's `a\nb` and a Windows box's `a\r\nb` are different forever and multi-line
  copies ping-pong at poll cadence. After applying, seed the slot from what the OS actually holds.

- A health gate must treat "I could not tell" as neither health nor failure (`control::stuck_from_rungs`).
  Deriving a connect gate from "first rung that is not Ok" makes a transiently unreadable section
  refuse a session that would have worked; deriving it from "first rung that is Fail" lets a partial
  report manufacture health. rhydra needs both answers, so it has two functions:
  `stuck_from_rungs` (Fail only — may a client connect?) and `first_unsatisfied_rung` (counts
  Unknown and absent — what should a human look at?). Conflating them costs real sessions either
  way round.

- The IDD section's `frame_seq` is a driver-written present counter readable by ANY process, with no viewer (`idd_section.rs:184`).
  This refuted a whole design: the wedge detector does not need to live in the client, because the
  agent can open `Global\mdrdp-idd` read-only every tick and see whether pixels are moving. Agent-side
  detection then survives client death, works between sessions, and lets `--doctor` answer instead of
  reporting "untested". Generation 0 is the driver's explicit "no pool" (`SharedPool.cpp` AdvertiseNoPool).

- Restarting rhydra's capture server against pool generation 0 turns a silent wedge into a 30 s crash loop.
  A *running* server at generation 0 loops on timeout forever; a *fresh* one refuses it
  (`idd_source.rs` wait_for_pool) and dies into doubling backoff. The remedy for "no pool" is
  restarting the CREATOR, which owns the device's lifetime and makes the driver republish. Cause-specific
  remediation matters here because the wrong remedy is worse than none.

- `with_resizable(false)` blocks only the USER: `request_inner_size` still resizes, so a fixed dialog can follow its content (`egui_host::resize_to`).
  Verified live on macOS 2026-08-19 — the Session-lost dialog opens at its measured
  closed height and grows when the Technical details disclosure opens. Two things make
  it behave: hold the disclosure's open state yourself rather than in egui's memory (so
  the dialog stays a pure function of what it is told, and a test can render both
  states), and set `animation_time = 0` for it, or the window is dragged through a
  dozen intermediate heights on the way open.

- Installing the OpenSSH capability REWRITES any firewall rule sharing its name back to Private; give ours its own (`hostscripts::setup_ssh`).
  Two traps, one symptom. Windows' built-in `OpenSSH-Server-In-TCP` is Private-profile only,
  and these hosts sit on a network Windows categorises Public, so it never applies — SSH
  connects time out while sshd runs and listens perfectly, which reads as a network fault.
  Worse, adding a correct Profile-Any rule under that same built-in name is silently undone
  the next time the capability is installed. Hence `mdrdp-sshd-in`, added AFTER the install.
  Diagnose from the client: timeout = firewall drop, refused = sshd absent. They are opposite
  fixes (`sshsetup::classify`).

- PowerShell 7 fails DISM with "Class not registered" and returns SILENT EMPTIES from Get-Net* — self-elevate to 5.1 (`hostscripts::setup_ssh`).
  Not a broken servicing stack, though it looks exactly like one. On temper the same
  `Add-WindowsCapability` that failed under pwsh 7 succeeded via `DISM.exe`, and
  `Get-NetTCPConnection -LocalPort 22` returned nothing while sshd was listening on
  0.0.0.0:22 — so the empty output read as "sshd is dead" and cost a wrong diagnosis.
  `Start-Process powershell -Verb RunAs` gets a 5.1 child from whatever shell was pasted
  into, which is why every host script here self-elevates.

- Windows OpenSSH ignores `~/.ssh/authorized_keys` for ANY admin; only `administrators_authorized_keys` counts (`hostscripts::setup_ssh`).
  Machine-wide, so one key there serves every admin account on the box — `marti` and `ano`
  both authenticate on quench from one entry. It also needs its ACL stripped to SYSTEM +
  Administrators or sshd refuses the file outright, silently.

- A fixed-size dialog holding server-supplied text loses its own buttons; measure the layout headlessly first (`end_dialog::window_size`).
  Aux windows are `with_resizable(false)`, so overflow is not a scrollbar — it is content
  drawn past an edge nobody can move. An IronRDP failure chain wraps to ten lines and pushed
  the whole footer off the 300 px Session-lost dialog. Two halves to the fix: run one headless
  `egui::Context::run_ui` pass at the fixed width and open at the height the content used, and
  cap the unbounded part so the total cannot run off a display. A fit test only proves this if
  it clips each galley to its own clip rect — otherwise a scroll area reads as overflow.

- A locked Windows console eats injected input: SendInput succeeds, thread still reads `WinSta0\Default`, nothing lands.
  A locked session's *input* desktop is the secure `Winlogon` one, so every diagnostic you can
  reach from a service or SSH agrees the injector is healthy — station, desktop, integrity level,
  session id, and the return value all look right. The tell is `tasklist /FI "IMAGENAME eq
  LogonUI.exe"` naming your session. On quench the rig's own `mdrdp-tocon` task
  (`tscon 2 /dest:console`) clears it. Cost a full session's diagnosis, ending on the wrong
  hypothesis (that headless IddCx cannot accept SendInput at all).

- `FrameSource::origin` defaults to (0,0); only dxgi overrode it, so IDD clicks missed by the display's offset (`win/idd_source.rs`).
  The IDD pool header carries no desktop coordinates, so the source cannot learn its own placement
  the way duplication learns it from `DXGI_OUTPUT_DESC::DesktopCoordinates` — GDI's `DEVMODEW::
  dmPosition` is the only route (`agent_ops::idd_display_origin`). Latent for as long as the
  virtual display is the only display; attaching a console pushed it to (1920,0) and every click
  landed 1920 px to its left, silently, because SendInput succeeds either way and the keyboard
  needs no coordinates. Typing works, the first click steals focus, everything after it vanishes.

- An RDP logon to quench's console recreates the IDD display and strands the capture server on a dead pool.
  The display renumbers (`\\.\DISPLAY12` → `15` → `16`) and the server keeps its old
  `Global\mdrdp-idd` generation, decoding nothing while looking healthy. Recovery is three ordered
  steps: `mdrdp-cycle-idd`; kill `mdrdp-idd-create.exe` so the agent respawns it and republishes the
  section (the device cycle destroys it — the server then logs `waiting up to 30s for
  Global\mdrdp-idd` and refuses connects with "the video server is not accepting yet"); restart
  `rhydra-server.exe`.

- rhydra sends a new viewer NOTHING until the desktop changes: the connect-edge keyframe request has nothing to encode (`win/pipeline.rs` capture_loop).
  `request_keyframe()` fires, then `acquire()` times out on a static desktop, so no AU is ever
  produced. Idle 1 s reconnects decoded 1/1/0/0/4 on a confirmed-healthy pipeline; with the desktop
  changing, 5/5. Any "connects but blank" report needs the desktop's activity stated before it is
  a bug in anything else. MDR-BUG-FLUX-00011.

- Intel's MF encoder MFTs ACCEPT 4:4:4 profiles and silently emit 4:2:0 — read the SPS back, never trust SetOutputType (`tools/mf-caps-probe`).
  On quench (Core Ultra 7 270K Plus) Intel HEVC takes Main_444_8 + ARGB32 and emits profile 1 /
  chroma_format_idc 1; Intel VP9 ignores the profile attribute outright (byte-identical output).
  The silicon can do HEVC 4:4:4 — D3D12 Video Encode reports Main_444/Main10_444 with AYUV/Y410 —
  but not via the MFT. Also: session 0 enumerates hardware MFTs but ActivateObject fails E_FAIL;
  D3D12 CheckFeatureSupport works there. See the quench SPIKE doc of 2026-08-19.
- M5 Max VideoToolbox decodes H.264 AND HEVC 4:4:4 in HARDWARE at 5K; VP9 has no VT decoder at all (`tools/vt-caps-probe`).
  `VTIsHardwareDecodeSupported` answers per codec only; the probe feeds real 4:2:0/4:2:2/4:4:4
  streams to a hardware-required session and counts frames. AV1 is hardware for 4:2:0 only. The
  H.264 4096x2304 ceiling we clamp to is the Windows encoder's, not the Mac decoder's. Results in
  `wrk_docs/2026.08.19 - SPIKE - VideoToolbox chroma and codec decode matrix on M5 Max.md`.
- A metric must encode the failure, not the change: B-spike count ROSE after fixing the blue flash; blue-flip count hit 0 (`avcreplay`).
  Naive "pixel moved for one frame" counts the fix's deliberate flat-average softening alongside
  the overshoot it removed (2,271 -> 3,603 while the defect went to zero). The decisive metric was
  hue inversion — blue-dominant for one frame on yellow-dominant content — which no correct
  rendering of the block's real colours can produce (MDR-BUG-FLUX-00010).
- Preserved chroma goes STALE when content changes under LC=1; paint flat avg until catch-up or reconstruction overshoots (`avc444::chroma_stale`).
  The FLUX-00007 preserve fix made gap frames worse: `4*new_avg - 3*stale` pushed U past any real
  hue (blue flash on yellow, wrong-colour window restores, up to ~1.4 s). Average-delta detects the
  change: re-encode noise <= 6, genuine change >= 41 (32 temper payloads) — threshold 10, no tuning.
- Windows AVC444 alternates LC=1/LC=2; a luma pass must PRESERVE delivered odd chroma or colours pump (`avc444::apply_luma`).
  FreeRDP replicates the luma frame's averaged chroma into all four 2x2 positions on every luma
  pass, wiping the aux samples — on dithered content that flips 33k/48k probe pixels by up to 98
  RGB units at every L↔C transition (measured, temper). The encoder re-sends chroma only when it
  changed. Repro/attribution recipe: `--capture-failures` + `examples/avcreplay.rs`.
- Suppress Output "allow" does NOT repaint: EGFX resumes only FUTURE deltas — reveal must send Refresh Rect (`session::visibility_pdus`).
  A session that connects occluded stays black forever: the logon-black connect burst is all it
  ever painted, the desktop appears server-side while suppressed (never sent), and a static desktop
  produces no future deltas. Kiln showed this as a healthy connection with a permanent black
  window, frames frozen at 80. Temper/crucible masked it because their desktops change constantly.
  MS-RDPBCGR 2.2.11.2 Refresh Rect after the allow is what mstsc does. MDR-BUG-FLUX-00006.

- softbuffer's CG backend colour-converts the WHOLE frame on CPU per present; IOSurface contents skip it (`present.rs`).
  An idle 1440p session burned 20–90% of a core: CoreAnimation re-renders every data-provider
  CGImage through a vImage ColorSync pass (~20 ms/frame at 2560x1440), plus softbuffer zero-allocs
  the buffer each frame. `present::LayerPresenter` hands CA an IOSurface instead — conversion moves
  to the GPU. Never rewrite the surface currently on glass: CA can short-circuit a `setContents`
  naming the object it already shows, so in-place writes silently stop updating the screen (hence
  the pool of three). `MDRDP_PRESENT=soft` forces the old path for A/B. MDR-BUG-FLUX-00005.

- spike-server records ZERO frames until a viewer connects (`win/pipeline.rs:609` — by design); a viewer-less smoke proves nothing.
  A whole afternoon's "dead capture" diagnosis on quench was this: the capture loop parks in 50 ms
  sleeps until the video port accepts a client. Arm it headlessly with an SSH tunnel plus
  `nc -d 127.0.0.1 9500 > /dev/null` (without `-d`, nc closes on stdin EOF and the server disarms).
  Two more rig traps stacked on top: schtasks context cannot `SetForegroundWindow` (SendKeys stimulus
  silently misses — post `WM_CHAR` to the hwnd instead), and `schtasks /end` kills only the cmd
  wrapper, orphaning `spike-server-inc3.exe` on ports 9500/9501 (`taskkill /im` it). Also: the IDD
  device lives only while `mdrdp-idd-create.exe --wait` runs — a reboot removes the device entirely;
  quench task `mdrdp-idd-create` recreates it.
- Windows monitor power-off silently poisons DXGI duplication: frames stay "successful", dirty metadata stays valid, pixels go black (`win/dxgi.rs:read_rects`).
  No error is ever raised, so AccessLost recreation never fires; GDI CopyFromScreen still sees the real
  desktop, which is the diagnostic. Restart the duplication session (i.e. the spike server) after any
  display power transition. quench now runs `powercfg /change monitor-timeout-ac 0`, and waking a
  blanked display needs SetThreadExecutionState from INSIDE the interactive session (task `mdrdp-wake`)
  — injected keyboard/mouse input does not relight it. Cost: a 76%-timeout glass run and ~an hour, all
  of which looked exactly like a latency regression.
- A CGEvent built with a NULL source is silently dropped by CGEventPostToPid; create it from a HIDSystemState source (`glass::inject::Injector`).
  Cost two burned quench sessions: python ctypes CGEventCreateKeyboardEvent(None, …) posted "successfully"
  (permission preflight true, no error) yet the target app never saw a keystroke. Same call with
  CGEventSourceCreate(kCGEventSourceStateHIDSystemState) delivered every tap.
- winit macOS folds BOTH ISO corner keys into Backquote and never emits IntlBackslash; split on the unmodified char (`input::backquote_scancode`).
  winit 0.30 `platform_impl/macos/event.rs` maps kVK_ISO_Section (0x0A) and kVK_ANSI_Grave (0x32) to
  `KeyCode::Backquote`, so the 102nd key next to left Shift sent scancode 0x29 and typed ` where a UK PC
  layout has \. `key_without_modifiers` is computed from the raw keycode before the collapse, so the two
  keys still differ there (§/± = top-left, `/~/\/|/</> = 102nd on ISO hardware per KBGetLayoutType).
  Validated live on quench: keycode 50 now types \, keycode 10 types `, matching Windows App 11.3.8.

A lesson is **never dropped to make room** for a new one — prepend it and let the oldest
fall out of the injected window. Soft target ~25 entries; past ~40, say it is due a prune
rather than pruning unasked. Durable project facts and conventions belong in `CLAUDE.md`,
not here.

---

- A UMDF driver installs under Secure Boot with cert trust alone (Root+TrustedPublisher) — no testsigning, no reboot (idd/deploy.ps1).
  `bcdedit /set testsigning on` is refused outright under Secure Boot, but that gate
  only exists for kernel code integrity: pnputil accepted the self-signed-and-trusted
  mdrdp-idd package and WUDFHost loaded it. `tools/latency-spike/idd/install-notest.ps1`
  on quench is the working runbook; signtool comes from the
  `Microsoft.Windows.SDK.BuildTools` NuGet, not the WDK one.
- Windows SSH sessions see no displays: user32 enumerates empty and DXGI duplication cannot run — use tscon + an Interactive scheduled task.
  sshd sessions are non-interactive (though fully elevated for admins). The working
  pattern on quench: `tscon <id> /dest:console` to put the user's session on the
  unlocked console, then run display-facing work via Register-ScheduledTask with
  `-LogonType Interactive` so it executes inside that session. Also: PowerShell `$null`
  as EnumDisplayDevices' first arg marshals wrongly — pass `[NullString]::Value`.

- Focus-routed CGEvent injection silently loses keystrokes to focus theft: pin delivery with `probe glass --target-pid` (CGEventPostToPid).
  On a busy desktop 25/30 glass trials timed out because another app took frontmost
  mid-run and the HID-tap events followed it. Posting to the target pid makes
  misrouting impossible; the window still has to be visible and unoccluded
  (`probe::glass::inject::Injector`).
- ScreenCaptureKit sees ZERO displays at the macOS lock screen, and inter-run pauses let the display sleep: glass runs now hold an IOPM display-wake assertion.
  Injected CGEvents reset the idle timer during a run, but the gaps between runs do
  not; the display slept and locked between sessions and ended the whole measurement
  phase (`probe::glass::wakelock::DisplayWake`, wrk_journals 2026.08.17).

- Accumulating macOS's ~0.1-line slow wheel clicks still reads as DEAD: floor each LineDelta detent at one notch (`input::detent_floor`).
  Second attempt at the slow-scroll bug. The 0.1.48 `WheelAccumulator` stopped *dropping*
  sub-notch travel, but macOS's acceleration curve reports a slowly-turned click as ~0.1
  of a line, so accumulation charged ten physical clicks per remote notch — correct
  arithmetic on the wrong model. A `LineDelta` event is a physical detent (winit emits it
  only for non-precise devices) and is owed a whole notch, as native Windows gives; ≥1-line
  values pass through so a fast spin keeps its acceleration and fractions. Precise pixel
  streams keep pure accumulation. iTerm2 and Chromium floor non-precise scrolls the same way.

- IronRDP AND FreeRDP 3.27 stop at ERRINFO 0x18, so a rebooting host's 0x1A failed the whole PDU decode (`disconnect`, `server_error_info`).
  MS-RDPBCGR 2.2.5.1.1 defines ERRINFO_SERVER_SHUTDOWN (0x19) and ERRINFO_SERVER_REBOOT
  (0x1A); both reference clients are missing them, so a missing code is not evidence the
  spec lacks one — check the spec, not another client. Worse than the gap was the shape of
  the failure: `ServerSetErrorInfoPdu::decode` rejected unknown codes, so the one PDU that
  says *why* a session is ending was turned into a parse error. Any decoder for an
  explanatory field should carry what it does not recognise.

- Advertise DPI scale in the GCC at connect: a mid-session RDPEDISP scale change DWM-blurs non-DPI-aware apps (`display::primary_display`).
  Windows treats a Display Control scale change like a monitor DPI change: per-monitor-
  DPI-aware processes re-render sharp, everything else is bitmap-stretched by DWM until
  relaunched — server-side blur no client can undo. So a session that will open fullscreen
  must connect at its planned scale, not renegotiate after logon. winit cannot supply the
  monitor pre-connect (monitors are only enumerable inside the running loop, whose single
  Resumed creates the session window), hence the CoreGraphics probe.

- A sub-notch RDP wheel PDU scrolls nothing: accumulate macOS's fractional lines/pixels into whole 120s (`input::WheelAccumulator`).
  Windows apps divide arriving rotation by WHEEL_DELTA, so anything under 120 units is
  discarded outright. macOS reports slow wheel travel as fractional *lines* (acceleration)
  and trackpads as pixels, so translating each winit event on its own sent a stream of
  sub-notch PDUs the remote threw away — slow scrolling did nothing until a fast flick
  crossed the threshold and then jumped. Holding the remainder per axis is the fix; the
  same event stream also proved that emitting one PDU per notch beats one clamped PDU,
  since 3 lines is 360 units and the 9-bit wire field silently truncates it to 255.

- DWMFRAMEINTERVAL=15 on the host doubles sustained RDP fps and halves motion RTT; below 15 is dead weight (wrk_journals 2026.08.17).
  The session's 60Hz virtual display floors the cadence at ~16ms — measured at DWMFI=5, no
  gain over 15. Typing keeps a ~33ms server floor either way.

- An AVC444 session sends ZERO SurfaceToCache/CacheToSurface PDUs — a quiet cache is the server, not a client bug (`gfx::apply_surface_to_cache`).
  Measured live on quench 2026-08-17: hits/misses/entries all 0 while 117 MB of Avc444v2
  painted. Full-frame H.264 carries everything, so the EGFX bitmap cache sits idle and
  `hit_rate()` correctly returns None (no `· cache %` title segment, empty diag grid).
  The sibling symptom — no codec name in the title — WAS a client bug: `on_bitmap_updated`
  never attributed painted bytes (fixed, see `gfx.rs` regression test).

- A non-resizable egui window needs a two-sided fit test; the 440px About dialog already overflowed unseen (`ui::help` tests).
  Aux windows open at a fixed size, so content that grows clips the Close button off the
  bottom and content that shrinks strands the chrome strip in mid-air. Laying out one real
  frame with `Context::run_ui` at exactly the window size and asserting every text rect sits
  inside it — and that the lowest one is within 30px of the bottom — caught a Vendored row
  that had been running 35px past the launcher's dialog edge since the dialog shipped.

- Typing latency to quench is ~30ms p50 and lives on the SERVER: client decode is 1.3ms, present 2ms (`stats::SessionStats`).
  The metrics `decode`/`present` split is what proves it. Quench's encoder ticks at ~60Hz
  but sustains ~30fps (the remote-session cap), and Windows App runs plain TCP on this LAN —
  no UDP advantage to chase (`stats::SessionStats::frame_gap`).

- A cross-target guard nobody can run is a guard that never ran — "verified clean" goes stale silently (`scripts/check-windows.sh`).
  The msvc check's "verified clean" date predated the dependencies that break it.

- An AVC server DIES rather than falls back past H.264's 4096x2304 ceiling; clamp requests (`session::clamp_to_encodable`).
  Past the ceiling `session::fullscreen_request` integer-fits (5K → 2560x1440@100), because
  the fractional clamp's 1.25x nearest stretch shreds glyphs.

- winit permits ONE EventLoop per process; `run_app_on_demand` reuses it, but every re-entry has traps (`ui::end_dialog`).
  `EventLoopBuilder::build` sets a process-global `EVENT_LOOP_CREATED` flag and every later
  build returns `EventLoopError::RecreationAttempt` (reset only on web), so a launcher window
  followed by a session window is two loops and fails at the worst possible moment — right
  after the user picks a favourite. `run_app_on_demand` takes `&mut self` and is documented
  for exactly this (macOS and Windows; not iOS or web). On re-entry: the loop parks until an
  event arrives, so prime it with one proxy user event (`ui::end_dialog::show`); `Resumed`
  fires once per process, not per cycle, so late windows are created in
  `new_events`/`about_to_wait`; and a closed window's queued `Destroyed` can land in the NEXT
  cycle, so match window ids before acting.

- macOS Cmd+Q exits the process inside winit's `run()`; clean-up must live in `ApplicationHandler::exiting` (`window::SessionWindow::on_exit`).
  winit installs a default menu whose Quit item is bound to `terminate:`, and it does not
  implement `applicationShouldTerminate:`, so AppKit takes NSTerminateNow. `run_app` never
  returns and every statement after it — including an RDP disconnect — is skipped, along
  with all destructors. winit does emit `LoopExiting` first, so `exiting()` is the only place
  left to disconnect. That hook runs on EVERY close, not just Cmd+Q, so epilogue state rides
  a shared slot (`main`). Beware the follow-on deadlock: a worker thread posting to the
  event-loop proxy while the main thread joins that worker inside `exiting()` hangs.

- EGFX advertises codecs by capability VERSION, and Windows strips AVC420_ENABLED unless the host enables H.264 (`gfx::capabilities`).
  So per-codec settings toggles have nothing honest to bind to. Advertising V8.1 + AVC420
  (the only version that can say "420 but not 444") negotiates cleanly, but quench confirmed
  avc420=false and kept sending ClearCodec/Progressive — same posture temper showed. The
  client side can be ready and provably negotiating; the server will not send H.264 until
  "Prioritize H.264/AVC 444 graphics mode" (or hardware encode) is enabled host-side. Test
  that policy first before debugging the client.

- Windows sends EGFX AVC as ANNEX B, not AVCC; VideoToolbox needs length-prefixed samples and 'f420' output (`h264::nal_units`).
  Captured quench Avc444v2 payloads are `00 00 00 01` + AUD/SPS/PPS/slices; nobody noticed
  upstream because ffmpeg/openh264 eat Annex B natively, and the first live run failed
  974/976 frames on exactly this. Separately measured with lossless fixtures
  (`h264::videotoolbox`): VT normalises output to the REQUESTED pixel format's range,
  ignoring stream VUI, so 'y420' (video range) squeezes full-range samples into 16-235
  irreversibly whichever range the stream used — 'f420' is the only correct planar choice.
  Also: VT always applies SPS cropping (1920x1088 coded → 1080 rows out), and a session
  rebuilt mid-GOP fails every P-frame until the next IDR, so rebuilds must never be routine
  (one output format for every decode path).

- Windows keys the ClearCodec glyph cache by CONTENT, not dims: equal-area hits arrive reshaped, 1x6 as 2x3 (`clearcodec::decode_over`).
  Measured live during window drag/resize: 24 glyph hits in 90 s, every one an exact area
  match at a different shape. FreeRDP only requires the cached bytes to cover the
  destination pixel count and reinterprets at the destination stride. A strict width/height
  equality check rejected all 24, each one leaving a stale rectangle behind — the
  drag/resize corruption. Dimensions on a glyph entry are advisory; area is the contract.

- Windows throttles and frame-skips EGFX for a client that ignores Bandwidth Measure probes (`vendor/ironrdp-session/src/x224/mod.rs`).
  ironrdp-session 0.11 answers RTT auto-detect requests but drops Bandwidth Measure
  Start/Stop as "not yet implemented". Quench sent 140 unanswered pairs in a 40 s session
  and paced graphics down to ~2 fps with 1–4 s input-to-paint latency, skipping frame ids
  mid-repaint — which then leaves whatever was on screen stale forever. Every counter reads
  healthy: zero decode errors, instant acks, 100% cache hits. The signature is server frame
  ids that SKIP plus multi-second silences after bursts.

- EGFX codec state dies with the SURFACE, not with ResetGraphics — resetting decoders there breaks V-bar hits (`gfx::on_reset_graphics`).
  Measured both ways on quench: resetting the ClearCodec decoder at ResetGraphics produced
  74 "V-bar cache miss on hit" failures on the next repaint (Windows keeps referencing
  pre-reset V-bars), while NOT dropping a deleted surface's progressive tile state lets a
  same-id same-dims recreate refine the old surface's pixels. So: persist ClearCodec caches,
  surfaces, and the bitmap cache across resets; drop progressive state in
  on_surface_deleted (FreeRDP: progressive_delete_surface_context). Do not copy FreeRDP's
  codecs_reset-at-ResetGraphics wholesale.

- Windows drops an EDISP monitor layout sent before its caps PDU; gate on `DisplayControlClient::ready` (`session::service_resize`).
  The Display Control channel being open is not the same state as capabilities having
  arrived, and `ActiveStage::encode_resize` checks only the former — it happily encodes a
  layout the server then ignores without any error. The first live fullscreen run sent the
  layout instantly and the session silently stayed at 1920x1080. Also observed: Windows can
  complete a resolution change entirely through EGFX ResetGraphics + new surfaces, without
  any DeactivateAll — do not assume the reactivation path is the only success signature.

- Measure each stage separately: success-only counters, reasonless failure tallies and blob spans all mislead (`audio::AudioStats`, `stagelog`).
  `current_format` is set when a wave plays, so a silent-but-healthy session left it `None`
  and the report printed "server negotiated no format" — blaming a stage it had never
  measured, and hiding the difference between "no audio channel opened" and "nothing was
  playing". "179 tiles failed to decode" was unactionable until tallied by reason: 84
  primary parse failures starving the v-bar and glyph caches caused 92 cascade failures —
  one bug, not four (`gfx::GfxStats::decode_error_reasons`). And one span covering CredSSP,
  MCS, licensing, capability exchange and finalization made "CredSSP is 84% of connect"
  arithmetic dressed as evidence; the real split is CredSSP ~6.5 ms median,
  ConnectionFinalization ~29 ms. Prefer reading a dependency's own instrumentation over
  reimplementing its loop to time it.

- Windows gates AUDIO_PLAYBACK_DVC on RDPDR presence, then reopens it — needs a no-device backend and a DVC listener (`connect::establish`).
  Quench suppressed every audio DVC for mdrdp and the current upstream IronRDP viewer,
  despite both advertising playback and joining static RDPSND; protocol-correct RDPDR with a
  no-op backend made Windows offer the playback DVC immediately, with no drive, printer,
  port or smart-card capability advertised. Then the reopen: Windows opens the DVC during
  setup, closes it, and opens a fresh instance, but `attach_dynamic_channel` consumes its
  processor on the first create so the second gets NO_LISTENER. A repeatable
  `DvcChannelListener` must create a fresh RDPSND state machine over the shared playback
  ring for every open (`audio::DynamicRdpsndListener`).

- CLIPRDR polling can beat Monitor Ready; defer and serialize FormatList PDUs (`clipboard::advertise`).
  The local 250 ms poll started before the server's initialization request and sent a
  FormatList that Windows silently ignored. Later rapid changes could also race multiple
  lists while one acknowledgement was outstanding. Holding the current content until
  Monitor Ready, then coalescing changes behind the one in-flight exchange, made the latest
  clipboard value deterministic on Quench.

- A codec no-op can report success while leaving stale pixels; diff output (`clearcodec::decode_subcodec_region`).
  ClearCodec's NSCodec branch parsed the region and returned `Ok(())` without painting it,
  so every health counter was zero while horizontal bands stayed visibly stale. Splitting
  one live command by layer and comparing each decoded buffer with FreeRDP isolated the
  subcodec layer, then proved the completed decoder byte-for-byte.

- ironrdp ships decoders it never wires up: ClearCodec and RFX Progressive live in `ironrdp-graphics` (`gfx::apply_wire_to_surface2`).
  `ironrdp-graphics::progressive::ProgressiveDecoder` is complete, with a doc comment
  showing the exact `WireToSurface2Pdu` call, and `ironrdp-pdu::codecs::rfx::progressive`
  parses the block stream. Before assuming a codec is "our work", grep the graphics crate
  — the decode may already exist and need only a seam.

- `enable_audio_playback: false` SETS `INFO_NOAUDIOPLAYBACK`, and upstream's field doc states the polarity backwards (`connect::establish`).
  MS-RDPBCGR defines that flag as "audio redirection MUST NOT take place". Leaving the
  field false while registering RDPSND joins the channel and then tells the server never
  to use it: the server obliges, no Wave PDU ever arrives, and audio looks wired up while
  producing permanent silence.

- CLIPRDR and RDPSND are *static* channels — register before the MCS join or never (`connect::Channels`).
  There is no API to add a static virtual channel to a live session, so the decision to
  support clipboard or audio has to be made before connecting. This is why the audio device
  is opened *before* the connection: if no output device exists we must not join the channel
  at all, since joining and then discarding every wave gives the server every reason to
  believe audio is working while the user hears silence.

- An assertion that cannot tell failure from success is not a test: non-black pixels, n=100 percentiles (`stats::percentiles`).
  A list widget fills each row with a background colour, so "non-zero pixel count > N"
  passes a full buffer even when not one glyph was rendered — the oracle has to match the
  *text* colour specifically (`launcher.rs` tests). And nearest-rank vs truncating percentiles differ only when
  `p × n` is fractional: with n=100 every interesting percentile (0.50, 0.95, 0.99) lands on
  an integer, so both definitions agree and the test passes over a wrong implementation —
  use n=7 or 13. Found by mutation testing, not by review. Same family as the
  all-fields-identical fixture trap: any fixture whose size makes the boundary case
  disappear is a test that cannot fail.

- macOS keychain ACLs bind to the exact binary, so every rebuild re-prompts — code signing is functional, not cosmetic (`creds`).
  "Always allow" grants access to a binary hash that the next `cargo build` invalidates.
  `-A` on the item bypasses ACLs for development. The real fix is a stable code signature,
  and it matters for the product: a launcher that spawns a process per session would
  prompt the user on every single launch without one.

- `ironrdp-tls` accepts every certificate — `NoCertificateVerification` in its rustls backend (`crate::trust`).
  Not a bug in the library: it cannot know the caller's trust policy. But it means a
  client using it never detects a man-in-the-middle, and the failure is silent. We
  replaced it with trust-on-first-use pinning; only the chain check is replaced, TLS
  signature verification still runs the provider's real algorithms.

- `set_read_timeout` bounds one syscall, not one message — a trickling peer defeats it (`probe::wire::read_tpkt`).
  A peer that declares a large length then sends a byte every 200 ms keeps `read_exact`
  blocked indefinitely while every individual read returns inside the timeout. The fix is
  a wall-clock deadline checked across the whole read, plus a cap on the declared length.
  Untestable while it lived in a binary; moving it into the library is what let a real
  adversarial-socket test exist at all.
