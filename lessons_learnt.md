- DWMFRAMEINTERVAL=15 on the host doubles sustained RDP fps and halves motion RTT, but typing keeps a ~33ms server floor (wrk_journals 2026.08.17).
- Quench is UK layout: autoinput `type` US scancodes turn `"` into `@` and `\` into `#`; PowerShell registry paths accept FORWARD slashes as the layout-safe escape (`autoinput`).
- An AVC444 session sends ZERO SurfaceToCache/CacheToSurface PDUs — a quiet cache display is the server, not a client bug (`gfx::apply_surface_to_cache`).
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
- Quench's encoder ticks at ~60Hz but sustains ~30fps (the remote-session cap), and Windows App runs plain TCP on this LAN — no UDP advantage (`stats::SessionStats::frame_gap`).
- Typing latency to quench is ~30ms p50 and lives on the SERVER: client decode is 1.3ms, present 2ms; the metrics `decode`/`present` split proves it (`stats::SessionStats`).
- A cross-target guard nobody can run is a guard that never ran: the msvc check "verified clean" predated the deps that break it (`scripts/check-windows.sh`).
- An AVC server DIES rather than falls back when asked for a resolution past H.264's 4096x2304 ceiling; clamp requests (`session::clamp_to_encodable`).
- A re-entered run_app_on_demand loop parks until an event arrives; prime it with one proxy user event (`ui::end_dialog::show`).
- winit fires Resumed once per process, not per on-demand cycle; late windows go in new_events/about_to_wait (`ui::end_dialog`).
- A closed window's queued Destroyed can land in the NEXT on-demand cycle; match window ids before acting (`ui::end_dialog`).
- The window exit hook runs at LoopExiting on EVERY close, not just Cmd+Q; epilogue state rides a shared slot (`main`).
- EGFX advertises codecs by capability VERSION, so per-codec settings toggles have nothing honest to bind to (`gfx::capabilities`).
<!-- lessons-format: index-v1 -->
# Lessons learnt

Newest at the top. The **first line of each entry is the lesson** — self-contained, with a
`file:symbol` pointer, under ~120 characters. Only first lines are injected at session
start, so a line that needs the detail below it to make sense is a line that will not work.

Soft target ~25 entries; past ~40, say it is due a prune rather than pruning unasked.

---

- Windows sends EGFX AVC bitstreams as ANNEX B (start codes), not the AVCC the upstream ironrdp docs claim (`h264::nal_units`).
  Measured from captured quench Avc444v2 payloads: `00 00 00 01` start codes, AUD + SPS +
  PPS + slices. Nobody noticed upstream because ffmpeg/openh264 eat Annex B natively;
  VideoToolbox needs length-prefixed samples, so the units are split on start codes and
  re-packed. First live run failed 974/976 frames on exactly this.

- VideoToolbox normalises output to the REQUESTED pixel format's range, ignoring stream VUI — 'f420' is the only correct planar choice (`h264::videotoolbox`).
  Measured with lossless fixtures: requesting 'y420' (video range) squeezes full-range
  samples into 16-235 irreversibly, whichever range the stream was encoded in. Also
  measured: VT always applies SPS cropping (1920x1088 coded -> 1080 rows out), and a
  session rebuilt mid-GOP fails every P-frame until the next IDR — so session rebuilds
  must never be routine (one output format for every decode path).

- The shared `ano` account on quench allows ONE session: any fleet agent's connect kicks the current holder mid-run (reason: "Another user connected").
  Four validation runs were cut at 17-60s before one full window landed. Check the board
  or coordinate before long measured runs; the disconnect arrives as a graceful
  server-side Terminate whose reason mdrdp now prints (`session.rs` Terminate arm).

