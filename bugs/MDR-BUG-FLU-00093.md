# MDR-BUG-FLU-00093 — Deleting the final mapped surface leaves stale pixels presented indefinitely

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/presentation-lifecycle
- **Raised:** 2026-08-24T09:37:41Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T145238Z-e2e013ef
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00093-run-verify-20260913T145238Z-e2e013ef
- **Owner base:** ca158ccf7b4ee3e5b635f87a99777cf67660fecf
- **Owner fingerprint:** sha256:4b44cb460dd13b68649a6a9e91cc6533500891aca72994251e279fa7e30bea1f
- **Owner since:** 2026-09-13T14:52:38Z
- **Owner until:** 2026-09-13T16:52:38Z
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
