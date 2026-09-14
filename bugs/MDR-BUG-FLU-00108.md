# MDR-BUG-FLU-00108 — Native reliable input can starve resize and visibility commands

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** native/input-latency
- **Raised:** 2026-08-24T12:24:25Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T090513Z-67f921de
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00108-run-verify-20260914T090513Z-67f921de
- **Owner base:** 52fa0e1ebc4f879be69b8bbe759532b30c336138
- **Owner fingerprint:** sha256:e30a5fea6631a363dea53f9238b2e309bfbd6b3c5de4b75e1edd0a5e6c58a5a8
- **Owner since:** 2026-09-14T09:05:13Z
- **Owner until:** 2026-09-14T11:05:13Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T12:24:25Z, raised via `deltic bugs new`) -> Fixed (2026-08-24T12:45:29Z, deltic:auto role=fix run=fix-20260824T123646Z-570c4a3a branch=task/bug-MDR-BUG-FLU-00108-run-fix-20260824T123646Z-570c4a3a code=303bb5a gate=manual)

## Observation

The native input loop drains the reliable event queue until empty before servicing resize and visibility commands. A continuously fed reliable or scripted stream can therefore starve commands and focus-loss releases indefinitely. Bound each reliable-input batch and interleave command service, with a regression that keeps enqueueing input while requiring bounded command delivery.

## Fix

<unfixed — raised only>

## Notes
