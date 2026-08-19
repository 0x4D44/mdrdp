# MDR-BUG-FLUX-00010 — AVC444: one-frame colour overshoot (blue tint) when content changes under a luma-only frame

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** gfx
- **Raised:** 2026-08-19T07:20:46Z
- **Discovery source:** Human
- **Owner:** -
- **Owner role:** -
- **Owner run:** -
- **Owner host:** -
- **Owner branch:** -
- **Owner base:** -
- **Owner fingerprint:** -
- **Owner since:** -
- **Owner until:** -
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-19T07:20:46Z, raised via `deltic bugs new`); Fixed (2026-08-19T08:15:00Z, claude on flux, fix commit 0345af8aa8fe33d4cdcbe98879a1a1ddb5ba2c85 on task branch — regression tests proven red-then-green, oracle 10/10, replay blue-flips 1,706 -> 0)

## Observation

Reported by Arthur 2026-08-19, watching the MDR-BUG-FLUX-00007 repro scene live on
temper (v0.1.69, which fixed the steady-state pumping): when the moving bar head
retreats (solid `█` -> dither `▒`), the newly-dithered dark-yellow region shows a
**blue tinge for about one frame**. A second report the same session widened the
scope: **large redraws (minimising/restoring a window) draw the restored contents
with wrong colours** until the chroma catches up.

Mechanism: Windows ships a content change's luma in a luma-only (LC=1) frame and
the matching chroma in a later aux frame (observed next-frame to ~1.4 s). During
the gap the block's preserved odd-position chroma still describes the *previous*
content, and paint-time reconstruction `4*new_avg - 3*stale` overshoots into hues
never on screen — dark yellow has low U, the new dither average sits near neutral,
so U overshoots upward: blue. (Pre-FLUX-00007 the same frame showed a flat 4:2:0
average — less garish per frame, but pumped continuously; the preservation fix
made the gap frame *worse* in exchange for fixing the steady state.)

Measured offline (`examples/avcreplay.rs` over the 32 captured temper payloads,
probe 8,40,808,280): the retreating-bar frame has **1,656 pixels flipping to
blue-dominant** (B > R + 20) for exactly one frame on content yellow-dominant on
both neighbouring frames, plus ~10-pixel blue-flip bursts at every head jump —
1,706 wrong-hue pixels across the run. The first-paint frames (the window-restore
analogue) render the region at mean B 71 against a true post-chroma value of 8.4.

## Fix

`Yuv444Buffer` gains a `chroma_stale` per-2x2-block bitset (vendored
`ironrdp-graphics/src/avc444.rs`). The luma pass sets a block's bit when its
delivered average moves more than `STALE_AVG_DELTA` (10) from the stored one in a
chroma'd block — sized from the captured payloads, where re-encode noise on
unchanged content tops out at 6 and genuine change starts at 41. Both chroma
passes clear the bit for blocks they fully cover. `to_rgba_into` paints a stale
block entirely from its even/even average — the correct hue at 4:2:0 fidelity —
instead of showing or reconstructing against the stale odd samples; full 4:4:4
detail returns when the catch-up lands. Nothing is destroyed: delivered odd
samples stay in the buffer throughout.

Validation: regression tests
`a_changed_average_under_a_luma_only_frame_paints_flat_until_chroma_catches_up`
and `an_unchanged_average_under_a_luma_only_frame_keeps_painting_detail`, both
proven red with the fix disabled (the first fails with painted `[255,23,255]` —
the magenta-blue overshoot — vs correct `[73,120,68]`). Replay of the same 32
payloads: blue-flips 1,706 -> **0**; first-paint mean B 71 -> 25 (true value 8.4
is the information-forced 4:2:0 floor until chroma arrives). FreeRDP differential
oracle still 10/10 (virgin path untouched). Full suite, clippy `-D warnings`,
fmt, `check-windows.sh` all green.

## Notes

Successor to MDR-BUG-FLUX-00007 (the steady-state pumping); the gap-frame
transient is that fix's documented trade-off, now closed. The deliberate residual
is one frame (up to the observed ~1.4 s worst case) of 4:2:0-flat colour on
freshly changed blocks — the correct hue, softened — which no client can avoid:
an LC=1 frame asserts "chroma unchanged" and the client cannot distinguish that
from "chroma changed but not yet sent". Replay tooling: `avcreplay` now prints
per-frame `avgd` average-delta distributions (sized the threshold) and a
post-run one-frame transient report (`spike` and `blueflip` counts).
