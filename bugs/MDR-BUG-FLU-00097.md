# MDR-BUG-FLU-00097 — AVC444 split chroma rectangles lose supplied detail

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/AVC444
- **Raised:** 2026-08-24T11:06:32Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T064734Z-aaf23e3b
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00097-run-verify-20260914T064734Z-aaf23e3b
- **Owner base:** a962df101232d1bd5f8d75e2ed42ad09061b1ade
- **Owner fingerprint:** sha256:9b2469215962444ae6fd46a6e5fdbe0781a487fc0e0fea0f6b8ad1d649d6b6f3
- **Owner since:** 2026-09-14T06:47:34Z
- **Owner until:** 2026-09-14T08:47:34Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T11:06:32Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T11:19:25Z, deltic:auto role=fix run=fix-20260824T110708Z-fad9c8b0 branch=task/bug-MDR-BUG-FLU-00097-run-fix-20260824T110708Z-fad9c8b0 code=abf5599 gate=manual)

## Observation

Two adjacent auxiliary chroma rectangles can collectively cover one 2x2 luma block, but avc444.rs marks chroma_seen only when one rectangle covers the whole block. A following luma update treats that block as missing chroma and overwrites the valid auxiliary U/V samples with replicated 4:2:0 averages, causing colour leakage and fuzzy coloured edges.

## Fix

<unfixed — raised only>

## Notes
