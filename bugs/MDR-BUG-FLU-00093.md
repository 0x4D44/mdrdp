# MDR-BUG-FLU-00093 — Deleting the final mapped surface leaves stale pixels presented indefinitely

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/presentation-lifecycle
- **Raised:** 2026-08-24T09:37:41Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T094417Z-dbd08b66
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00093-run-fix-20260824T094417Z-dbd08b66
- **Owner base:** 82af91e62d793b258fb83feb3e6623ae6ce30fda
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T09:44:17Z
- **Owner until:** 2026-08-24T11:44:17Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T09:37:41Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

Observation: SurfaceStore::delete removes the mapped output but retains presentation_fallback, and copy_presentation continues returning that fallback as Copied without a generation change. Painting and mapping a red surface, deleting it, then copying presentation still returns red forever when no replacement surface is mapped. Expected: deleting the final mapped surface invalidates the terminal presentation. Actual: the deleted surface remains visible indefinitely.

## Fix

<unfixed — raised only>

## Notes
