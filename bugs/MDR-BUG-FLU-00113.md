# MDR-BUG-FLU-00113 — Native sparse and regional updates rescan the full codec grid for each damage region

- **State:** Fixed
- **Priority:** Must
- **Severity:** Medium
- **Area:** native/latency
- **Raised:** 2026-08-24T17:13:35Z
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
- **State history:** Open (2026-08-24T17:13:35Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T17:20:24Z, deltic:auto role=fix run=fix-20260824T171359Z-6508958c branch=task/bug-MDR-BUG-FLU-00113-run-fix-20260824T171359Z-6508958c code=00c498b gate=manual)

## Observation

At 5120x2880, BlockFences::visible allocates and scans a 57,600-entry full-canvas bitmap on every call. SparseSink discards one global result and then repeats the full scan per rectangle; NativeSink does the same per decoded regional tile. Tiny damage therefore pays full-canvas work multiple times directly on decode-to-paint, adding avoidable latency and allocation pressure. Validate aggregate alignment/overlap once, then derive visibility by scanning only the blocks inside each already-validated damage set.

## Fix

`BlockFences` now validates aggregate geometry and overlap once, using storage
proportional to the damaged blocks. The later precedence selection walks only each
already-validated region, then restores row-major block order before coalescing. Sparse
and regional video paths no longer allocate or scan a full-canvas bitmap once globally
and again for every rectangle or tile.

The 5K regression instruments selection work: one 16x16 damaged block inspected all
57,600 grid entries before the fix and exactly one afterward. A second red-then-green
test preserves the old row-major output when regions arrive in reverse order. All 41
native-session tests and strict focused Clippy pass.

## Notes
