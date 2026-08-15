<!-- lessons-format: index-v1 -->
# Lessons learnt

Newest at the top. The **first line of each entry is the lesson** — self-contained, with a
`file:symbol` pointer, under ~120 characters. Only first lines are injected at session
start, so a line that needs the detail below it to make sense is a line that will not work.

Soft target ~25 entries; past ~40, say it is due a prune rather than pruning unasked.

---

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
