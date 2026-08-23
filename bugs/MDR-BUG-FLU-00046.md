# MDR-BUG-FLU-00046 — A 256-event RDP input burst terminates the session

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** session/input
- **Raised:** 2026-08-23T11:57:47Z
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
- **State history:** Open (2026-08-23T11:57:47Z, raised via `deltic bugs new`) -> Fixed (2026-08-23T12:07:12Z, deltic:auto role=fix run=fix-20260823T115827Z-c9dcbf14 branch=task/bug-MDR-BUG-FLU-00046-run-fix-20260823T115827Z-c9dcbf14 code=59fcb85 gate=manual)

## Observation

The RDP pump drains every queued input event into one fast-path PDU. Fast-path has an 8-bit event count and rejects 256 or more events, so a normal accumulated burst returns a protocol error and ends the live session. Expected: input is sent in bounded valid batches and the pump yields to inbound work while excess events remain queued.

## Fix

<unfixed — raised only>

## Notes
