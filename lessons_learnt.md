<!-- lessons-format: index-v1 -->
# Lessons learnt

Newest at the top. The **first line of each entry is the lesson** — self-contained, with a
`file:symbol` pointer, under ~120 characters. Only first lines are injected at session
start, so a line that needs the detail below it to make sense is a line that will not work.
Indented lines below the first are detail: kept for lookup, never injected.

- Share one absolute outbound deadline per batch; reset after inbound waits (`connect.rs:write_framed`).
  Per-syscall timeouts let a dribbling peer monopolize the session. A partial expiry is terminal
  because retrying another RDP frame would make the byte stream ambiguous.

- Cancel pending present samples on both occlusion edges (`window.rs:Occluded`, `stats.rs:cancel_pending_present`).
  Suppress Output is asynchronous, so late hidden paints can turn the reveal redraw into fake renderer latency.

- Partial framed reads need nonblocking pump slices, not socket timeouts (`session.rs:with_nonblocking_framed_read`).
  `Framed` and rustls may perform several socket reads for one PDU, and a trickling peer resets a
  per-call timeout. Preserve their partial buffers, yield on `WouldBlock`, then restore blocking
  mode before the shared stream can write.

- Damage cadence must read current store dimensions, not the old snapshot (`window.rs:damage_state`).
  A large frame can remain in the reusable copy while a small replacement is already presentable;
  sizing the throttle from that stale copy delays the cheap redraw by the full cadence interval.

- A recreated surface ID invalidates its old geometry until remapped (`surface.rs:output_mapping_stale`).
  Retain the last pixels and mapping atomically, but keep the numeric ID association so delete and
  EndFrame can retire the fallback correctly before the replacement receives a fresh MapSurface PDU.

- Union unaligned AVC444 rect coverage within one PDU before preserving detail (`avc444.rs:mark_chroma_seen`).
  A per-rectangle full-block test loses valid samples when adjacent regions split a 2x2 block.
  Keep partial coverage PDU-local so a later partial frame cannot falsely clear stale chroma.

- Drive activation states with no PDU hint; they emit output, not stalls (`session.rs:drive_reactivation`).
  Connection finalization sends Synchronize and control PDUs through `step_no_input`. During the
  later server wait, poll the input doorbell and keep the session thread as the sole socket writer.

- Arm latency clocks before blocking delivery and roll back failed sends (`session.rs:drain_input`).
  Starting after flush hides encoding and socket backpressure from input-to-paint latency. Preserve
  the earliest unanswered sample, and restore it if encoding or delivery fails.

- Carry EGFX geometry with raw snapshots; scale outside the store lock (`surface.rs:PresentationMapping`).
  Pair fallback pixels with their mapping so a partial replacement cannot move or rescale old
  pixels. A second composed output canvas would add about 56 MiB at 5K and hold up decoding.

- Preflight IOSurface ownership before 5K frame work, then recheck at present (`present.rs:can_start_frame`).
  Compositor ownership can change after any read, so the preflight may only defer work; it must
  never reserve or authorize a surface. A resized or empty pool must stay ready for lazy rebuild.

- Clear a terminally deleted output at the frame boundary (`surface.rs:commit_frame`).
  Retain the last-good desktop during an active frame, but publish Empty if that frame commits
  without a mapped surface. Keeping the fallback then leaves destroyed pixels visible forever.

- Bound total live surface pixels before callbacks allocate (`client.rs:handle_create_surface`).
  A valid u16 width and height can still request gigabytes, and many individually bounded surface
  IDs can do the same in aggregate. Account same-ID replacement and deletion inside the client.

- AVC region structs are inclusive but stream masks are exclusive (`pdu/avc.rs:Avc420Region::to_rectangle`).
  Convert right and bottom only when building the wire rectangle. Copying the stored bounds omits
  the final row and column from AVC420, both AVC444 substreams, and mixed-tile updates.

- Instrumentation must drop visibly before it blocks decode (`viewer/stats.rs:StatsLog::enqueue`).
  Put durable per-line writes on their own thread behind a bounded non-blocking queue. If storage
  cannot keep up, emit an explicit loss count when it recovers instead of distorting the measured path.

- LC2-before-LC1 needs an explicit average-valid bit (`avc444.rs:Yuv444Buffer::luma_avg_seen`).
  Neutral plane initialization is not an aux-confirmed chroma average. Keep validity separate so
  the first real luma average establishes the baseline instead of suppressing valid chroma detail.

- A short/timed-out frameless write must close the link before another record (`viewer/input_link.rs:InputLink::send`).
  `Write::write` may send only a prefix or time out. Retrying a later input on the same
  stream corrupts framing, so fail closed and let disconnect release held input.

- A dropped muda menu can stay installed in AppKit; detach first (`window.rs:SessionMenuBar::detach`).
  `muda::Menu::drop` frees its Rust child bookkeeping but does not clear `NSApp.mainMenu`.
  Remove the menu explicitly while its items are live, including a drop fallback for error paths.

