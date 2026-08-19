<!-- lessons-format: index-v1 -->
# Lessons learnt

Newest at the top. The **first line of each entry is the lesson** — self-contained, with a
`file:symbol` pointer, under ~120 characters. Only first lines are injected at session
start, so a line that needs the detail below it to make sense is a line that will not work.
Indented lines below the first are detail: kept for lookup, never injected.

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
