# Scratchpad

Out-of-scope observations. A separate human-invoked review triages these.

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

- [ ] 2026-08-15: ClearCodec regions paint GREY BLOCKS over the desktop, and the logon UI
  renders washed out — while the decoder reports zero errors, so this is silent corruption
  rather than a failure. Visible in
  `/private/tmp/claude-501/-Users-md-language-mdrdp/a29dcc5c-d861-42a3-9b63-aebfd65f8fd8/rlex.png`;
  reproduce with `mdrdp quench --user ano --password-stdin --duration 12 --screenshot f.bmp`.
  The RFX Progressive wallpaper behind them is correct, so this is confined to the
  ClearCodec composite. Not yet investigated: the glyph cache path
  (`vendor/ironrdp-graphics/src/clearcodec/mod.rs`, `glyph_cache.rs`) against FreeRDP's
  `clear_decompress_glyph_data` (freerdp-ref/clear.c:929+), and the residual/bands
  compositing order. Note the blocks predate the rlex fix — they are visible in the
  earlier `run2.png` capture too, so they are a separate defect from the decode errors.

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
  **Blocked on a reference capture**: settling whether tile 11 is wrong or merely
  different needs FreeRDP's output for the same frame, and screencapture returns black
  while the Mac is locked.
