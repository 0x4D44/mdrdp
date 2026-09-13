# MDR-BUG-FLU-00122 — RDP keystrokes can stall until the Kiln session reconnects

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/input
- **Raised:** 2026-08-25T10:06:15Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T055445Z-3e34e297
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00122-run-verify-20260913T055445Z-3e34e297
- **Owner base:** cc25bb1699abc168c6cfd50232dd915672585036
- **Owner fingerprint:** sha256:691c19757c47b4ec8c481c0ca0f3f65d08ea2975b525386bbb21c03bed4550aa
- **Owner since:** 2026-09-13T05:54:45Z
- **Owner until:** 2026-09-13T07:54:45Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-25T10:06:15Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-25T23:00:44Z, deltic:auto role=fix run=fix-20260825T221643Z-587da9f0 branch=task/bug-MDR-BUG-FLU-00122-run-fix-20260825T221643Z-587da9f0 code=2dc13af4107778571be01676b027596229b62bba gate=manual)

## Observation

On Kiln with mdrdp v0.1.234, an established RDP session stopped accepting keystrokes or delayed them beyond five seconds. Arthur abandoned the wait and reconnected; keystrokes worked immediately in the replacement session. Expected: reliable key transitions reach the server promptly for the lifetime of a connected session. Actual: session-local input delivery can remain unusable until reconnect. The failed session ended gracefully with AVC444v2 active and no decode or surface errors, so the report does not establish whether the stall is before wire delivery, in the server input path, or only in paint feedback; diagnose those stages before changing the protocol.

## Fix

<unfixed — raised only>

## Notes
