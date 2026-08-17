# Scratchpad

Out-of-scope observations. A separate human-invoked review triages these.

- [ ] 2026-08-17: the diag cache window has no explicit "this server is not using the
  bitmap cache" empty state, so an AVC444 session's blank grid reads as broken rather
  than idle-by-design (`src/diag/cache.rs:grid_caption`; server behaviour proven in
  lessons_learnt 2026-08-17). The session-end summary already says it; the live window
  should too.
- [ ] 2026-08-17: the launcher's `View` submenu is still built with no items, so it opens
  to nothing — the same defect the Help menus had until today
  (`src/shell/mod.rs:menus::LauncherMenu::install`). Either drop it until the sort toggles
  it was reserved for actually exist, or fill it. Left alone because the ask was Help.
- [ ] 2026-08-17: present p50 measured 1.97ms on 08-16 but 6.4-9.1ms in today's runs
  (same code path). Suspect display/backlight state during unattended runs throttles
  the redraw. Worth pinning down before quoting present-segment numbers
  (`stats::SessionStats::present`).
- [ ] 2026-08-16: the UI thread spends most of its CPU on a full-frame copy per present
  (`_platform_memmove` 785 ms + `window::present_into` 294 ms over 75 s, samply against
  quench) even when one caret changed. softbuffer has `present_with_damage`; plumbing
  EGFX dirty rects through `SurfaceStore` to it would cut most of that. CPU cost, not
  latency — present p99 measured 3.6 ms (`src/window.rs:present_into`).
- [ ] 2026-08-16: upstream `ironrdp_blocking::Framed::read` pulls 1024 bytes per syscall
  (`~/.cargo/.../ironrdp-blocking-0.10.0/src/framed.rs:119`); at 7 MB/s of AVC that is
  thousands of read calls/s through rustls. Upstream FIXME acknowledges it. Only worth
  vendoring if session-thread CPU ever matters (it idles ~1% today).
- [x] 2026-08-16: ~~the Windows cross-check dies in libz-sys on this Mac~~ — fixed the
  same day, and the diagnosis was shallow: libz-sys came from sspi's smart-card feature
  (now off by default in the vendored connector, not the image stack), and behind it
  `ring` needed real MSVC headers — the check had in fact NEVER passed with IronRDP in
  the tree (the 2026-08-14 "verified clean" predates commit 77e97ce). Now provisioned
  via xwin and wrapped in `scripts/check-windows.sh`; it promptly caught a real
  Windows-only compile error in `src/shell/mod.rs`.
- [x] 2026-08-16: ~~video-class content is frame-starved without AVC~~ — superseded twice:
  the bandwidth-measure fix alone lifted the same YouTube run to ~25 fps on
  ClearCodec+Progressive, and the VideoToolbox AVC420 decoder is now implemented
  (src/h264.rs) with a V8.1+AVC420 advertisement. Remaining follow-ups below.
- [ ] 2026-08-16: AVC negotiation on quench, measured with the H.264 policy LIVE
  (AVC444ModePreferred=1 + AVCHardwareEncodePreferred=1; gpupdate sufficed, no reboot):
  the server confirms our V8.1 offer at V8.1 but STRIPS AVC420_ENABLED — modern Windows
  has abandoned the legacy 8.1 AVC path. Advertising V10.7 without AVC_DISABLED confirms
  avc420=true avc444=true and the server immediately sends **Avc444v2 for the whole
  desktop**; adding AVC_THIN_CLIENT changes nothing. So hardware H.264 on a
  policy-enabled host requires AVC444v2 support (next entry). Cheap counter-test worth
  one try: delete AVC444ModePreferred (keep AVCHardwareEncodePreferred) and rerun the
  V10.7 probe — Windows may then choose AVC420, which src/h264.rs already decodes.
- [x] (done 2026-08-16, task avc444v2-decode: implemented + validated live, 2933 frames zero errors) AVC444 is unimplemented end-to-end: the vendored egfx client forwards
  Avc444 PDUs to on_unhandled_pdu, and the upstream `H264Decoder` trait (RGBA out) cannot
  express the dual-stream luma+chroma combination AVC444 needs (it must happen in YUV
  space before RGB conversion). Real design work: extend the trait to YUV output or embed
  the combination in the vendored client, mirroring FreeRDP's avc444 path.
- [ ] 2026-08-16: the remote cursor bitmap is not scaled by the viewport — a heavily
  letterboxed window shows a slightly-too-large cursor (src/window.rs
  `apply_remote_cursor`). Scale the RGBA by the letterbox factor if it reads wrong in
  daily use. Also worth a look on Retina: winit CustomCursor pixel dimensions vs points.