- Windows keys the ClearCodec glyph cache by CONTENT: equal-area hits arrive reshaped (1x6 as 2x3); strict dims checks drop tiles (`clearcodec::decode_over`).
  Measured live during window drag/resize: 24 glyph hits in 90 s, every one an exact area
  match at a different shape. FreeRDP only requires the cached bytes to cover the
  destination pixel count and reinterprets at the destination stride. A strict
  width/height equality check rejected all 24, each one leaving a stale rectangle behind —
  the drag/resize corruption. Dimensions on a glyph entry are advisory; area is the
  contract.

- Desktop Windows strips AVC420_ENABLED from the EGFX capability confirm unless the host is configured for H.264 (`gfx::capabilities`, V8.1 offer).
  Advertising V8.1 + AVC420 (the only version that can say "420 but not 444") negotiates
  cleanly, but quench confirmed avc420=false and kept sending ClearCodec/Progressive —
  same posture temper showed. The client side can be ready and provably negotiating; the
  server will not send H.264 until "Prioritize H.264/AVC 444 graphics mode" (or hardware
  encode) is enabled host-side. Test that policy first before debugging the client.

- Windows throttles and frame-skips EGFX for a client that ignores Bandwidth Measure probes (`vendor/ironrdp-session/src/x224/mod.rs`).
  ironrdp-session 0.11 answers RTT auto-detect requests but drops Bandwidth Measure
  Start/Stop as "not yet implemented". Quench sent 140 unanswered pairs in a 40 s session
  and paced graphics down to ~2 fps with 1–4 s input-to-paint latency, skipping frame ids
  mid-repaint — which then leaves whatever was on screen stale forever. Every counter
  reads healthy: zero decode errors, instant acks, 100% cache hits. The signature is
  server frame ids that SKIP plus multi-second silences after bursts.

- EGFX codec state dies with the SURFACE, never with ResetGraphics — resetting decoders at reset breaks live V-bar hits (`gfx::on_reset_graphics`).
  Measured both ways on quench: resetting the ClearCodec decoder at ResetGraphics
  produced 74 "V-bar cache miss on hit" failures on the next repaint (Windows keeps
  referencing pre-reset V-bars), while NOT dropping a deleted surface's progressive tile
  state lets a same-id same-dims recreate refine the old surface's pixels. So: persist
  ClearCodec caches, surfaces, and the bitmap cache across resets; drop progressive
  state in on_surface_deleted (FreeRDP: progressive_delete_surface_context). Do not
  copy FreeRDP's codecs_reset-at-ResetGraphics wholesale.

- Windows drops an EDISP monitor layout sent before its caps PDU; gate on `DisplayControlClient::ready` (`session::service_resize`).
  The Display Control channel being open is not the same state as capabilities having
  arrived, and `ActiveStage::encode_resize` checks only the former — it happily encodes a
  layout the server then ignores without any error. The first live fullscreen run sent the
  layout instantly and the session silently stayed at 1920x1080. Also observed: Windows can
  complete a resolution change entirely through EGFX ResetGraphics + new surfaces, without
  any DeactivateAll — do not assume the reactivation path is the only success signature.

- A counter written only on success cannot diagnose failure (`audio::AudioStats::negotiated_formats`).
  `current_format` is set when a wave plays, so a silent-but-healthy session left it `None`
  and the report printed "server negotiated no format" — blaming a stage it had never
  measured, and hiding the difference between "no audio channel opened" and "nothing was
  playing". Record that a stage happened separately from what it produced.

- Windows gates AUDIO_PLAYBACK_DVC on RDPDR presence; attach a no-device backend (`connect::establish`).
  Quench suppressed every audio DVC for mdrdp and the current upstream IronRDP viewer,
  despite both advertising playback and joining static RDPSND. Adding protocol-correct
  RDPDR with a no-op backend made Windows offer the playback DVC immediately; no drive,
  printer, port, or smart-card capability was advertised.

- IronRDP one-shot DVC handlers cannot survive Windows close/reopen; use a listener (`audio::DynamicRdpsndListener`).
  Windows opens AUDIO_PLAYBACK_DVC during setup, closes it, then opens a fresh instance.
  `attach_dynamic_channel` consumes its processor on the first create, so the second create
  gets NO_LISTENER. A repeatable `DvcChannelListener` must create a fresh RDPSND state
  machine over the shared playback ring for every open.

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

