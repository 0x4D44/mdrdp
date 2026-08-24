# MDR-BUG-FLU-00104 — Non-AVC surface updates leave stale AVC444 detail attached

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/codec-switch
- **Raised:** 2026-08-24T12:03:48Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T121141Z-3afa245c
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00104-run-fix-20260824T121141Z-3afa245c
- **Owner base:** c9a3c46d685b1a86bcc857e3b888202fc70e4962
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T12:11:41Z
- **Owner until:** 2026-08-24T14:11:41Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T12:03:48Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

GraphicsPipelineClient retains each surface AVC444 buffer across SolidFill, SurfaceToSurface, CacheToSurface, Uncompressed, AVC420, ClearCodec, and Progressive updates. A later LC1 luma preserves old chroma_seen samples and can repaint stale color over newer text or fills. Invalidate the destination surface AVC444 state on every non-AVC mutation and prevent chroma-only repaint until a fresh luma baseline exists.

## Fix

<unfixed — raised only>

## Notes
