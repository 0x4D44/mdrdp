# MDR-BUG-FLU-00122 — RDP keystrokes can stall until the Kiln session reconnects

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/input
- **Raised:** 2026-08-25T10:06:15Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260825T221643Z-587da9f0
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00122-run-fix-20260825T221643Z-587da9f0
- **Owner base:** d5d749b27ed4ce74bacfe1cec82411b13f8d6434
- **Owner fingerprint:** -
- **Owner since:** 2026-08-25T22:16:43Z
- **Owner until:** 2026-08-26T00:16:43Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-25T10:06:15Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

On Kiln with mdrdp v0.1.234, an established RDP session stopped accepting keystrokes or delayed them beyond five seconds. Arthur abandoned the wait and reconnected; keystrokes worked immediately in the replacement session. Expected: reliable key transitions reach the server promptly for the lifetime of a connected session. Actual: session-local input delivery can remain unusable until reconnect. The failed session ended gracefully with AVC444v2 active and no decode or surface errors, so the report does not establish whether the stall is before wire delivery, in the server input path, or only in paint feedback; diagnose those stages before changing the protocol.

## Fix

<unfixed — raised only>

## Notes
