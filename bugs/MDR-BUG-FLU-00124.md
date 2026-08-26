# MDR-BUG-FLU-00124 — AVC444 refinement debounce starves continuous-motion presentation

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/presentation
- **Raised:** 2026-08-26T13:20:26Z
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
- **State history:** Open (2026-08-26T13:20:26Z, raised via `deltic bugs new`) -> Fixed (2026-08-26T14:07:48Z, deltic:auto role=fix run=fix-20260826T132111Z-fdc36f77 branch=task/bug-MDR-BUG-FLU-00124-run-fix-20260826T132111Z-fdc36f77 code=4ba656c2588a654d2c5a250bbbe713029e476c3c gate=manual)

## Observation

Arthur reports that full-screen window dragging feels very sticky in mdrdp 0.1.237 against both Crucible and Kiln. The behavior appeared after the 100 ms AVC444 chroma-refinement settling change. Expected: fresh picture frames remain promptly visible throughout continuous motion while late chroma-only refinements settle without a 4:2:0-to-4:4:4 flash. Actual: coalesced LC1 and LC2 damage can leave only the LC2 deadline visible to the window thread, and repeated refinements move that deadline forward until motion pauses.

## Fix

<unfixed — raised only>

## Notes
