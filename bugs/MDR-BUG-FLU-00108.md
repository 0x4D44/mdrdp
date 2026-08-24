# MDR-BUG-FLU-00108 — Native reliable input can starve resize and visibility commands

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** native/input-latency
- **Raised:** 2026-08-24T12:24:25Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T123646Z-570c4a3a
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00108-run-fix-20260824T123646Z-570c4a3a
- **Owner base:** f61afa141bffd5dfde0fc6e62d21d7238687bd55
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T12:36:46Z
- **Owner until:** 2026-08-24T14:36:46Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T12:24:25Z, raised via `deltic bugs new`)

## Observation

The native input loop drains the reliable event queue until empty before servicing resize and visibility commands. A continuously fed reliable or scripted stream can therefore starve commands and focus-loss releases indefinitely. Bound each reliable-input batch and interleave command service, with a regression that keeps enqueueing input while requiring bounded command delivery.

## Fix

<unfixed — raised only>

## Notes