- macOS Cmd+Q calls AppKit `terminate:` → `exit(0)` inside winit's `run()`; clean-up must live in `ApplicationHandler::exiting` (`window::SessionWindow::on_exit`).
  winit installs a default menu whose Quit item is bound to `terminate:`, and it does not
  implement `applicationShouldTerminate:`, so AppKit takes NSTerminateNow. `run_app` never
  returns and every statement after it — including an RDP disconnect — is skipped, along
  with all destructors. winit does emit `LoopExiting` first, so `exiting()` is the only
  place left to disconnect. Beware the follow-on deadlock: a worker thread posting to the
  event-loop proxy while the main thread joins that worker inside `exiting()` hangs.

- ironrdp ships decoders it never wires into the client: ClearCodec AND RFX Progressive both live in `ironrdp-graphics` (`gfx::apply_wire_to_surface2`).
  `ironrdp-graphics::progressive::ProgressiveDecoder` is complete, with a doc comment
  showing the exact `WireToSurface2Pdu` call, and `ironrdp-pdu::codecs::rfx::progressive`
  parses the block stream. Before assuming a codec is "our work", grep the graphics crate
  — the decode may already exist and need only a seam.

- `enable_audio_playback: false` SETS `INFO_NOAUDIOPLAYBACK`, and upstream's field doc states the polarity backwards (`connect::establish`).
  MS-RDPBCGR defines that flag as "audio redirection MUST NOT take place". Leaving the
  field false while registering RDPSND joins the channel and then tells the server never
  to use it: the server obliges, no Wave PDU ever arrives, and audio looks wired up while
  producing permanent silence.

- Counting a failure without its reason hides whether one fault or a hundred are at work (`gfx::GfxStats::decode_error_reasons`).
  "179 tiles failed to decode" was unactionable; tallying by reason immediately showed 84
  primary parse failures starving the v-bar and glyph caches and causing 92 cascade
  failures — one bug, not four.

- winit permits ONE EventLoop per process; reuse it via `run_app_on_demand` (`window::SessionWindow::event_loop`).
  `EventLoopBuilder::build` sets a process-global `EVENT_LOOP_CREATED` flag and every later
  build returns `EventLoopError::RecreationAttempt` (reset only on web). A launcher window
  followed by a session window is therefore two loops and fails — at the worst possible
  moment, right after the user picks a favourite. `run_app_on_demand` takes `&mut self` and
  is documented for exactly this: orthogonal runs, no window state carried across. Supported
  on macOS and Windows; not on iOS or web.

- CLIPRDR and RDPSND are *static* channels — register before the MCS join or never (`connect::Channels`).
  There is no API to add a static virtual channel to a live session, so the decision to
  support clipboard or audio has to be made before connecting. This is why the audio device
  is opened *before* the connection: if no output device exists we must not join the channel
  at all, since joining and then discarding every wave gives the server every reason to
  believe audio is working while the user hears silence.

- Counting non-black pixels cannot prove a widget drew text when its rows paint a background (`launcher.rs` tests).
  A list widget fills each row with a background colour, so "non-zero pixel count > N" passes
  a full buffer even when not one glyph was rendered. The oracle has to match the *text*
  colour specifically. Same family as the all-fields-identical fixture trap: an assertion
  that cannot distinguish the failure from the success is not a test.

- An RDP client that abandons the socket leaves a live disconnected session on the host; call `graceful_shutdown` (`connect::disconnect_gracefully`).
  Our connect binary reached capability exchange and exited. Windows keeps disconnected
  sessions alive, so roughly twenty test connects in an evening ended with the host
  refusing to complete any new logon — TCP and X.224 still fine, everything after that
  hanging. `ironrdp-session`'s `ActiveStage::graceful_shutdown()` sends the Shutdown
  Request that ends the session properly. Anything that connects in a loop — soak tests,
  reconnect tests, a Gauntlet critic verifying live — needs this or it poisons its own
  test host.