- [ ] 2026-08-16: we advertise `SMALL_CACHE` in the EGFX capabilities (src/gfx.rs
  `capabilities()`), capping the server's bitmap cache at the small profile. quench's
  cache already serves ~43% of painted pixels; dropping the flag gives the server a
  bigger cache and should cut wire traffic further, at the cost of client memory.
  Measure before/after (`bytes_from_wire` in --metrics-json) before keeping it.
- [ ] 2026-08-16: the presenter (src/window.rs `present_into`) rescales the full frame
  on the CPU every redraw — at 5120x2880 that is a ~56 MB pass per frame even when one
  tile changed. Damage-rect-aware presentation (the store already tracks a generation;
  it could track dirty rects) or a GPU present path would cut the cost.
- [ ] 2026-08-16: `SurfaceStore::surface_to_surface` (src/surface.rs:323) blits with the
  UNCLIPPED source width as stride, but `extract` clips — a source rect overhanging the
  source surface would produce rows narrower than the stride and fail as ShortSource (or
  shear). Servers do not send such rects, so this is robustness, not a live bug.
- [x] 2026-08-16: ~~the session's tracing calls go nowhere~~ — resolved: `MDRDP_LOG=<filter>`
  now installs a global stderr subscriber (src/main.rs `install_diagnostics_subscriber`);
  a normal run is unchanged.
- [ ] 2026-08-16: `SessionCommand::Resize` reactivation path (DeactivateAll) is written
  but live-unexercised — quench resizes via EGFX ResetGraphics instead. Worth exercising
  against a server that takes the DeactivateAll route before trusting it fully.

- [ ] 2026-08-15: the mandated Windows cross-check no longer runs on this Mac.
  `cargo check --target x86_64-pc-windows-msvc --all-targets` dies in `ring`'s build
  script (`cc` targeting windows-msvc cannot find `assert.h`), so the gate
  `~/language/mdrdp/CLAUDE.md` requires after any platform-facing change is
  unavailable, not merely unrun. The failure is in a dependency's C build, independent of
  our own `cfg` usage, so the check cannot currently catch the drift it exists to catch.
  Fixing it means supplying the MSVC CRT/SDK headers (e.g. `xwin`) or moving the rustls
  provider off `ring`. Until then say "Windows check UNAVAILABLE", never "clean" — and
  `~/language/mdrdp/CLAUDE.md`'s "Verified clean as of 2026-08-14" line is stale.

- [ ] 2026-08-15: two nearest-rank percentile implementations —
  `~/language/mdrdp/src/probe/stats.rs:30` (`percentile`, `&[u64]`, f64 fraction) and
  `~/language/mdrdp/src/stats.rs:127` (`nearest_rank`, `&[u32]`, u32 percent).
  Same statistic, two bodies. Both are pinned by hand-computed tests so they cannot silently
  disagree today, which is why this is parked rather than fixed. Unify behind one generic
  helper if a third caller appears, or if either grows past the plain formula.

