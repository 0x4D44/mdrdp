# MDR-BUG-FLU-00043 — Rhydra NV12 converter pool can grow GPU memory without bound

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/video-memory
- **Raised:** 2026-08-22T19:40:40Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T133831Z-735aea9e
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00043-run-fix-20260823T133831Z-735aea9e
- **Owner base:** f265c46e547b899b6a9e4dbbac84197778888698
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T13:38:31Z
- **Owner until:** 2026-08-23T15:38:31Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

tools/latency-spike/server/src/win/convert.rs:231-255 allocates another GPU surface whenever all current surfaces remain leased. The capture loop at pipeline.rs:1056-1099 keeps converting tiles, so an MFT that accepts input without retiring output can grow the pool until GPU allocation fails. Bound the pool and apply backpressure or fail explicitly.

## Fix

The converter now owns a fixed four-slot lease budget per tile and never allocates
another NV12 texture after startup. Before each captured frame, the pipeline pumps
every tile encoder and releases its retired surfaces. If any tile still has no free
slot, it discards the complete desktop frame before conversion, rect emission, or
pixel-diff baseline advancement. This preserves tile and decoder coherence while
bounding the normal 5K two-tile NV12 pool at about 88 MiB. Persistent saturation
for 500 ms exits explicitly so the session supervisor rebuilds the encoder rather
than leaving a live process with permanently frozen video.

Portable lease tests prove exhaustion refuses a fifth lease, released slots are
reused round-robin, and invalid releases fail. The recovery test proves a frame
discarded before encoding increments loss telemetry without needlessly entering
keyframe recovery.

## Notes