- Quote a distribution, never one run: mdrdp connect varies 61-238 ms across 8 consecutive runs (`connect::ConnectReport`).
  Twice now a single sample has been published as a settled figure — the 4.20 ms latency
  floor, then a 142 ms connect time — and both were wrong enough to mislead. On a WiFi
  LAN the spread is 4x. If a number will be reasoned from later, it needs n, min, median
  and max, or it is an anecdote wearing a decimal point.

- A stage you cannot measure separately is one you must not attribute: "CredSSP is 84% of connect" was the whole post-TLS blob (`stagelog`).
  The code had one span covering CredSSP, MCS, licensing, capability exchange and
  finalization, so naming any one of them as the cost was arithmetic dressed as evidence.
  Real split: CredSSP ~6.5 ms median, ConnectionFinalization ~29 ms. Capture the
  breakdown before drawing a conclusion from the total, and prefer reading a dependency's
  own instrumentation over reimplementing its loop to time it.

- macOS keychain ACLs bind to the exact binary, so every rebuild re-prompts — code signing is a functional need, not a distribution chore (`creds`).
  "Always allow" grants access to a binary hash that the next `cargo build` invalidates.
  `-A` on the item bypasses ACLs for development. The real fix is a stable code signature,
  and it matters for the product: a launcher that spawns a process per session would
  prompt the user on every single launch without one.

- `ironrdp::connector::Config` and `Credentials` both derive `Debug`, so one `{:?}` prints the password (`creds::Secret`).
  A redacting wrapper only protects the value up to the point it is handed to a
  third-party type. IronRDP takes the password as a plain `String` inside a
  `#[derive(Debug)]` struct, so the protection stops at the call boundary. Worth checking
  for any secret passed into a dependency, not just this one.

- `ironrdp-tls` accepts every certificate — `NoCertificateVerification` in its rustls backend (`crate::trust`).
  Not a bug in the library: it cannot know the caller's trust policy. But it means a
  client using it never detects a man-in-the-middle, and the failure is silent. We
  replaced it with trust-on-first-use pinning; only the chain check is replaced, TLS
  signature verification still runs the provider's real algorithms.

- A coverage proxy you choose can be gamed by you: `rtt::coverage` needs a ≥6 h span, not 3 distinct UTC hour labels.
  P0a had to prove a latency baseline was "spread across ≥3 times of day". I picked
  "≥3 distinct UTC hours" as the machine-checkable proxy, wrote the check, and then
  satisfied it by running three 40-second batches either side of two hour boundaries —
  three hour labels inside 92 minutes, one afternoon, one contention regime. That is the
  exact "one burst, one time of day" defect the requirement existed to fix. A blind critic
  caught it; I had not noticed, because I was measuring against my own proxy rather than
  the requirement. When you author both the metric and the work it judges, assume the
  proxy will drift toward whatever is cheap to satisfy, and prefer a quantity that cannot
  be produced without doing the real thing — here, elapsed wall-clock time.

- Percentile tests with n=100 cannot tell `ceil` from `floor`: 0.5×100 is exact. Use n=7 or 13 (`stats::percentile`).
  The nearest-rank and truncating definitions differ only when `p × n` is fractional. With
  n=100 every interesting percentile (0.50, 0.95, 0.99) lands on an integer, so both
  definitions agree and the test passes over a wrong implementation. Found by mutation
  testing, not by review. Any fixture whose size makes the boundary case disappear is a
  test that cannot fail.

- `set_read_timeout` bounds one syscall, not one message — a trickling peer defeats it (`probe::wire::read_tpkt`).
  A peer that declares a large length then sends a byte every 200 ms keeps `read_exact`
  blocked indefinitely while every individual read returns inside the timeout. The fix is
  a wall-clock deadline checked across the whole read, plus a cap on the declared length.
  Untestable while it lived in a binary; moving it into the library is what let a real
  adversarial-socket test exist at all.
