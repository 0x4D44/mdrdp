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
