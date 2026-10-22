# MDR-BUG-FLUX-00007 — AVC444 luma pass clobbers delivered chroma: colour of dithered content pumps on LC=1/LC=2 alternation

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** gfx
- **Raised:** 2026-08-19T06:42:42Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20261022T232503Z-fedab8da
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLUX-00007-run-verify-20261022T232503Z-fedab8da
- **Owner base:** 4e0e213ae3c602b4da35a7268e79003e06f4f84e
- **Owner fingerprint:** sha256:bc43cb0891d30b84bfa02af7b0d7ec7cc618b3894bd139b21bf95223e6f24026
- **Owner since:** 2026-10-22T23:25:03Z
- **Owner until:** 2026-10-23T03:25:03Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-19T06:42:42Z, raised via `deltic bugs new`); Fixed (2026-08-19T07:05:00Z, claude on flux, fix commit 4763ba49ebc102dfa69ca3f16282d39536a597ff on task branch — regression tests proven red-then-green, oracle 10/10, offline replay of captured payloads stable; integrated code=c5f2debaf649789e4eb114da14fd86806993281e)

## Observation

Reported by Arthur 2026-08-18: certain UI elements — dithered/stippled fills, e.g.
a btop-style meter's `▒` shade-block tail — show colour instability in an mdrdp
session: the region's tone visibly keeps changing while the content is static.
Reproduced live against temper 2026-08-19 (PowerShell drawing a DarkYellow bar with
a moving `█` head and static `▒` tail, redrawn 5x/s): Arthur watched the dithered
half pump between two tones across two separate sessions.

Measured offline (32 captured AVC444v2 payloads replayed through the production
pipeline, `examples/avcreplay.rs`): temper's steady state alternates luma-only
(LC=1) and chroma-only (LC=2) updates. Each chroma frame delivers correct 4:4:4
colour (probe mean B≈5); the next luma-only frame degraded the same region back to
replicated 4:2:0 block averages (probe mean B≈37), flipping 33,000 of 48,000 probe
pixels by up to 98 RGB units — at every L↔C transition, i.e. continuous visible
pumping.

Root cause: `Yuv444Buffer::apply_luma` (vendored `ironrdp-graphics/src/avc444.rs`)
replicated the main frame's 2x2-averaged chroma into ALL four positions of each
block, overwriting the true odd-position samples previously delivered by aux
chroma frames. The encoder's contract is that a luma-only update leaves delivered
chroma intact (it re-sends chroma only when chroma changed). The replication was
faithfully transcribed from FreeRDP, which carries the same defect; mstsc does not
pump on the same content.

## Fix

`Yuv444Buffer` now tracks which 2x2 blocks have received true aux chroma
(`chroma_seen`, one bit per block, set by both chroma passes for fully-covered
blocks only). `apply_luma` writes the main frame's average only to the even/even
position (its MS-RDPEGFX home, B2/B3) in chroma'd blocks, preserving delivered
odd-position samples; virgin blocks keep the old full replication so first paint
ahead of any chroma pass still degrades to 4:2:0 rather than neutral grey.
Documented as the module's second deliberate FreeRDP divergence.

Validation: regression tests `a_luma_pass_preserves_previously_delivered_chroma_detail`
and `a_luma_pass_still_replicates_where_no_chroma_was_delivered` (both proven red
with the fix disabled, green with it); FreeRDP differential oracle
(`tools/codec-oracles/avc444diff`) still 10/10; offline replay of the same 32
captured payloads shows zero moved pixels across all L/C alternations after the
first chroma delivery; full suite 577+13+5 green; clippy/fmt clean;
check-windows.sh green; live temper sessions re-verified.

## Notes

Repro capture and replay: `mdrdp <host> --capture-failures <dir>` then
`cargo run --release --example avcreplay -- <dir> <l,t,r,b>`.

**Live post-fix confirmation (2026-08-19, Arthur, temper, v0.1.69):** watching the
original bar scene, both the solid and dithered regions now hold a steady tone —
the pumping is gone. One expected residual: when the bar head retreats
(solid `█` → dither `▒`), the newly-dithered region shows a blue tinge for
roughly one frame. That is the documented information-forced transient: the
luma-only frame carrying the content change paints against one-frame-stale odd
chroma, and the even/even reconstruction `4*avg - neighbours` overshoots U
upward (blue) until the next chroma-only frame lands. Pre-fix, the same frame
showed a flat 4:2:0 average instead — but then pumped continuously. A possible
refinement (fall back to the flat average in blocks whose luma changed
substantially, until the next chroma pass) is noted for a separate task if the
transient proves bothersome on real content.