- [ ] 2026-08-15: RFX Progressive renders a photographic wallpaper wrongly (~85% pure
  black on Quench's logon screen) while ClearCodec text renders perfectly.
  `~/language/mdrdp/src/gfx.rs:apply_wire_to_surface2`. Reproduce and capture with
  `mdrdp <host> --screenshot frame.bmp`.
  **Three hypotheses tested and REJECTED — do not repeat them:**
  1. *ctx 2 never gets a CONTEXT block* — patched a vendored `ironrdp-graphics` to fall
     back to an already-established context. The refinement frames then decoded and the
     image got WORSE: 60% black, garish posterisation.
  2. *Tile state keyed by context instead of surface* — passed `surface_id` as the context
     key so every frame shared one tile grid. Black fell to 16.6%, but the wallpaper
     became a flat blue field.
  3. *Per-region DWT variant ignored* — `ProgressiveRegion::uses_reduce_extrapolate()`
     exists and upstream uses the CONTEXT's value for every region. Instrumented it: every
     region on this server reports `re=true`, identical to the context. Changes nothing here.
  Measured facts: one surface; ctx 1 carries SYNC+CONTEXT (`re=true`) then frames without
  CONTEXT; ctx 2 never receives a CONTEXT block; every region has quant=1 and 510 tiles
  (a full 1920x1080 grid); the third frame of each context has `prog=0`, which is what
  raises `quant index 255 exceeds table length 0`.
  **Next step is a REFERENCE, not another guess:** capture the same desktop with FreeRDP
  (`sdl-freerdp`, not `xfreerdp`) or Microsoft's client and diff against ours. Without
  ground truth each change merely moves the corruption around — which is precisely what
  all three attempts above did.

- [ ] 2026-08-15: ClearCodec RLEX subcodec still fails with `rlex: suite exceeds region
  pixel count` (`~/language/mdrdp/vendor/ironrdp-graphics/src/clearcodec/mod.rs:323`).
  Ruled out already: the segment bit extraction matches FreeRDP
  (`stop_index = packed & stop_mask`, `suite_depth = (packed >> stop_index_bits) & depth_mask`,
  `start_index = stop_index - suite_depth`), the variable-length run-length decode matches,
  and `remaining` being ignored is harmless because it equals the cursor length. So the
  overrun is in pixel accounting or in how much data reaches `decode_rlex`. Compare against
  `clear_decompress_subcode_rlex` (freerdp-ref/clear.c:151-300), noting that FreeRDP bounds
  its loop by the declared `bitmapDataByteCount` and writes `runLengthFactor` pixels of
  `palette[startIndex]` followed by `suiteDepth + 1` pixels stepping the palette.

- [ ] 2026-08-15: ONE progressive tile renders wrong — grid (11, 0), i.e. pixels
  x 704-767, y 0-63. Consistently ~+21 R, +11 G, -5 B against its neighbours (deviation
  exactly 38 in every capture). Everything else on screen is correct.
  Reproduce: `mdrdp quench --user ano --password-stdin --duration 14 --screenshot f.bmp`,
  then compare the mean colour of 64px tile 11 in row 0 against tiles 10 and 12.
  **Ruled out by experiment, do not repeat:**
  1. *Upgrade passes* — skipping every TILE_UPGRADE leaves the deviation at 39. It comes
     from the first pass.
  2. *The bitmap cache* — skipping every CacheToSurface leaves it at exactly 38.
  3. *Tile state contamination* — decoding the same tile data into a pristine `TileState`
     and comparing reconstructions gives a mean absolute difference of 0.00, so our decode
     is deterministic and carries nothing across frames.
  4. *Content dependence* — the artefact sits at the same tile with the same deviation
     across two completely different Spotlight wallpapers, while the tile's colour tracks
     the wallpaper. So it decodes real content, consistently, but wrongly.
  Note the tile's FIRST-pass chroma is unusually short (cr = 6 bytes where its neighbour
  has 17-20), which is the most promising lead: a short RLGR chroma stream may be
  mis-decoded. The workflow's RLGR comparison found only an ENCODER divergence
  (UP_GR vs UQ_GR) and judged the decoder to match, so that would need re-checking
  specifically for short/exhausted streams.
  **No longer blocked on the reference capture** (2026-08-16): two FreeRDP reference
  captures are now in the repo at `~/language/mdrdp/baseline/freerdp-reference/`
  (`ref2-full.png`, `ref3-full.png`, 5120x2880 Retina grabs of the FreeRDP window), and
  `~/language/mdrdp/tools/codec-oracles/refdec.c` decodes a captured Progressive
  payload through FreeRDP's own `progressive_decompress` and prints per-tile means. Both
  were rescued from session scratch that was about to be deleted. What is still needed is
  the comparison itself, not the evidence to do it with.
  The RLGR encoder divergence above is confirmed and reproducible:
  `cd ~/language/mdrdp/tools/codec-oracles/rlgrdiff && cargo run --release`
  reports `ref!=orig = 2831` of 3000, and `FIX_UQ=1 cargo run --release` reports 0.
  `~/language/mdrdp/vendor/ironrdp-graphics/src/rlgr.rs:125` uses `UP_GR` where
  its own decoder at line 322 uses `UQ_GR`. Encode is server-side and mdrdp never runs
  it, so this is latent, not a shipped defect — but it means the default run of that
  oracle tests decoder agreement over malformed input, not correctness.
- [ ] 2026-08-16: `cargo check --target x86_64-pc-windows-msvc --all-targets` fails on the UNTOUCHED trunk: ring + libz-sys C builds get "sys/types.h / assert.h not found" (host clang, msvc target, no Windows SDK). Environment drift since the 2026-08-14 "verified clean" note in CLAUDE.md — needs xwin/clang-cl setup or the note revising. Blocks the mandated Windows cross-check for all current work.
- [ ] 2026-08-17: the §7 epilogue dialogs are fixed-size with the footer stacked under the content, so a short message leaves ~120px of bare background under the button row (`~/language/mdrdp/src/ui/end_dialog.rs:draw`). Fine when the Lost dialog carries a long error, odd for a two-line server reason. Either size to content or pin the footer to the bottom.
- [ ] 2026-08-17: `~/language/mdrdp/src/presence.rs:355` sizes the `--sessions` columns with `str::len` (bytes) but pads with `{:<w$}` (chars), so a favourite name or account holding any non-ASCII character over-pads its column by one space per extra byte. Cosmetic only, and it predates the colour work; `chars().count()` is the fix, with a test carrying an accented favourite name.