- Sparse auxiliary writes need explicit coverage, not seeded-pixel differences (`clearcodec::decode_over_with_coverage`).
  A decoded chroma or ClearCodec buffer starts with retained pixels, so comparing final colours
  cannot distinguish an explicit same-colour write from an untouched seed. Carry exact decoder
  coverage into `SurfaceStore`; use it for replacement readiness, caching, damage, and telemetry.

- A logical frame can retain one published snapshot instead of cloning every surface (`surface.rs:FrameState`).
  Keep writes private while a frame is active, publish once on matching EndFrame, and make abort
  terminal until a complete replacement arrives. This preserves atomic presentation without a
  multi-megabyte clone at every frame boundary.

- Bound decoded expansion, not just encoded input, before allocating (`zgfx::decompress_segment_with_limit`).
  A small compressed segment can expand far beyond its wire size. Pass the remaining decoded-output
  budget into every segment and history-copy path, and reject the segment before extending output.

- Input latency starts at the first wire write, not after the burst (`native/session.rs:write_native_records`).
  Timestamping after `write_all` excludes socket backpressure and makes a slow send look fast. Arm
  the measurement when the first byte is accepted, then close it when that input's painted frame
  arrives.

- Windows display topology belongs to CCD, not `ChangeDisplaySettingsExW` (`agent_ops.rs:make_idd_primary`).
  Quench accepted width/height/refresh changes through the legacy GDI mode API but returned
  `DISP_CHANGE_FAILED` for both complete and position-only physical-display requests. Preserve the
  active `QueryDisplayConfig` path/mode arrays, move only source positions, and submit them through
  `SetDisplayConfig`; verify full target state and re-check identities immediately before rollback.

- Measure live Windows scale from a PMv2 window, not the monitor factor (`agent_ops.rs:DpiProbeWindow`).
  On Quench, `GetScaleFactorForMonitor` returned 180% after the user selected 200%. A fresh hidden
  window placed on the target monitor receives the effective content DPI and can be queried through
  the supported `GetDpiForWindow` API. Treat failure to establish PMv2 as fatal; otherwise an unaware
  probe can confidently report 96 DPI / 100%.

- Visual exactness may suppress predictive pixels, never codec input (`native/session.rs:on_tile_au`).
  Raw rects can make an encoded tile visually redundant while its P-picture remains a required
  reference. Always decode and validate the tile AU, then suppress only its canvas blit and paint
  accounting. Dropping it before decode breaks VideoToolbox on the next dependent P-picture.

- Tiled frames need per-tile exactness; one global sequence suppresses later tiles (`native/session.rs:on_tile_au`).
  All encoders for one capture deliberately carry the same sequence. Gate each decoder chain independently,
  then advance the desktop's exact-through value to the minimum completed tile. Count the logical frame only
  when every tile for that sequence has painted, or FPS doubles and input latency closes on a half-frame.

- RDP's Remote Audio endpoint is USER-mode, but only protocol-provider sessions get one; the console never can (PLAN 2026.08.20 tranche 6).
  `MMDevAPI` loads an enumerator DLL (`GetTSAudioEndpointEnumeratorForSession`) only for sessions whose
  Remote Desktop protocol provider names one via `WTS_QUERY_AUDIOENUM_DLL`. No provider, no hook — so a
  console-owning host (rhydra, Sunshine) cannot borrow it and must supply a real endpoint instead: a
  signed virtual device (VB-Cable, Steam Streaming Speakers) or an attestation-signed driver of our own.

- Windows opens AUDIO_PLAYBACK_DVC on every RDP session but starts RDPSND only once the desktop plays sound (`audio.rs:session_summary`).
  So an idle remote desktop looks exactly like a server that refused audio: channel open, zero PDUs, no format
  exchange. Testing audio negotiation against a quiet desktop measures nothing. Make the session render sound
  first — a scheduled task registered `/ru <user> /it` runs inside the interactive session and works over ssh.
  This cost MDR-BUG-FLUX-00019: the epilogue printed "no audio channel was opened by the server" on sessions
  where the server had opened one twice, because it inferred channel state from an absent format exchange.
  FreeRDP is not a counter-example — its `[dynamic] Loaded mac backend for rdpsnd` fires in `rdpsnd_on_open`,
  at channel creation, not on receiving formats, so it proves nothing about negotiation.

- H.264 reference state belongs to an EGFX surface incarnation, not the channel (`client::h264_decoders`).
  A real display transition can overlap two surface lifetimes, so resetting one shared decoder
  on every numeric-id switch destroys both chains; same-id `CreateSurface` reuse has the opposite
  failure and retains stale references. Keep one lazy decoder per live surface, drop it on delete
  or id reuse, and retain the last painted output while its zero-filled replacement is unpainted.
  MDR-BUG-FLUX-00008.

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
