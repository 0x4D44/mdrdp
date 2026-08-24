# MDR-BUG-FLU-00093 — Deleting the final mapped surface leaves stale pixels presented indefinitely

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/presentation-lifecycle
- **Raised:** 2026-08-24T09:37:41Z
- **Discovery source:** Agent
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
- **State history:** Open (2026-08-24T09:37:41Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T09:50:26Z, deltic:auto role=fix run=fix-20260824T094417Z-dbd08b66 branch=task/bug-MDR-BUG-FLU-00093-run-fix-20260824T094417Z-dbd08b66 code=2603b58 gate=manual)

## Observation

Observation: SurfaceStore::delete removes the mapped output but retains presentation_fallback, and copy_presentation continues returning that fallback as Copied without a generation change. Painting and mapping a red surface, deleting it, then copying presentation still returns red forever when no replacement surface is mapped. Expected: deleting the final mapped surface invalidates the terminal presentation. Actual: the deleted surface remains visible indefinitely.

## Fix

<unfixed — raised only>

## Notes
