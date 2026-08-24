# MDR-BUG-FLU-00097 — AVC444 split chroma rectangles lose supplied detail

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/AVC444
- **Raised:** 2026-08-24T11:06:32Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T110708Z-fad9c8b0
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00097-run-fix-20260824T110708Z-fad9c8b0
- **Owner base:** a01bfd14b28d78d7af7304e944133c95384aca33
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T11:07:08Z
- **Owner until:** 2026-08-24T13:07:08Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T11:06:32Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

Two adjacent auxiliary chroma rectangles can collectively cover one 2x2 luma block, but avc444.rs marks chroma_seen only when one rectangle covers the whole block. A following luma update treats that block as missing chroma and overwrites the valid auxiliary U/V samples with replicated 4:2:0 averages, causing colour leakage and fuzzy coloured edges.

## Fix

<unfixed — raised only>

## Notes
