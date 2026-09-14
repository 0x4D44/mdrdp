# MDR-BUG-FLU-00104 — Non-AVC surface updates leave stale AVC444 detail attached

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/codec-switch
- **Raised:** 2026-08-24T12:03:48Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T083004Z-d8306d1e
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00104-run-verify-20260914T083004Z-d8306d1e
- **Owner base:** f17cd847d8810d010683faec1a1c58afde7eb63c
- **Owner fingerprint:** sha256:a5b3b2e74287be75a9018a4f82ae7759faf9f82f2737c491d4e5414262ddffaf
- **Owner since:** 2026-09-14T08:30:04Z
- **Owner until:** 2026-09-14T10:30:04Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T12:03:48Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-24T12:22:44Z, deltic:auto role=fix run=fix-20260824T121141Z-3afa245c branch=task/bug-MDR-BUG-FLU-00104-run-fix-20260824T121141Z-3afa245c code=0db0fa9 gate=manual)

## Observation

GraphicsPipelineClient retains each surface AVC444 buffer across SolidFill, SurfaceToSurface, CacheToSurface, Uncompressed, AVC420, ClearCodec, and Progressive updates. A later LC1 luma preserves old chroma_seen samples and can repaint stale color over newer text or fills. Invalidate the destination surface AVC444 state on every non-AVC mutation and prevent chroma-only repaint until a fresh luma baseline exists.

## Fix

<unfixed — raised only>

## Notes
